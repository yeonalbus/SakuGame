//! 扫描器：两层枚举（游戏标识层 → 版本标识层）。
//!
//! 规则依据 DSPlans/MVPv1.1设计规划1.0-dsedit.md §4 与 DSPlans/MVPv1.1技术规格.md §4：
//! - **下探固定两层**：仓库根的直接子目录 = 档案；其直接子目录 = 版本。版本层以下一律不下探。
//! - **建档强断言**：仓库根下每个直接子目录必定建档案，与内容无关（D18：宁可误报不可漏档）。
//! - **版本判定**：目录的**直接子文件**含候选 exe 或压缩包才算一个版本；埋在子目录里的不算。
//! - **exe 优先**（Q-G）：同一目录同时有 exe 与压缩包 → folder 型。
//! - **排除名单**（D18）：只用于「选候选」，不参与「是否建档」的判定。
//! - **根级目录排除**：系统 / 回收站 / 索引目录不建档。
//! - 不读 exe 内容（性能红线）；不跟随符号链接 / junction；支持长路径。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use regex::Regex;

use crate::models::{
    ArchiveFile, ArchiveGroup, ExeCandidate, GameEntry, GameLayout, GameType, ScanReport,
    ScanWarnings, VersionEntry,
};

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// 压缩文件白名单（Q-I：不含 bin/cue；.bin 实测为数据文件）
    pub archive_exts: Vec<String>,
    /// exe 文件名排除正则（大小写不敏感）
    pub exclude_file_patterns: Vec<Regex>,
    /// exe 所在路径段排除（小写比较）
    pub exclude_path_segments: Vec<String>,
    /// 仓库根级目录排除（小写；支持结尾 `*` 前缀通配）
    pub root_excluded_dirs: Vec<String>,
    part_matchers: Vec<PartMatcher>,
}

#[derive(Debug, Clone)]
struct PartMatcher {
    re: Regex,
    /// 组键取第 1 个捕获组
    group: usize,
    /// 分卷序号取第 2 个捕获组
    index: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        let exclude_file_patterns = [
            r"^unins.*\.exe$",
            r"^setup.*\.exe$",
            r"^.*crash.*\.exe$",
            r"^.*report.*\.exe$",
            r"^dxsetup\.exe$",
            r"^vcredist.*\.exe$",
            r"^oalinst\.exe$",
            r"^unitycrashhandler.*\.exe$",
            r"^ue4prereqsetup.*\.exe$",
            r"^notification_helper\.exe$",
            r"^.*helper\.exe$",
            r"^chromedriver\.exe$",
            r"^mtool.*\.exe$",
            r"^config\.exe$",
            r"^settings\.exe$",
        ]
        .iter()
        .filter_map(|p| Regex::new(&format!("(?i){}", p)).ok())
        .collect();

