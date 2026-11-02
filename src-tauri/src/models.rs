//! 数据模型：与前端交互的 DTO + 扫描器内部结构。
//! 依据：DSPlans/MVPv1.1技术规格.md §3（数据结构）/ §5（表结构）

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 枚举
// ---------------------------------------------------------------------------

/// 游戏形态（v1.1 仓库口径）：紧凑 = 游戏标识层自身含可执行文件或压缩包；标准 = 由版本层组织
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameLayout {
    Compact,
    Standard,
}

impl GameLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            GameLayout::Compact => "compact",
            GameLayout::Standard => "standard",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "compact" => Some(GameLayout::Compact),
            "standard" => Some(GameLayout::Standard),
            _ => None,
        }
    }
}

/// 版本类型（D5a / Q-G：exe 优先于压缩包）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameType {
    Folder,
    Archive,
}

impl GameType {
    pub fn as_str(self) -> &'static str {
        match self {
            GameType::Folder => "folder",
            GameType::Archive => "archive",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "folder" => Some(GameType::Folder),
            "archive" => Some(GameType::Archive),
            _ => None,
        }
    }
}

/// 版本来源（v1.1 §2「版本管理与可执行文件配置」）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionSource {
    /// 扫描发现
    Scan,
    /// 用户手动新建
    Manual,
}

impl VersionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionSource::Scan => "scan",
            VersionSource::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "scan" => Some(VersionSource::Scan),
            "manual" => Some(VersionSource::Manual),
            _ => None,
        }
    }
}

/// 档案 / 版本状态（D11）：missing = 上次扫到、本次未扫到，UI 灰显保留。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameStatus {
    Ok,
    Missing,
}

impl GameStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            GameStatus::Ok => "ok",
            GameStatus::Missing => "missing",
        }
    }
}

/// 仓库定位项类型（D16）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocatorKind {
    Drive,
    Ip,
    Host,
}

impl LocatorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LocatorKind::Drive => "drive",
            LocatorKind::Ip => "ip",
            LocatorKind::Host => "host",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "drive" => Some(LocatorKind::Drive),
            "ip" => Some(LocatorKind::Ip),
            "host" => Some(LocatorKind::Host),
            _ => None,
        }
    }

    /// 探测顺序：盘符 → IP → 主机名（D16）
    pub fn priority(self) -> u8 {
        match self {
            LocatorKind::Drive => 0,
            LocatorKind::Ip => 1,
            LocatorKind::Host => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// 扫描器内部结构
// ---------------------------------------------------------------------------

/// 压缩包组（分卷聚合的单元；v1.1 只作为版本内部结构，不再产生独立档案）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveGroup {
    /// 组键（去掉分卷后缀的主体名）
    pub base_name: String,
    pub files: Vec<ArchiveFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveFile {
    pub file_name: String,
    pub size_bytes: u64,
    pub part_index: Option<u32>,
}

/// 版本内的候选可执行文件（未命中排除名单）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExeCandidate {
    /// 相对仓库根（可能是紧凑型的游戏目录，也可能是版本目录）
    pub relative_path: String,
    pub file_name: String,
    pub size_bytes: u64,
}

/// 扫描判定出的一个版本（= 版本标识层；紧凑型 = 游戏标识层自身）
#[derive(Debug, Clone)]
pub struct VersionEntry {
    /// 相对仓库根
    pub relative_path: String,
    /// 目录原名（用户可改显示名，此为默认值）
    pub version_label: String,
    pub game_type: GameType,
    /// 本层文件大小之和（不含子目录）
    pub size_bytes: u64,
    pub executables: Vec<ExeCandidate>,
    pub archive_groups: Vec<ArchiveGroup>,
}

/// 扫描判定出的一个游戏档案（= 游戏标识层，一层一条）
#[derive(Debug, Clone)]
pub struct GameEntry {
    /// 游戏标识层，相对仓库根
    pub relative_path: String,
    pub layout: GameLayout,
    /// 初始显示名 = 目录名（用户可改）
    pub display_name: String,
    /// = 各版本 size 之和
    pub size_bytes: u64,
    /// 可为空（反例结构：版本层内探测不到可执行文件 / 压缩包）
    pub versions: Vec<VersionEntry>,
}

/// 扫描可见性告警（v1.1 新增）
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanWarnings {
    /// 仓库根下的散落文件数（不建档）
    pub root_loose_files: u32,
    /// 因自身含 exe / 压缩包而被截断、且其子目录也含 exe / 压缩包的目录数
    pub truncated_with_version_dirs: u32,
    /// 目录枚举失败数（权限 / 长路径 / 网络失败）
    pub enum_failed: u32,
}

impl ScanWarnings {
    pub fn total(&self) -> u32 {
        self.root_loose_files + self.truncated_with_version_dirs + self.enum_failed
    }
}

/// 一次扫描的完整结果
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub games: Vec<GameEntry>,
    pub warnings: ScanWarnings,
}

