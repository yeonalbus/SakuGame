//! SQLite 持久化层。
//! 依据：DSPlans/MVPv1.1技术规格.md §5（schema v2）、§5 迁移说明（v1 数据不迁移）

use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::{params, Connection};

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

        conn.execute_batch(SCHEMA_MAIN)?;
        // v4 → v5：transfer_tasks 的 status CHECK 无法 ALTER，按「重命名 → 建新表 → 搬行 → 删旧表」重建
        if table_exists(&conn, "transfer_tasks")? && !table_has_column(&conn, "transfer_tasks", "direction")?
        {
            migrate_transfer_tasks_v5(&conn)?;
        }
        conn.execute_batch(TABLE_TRANSFER_TASKS)?;
        conn.execute_batch(SCHEMA_TAIL)?;
        conn.execute_batch(INDEXES)?;
        // v3 → v4：repositories 补 cache_slot 列（CREATE TABLE IF NOT EXISTS 不会给旧表加列）
        if !table_has_column(&conn, "repositories", "cache_slot")? {
            conn.execute("ALTER TABLE repositories ADD COLUMN cache_slot INTEGER", [])?;
        }
        ensure_v21_columns(&conn)?;
        backfill_cache_slots(&conn)?;
        backfill_v21_uuids(&conn)?;
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
/// v5（MVP v2.1）：transfer_tasks 追加 direction / stage 并放开 waiting_confirm、cancelled；
///     games 追加 game_uuid；game_versions 追加 version_uuid / last_known_path / anchor_hash / user_path；
///     追加 alter_files / mtime_baseline / writeback_packages。
pub const SCHEMA_VERSION: i32 = 5;

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

const SCHEMA_MAIN: &str = r#"
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

-- 变更组文件（v2.1；scope：bg = 游戏本体，ud = 用户路径；回写后用新内容 hash 覆盖）
CREATE TABLE IF NOT EXISTS alter_files (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    version_id INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    scope      TEXT    NOT NULL CHECK (scope IN ('bg','ud')),
    rel_path   TEXT    NOT NULL,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    hash       TEXT    NOT NULL,
    updated_at TEXT    NOT NULL,
    UNIQUE (version_id, scope, rel_path)
);

-- mtime 基线（v2.1；注入后整体覆盖写，检测时与当前 mtime/size 比对）
CREATE TABLE IF NOT EXISTS mtime_baseline (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    version_id INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    scope      TEXT    NOT NULL CHECK (scope IN ('bg','ud')),
    rel_path   TEXT    NOT NULL,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    mtime_ms   INTEGER NOT NULL,
    UNIQUE (version_id, scope, rel_path)
);
"#;

/// 搬运任务（v2 建立；v2.1 追加 direction / stage，状态放开 waiting_confirm 与 cancelled）
const TABLE_TRANSFER_TASKS: &str = r#"
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
    direction      TEXT    NOT NULL DEFAULT 'pull' CHECK (direction IN ('pull','writeback')),
    stage          TEXT,
    status         TEXT    NOT NULL DEFAULT 'queued'
                   CHECK (status IN ('queued','running','paused','waiting_confirm','error','done','cancelled')),
    error_message  TEXT,
    failed_files   TEXT,
    created_at     TEXT    NOT NULL,
    started_at     TEXT,
    finished_at    TEXT,
    updated_at     TEXT    NOT NULL
);
"#;

const SCHEMA_TAIL: &str = r#"

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

-- 回写包（v2.1；.waiting/<task_id>/ 的 DB 镜像，崩溃恢复依据）
CREATE TABLE IF NOT EXISTS writeback_packages (
    task_id    INTEGER PRIMARY KEY REFERENCES transfer_tasks(id) ON DELETE CASCADE,
    version_id INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    stage      TEXT    NOT NULL,
    manifest   TEXT    NOT NULL,
    created_at TEXT    NOT NULL,
    updated_at TEXT    NOT NULL
);
"#;

const INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_games_repo_status  ON games(repository_id, status);
CREATE INDEX IF NOT EXISTS idx_games_display_name ON games(display_name);
CREATE INDEX IF NOT EXISTS idx_versions_game      ON game_versions(game_id);
CREATE INDEX IF NOT EXISTS idx_exes_version       ON game_version_executables(version_id);
CREATE INDEX IF NOT EXISTS idx_vfiles_version     ON game_version_files(version_id);
CREATE INDEX IF NOT EXISTS idx_blocklist_repo     ON blocklist(repository_id, kind);
CREATE UNIQUE INDEX IF NOT EXISTS uq_transfer_active
    ON transfer_tasks(version_id) WHERE status IN ('queued','running','paused','waiting_confirm');
CREATE INDEX IF NOT EXISTS idx_transfer_state   ON transfer_tasks(status);
CREATE INDEX IF NOT EXISTS idx_cache_repo       ON cache_entries(repository_id, status);
CREATE INDEX IF NOT EXISTS idx_cache_version    ON cache_entries(version_id);
CREATE INDEX IF NOT EXISTS idx_alter_version    ON alter_files(version_id, scope);
CREATE INDEX IF NOT EXISTS idx_baseline_version ON mtime_baseline(version_id, scope);
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

/// v4 → v5：`transfer_tasks` 的 status CHECK 无法 ALTER，按「重命名 → 建新表 → 搬行 → 删旧表」重建。
/// 保留历史任务行（direction 统一回填 'pull'，stage 留空）。
fn migrate_transfer_tasks_v5(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "DROP INDEX IF EXISTS uq_transfer_active;
         DROP INDEX IF EXISTS idx_transfer_state;
         ALTER TABLE transfer_tasks RENAME TO _transfer_tasks_v4;",
    )?;
    conn.execute_batch(TABLE_TRANSFER_TASKS)?;
    conn.execute_batch(
        "INSERT INTO transfer_tasks(id, repository_id, game_id, version_id, repo_no, source_rel,
             dest_rel, total_bytes, total_files, written_bytes, written_files, concurrency,
             direction, stage, status, error_message, failed_files, created_at, started_at,
             finished_at, updated_at)
         SELECT id, repository_id, game_id, version_id, repo_no, source_rel,
             dest_rel, total_bytes, total_files, written_bytes, written_files, concurrency,
             'pull', NULL, status, error_message, failed_files, created_at, started_at,
             finished_at, updated_at
         FROM _transfer_tasks_v4;
         DROP TABLE _transfer_tasks_v4;",
    )?;
    Ok(())
}

/// v2.1 补列：`games.game_uuid`、`game_versions.version_uuid / last_known_path / anchor_hash / user_path`
fn ensure_v21_columns(conn: &Connection) -> AppResult<()> {
    let adds: [(&str, &str); 5] = [
        ("games", "game_uuid"),
        ("game_versions", "version_uuid"),
        ("game_versions", "last_known_path"),
        ("game_versions", "anchor_hash"),
        ("game_versions", "user_path"),
    ];
    for (table, column) in adds {
        if !table_has_column(conn, table, column)? {
            conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"), [])?;
        }
    }
    Ok(())
}

