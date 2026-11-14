//! SQLite 持久化层。
//! 依据：DSPlans/MVPv1.1技术规格.md §5（schema v2）、§5 迁移说明（v1 数据不迁移）

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
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;

        // v1 → v1.1：不迁移，检测到旧结构（games 无 layout 列）即清空业务表重建
        if table_exists(&conn, "games")? && !table_has_column(&conn, "games", "layout")? {
            conn.execute_batch(DROP_V1_BUSINESS_TABLES)?;
        }

        conn.execute_batch(SCHEMA)?;
        // v3 → v4：repositories 补 cache_slot 列（CREATE TABLE IF NOT EXISTS 不会给旧表加列）
        if !table_has_column(&conn, "repositories", "cache_slot")? {
            conn.execute("ALTER TABLE repositories ADD COLUMN cache_slot INTEGER", [])?;
        }
        backfill_cache_slots(&conn)?;
        conn.execute(
            "INSERT OR REPLACE INTO settings(key, value) VALUES('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO settings(key, value) VALUES('root_excluded_dirs', ?1)",
            [DEFAULT_ROOT_EXCLUDED_DIRS],
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

    /// 读取字符串设置项
    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        self.with(|conn| {
            let v = conn
                .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0))
                .ok();
            Ok(v)
        })
    }

    /// 写入字符串设置项
    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.with(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO settings(key, value) VALUES(?1, ?2)",
                [key, value],
            )?;
            Ok(())
        })
    }
}

/// 当前 schema。所有表结构变更都必须追加版本号（见 settings.schema_version）。
/// v2（MVP v2）：追加 transfer_tasks / cache_entries，不破坏 v1.1 既有表。
/// v4：repositories 追加 cache_slot（缓存池槽位号，删库后空号可复用）。
pub const SCHEMA_VERSION: i32 = 4;

/// 仓库根级目录排除名单默认值（JSON 数组；支持结尾 `*` 前缀通配，大小写不敏感）
pub const DEFAULT_ROOT_EXCLUDED_DIRS: &str = r##"["#recycle","@eaDir","#snapshot","$RECYCLE.BIN","System Volume Information","lost+found",".Trash*",".DS_Store"]"##;

/// v1 旧业务表（清空重建用；按外键依赖顺序：先子表后主表）
const DROP_V1_BUSINESS_TABLES: &str = r#"
DROP TABLE IF EXISTS game_files;
DROP TABLE IF EXISTS game_version_files;
DROP TABLE IF EXISTS game_version_executables;
DROP TABLE IF EXISTS game_versions;
DROP TABLE IF EXISTS games;
DROP TABLE IF EXISTS blocklist;
"#;

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
    is_offline       INTEGER NOT NULL DEFAULT 0,
    cache_slot       INTEGER
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

-- 游戏档案（= 游戏标识层，一层一条；v1.1 两层结构）
CREATE TABLE IF NOT EXISTS games (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id       INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    relative_path       TEXT    NOT NULL,
    layout              TEXT    NOT NULL CHECK (layout IN ('compact','standard')),
    display_name        TEXT    NOT NULL,
    game_code           TEXT,
    cover_path          TEXT,
    size_bytes          INTEGER NOT NULL DEFAULT 0,
    selected_version_id INTEGER,
    status              TEXT    NOT NULL DEFAULT 'ok' CHECK (status IN ('ok','missing')),
    created_at          TEXT    NOT NULL,
    updated_at          TEXT    NOT NULL,
    UNIQUE (repository_id, relative_path)
);

-- 版本（= 版本标识层；紧凑型为游戏标识层自身）
CREATE TABLE IF NOT EXISTS game_versions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id       INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    relative_path TEXT    NOT NULL,
    version_label TEXT    NOT NULL,
    display_name  TEXT    NOT NULL,
    note          TEXT,
    game_type     TEXT    NOT NULL CHECK (game_type IN ('folder','archive')),
    size_bytes    INTEGER NOT NULL DEFAULT 0,
    source        TEXT    NOT NULL DEFAULT 'scan' CHECK (source IN ('scan','manual')),
    status        TEXT    NOT NULL DEFAULT 'ok' CHECK (status IN ('ok','missing')),
    created_at    TEXT    NOT NULL,
    updated_at    TEXT    NOT NULL,
    UNIQUE (game_id, relative_path)
);

