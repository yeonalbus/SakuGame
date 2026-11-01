//! SQLite 持久化层。
//! 依据：DSPlans/MVPv1扫描器规格.md §6（含 D16 的 repository_locators、D10 的 scan_checkpoints）

use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::Connection;

use crate::error::{AppError, AppResult};

pub struct Db {
    pub conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Db {
            conn: Mutex::new(conn),
        };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> AppResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR IGNORE INTO settings(key, value) VALUES('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    pub fn now() -> String {
        Utc::now().to_rfc3339()
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        f(&conn)
    }

    pub fn with_mut<T>(&self, f: impl FnOnce(&mut Connection) -> AppResult<T>) -> AppResult<T> {
        let mut conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        f(&mut conn)
    }
}

/// v1 schema。所有表结构变更都必须追加版本号（见 settings.schema_version）。
pub const SCHEMA_VERSION: i32 = 1;

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

-- 仓库（路径条目）
CREATE TABLE IF NOT EXISTS repositories (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    name             TEXT    NOT NULL,
    share_name       TEXT,
    relative_root    TEXT    NOT NULL,
    active_locator   TEXT,
    created_at       TEXT    NOT NULL,
    last_scan_at     TEXT,
    is_offline       INTEGER NOT NULL DEFAULT 0
);

-- 仓库定位项（D16：盘符 / IP / 主机名三定位项并存，按可达性探测）
CREATE TABLE IF NOT EXISTS repository_locators (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    kind          TEXT    NOT NULL CHECK (kind IN ('drive','ip','host')),
    path_template TEXT    NOT NULL,
    last_ok_at    TEXT,
    UNIQUE (repository_id, kind)
);

-- 游戏档案（D17：粒度 = 目录；D19：无 exe 字段；D20：无版本字段）
-- group_key：Q-B 匹配键追加组键（同一目录多个压缩包组时各建一条档案；folder 型为空串）
CREATE TABLE IF NOT EXISTS games (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id  INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    relative_path  TEXT    NOT NULL,
    group_key      TEXT    NOT NULL DEFAULT '',
    game_type      TEXT    NOT NULL CHECK (game_type IN ('folder','archive')),
    display_name   TEXT    NOT NULL,
    game_code      TEXT,
    cover_path     TEXT,
    size_bytes     INTEGER NOT NULL DEFAULT 0,
    status         TEXT    NOT NULL DEFAULT 'ok' CHECK (status IN ('ok','missing')),
    created_at     TEXT    NOT NULL,
    updated_at     TEXT    NOT NULL,
    UNIQUE (repository_id, relative_path, group_key)
);

-- 压缩包型游戏的文件清单（D6：分卷聚合）
CREATE TABLE IF NOT EXISTS game_files (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id    INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    group_key  TEXT    NOT NULL,
    file_name  TEXT    NOT NULL,
    part_index INTEGER,
    size_bytes INTEGER NOT NULL,
    UNIQUE (game_id, group_key, file_name)
);

-- 扫描任务历史（留痕用于排查）
CREATE TABLE IF NOT EXISTS scan_jobs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    started_at    TEXT    NOT NULL,
    finished_at   TEXT,
    status        TEXT    NOT NULL,
    found_count   INTEGER NOT NULL DEFAULT 0,
    warning_count INTEGER NOT NULL DEFAULT 0,
    error_message TEXT
);

-- 扫描断点（D10：暂停可续扫 / 进程退出保留断点）
CREATE TABLE IF NOT EXISTS scan_checkpoints (
    repository_id   INTEGER PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    next_index      INTEGER NOT NULL,
    total_entries   INTEGER NOT NULL,
    committed_games INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT    NOT NULL
);

-- 应用与判定相关设置（D12：判定参数必须可配置，不得写死）
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_games_repo_status ON games(repository_id, status);
CREATE INDEX IF NOT EXISTS idx_games_display_name ON games(display_name);
CREATE INDEX IF NOT EXISTS idx_game_files_game ON game_files(game_id);
"#;