/// 给缺 UUID 的历史行补 UUID v4（v2.1 首次启动时一次性完成）
fn backfill_v21_uuids(conn: &Connection) -> AppResult<()> {
    for (table, column) in [("games", "game_uuid"), ("game_versions", "version_uuid")] {
        let ids: Vec<i64> = {
            let mut stmt =
                conn.prepare(&format!("SELECT id FROM {table} WHERE {column} IS NULL OR {column}=''"))?;
            let it = stmt.query_map([], |r| r.get(0))?;
            it.filter_map(|r| r.ok()).collect()
        };
        for id in ids {
            conn.execute(
                &format!("UPDATE {table} SET {column}=?1 WHERE id=?2"),
                params![uuid::Uuid::new_v4().to_string(), id],
            )?;
        }
    }
    Ok(())
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
            Some("5")
        );
        let excluded = db.get_setting("root_excluded_dirs").unwrap().unwrap();
        assert!(excluded.contains("#recycle") && excluded.contains("@eaDir"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v4 旧库（transfer_tasks 无 direction 列）→ 重建表并保留历史行
    #[test]
    fn migrate_v4_transfer_tasks_keeps_rows() {
        let dir = std::env::temp_dir().join(format!("sakugame_db_v4_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v4.db");

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE repositories(id INTEGER PRIMARY KEY, name TEXT, share_name TEXT,
                    relative_root TEXT, active_locator TEXT, created_at TEXT, last_scan_at TEXT,
                    is_offline INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE games(id INTEGER PRIMARY KEY, repository_id INTEGER,
                    relative_path TEXT, layout TEXT, display_name TEXT, game_code TEXT,
                    cover_path TEXT, size_bytes INTEGER, selected_version_id INTEGER,
                    status TEXT, created_at TEXT, updated_at TEXT);
                 CREATE TABLE game_versions(id INTEGER PRIMARY KEY, game_id INTEGER,
                    relative_path TEXT, version_label TEXT, display_name TEXT, note TEXT,
                    game_type TEXT, size_bytes INTEGER, source TEXT, status TEXT,
                    created_at TEXT, updated_at TEXT);
                 CREATE TABLE transfer_tasks(
                    id INTEGER PRIMARY KEY AUTOINCREMENT, repository_id INTEGER NOT NULL,
                    game_id INTEGER NOT NULL, version_id INTEGER NOT NULL, repo_no TEXT NOT NULL,
                    source_rel TEXT NOT NULL, dest_rel TEXT NOT NULL, total_bytes INTEGER NOT NULL,
                    total_files INTEGER NOT NULL, written_bytes INTEGER NOT NULL DEFAULT 0,
                    written_files INTEGER NOT NULL DEFAULT 0, concurrency INTEGER NOT NULL,
                    status TEXT NOT NULL DEFAULT 'queued'
                      CHECK (status IN ('queued','running','paused','error','done','cancelled')),
                    error_message TEXT, failed_files TEXT, created_at TEXT NOT NULL,
                    started_at TEXT, finished_at TEXT, updated_at TEXT NOT NULL);
                 INSERT INTO repositories(id,name,relative_root,created_at) VALUES(1,'R','','x');
                 INSERT INTO games(id,repository_id,relative_path,layout,display_name,size_bytes,
                    status,created_at,updated_at) VALUES(1,1,'G','standard','G',0,'ok','x','x');
                 INSERT INTO game_versions(id,game_id,relative_path,version_label,display_name,
                    game_type,size_bytes,source,status,created_at,updated_at)
                    VALUES(1,1,'G/Ver','Ver','Ver','folder',0,'scan','ok','x','x');
                 INSERT INTO transfer_tasks(id,repository_id,game_id,version_id,repo_no,source_rel,
                    dest_rel,total_bytes,total_files,concurrency,status,created_at,updated_at)
                    VALUES(7,1,1,1,'R01','G/Ver','R01/G/Ver',10,1,4,'done','x','x');",
            )
            .unwrap();
        }

        let db = Db::open(&path).unwrap();
        db.with(|c| {
            let (dir_col, stage_col) = (
                table_has_column(c, "transfer_tasks", "direction")?,
                table_has_column(c, "transfer_tasks", "stage")?,
            );
            assert!(dir_col && stage_col, "应补出 direction / stage 列");
            let (n, direction): (i64, String) = c.query_row(
                "SELECT COUNT(*), COALESCE(MIN(direction),'') FROM transfer_tasks",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(n, 1, "历史行必须保留");
            assert_eq!(direction, "pull", "历史行 direction 回填 pull");
            // 新状态值必须可用
            c.execute(
                "INSERT INTO transfer_tasks(repository_id,game_id,version_id,repo_no,source_rel,
                    dest_rel,total_bytes,total_files,concurrency,direction,stage,status,
                    created_at,updated_at)
                 VALUES(1,1,1,'R01','G/Ver','R01/G/Ver',1,1,4,'writeback','waiting_confirm',
                    'waiting_confirm','x','x')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v5 新列与新表齐备，且历史行 UUID 已回填
    #[test]
    fn schema_v5_columns_and_uuids() {
        let (db, dir) = temp_db("v5");
        db.with(|c| {
            assert!(table_has_column(c, "games", "game_uuid")?);
            for col in ["version_uuid", "last_known_path", "anchor_hash", "user_path"] {
                assert!(table_has_column(c, "game_versions", col)?, "缺列 {col}");
            }
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN
                 ('alter_files','mtime_baseline','writeback_packages')",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 3, "v2.1 三张新表应建齐");
            Ok(())
        })
        .unwrap();
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