-- 版本内候选 / 手填可执行文件
CREATE TABLE IF NOT EXISTS game_version_executables (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    version_id    INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    relative_path TEXT    NOT NULL,
    file_name     TEXT    NOT NULL,
    size_bytes    INTEGER NOT NULL DEFAULT 0,
    is_selected   INTEGER NOT NULL DEFAULT 0,
    source        TEXT    NOT NULL CHECK (source IN ('scan','manual')),
    invalid       INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT    NOT NULL,
    UNIQUE (version_id, relative_path)
);

-- 版本内压缩包文件（v2 搬运依据；group_key 仅表示分卷组）
CREATE TABLE IF NOT EXISTS game_version_files (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    version_id INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    group_key  TEXT    NOT NULL,
    file_name  TEXT    NOT NULL,
    part_index INTEGER,
    size_bytes INTEGER NOT NULL,
    UNIQUE (version_id, file_name)
);

-- 黑名单（删除的档案 / 版本，防止重扫重复判定）
CREATE TABLE IF NOT EXISTS blocklist (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    kind          TEXT    NOT NULL CHECK (kind IN ('game','version')),
    relative_path TEXT    NOT NULL,
    created_at    TEXT    NOT NULL,
    UNIQUE (repository_id, kind, relative_path)
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

-- 搬运任务（v2；持久化以支撑队列重建与崩溃恢复）
CREATE TABLE IF NOT EXISTS transfer_tasks (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id  INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    game_id        INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    version_id     INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    repo_no        TEXT    NOT NULL,
    source_rel     TEXT    NOT NULL,
    dest_rel       TEXT    NOT NULL,
    total_bytes    INTEGER NOT NULL,
    total_files    INTEGER NOT NULL,
    written_bytes  INTEGER NOT NULL DEFAULT 0,
    written_files  INTEGER NOT NULL DEFAULT 0,
    concurrency    INTEGER NOT NULL,
    status         TEXT    NOT NULL DEFAULT 'queued'
                   CHECK (status IN ('queued','running','paused','error','done','cancelled')),
    error_message  TEXT,
    failed_files   TEXT,
    created_at     TEXT    NOT NULL,
    started_at     TEXT,
    finished_at    TEXT,
    updated_at     TEXT    NOT NULL
);

-- 缓存记录（v2；已完成搬运的落点，对账用）
CREATE TABLE IF NOT EXISTS cache_entries (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id    INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    game_id          INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    version_id       INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    repo_no          TEXT    NOT NULL,
    dest_rel         TEXT    NOT NULL,
    size_bytes       INTEGER NOT NULL,
    file_count       INTEGER NOT NULL,
    status           TEXT    NOT NULL DEFAULT 'ok' CHECK (status IN ('ok','missing')),
    created_at       TEXT    NOT NULL,
    last_verified_at TEXT,
    UNIQUE (version_id)
);

CREATE INDEX IF NOT EXISTS idx_games_repo_status  ON games(repository_id, status);
CREATE INDEX IF NOT EXISTS idx_games_display_name ON games(display_name);
CREATE INDEX IF NOT EXISTS idx_versions_game      ON game_versions(game_id);
CREATE INDEX IF NOT EXISTS idx_exes_version       ON game_version_executables(version_id);
CREATE INDEX IF NOT EXISTS idx_vfiles_version     ON game_version_files(version_id);
CREATE INDEX IF NOT EXISTS idx_blocklist_repo     ON blocklist(repository_id, kind);
CREATE UNIQUE INDEX IF NOT EXISTS uq_transfer_active
    ON transfer_tasks(version_id) WHERE status IN ('queued','running','paused');
CREATE INDEX IF NOT EXISTS idx_transfer_state   ON transfer_tasks(status);
CREATE INDEX IF NOT EXISTS idx_cache_repo       ON cache_entries(repository_id, status);
CREATE INDEX IF NOT EXISTS idx_cache_version    ON cache_entries(version_id);
"#;

fn table_exists(conn: &Connection, name: &str) -> AppResult<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> AppResult<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(cols.iter().any(|c| c.eq_ignore_ascii_case(column)))
}

