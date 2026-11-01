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
            AppError::ScanBusy => "scan_busy",
            AppError::ScanInProgress => "scan_in_progress",
            AppError::NoActiveScan => "no_active_scan",
            AppError::RepositoryUnreachable => "repository_unreachable",
            AppError::InvalidInput(_) => "invalid_input",
            AppError::Internal(_) => "internal",
        };
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", kind)?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
