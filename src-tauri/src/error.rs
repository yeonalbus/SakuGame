//! 统一错误类型：所有 Tauri 命令返回 `Result<T, AppError>`。

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("文件系统错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("正则表达式错误: {0}")]
    Regex(#[from] regex::Error),

    #[error("仓库不存在: {0}")]
    RepositoryNotFound(i64),

    #[error("档案不存在: {0}")]
    GameNotFound(i64),

    #[error("版本不存在: {0}")]
    VersionNotFound(i64),

    #[error("可执行文件配置不存在: {0}")]
    ExecutableNotFound(i64),

    #[error("黑名单记录不存在: {0}")]
    BlocklistNotFound(i64),

    #[error("已有扫描任务在运行（ScanBusy）")]
    ScanBusy,

    #[error("扫描进行中，仓库配置已锁定（ScanInProgress）")]
    ScanInProgress,

    #[error("没有正在运行的扫描任务")]
    NoActiveScan,

    #[error("仓库路径全部不可达，请检查网络配置")]
    RepositoryUnreachable,

    #[error("非法输入: {0}")]
    InvalidInput(String),

    // ---- v2：缓存池 / 搬运 / 启动 ----
    #[error("缓存池未配置，请先在设置中指定缓存池路径")]
    CacheNotConfigured,

    #[error("缓存池配额不足：需要 {required} 字节，可用 {available} 字节")]
    QuotaExceeded { required: i64, available: i64 },

    #[error("缓存池所在卷空间不足：需要 {required} 字节，可用 {available} 字节")]
    DiskExceeded { required: i64, available: i64 },

    #[error("搬运任务不存在: {0}")]
    TransferNotFound(i64),

    #[error("该版本已有搬运任务")]
    TransferBusy,

    #[error("该任务当前不可恢复（{0}）")]
    TransferNotResumable(String),

    #[error("该版本正在搬运中，请先取消任务")]
    CacheRemoveBusy,

    #[error("压缩包形态暂不支持搬运，请先在仓库中解压后重扫")]
    ArchiveUnsupported,

    #[error("缓存池路径不得设在任何仓库根之内")]
    PoolPathInsideRepository,

    #[error("已有缓存或搬运任务，缓存池路径已锁定；请先清空缓存池")]
    CachePathLocked,

    #[error("该版本尚未配置启动项")]
    LaunchNoExecutable,

    #[error("启动失败: {0}")]
    LaunchFailed(String),

    #[error("内部错误: {0}")]
    Internal(String),
}

/// 让错误可以跨 IPC 传给前端：序列化为 { kind, message }
impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let kind = match self {
            AppError::Db(_) => "db",
            AppError::Io(_) => "io",
            AppError::Regex(_) => "regex",
            AppError::RepositoryNotFound(_) => "repository_not_found",
            AppError::GameNotFound(_) => "game_not_found",
            AppError::VersionNotFound(_) => "version_not_found",
            AppError::ExecutableNotFound(_) => "executable_not_found",
            AppError::BlocklistNotFound(_) => "blocklist_not_found",
            AppError::ScanBusy => "scan_busy",
            AppError::ScanInProgress => "scan_in_progress",
            AppError::NoActiveScan => "no_active_scan",
            AppError::RepositoryUnreachable => "repository_unreachable",
            AppError::InvalidInput(_) => "invalid_input",
            AppError::CacheNotConfigured => "cache_not_configured",
            AppError::QuotaExceeded { .. } => "quota_exceeded",
            AppError::DiskExceeded { .. } => "disk_exceeded",
            AppError::TransferNotFound(_) => "transfer_not_found",
            AppError::TransferBusy => "transfer_busy",
            AppError::TransferNotResumable(_) => "transfer_not_resumable",
            AppError::CacheRemoveBusy => "cache_remove_busy",
            AppError::ArchiveUnsupported => "archive_unsupported",
            AppError::PoolPathInsideRepository => "pool_path_inside_repository",
            AppError::CachePathLocked => "cache_path_locked",
            AppError::LaunchNoExecutable => "launch_no_executable",
            AppError::LaunchFailed(_) => "launch_failed",
            AppError::Internal(_) => "internal",
        };
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", kind)?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
