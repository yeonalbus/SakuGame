//! 数据模型：与前端交互的 DTO + 扫描器内部结构。
//! 依据：DSPlans/MVPv1扫描器规格.md §5

// 部分结构/方法为 v2 预留（如 ScanOutcome、ScanDelta），v1 尚未全部使用
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// 游戏类型（D5a）。v1 不展示，但必须落库——v2 搬运行为依赖它。
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

/// 档案状态（D11）。missing = 上次扫到、本次未扫到，UI 灰显保留。
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

/// 压缩包组（D6：分卷聚合的单元）
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

/// 扫描判定出的一个游戏单元
#[derive(Debug, Clone)]
pub struct GameEntry {
    /// 相对仓库根的路径（匹配键的一半）
    pub relative_path: String,
    /// 压缩包组键（Q-B 匹配键的另一半；folder 型为空串）
    pub group_key: String,
    pub game_type: GameType,
    /// 初始显示名 = 目录名 / 压缩包组主名（用户可改）
    pub display_name: String,
    pub size_bytes: u64,
    pub groups: Vec<ArchiveGroup>,
}

/// 判定结果
#[derive(Debug, Clone)]
pub enum ScanOutcome {
    Game(GameEntry),
    /// 容器目录：继续下探
    Container,
    /// 超过深度上限或不可判定：放弃该分支
    Skip,
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

/// 推给前端的进度（D8：分母 = 顶层条目数）
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
}

/// 重扫差异（§8）
#[derive(Debug, Default)]
pub struct ScanDelta {
    pub added: Vec<GameEntry>,
    pub updated: Vec<(i64, GameEntry)>,
    pub missing: Vec<i64>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameRow {
    pub id: i64,
    pub repository_id: i64,
    pub relative_path: String,
    pub group_key: String,
    pub game_type: String,
    pub display_name: String,
    pub game_code: Option<String>,
    pub cover_path: Option<String>,
    pub size_bytes: i64,
    pub status: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameDetail {
    #[serde(flatten)]
    pub row: GameRow,
    /// 拼接后的绝对路径（详情页"本机详情"）
    pub absolute_path: Option<String>,
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
}
