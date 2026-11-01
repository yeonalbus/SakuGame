//! 扫描器：目录判定与档案产出。
//!
//! 规则依据 DSPlans/MVPv1扫描器规格.md §3：
//! - 判定优先级（§3.2，Q-J/Q-G）：目录自身含压缩文件 → archive；否则自身含候选 exe → folder；否则容器下探
//! - 压缩白名单（Q-I）：`.bin`/`.cue` 已移出，它们是数据文件而非压缩包
//! - 分卷聚合（D6）：`.partN.rar` / `.rNN` / `.zNN` / `.7z.001`
//! - 排除名单（D18）：只用于"选代表"，不得用于过滤掉目录——有非名单 exe 就必须建档
//! - 深度上限 3（D5）、不跟随符号链接/junction（§3.4）
//! - 不读 exe 内容（性能红线）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use regex::Regex;

use crate::models::{ArchiveFile, ArchiveGroup, GameEntry, GameType};

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// 最大下探深度（D5：3）
    pub max_depth: u32,
    /// 压缩文件白名单（Q-I：不含 bin/cue）
    pub archive_exts: Vec<String>,
    /// exe 文件名排除正则（大小写不敏感）
    pub exclude_file_patterns: Vec<Regex>,
    /// exe 所在路径段排除（小写比较）
    pub exclude_path_segments: Vec<String>,
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
            max_depth: 3,
            archive_exts: [
                "zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst", "iso",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            exclude_file_patterns,
            exclude_path_segments,
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

    fn is_excluded_exe(&self, rel_path_lower: &str, file_name: &str) -> bool {
        if self
            .exclude_file_patterns
            .iter()
            .any(|re| re.is_match(file_name))
        {
            return true;
        }
        self.exclude_path_segments
            .iter()
            .any(|seg| rel_path_lower.contains(seg.as_str()))
    }
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

    /// 阻塞到可继续；返回是否取消
    pub fn wait_until_ready(&self) -> bool {
        loop {
            match self.signal() {
                ControlSignal::Cancelled => return false,
                ControlSignal::Paused => std::thread::sleep(std::time::Duration::from_millis(30)),
                ControlSignal::Continue => return true,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 顶层扫描
// ---------------------------------------------------------------------------

pub struct ScanProgressEvent<'a> {
    pub done: u32,
    pub total: u32,
    pub current: Option<&'a str>,
    pub found: u32,
}

pub struct ScanRunResult {
    pub games: Vec<GameEntry>,
    pub warnings: u32,
    /// 已处理的顶层条目数（用于断点）
    #[allow(dead_code)]
    pub processed: u32,
    /// 是否被取消
    pub cancelled: bool,
}

/// 扫描一个仓库根目录。
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
    let mut games: Vec<GameEntry> = Vec::new();
    let mut warnings: u32 = 0;

    // 顶层条目
    let entries = match list_dir(&root_ext) {
        Ok(v) => v,
        Err(_) => {
            return ScanRunResult {
                games,
                warnings: 1,
                processed: 0,
                cancelled: false,
            }
        }
    };
    let total = entries.len() as u32;
    let mut processed: u32 = 0;

    for (i, de) in entries.iter().enumerate() {
        let i = i as u32;
        if i < start_index {
            continue;
        }
        // 暂停/取消检查（仅在顶层条目之间，§3.6）
        if !control.wait_until_ready() {
            return ScanRunResult {
                games,
                warnings,
                processed,
                cancelled: true,
            };
        }

        let rel = de
            .path
            .strip_prefix(&root_ext)
            .map(|p| normalize_rel(p))
            .unwrap_or_else(|_| de.name.clone());

        if de.is_dir {
            if de.reparse {
                // 不跟随符号链接 / junction（§3.4）
            } else {
                let found = walk_dir(&de.path, &rel, 1, config, &control, &mut warnings);
                games.extend(found);
            }
        } else {
            // 顶层散落的压缩包文件（DB3-1 之外的通用情形）
            if let Some(ext) = config.is_archive_ext(&de.name) {
                if let Some(group) = build_single_archive(&de.name, ext, de.size) {
                    games.push(GameEntry {
                        relative_path: de.name.clone(),
                        group_key: de.name.clone(),
                        game_type: GameType::Archive,
                        display_name: strip_archive_ext(&de.name),
                        size_bytes: de.size,
                        groups: vec![group],
                    });
                }
            }
        }

        processed = i + 1;
        on_progress(&ScanProgressEvent {
            done: processed,
            total,
            current: Some(&de.name),
            found: games.len() as u32,
        });
    }

    ScanRunResult {
        games,
        warnings,
        processed,
        cancelled: false,
    }
}

/// 递归判定一个目录；返回该目录（含其子目录）产出的所有游戏。
///
/// 判定优先级（Q-J / Q-G）：压缩文件优先于 exe 是错误的，正确的是 **exe 优先**；
/// 但只有「非排除名单」的 exe 才算数（D18）。
fn walk_dir(
    dir: &Path,
    rel: &str,
    depth: u32,
    config: &ScanConfig,
    control: &ScanControl,
    warnings: &mut u32,
) -> Vec<GameEntry> {
    let mut out = Vec::new();

    let entries = match list_dir(dir) {
        Ok(v) => v,
        Err(_) => {
            *warnings += 1;
            return out;
        }
    };

    // 收集本目录信息
    let mut archive_files: Vec<(String, &DirEntry)> = Vec::new(); // (组键用文件名)
    let mut single_archives: Vec<(&DirEntry, String)> = Vec::new(); // (entry, ext)
    let mut exe_candidates: Vec<&DirEntry> = Vec::new();
    let mut subdirs: Vec<&DirEntry> = Vec::new();

    for de in &entries {
        if de.reparse {
            continue; // 不跟随（§3.4）
        }
        if de.is_dir {
            subdirs.push(de);
            continue;
        }
        // 分卷优先判断
        if config.match_part(&de.name).is_some() {
            archive_files.push((de.name.clone(), de));
            continue;
        }
        if let Some(ext) = config.is_archive_ext(&de.name) {
            single_archives.push((de, ext));
            continue;
        }
        if de.name.to_lowercase().ends_with(".exe") {
            let file_lower = de.name.to_lowercase();
            let rel_lower = format!("{}/{}", rel.to_lowercase(), file_lower);
            if !config.is_excluded_exe(&rel_lower, &file_lower) {
                exe_candidates.push(de);
            }
        }
    }

    // ---- 判定 1：目录自身存在非排除名单的 exe → folder（D18：必建，不看得分） ----
    if !exe_candidates.is_empty() {
        let size: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
        out.push(GameEntry {
            relative_path: rel.to_string(),
            group_key: String::new(),
            game_type: GameType::Folder,
            display_name: display_name_of(rel),
            size_bytes: size,
            groups: Vec::new(),
        });
        return out;
    }

    // ---- 判定 2：目录自身存在压缩文件 → archive（含分卷聚合，D6） ----
    if !archive_files.is_empty() || !single_archives.is_empty() {
        let groups = build_archive_groups(config, &archive_files, &single_archives);
        if !groups.is_empty() {
            // Q-B：同目录多个压缩组 → 各建一条档案，用 group_key 区分
            for group in groups {
                let size: u64 = group.files.iter().map(|f| f.size_bytes).sum();
                let key = group.base_name.clone();
                out.push(GameEntry {
                    relative_path: rel.to_string(),
                    group_key: key,
                    game_type: GameType::Archive,
                    display_name: display_name_of(rel),
                    size_bytes: size,
                    groups: vec![group],
                });
            }
        }
        return out;
    }

    // ---- 判定 3：容器目录 → 下探（深度上限 D5） ----
    if depth >= config.max_depth {
        return out;
    }
    for sub in subdirs {
        if control.signal() == ControlSignal::Cancelled {
            return out;
        }
        let sub_rel = format!("{}/{}", rel, sub.name);
        let mut sub_games = walk_dir(&sub.path, &sub_rel, depth + 1, config, control, warnings);
        out.append(&mut sub_games);
    }
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

fn normalize_rel(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 显示名：取最后一段路径
fn display_name_of(rel: &str) -> String {
    rel.rsplit('/').next().unwrap_or(rel).to_string()
}

fn strip_archive_ext(name: &str) -> String {
    let path = Path::new(name);
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string())
}

fn build_single_archive(name: &str, _ext: String, size: u64) -> Option<ArchiveGroup> {
    Some(ArchiveGroup {
        base_name: name.to_string(),
        files: vec![ArchiveFile {
            file_name: name.to_string(),
            size_bytes: size,
            part_index: None,
        }],
    })
}

/// 把本目录收集到的压缩文件按组聚合（D6 / Q-B）
fn build_archive_groups(
    config: &ScanConfig,
    part_files: &[(String, &DirEntry)],
    singles: &[(&DirEntry, String)],
) -> Vec<ArchiveGroup> {
    let mut groups: HashMap<String, Vec<ArchiveFile>> = HashMap::new();

    for (name, de) in part_files {
        if let Some((key, idx)) = config.match_part(name) {
            groups.entry(key).or_default().push(ArchiveFile {
                file_name: name.clone(),
                size_bytes: de.size,
                part_index: Some(idx),
            });
        }
    }

    for (de, _ext) in singles {
        // 单文件压缩包：组键 = 文件名
        groups.entry(de.name.clone()).or_default().push(ArchiveFile {
            file_name: de.name.clone(),
            size_bytes: de.size,
            part_index: None,
        });
    }

    let mut out: Vec<ArchiveGroup> = groups
        .into_iter()
        .map(|(base_name, mut files)| {
            files.sort_by(|a, b| a.file_name.cmp(&b.file_name));
            ArchiveGroup { base_name, files }
        })
        .collect();
    // 稳定输出顺序
    out.sort_by(|a, b| a.base_name.cmp(&b.base_name));
    out
}

// ---------------------------------------------------------------------------
// 测试（核心判定规则）
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

    fn scan(root: &Path) -> Vec<GameEntry> {
        let cfg = ScanConfig::default();
        let ctl = ScanControl::new();
        scan_repository(root, &cfg, &ctl, 0, |_| {}).games
    }

    #[test]
    fn t25_numbered_dir_with_unrelated_exe_builds_archive() {
        // 目录名 = 游戏编号，exe 名与目录名无关 → 仍须建档（Q-K 修正后）
        let root = tmp_root("t25");
        let game = root.join("RJ01005841");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("IllegalMahjong.exe"), b"x").unwrap();

        let games = scan(&root);
        assert_eq!(games.len(), 1, "编号目录必须建档");
        assert_eq!(games[0].game_type, GameType::Folder);
        assert_eq!(games[0].display_name, "RJ01005841");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t26_bin_is_not_archive() {
        // .bin 是数据文件，不算压缩包（Q-I）
        let root = tmp_root("t26");
        let game = root.join("game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("game.exe"), b"x").unwrap();
        fs::write(game.join("data.bin"), b"x").unwrap();
        fs::write(game.join("sound.bin"), b"x").unwrap();

        let games = scan(&root);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_type, GameType::Folder, "有 exe 时应判 folder");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t27_exe_wins_over_archive_in_same_dir() {
        let root = tmp_root("t27");
        let game = root.join("game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("game.exe"), b"x").unwrap();
        fs::write(game.join("data.zip"), b"x").unwrap();

        let games = scan(&root);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_type, GameType::Folder);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t28_only_noise_exe_is_not_game() {
        // 只有噪音 exe（全部在排除名单）→ 不建档
        let root = tmp_root("t28");
        let dir = root.join("notagame");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("notification_helper.exe"), b"x").unwrap();
        fs::write(dir.join("chromedriver.exe"), b"x").unwrap();

        let games = scan(&root);
        assert!(games.is_empty(), "全为排除名单 exe 时不应建档");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t35_noise_plus_real_exe_still_builds() {
        // D18：排除名单不得过滤掉目录
        let root = tmp_root("t35");
        let dir = root.join("game");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("notification_helper.exe"), b"x").unwrap();
        fs::write(dir.join("real_game.exe"), b"x").unwrap();

        let games = scan(&root);
        assert_eq!(games.len(), 1, "有非名单 exe 就必须建档");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t7_split_rar_is_one_archive() {
        let root = tmp_root("t7");
        let dir = root.join("game");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("game.part1.rar"), b"x").unwrap();
        fs::write(dir.join("game.part2.rar"), b"x").unwrap();
        fs::write(dir.join("game.part3.rar"), b"x").unwrap();

        let games = scan(&root);
        assert_eq!(games.len(), 1, "分卷应聚合为一条档案");
        assert_eq!(games[0].game_type, GameType::Archive);
        assert_eq!(games[0].groups.len(), 1);
        assert_eq!(games[0].groups[0].files.len(), 3);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t8_seven_zip_split() {
        let root = tmp_root("t8");
        let dir = root.join("game");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("game.7z.001"), b"x").unwrap();
        fs::write(dir.join("game.7z.002"), b"x").unwrap();

        let games = scan(&root);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].groups[0].files.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t32_multi_version_each_becomes_archive() {
        // D15：每个版本各建一条档案
        let root = tmp_root("t32");
        let v1 = root.join("RJ1234").join("Ver1.0");
        let v2 = root.join("RJ1234").join("Ver2.0");
        fs::create_dir_all(&v1).unwrap();
        fs::create_dir_all(&v2).unwrap();
        fs::write(v1.join("game.exe"), b"x").unwrap();
        fs::write(v1.join("game_chs.exe"), b"x").unwrap();
        fs::write(v2.join("game.exe"), b"x").unwrap();

        let games = scan(&root);
        assert_eq!(games.len(), 2, "多版本应各建一条档案");
        assert!(games.iter().any(|g| g.relative_path == "RJ1234/Ver1.0"));
        assert!(games.iter().any(|g| g.relative_path == "RJ1234/Ver2.0"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t3_depth_limit() {
        // 超过深度 3 的分支放弃
        let root = tmp_root("t3");
        let p = root.join("a").join("b").join("c").join("d");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("game.exe"), b"x").unwrap();

        let games = scan(&root);
        assert!(games.is_empty(), "深度 4 应被放弃");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn t9_archive_not_through_subdir() {
        // Q-A：压缩文件判定不穿透子目录
        let root = tmp_root("t9");
        let outer = root.join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("a.zip"), b"x").unwrap();

        let games = scan(&root);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].relative_path, "outer/inner");
        assert_eq!(games[0].game_type, GameType::Archive);
        let _ = fs::remove_dir_all(&root);
    }

    /// T17：取消后立即停止，不产出档案（D10）
    #[test]
    fn t17_cancel_stops_scan() {
        let root = tmp_root("t17");
        for i in 0..5 {
            let d = root.join(format!("g{i}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("game.exe"), b"x").unwrap();
        }
        let cfg = ScanConfig::default();
        let ctl = ScanControl::new();
        ctl.cancel();

        let r = scan_repository(&root, &cfg, &ctl, 0, |_| {});
        assert!(r.cancelled, "取消后应返回 cancelled");
        assert!(r.games.is_empty(), "取消不应产出档案");
        let _ = fs::remove_dir_all(&root);
    }

    /// T16：断点续扫只处理剩余顶层条目（D10）
    #[test]
    fn t16_resume_from_checkpoint() {
        let root = tmp_root("t16");
        for i in 0..3 {
            let d = root.join(format!("g{i}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("game.exe"), b"x").unwrap();
        }
        let cfg = ScanConfig::default();
        let ctl = ScanControl::new();

        let full = scan_repository(&root, &cfg, &ctl, 0, |_| {});
        assert_eq!(full.games.len(), 3);

        let resumed = scan_repository(&root, &cfg, &ctl, 2, |_| {});
        assert_eq!(resumed.games.len(), 1, "从索引 2 续扫应只处理剩余 1 个");
        assert!(!resumed.cancelled);
        let _ = fs::remove_dir_all(&root);
    }

    /// T30：中文目录名 + 中文 exe 名（ACC-02-34）
    #[test]
    fn t30_chinese_names() {
        let root = tmp_root("t30");
        let dir = root.join("巫女神さまIF");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("巫女神さま.exe"), b"x").unwrap();

        let games = scan(&root);
        assert_eq!(games.len(), 1, "中文目录必须能建档");
        assert_eq!(games[0].display_name, "巫女神さまIF");
        assert_eq!(games[0].game_type, GameType::Folder);
        let _ = fs::remove_dir_all(&root);
    }
}