/// 扫描相位
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    Idle,
    Scanning,
    Paused,
    Finishing,
}

/// 扫描范围（v1.1 新增：全量 / 单档案）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanScope {
    Full,
    Single,
}

impl ScanScope {
    pub fn as_str(self) -> &'static str {
        match self {
            ScanScope::Full => "full",
            ScanScope::Single => "single",
        }
    }
}

/// 推给前端的进度（D8：分母 = 顶层条目数；单档案重扫 total 恒为 1）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanProgress {
    pub job_id: u64,
    pub repository_id: i64,
    pub done: u32,
    pub total: u32,
    pub found_games: u32,
    pub current: Option<String>,
    pub phase: ScanPhase,
    pub warnings: u32,
    pub scope: ScanScope,
}

// ---------------------------------------------------------------------------
// 数据库 / 前端 DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryLocator {
    pub kind: String,
    pub path_template: String,
    /// 最近一次探测成功时间（RFC3339）
    pub last_ok_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryRow {
    pub id: i64,
    pub name: String,
    pub share_name: Option<String>,
    pub relative_root: String,
    pub active_locator: Option<String>,
    pub is_offline: bool,
    pub last_scan_at: Option<String>,
    pub locators: Vec<RepositoryLocator>,
    /// 当前可达的绝对路径（探测后填充；不可达为 None）
    pub resolved_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewRepository {
    /// 用户可读名；为空时自动取盘符路径
    pub name: Option<String>,
    /// 共享名（如 "资源盘"），可选
    pub share_name: Option<String>,
    /// 共享（或盘符根）之下的相对路径，如 "Game"
    pub relative_root: String,
    /// 定位项：至少一个 drive
    pub locators: Vec<NewLocator>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewLocator {
    pub kind: String,
    pub path_template: String,
}

/// 档案行（= 游戏标识层，1 游戏 1 行）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameRow {
    pub id: i64,
    pub repository_id: i64,
    pub relative_path: String,
    pub layout: String,
    pub display_name: String,
    pub game_code: Option<String>,
    pub cover_path: Option<String>,
    pub size_bytes: i64,
    pub status: String,
    /// 版本数量
    pub version_count: i64,
    /// 启动按钮当前选中版本
    pub selected_version_id: Option<i64>,
    /// 选中版本的显示名（便于前端直接展示按钮文案）
    pub selected_version_label: Option<String>,
    pub updated_at: String,
}

/// 版本内的可执行文件行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutableRow {
    pub id: i64,
    /// 相对仓库根
    pub relative_path: String,
    pub file_name: String,
    pub size_bytes: i64,
    /// 该版本的启动项
    pub is_selected: bool,
    /// scan / manual
    pub source: String,
    /// 上次重扫已找不到该文件
    pub invalid: bool,
}

/// 版本行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameVersionRow {
    pub id: i64,
    pub game_id: i64,
    /// 相对仓库根
    pub relative_path: String,
    /// 目录原名（扫描刷新）
    pub version_label: String,
    /// 用户可改显示名，默认 = version_label
    pub display_name: String,
    pub note: Option<String>,
    pub game_type: String,
    pub size_bytes: i64,
    /// scan / manual
    pub source: String,
    pub status: String,
    pub executables: Vec<ExecutableRow>,
    pub updated_at: String,
}

/// 档案详情（含版本列表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameDetail {
    #[serde(flatten)]
    pub row: GameRow,
    pub versions: Vec<GameVersionRow>,
    /// 拼接后的绝对路径（详情页"本机详情"）
    pub absolute_path: Option<String>,
}

/// 黑名单记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlocklistRow {
    pub id: i64,
    pub repository_id: i64,
    /// game / version
    pub kind: String,
    /// 档案：游戏标识层；版本：<游戏路径>/<版本目录名>
    pub relative_path: String,
    pub created_at: String,
}

/// 取消拉黑的结果（v1.1：删除记录后立即尝试恢复，不再等下次扫描）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnblockOutcome {
    /// 是否已立即恢复
    pub restored: bool,
    pub message: String,
}

/// 扫描结束摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSummary {
    pub job_id: u64,
    pub repository_id: i64,
    pub found_count: u32,
    pub added: u32,
    pub updated: u32,
    pub missing: u32,
    pub warning_count: u32,
    /// 可见性明细（v1.1）
    pub root_loose_files: u32,
    pub truncated_with_version_dirs: u32,
    pub enum_failed: u32,
    pub scope: ScanScope,
}