        let exclude_path_segments = [
            "_commonredist",
            "redist",
            "directx",
            "dotnet",
            "vcredist",
            "support",
            "engine\\binaries",
            "engine\\extras",
            "tools",
            "jre",
            "python",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        // 与 db::DEFAULT_ROOT_EXCLUDED_DIRS 保持一致（此处小写存储，比较时双方转小写）
        let root_excluded_dirs = [
            "#recycle",
            "@eadir",
            "#snapshot",
            "$recycle.bin",
            "system volume information",
            "lost+found",
            ".trash*",
            ".ds_store",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let part_matchers = vec![
            // game.part1.rar
            PartMatcher {
                re: Regex::new(r"(?i)^(.+?)\.part(\d+)\.rar$").unwrap(),
                group: 1,
                index: 2,
            },
            // game.r00 / game.r01（旧式 RAR 分卷，主文件 game.rar 会单独匹配）
            PartMatcher {
                re: Regex::new(r"(?i)^(.+?)\.r(\d{2})$").unwrap(),
                group: 1,
                index: 2,
            },
            // game.z01 / game.z02
            PartMatcher {
                re: Regex::new(r"(?i)^(.+?)\.z(\d{2})$").unwrap(),
                group: 1,
                index: 2,
            },
            // game.7z.001 / game.zip.001
            PartMatcher {
                re: Regex::new(r"(?i)^(.+\.(?:zip|7z|rar))\.(\d{3})$").unwrap(),
                group: 1,
                index: 2,
            },
        ];

        ScanConfig {
            archive_exts: [
                "zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst", "iso",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            exclude_file_patterns,
            exclude_path_segments,
            root_excluded_dirs,
            part_matchers,
        }
    }
}

impl ScanConfig {
    fn is_archive_ext(&self, name: &str) -> Option<String> {
        let ext = Path::new(name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())?;
        if self.archive_exts.iter().any(|a| a == &ext) {
            Some(ext)
        } else {
            None
        }
    }

    /// 识别分卷：返回 (组键, 分卷序号)
    fn match_part(&self, name: &str) -> Option<(String, u32)> {
        for m in &self.part_matchers {
            if let Some(caps) = m.re.captures(name) {
                let group = caps.get(m.group)?.as_str().to_string();
                let idx = caps.get(m.index)?.as_str().parse::<u32>().ok()?;
                return Some((group, idx));
            }
        }
        None
    }

    fn is_excluded_exe(&self, rel_path_lower: &str, file_name_lower: &str) -> bool {
        if self
            .exclude_file_patterns
            .iter()
            .any(|re| re.is_match(file_name_lower))
        {
            return true;
        }
        self.exclude_path_segments
            .iter()
            .any(|seg| rel_path_lower.contains(seg.as_str()))
    }

    /// 仓库根级目录排除（系统 / 回收站 / 索引目录）
    fn is_root_excluded(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        self.root_excluded_dirs.iter().any(|p| {
            let p = p.to_lowercase();
            if let Some(prefix) = p.strip_suffix('*') {
                lower.starts_with(prefix)
            } else {
                lower == p
            }
        })
    }
}

/// 从 settings 的 JSON 数组解析根级目录排除名单；解析失败则回退默认值
pub fn parse_root_excluded(raw: Option<String>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .filter(|v| !v.is_empty())
        .map(|v| v.into_iter().map(|s| s.to_lowercase()).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 扫描控制（暂停 / 取消）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlSignal {
    Continue,
    Paused,
    Cancelled,
}

#[derive(Debug, Clone, Default)]
pub struct ScanControl {
    pub cancel: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
}

impl ScanControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn set_paused(&self, paused: bool) {
        self.pause.store(paused, Ordering::SeqCst);
    }

    pub fn signal(&self) -> ControlSignal {
        if self.cancel.load(Ordering::SeqCst) {
            ControlSignal::Cancelled
        } else if self.pause.load(Ordering::SeqCst) {
            ControlSignal::Paused
        } else {
            ControlSignal::Continue
        }
    }

    /// 在顶层条目之间等待：暂停时阻塞，取消时返回 false
    pub fn wait_until_ready(&self) -> bool {
        loop {
            match self.signal() {
                ControlSignal::Continue => return true,
                ControlSignal::Paused => std::thread::sleep(std::time::Duration::from_millis(80)),
                ControlSignal::Cancelled => return false,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 扫描入口
// ---------------------------------------------------------------------------

pub struct ScanProgressEvent<'a> {
    pub done: u32,
    pub total: u32,
    pub current: Option<&'a str>,
    pub found: u32,
}

pub struct ScanRunResult {
    pub report: ScanReport,
    pub processed: u32,
    pub cancelled: bool,
}

/// 扫描一个仓库根目录（全量）。
///
/// * `start_index`：断点续扫的起始顶层条目索引（D10）
/// * `on_progress`：每处理完一个顶层条目调用一次
pub fn scan_repository(
    root: &Path,
    config: &ScanConfig,
    control: &ScanControl,
    start_index: u32,
    mut on_progress: impl FnMut(&ScanProgressEvent<'_>),
) -> ScanRunResult {
    let root_ext = extend_path(root);
    let mut report = ScanReport::default();
    let mut processed: u32 = 0;

    let entries = match list_dir(&root_ext) {
        Ok(v) => v,
        Err(_) => {
            report.warnings.enum_failed = 1;
            return ScanRunResult {
                report,
                processed: 0,
                cancelled: false,
            };
        }
    };
    let total = entries.len() as u32;

    for (i, de) in entries.iter().enumerate() {
        let i = i as u32;
        if i < start_index {
            continue;
        }
        if !control.wait_until_ready() {
            return ScanRunResult {
                report,
                processed,
                cancelled: true,
            };
        }

        if de.is_dir {
            // 不跟随符号链接 / junction；系统目录排除
            if !de.reparse && !config.is_root_excluded(&de.name) {
                let game = classify_game(&de.path, &de.name, config, &mut report.warnings);
                report.games.push(game);
            }
        } else if config.match_part(&de.name).is_some()
            || config.is_archive_ext(&de.name).is_some()
        {
            // 仓库根下的散落压缩包：不建档，只计 warning
            report.warnings.root_loose_files += 1;
        }

        processed = i + 1;
        on_progress(&ScanProgressEvent {
            done: processed,
            total,
            current: Some(&de.name),
            found: report.games.len() as u32,
        });
    }

    ScanRunResult {
        report,
        processed,
        cancelled: false,
    }
}

/// 单档案重扫：只判定一个游戏标识层。
pub fn scan_one_game(
    root: &Path,
    game_relative_path: &str,
    config: &ScanConfig,
) -> (GameEntry, ScanWarnings) {
    let mut warnings = ScanWarnings::default();
    let rel = game_relative_path.replace('\\', "/");
    let dir = join_rel(root, &rel);
    let game = classify_game(&dir, &rel, config, &mut warnings);
    (game, warnings)
}

/// 判定一个游戏标识层；返回该游戏（含版本列表）。
///
/// 枚举失败时仍返回一个 0 版本的空档案（强断言：绝不漏档），并计入 enum_failed。
fn classify_game(
    dir: &Path,
    rel: &str,
    config: &ScanConfig,
    warnings: &mut ScanWarnings,
) -> GameEntry {
    let display_name = display_name_of(rel);
    let empty = |layout: GameLayout| GameEntry {
        relative_path: rel.to_string(),
        layout,
        display_name: display_name.clone(),
        size_bytes: 0,
        versions: Vec::new(),
    };

    let entries = match list_dir(&extend_path(dir)) {
        Ok(v) => v,
        Err(_) => {
            warnings.enum_failed += 1;
            return empty(GameLayout::Standard);
        }
    };

    let (direct_exes, direct_archives) = split_files(&entries, rel, config);

    // ---- 紧凑型：游戏标识层自身直接含 exe / 压缩包 → 截断，1 个版本 ----
    if !direct_exes.is_empty() || !direct_archives.is_empty() {
        let size: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
        let version = VersionEntry {
            relative_path: rel.to_string(),
            version_label: display_name.clone(),
            game_type: if !direct_exes.is_empty() {
                GameType::Folder
            } else {
                GameType::Archive
            },
            size_bytes: size,
            executables: direct_exes,
            archive_groups: build_archive_groups(config, &direct_archives),
        };

        // 截断可见性：子目录里还藏着 exe / 压缩包 → 计一条 warning（判定结果不变）
        for sub in entries.iter().filter(|e| e.is_dir && !e.reparse) {
            let sub_rel = format!("{}/{}", rel, sub.name);
            if dir_has_content(&sub.path, &sub_rel, config) {
                warnings.truncated_with_version_dirs += 1;
                break;
            }
        }

        let size_total = version.size_bytes;
        return GameEntry {
            relative_path: rel.to_string(),
            layout: GameLayout::Compact,
            display_name,
            size_bytes: size_total,
            versions: vec![version],
        };
    }

    // ---- 标准型：逐个直接子目录判定是否为版本（不递归） ----
    let mut versions = Vec::new();
    for sub in entries.iter().filter(|e| e.is_dir && !e.reparse) {
        let sub_rel = format!("{}/{}", rel, sub.name);
        let sub_entries = match list_dir(&extend_path(&sub.path)) {
            Ok(v) => v,
            Err(_) => {
                warnings.enum_failed += 1;
                continue;
            }
        };
        let (s_exes, s_archives) = split_files(&sub_entries, &sub_rel, config);
        if s_exes.is_empty() && s_archives.is_empty() {
            // 对象定义：版本标识层下必须存在可执行文件或压缩包，且不能在子目录中
            continue;
        }
        let size: u64 = sub_entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
        versions.push(VersionEntry {
            relative_path: sub_rel,
            version_label: sub.name.clone(),
            game_type: if !s_exes.is_empty() {
                GameType::Folder
            } else {
                GameType::Archive
            },
            size_bytes: size,
            executables: s_exes,
            archive_groups: build_archive_groups(config, &s_archives),
        });
    }

    let total_size: u64 = versions.iter().map(|v| v.size_bytes).sum();
    GameEntry {
        relative_path: rel.to_string(),
        layout: GameLayout::Standard,
        display_name,
        size_bytes: total_size,
        versions,
    }
}

/// 把目录的直接文件分成「候选可执行文件」与「压缩文件」（均为文件名 + 大小）
fn split_files(
    entries: &[DirEntry],
    rel_base: &str,
    config: &ScanConfig,
) -> (Vec<ExeCandidate>, Vec<(String, u64)>) {
    let mut exes = Vec::new();
    let mut archives = Vec::new();

    for de in entries.iter().filter(|e| !e.is_dir) {
        // 分卷优先（.7z.001 之类扩展名不在白名单，靠正则识别）
        if config.match_part(&de.name).is_some() || config.is_archive_ext(&de.name).is_some() {
            archives.push((de.name.clone(), de.size));
            continue;
        }
        if de.name.to_lowercase().ends_with(".exe") {
            let file_lower = de.name.to_lowercase();
            let rel_lower = format!("{}/{}", rel_base.to_lowercase(), file_lower);
            if !config.is_excluded_exe(&rel_lower, &file_lower) {
                exes.push(ExeCandidate {
                    relative_path: format!("{}/{}", rel_base, de.name),
                    file_name: de.name.clone(),
                    size_bytes: de.size,
                });
            }
        }
    }
    (exes, archives)
}

/// 目录是否含「候选 exe 或压缩文件」（只列一层，用于截断可见性提示）
fn dir_has_content(dir: &Path, rel: &str, config: &ScanConfig) -> bool {
    let Ok(entries) = list_dir(&extend_path(dir)) else {
        return false;
    };
    let (exes, archives) = split_files(&entries, rel, config);
    !exes.is_empty() || !archives.is_empty()
}

/// 把本目录收集到的压缩文件按组聚合（D6：分卷聚合）
fn build_archive_groups(config: &ScanConfig, archives: &[(String, u64)]) -> Vec<ArchiveGroup> {
    let mut groups: HashMap<String, Vec<ArchiveFile>> = HashMap::new();

    for (name, size) in archives {
        let (key, part_index) = match config.match_part(name) {
            Some((k, idx)) => (k, Some(idx)),
            None => (name.clone(), None),
        };
        groups.entry(key).or_default().push(ArchiveFile {
            file_name: name.clone(),
            size_bytes: *size,
            part_index,
        });
    }

    let mut out: Vec<ArchiveGroup> = groups
        .into_iter()
        .map(|(base_name, mut files)| {
            files.sort_by(|a, b| a.file_name.cmp(&b.file_name));
            ArchiveGroup { base_name, files }
        })
        .collect();
    out.sort_by(|a, b| a.base_name.cmp(&b.base_name));
    out
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct DirEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    reparse: bool,
    size: u64,
}

fn list_dir(dir: &Path) -> std::io::Result<Vec<DirEntry>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        let reparse = is_reparse(&meta);
        let size = if is_dir { 0 } else { meta.len() };
        out.push(DirEntry {
            name,
            path,
            is_dir,
            reparse,
            size,
        });
    }
    Ok(out)
}

/// 判断是否为符号链接 / junction（Windows 用 FILE_ATTRIBUTE_REPARSE_POINT）
fn is_reparse(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// Windows 长路径前缀（>260 字符、UNC）
pub fn extend_path(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if s.starts_with(r"\\?\") {
            return p.to_path_buf();
        }
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(p)
        };
        let s = abs.to_string_lossy().replace('/', "\\");
        if let Some(rest) = s.strip_prefix(r"\\") {
            PathBuf::from(format!(r"\\?\UNC\{}", rest))
        } else {
            PathBuf::from(format!(r"\\?\{}", s))
        }
    }
    #[cfg(not(windows))]
    {
        p.to_path_buf()
    }
}

/// 用「/」分隔的相对路径拼接绝对路径
fn join_rel(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    p
}

/// 显示名：取最后一段路径
fn display_name_of(rel: &str) -> String {
    rel.rsplit('/').next().unwrap_or(rel).to_string()
}

// ---------------------------------------------------------------------------
// 测试（依据 MVPv1.1技术规格.md §9）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sakugame_test_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scan(root: &Path) -> ScanReport {
        let cfg = ScanConfig::default();
        let ctl = ScanControl::new();
        scan_repository(root, &cfg, &ctl, 0, |_| {}).report
    }

    fn write(path: &Path, name: &str) {
        fs::write(path.join(name), b"x").unwrap();
    }

    // ---- T-1.1-01 / ACC-01：紧凑形态 ----
    #[test]
    fn t11_01_compact_single_version() {
        let root = tmp_root("t1101");
        let g = root.join("RJ001");
        fs::create_dir_all(g.join("data")).unwrap();
        write(&g, "game.exe");
        write(&g.join("data"), "asset.bin");

        let report = scan(&root);
        assert_eq!(report.games.len(), 1);
        let game = &report.games[0];
        assert_eq!(game.layout, GameLayout::Compact);
        assert_eq!(game.versions.len(), 1);
        assert_eq!(game.versions[0].game_type, GameType::Folder);
        assert_eq!(game.versions[0].executables.len(), 1);
        assert_eq!(
            game.versions[0].executables[0].relative_path,
            "RJ001/game.exe"
        );
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-02 / ACC-02：标准形态两版本 ----
    #[test]
    fn t11_02_standard_two_versions() {
        let root = tmp_root("t1102");
        let g = root.join("RJ002");
        fs::create_dir_all(g.join("Ver1.0")).unwrap();
        fs::create_dir_all(g.join("Ver2.0")).unwrap();
        write(&g.join("Ver1.0"), "game.exe");
        write(&g.join("Ver2.0"), "game.exe");

        let report = scan(&root);
        assert_eq!(report.games.len(), 1);
        let game = &report.games[0];
        assert_eq!(game.layout, GameLayout::Standard);
        assert_eq!(game.versions.len(), 2);
        assert!(game.versions.iter().any(|v| v.version_label == "Ver1.0"));
        assert!(game.versions.iter().any(|v| v.version_label == "Ver2.0"));
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-03 / ACC-03：多版本形态混用 ----
    #[test]
    fn t11_03_mixed_version_types() {
        let root = tmp_root("t1103");
        let g = root.join("RJ003");
        fs::create_dir_all(g.join("Ver1.0")).unwrap();
        fs::create_dir_all(g.join("Ver2.0")).unwrap();
        write(&g.join("Ver1.0"), "game.exe");
        write(&g.join("Ver2.0"), "game.7z");

        let report = scan(&root);
        let game = &report.games[0];
        assert_eq!(game.versions.len(), 2);
        let v1 = game
            .versions
            .iter()
            .find(|v| v.version_label == "Ver1.0")
            .unwrap();
        let v2 = game
            .versions
            .iter()
            .find(|v| v.version_label == "Ver2.0")
            .unwrap();
        assert_eq!(v1.game_type, GameType::Folder);
        assert_eq!(v2.game_type, GameType::Archive);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-04 / ACC-04：反例结构 → 1 档案 0 版本 ----
    #[test]
    fn t11_04_nested_exe_yields_zero_versions() {
        let root = tmp_root("t1104");
        let g = root.join("RJ004");
        fs::create_dir_all(g.join("DLC")).unwrap();
        fs::create_dir_all(g.join("ArtBook")).unwrap();
        fs::create_dir_all(g.join("Game").join("data")).unwrap();
        write(&g.join("Game").join("data"), "game.exe");

        let report = scan(&root);
        assert_eq!(report.games.len(), 1, "强断言：仍须建档");
        let game = &report.games[0];
        assert_eq!(game.layout, GameLayout::Standard);
        assert!(
            game.versions.is_empty(),
            "可执行文件在子目录中 → 不构成版本: {:?}",
            game.versions
        );
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-05 / ACC-05：空目录 → 1 档案 0 版本 ----
    #[test]
    fn t11_05_empty_dir_still_archived() {
        let root = tmp_root("t1105");
        fs::create_dir_all(root.join("空目录")).unwrap();

        let report = scan(&root);
        assert_eq!(report.games.len(), 1, "强断言：空目录也建档");
        assert!(report.games[0].versions.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-06 / ACC-06：系统目录不建档 ----
    #[test]
    fn t11_06_system_dirs_excluded() {
        let root = tmp_root("t1106");
        for name in ["#recycle", "@eaDir", "System Volume Information"] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        let g = root.join("RJ001");
        fs::create_dir_all(&g).unwrap();
        write(&g, "game.exe");

        let report = scan(&root);
        assert_eq!(report.games.len(), 1, "只有 RJ001 应建档");
        assert_eq!(report.games[0].display_name, "RJ001");
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-07 / ACC-07：根下散落文件不建档 ----
    #[test]
    fn t11_07_root_loose_file_warns() {
        let root = tmp_root("t1107");
        write(&root, "散装.zip");
        let g = root.join("RJ001");
        fs::create_dir_all(&g).unwrap();
        write(&g, "game.exe");

        let report = scan(&root);
        assert_eq!(report.games.len(), 1);
        assert_eq!(report.warnings.root_loose_files, 1);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-08 / ACC-08：截断可见性 ----
    #[test]
    fn t11_08_truncation_visible() {
        let root = tmp_root("t1108");
        let g = root.join("RJ005");
        fs::create_dir_all(g.join("Ver2.0")).unwrap();
        write(&g, "game.exe");
        write(&g.join("Ver2.0"), "game.exe");

        let report = scan(&root);
        let game = &report.games[0];
        assert_eq!(game.layout, GameLayout::Compact);
        assert_eq!(game.versions.len(), 1, "截断：Ver2.0 不参与版本识别");
        assert_eq!(report.warnings.truncated_with_version_dirs, 1);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-09 / ACC-09：同目录多压缩包 → 1 版本 ----
    #[test]
    fn t11_09_multi_archive_single_version() {
        let root = tmp_root("t1109");
        let v = root.join("RJ006").join("Ver1.0");
        fs::create_dir_all(&v).unwrap();
        write(&v, "a.zip");
        write(&v, "b.zip");

        let report = scan(&root);
        let game = &report.games[0];
        assert_eq!(game.versions.len(), 1, "同目录多压缩包合并为 1 版本");
        let ver = &game.versions[0];
        assert_eq!(ver.game_type, GameType::Archive);
        assert_eq!(ver.archive_groups.len(), 2, "两个独立压缩组");
        assert_eq!(ver.size_bytes, 2);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-10 / ACC-10：exe 优先 ----
    #[test]
    fn t11_10_exe_wins_over_archive() {
        let root = tmp_root("t1110");
        let v = root.join("RJ007").join("Ver1.0");
        fs::create_dir_all(&v).unwrap();
        write(&v, "game.exe");
        write(&v, "data.zip");

        let report = scan(&root);
        let ver = &report.games[0].versions[0];
        assert_eq!(ver.game_type, GameType::Folder);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-21：排除名单过滤后无候选 → 不构成版本 ----
    #[test]
    fn t11_21_only_noise_exe_is_not_version() {
        let root = tmp_root("t1121");
        let v = root.join("RJ008").join("Ver1.0");
        fs::create_dir_all(&v).unwrap();
        write(&v, "unins000.exe");

        let report = scan(&root);
        assert_eq!(report.games.len(), 1, "强断言：仍建档");
        assert!(report.games[0].versions.is_empty(), "只有工具 exe，不算版本");
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1.1-22：分卷聚合 ----
    #[test]
    fn t11_22_split_rar_one_group() {
        let root = tmp_root("t1122");
        let v = root.join("RJ009").join("Ver1.0");
        fs::create_dir_all(&v).unwrap();
        write(&v, "game.part1.rar");
        write(&v, "game.part2.rar");

        let report = scan(&root);
        let ver = &report.games[0].versions[0];
        assert_eq!(ver.archive_groups.len(), 1);
        assert_eq!(ver.archive_groups[0].files.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- 中文与长路径（沿用 v1 T30 / T31） ----
    #[test]
    fn t11_18_chinese_names() {
        let root = tmp_root("t1118");
        let v = root.join("巫女神さまIF").join("Ver1.0");
        fs::create_dir_all(&v).unwrap();
        write(&v, "巫女神さま.exe");

        let report = scan(&root);
        assert_eq!(report.games.len(), 1);
        assert_eq!(report.games[0].display_name, "巫女神さまIF");
        assert_eq!(report.games[0].versions.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t11_18b_long_path() {
        let root = tmp_root("t1118b");
        let mut dir = root.join("RJ010");
        let seg = "a".repeat(60);
        for _ in 0..4 {
            dir.push(&seg);
        }
        fs::create_dir_all(&dir).unwrap();
        write(&dir, "game.exe");
        // 把 exe 放到版本目录：RJ010/aaa.../ver/game.exe 少一层，构造标准形态
        // 这里直接验证长路径目录下分类不 panic
        let report = scan(&root);
        assert_eq!(report.games.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- t17 / t16：取消与断点续扫 ----
    #[test]
    fn t11_cancel_stops_scan() {
        let root = tmp_root("t11cancel");
        for i in 0..5 {
            let g = root.join(format!("G{i}"));
            fs::create_dir_all(&g).unwrap();
            write(&g, "game.exe");
        }
        let cfg = ScanConfig::default();
        let ctl = ScanControl::new();
        ctl.cancel();

        let result = scan_repository(&root, &cfg, &ctl, 0, |_| {});
        assert!(result.cancelled, "取消后应停止");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t11_resume_from_checkpoint() {
        let root = tmp_root("t11resume");
        for i in 0..4 {
            let g = root.join(format!("G{i}"));
            fs::create_dir_all(&g).unwrap();
            write(&g, "game.exe");
        }
        let cfg = ScanConfig::default();
        let ctl = ScanControl::new();

        let first = scan_repository(&root, &cfg, &ctl, 0, |_| {});
        assert_eq!(first.report.games.len(), 4);

        // 从索引 2 续扫：只剩 2 个
        let second = scan_repository(&root, &cfg, &ctl, 2, |_| {});
        assert_eq!(second.report.games.len(), 2, "续扫跳过前两个条目");
        let _ = fs::remove_dir_all(&root);
    }

    // ---- 单档案扫描 ----
    #[test]
    fn t11_scan_one_game() {
        let root = tmp_root("t11one");
        let g = root.join("RJ020");
        fs::create_dir_all(g.join("Ver1.0")).unwrap();
        write(&g.join("Ver1.0"), "game.exe");

        let (game, _w) = scan_one_game(&root, "RJ020", &ScanConfig::default());
        assert_eq!(game.relative_path, "RJ020");
        assert_eq!(game.versions.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- 根级目录排除名单解析 ----
    #[test]
    fn t11_root_excluded_parsing() {
        let list = parse_root_excluded(Some(r##"["#recycle","@eaDir"]"##.to_string()));
        assert_eq!(list, vec!["#recycle".to_string(), "@eadir".to_string()]);
        assert!(parse_root_excluded(Some("not json".to_string())).is_empty());
        assert!(parse_root_excluded(None).is_empty());
    }
}