/// 给缺 `cache_slot` 的仓库补号：按 id 顺序取最小可用槽位，避免历史删除造成跳号。
fn backfill_cache_slots(conn: &Connection) -> AppResult<()> {
    let ids: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT id FROM repositories WHERE cache_slot IS NULL ORDER BY id")?;
        let it = stmt.query_map([], |r| r.get(0))?;
        it.filter_map(|r| r.ok()).collect()
    };
    if ids.is_empty() {
        return Ok(());
    }
    let mut used: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT cache_slot FROM repositories WHERE cache_slot IS NOT NULL")?;
        let it = stmt.query_map([], |r| r.get(0))?;
        it.filter_map(|r| r.ok()).collect()
    };
    used.sort_unstable();
    for id in ids {
        let mut n = 1i64;
        while used.contains(&n) {
            n += 1;
        }
        conn.execute("UPDATE repositories SET cache_slot=?1 WHERE id=?2", [n, id])?;
        used.push(n);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(tag: &str) -> (Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sakugame_db_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        (Db::open(&path).expect("open db"), dir)
    }

    /// ACC-CON-01 / D19 护栏：games 表不得含 exe 字段、不得含版本号字段
    #[test]
    fn schema_v2_columns() {
        let (db, dir) = temp_db("cols");
        db.with(|c| {
            let mut stmt = c.prepare("PRAGMA table_info(games)")?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            assert!(cols.iter().any(|c| c == "layout"), "games 应有 layout 列: {cols:?}");
            assert!(
                !cols.iter().any(|c| c.to_lowercase().contains("exe")),
                "games 不得有 exe 字段（D19）"
            );
            assert!(
                !cols.iter().any(|c| c.to_lowercase().contains("version_id") && c != "selected_version_id"),
                "除 selected_version_id 外不得有版本关联列"
            );

            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN
                 ('game_versions','game_version_executables','game_version_files','blocklist')",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 4, "v2 应建齐版本 / 可执行文件 / 压缩包 / 黑名单四张表");
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// schema_version 与默认目录排除名单已写入
    #[test]
    fn schema_meta_written() {
        let (db, dir) = temp_db("meta");
        assert_eq!(
            db.get_setting("schema_version").unwrap().as_deref(),
            Some("4")
        );
        let excluded = db.get_setting("root_excluded_dirs").unwrap().unwrap();
        assert!(excluded.contains("#recycle") && excluded.contains("@eaDir"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v1 旧库（games 无 layout 列）→ 清空重建
    #[test]
    fn migrate_from_v1_drops_business_tables() {
        let dir = std::env::temp_dir().join(format!("sakugame_db_v1_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE games(id INTEGER PRIMARY KEY, repository_id INTEGER,
                    relative_path TEXT, group_key TEXT, game_type TEXT, display_name TEXT,
                    size_bytes INTEGER, status TEXT, created_at TEXT, updated_at TEXT);
                 CREATE TABLE game_files(id INTEGER PRIMARY KEY, game_id INTEGER);
                 INSERT INTO games(id, repository_id, relative_path, group_key, game_type,
                    display_name, size_bytes, status, created_at, updated_at)
                 VALUES(1, 1, 'RJ1234/Ver1.0', '', 'folder', 'Ver1.0', 1, 'ok', 'x', 'x');",
            )
            .unwrap();
        }

        let db = Db::open(&path).unwrap();
        db.with(|c| {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM games", [], |r| r.get(0))?;
            assert_eq!(n, 0, "v1 数据不迁移，应被清空");
            let has_layout = table_has_column(c, "games", "layout")?;
            assert!(has_layout);
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
