//! `.SakuAnchor.json` 锚点：生成、校验、重建、搬运源定位。
//! 依据：DSPlans/Specs/MVPv2.1技术规格.md §5
//!
//! 统一路径公式（两种布局通用）：
//! `<仓库根>/<version.relative_path>/SakuAlter/.SakuAnchor.json`
//! 紧凑型的 `version.relative_path` 等于游戏标识层路径，公式自动成立。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::cache::join_rel;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::models::SakuAnchor;
use crate::scanner::extend_path;

pub const ALTER_DIR: &str = "SakuAlter";
pub const BG_DIR: &str = "BG";
pub const UD_DIR: &str = "UD";
pub const ANCHOR_FILE: &str = ".SakuAnchor.json";

pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// blake3 hex（小写）
pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// 文件内容 hash（blake3）
pub fn hash_file(path: &Path) -> AppResult<String> {
    let mut f = std::fs::File::open(extend_path(path))?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn alter_dir(version_root: &Path) -> PathBuf {
    version_root.join(ALTER_DIR)
}

pub fn anchor_path(version_root: &Path) -> PathBuf {
    version_root.join(ALTER_DIR).join(ANCHOR_FILE)
}

pub fn read_anchor(path: &Path) -> Option<SakuAnchor> {
    let bytes = std::fs::read(extend_path(path)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 原子写（临时文件 + rename），避免半写锚点被误判
pub fn write_anchor(path: &Path, anchor: &SakuAnchor) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(extend_path(parent))?;
    }
    let json = serde_json::to_vec_pretty(anchor)
        .map_err(|e| AppError::Internal(format!("序列化锚点失败: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(extend_path(&tmp), &json)?;
    std::fs::rename(extend_path(&tmp), extend_path(path))?;
    Ok(())
}

/// 锚点同步结果
pub struct AnchorSync {
    /// ok / created / rewritten / skipped
    pub state: &'static str,
    /// 落盘后锚点字节的 blake3；skipped 时为空
    pub hash: String,
}

impl AnchorSync {
    fn skipped() -> Self {
        AnchorSync {
            state: "skipped",
            hash: String::new(),
        }
    }
}

/// 保证版本层根下有正确锚点。
/// `SakuAlter` 不存在时**不创建目录**、直接 skipped（不污染用户仓库）。
pub fn ensure_anchor(version_root: &Path, game_uuid: &str, version_uuid: &str) -> AppResult<AnchorSync> {
    if game_uuid.is_empty() || version_uuid.is_empty() {
        return Ok(AnchorSync::skipped());
    }
    let dir = alter_dir(version_root);
    if !dir.exists() {
        return Ok(AnchorSync::skipped());
    }
    let path = dir.join(ANCHOR_FILE);
    let existing = read_anchor(&path);
    let need_write = match &existing {
        None => true,
        Some(a) => a.game_uuid != game_uuid || a.version_uuid != version_uuid,
    };
    let state = if need_write {
        let anchor = SakuAnchor {
            schema_version: app_version().to_string(),
            game_uuid: game_uuid.to_string(),
            version_uuid: version_uuid.to_string(),
            created_at: chrono::Utc::now().timestamp(),
        };
        write_anchor(&path, &anchor)?;
        if existing.is_some() {
            "rewritten"
        } else {
            "created"
        }
    } else {
        "ok"
    };
    let bytes = std::fs::read(extend_path(&path)).unwrap_or_default();
    Ok(AnchorSync {
        state,
        hash: hash_bytes(&bytes),
    })
}

/// 扫描落库后同步整仓锚点（只处理已存在 `SakuAlter` 的版本）。
pub fn sync_repository(db: &Db, repository_root: &Path, repository_id: i64) -> AppResult<u32> {
    let rows: Vec<(i64, String, String, String)> = db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT v.id, v.relative_path, COALESCE(g.game_uuid,''), COALESCE(v.version_uuid,'')
             FROM game_versions v JOIN games g ON g.id = v.game_id
             WHERE g.repository_id=?1 AND v.status='ok'",
        )?;
        let it = stmt.query_map(params![repository_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        Ok(it.filter_map(|r| r.ok()).collect())
    })?;

    let mut written = 0u32;
    for (version_id, version_rel, game_uuid, version_uuid) in rows {
        let vroot = join_rel(repository_root, &version_rel);
        let sync = ensure_anchor(&vroot, &game_uuid, &version_uuid)?;
        if sync.state == "skipped" {
            continue;
        }
        if sync.state != "ok" {
            written += 1;
        }
        db.with(|conn| {
            conn.execute(
                "UPDATE game_versions SET anchor_hash=?1, last_known_path=?2 WHERE id=?3",
                params![sync.hash, vroot.to_string_lossy().to_string(), version_id],
            )?;
            Ok(())
        })?;
    }
    Ok(written)
}

// ---------------------------------------------------------------------------
// 源定位（点搬运时）
// ---------------------------------------------------------------------------

pub struct VersionLocation {
    pub version_rel: String,
    pub last_known_path: Option<String>,
    pub version_uuid: String,
    pub repository_id: i64,
}

pub fn load_location(db: &Db, version_id: i64) -> AppResult<VersionLocation> {
    db.with(|conn| {
        conn.query_row(
            "SELECT v.relative_path, v.last_known_path, COALESCE(v.version_uuid,''), g.repository_id
             FROM game_versions v JOIN games g ON g.id = v.game_id
             WHERE v.id=?1",
            params![version_id],
            |r| {
                Ok(VersionLocation {
                    version_rel: r.get(0)?,
                    last_known_path: r.get(1)?,
                    version_uuid: r.get(2)?,
                    repository_id: r.get(3)?,
                })
            },
        )
        .map_err(|_| AppError::VersionNotFound(version_id))
    })
}

/// 定位版本层根（绝对路径）。顺序：last_known_path 快路径 → 全库锚点兜底 → locator 回退。
pub fn resolve_version_root(db: &Db, version_id: i64) -> AppResult<PathBuf> {
    let loc = load_location(db, version_id)?;

    // 1. last_known_path 快路径
    if let Some(p) = loc.last_known_path.as_deref() {
        if !p.is_empty() {
            let root = PathBuf::from(p);
            if root.exists() {
                return Ok(root);
            }
        }
    }

    // 2. 全库锚点兜底（文件夹被移动/改名后仍能找到）
    if !loc.version_uuid.is_empty() {
        if let Some(found) = find_by_anchor(db, &loc.version_uuid)? {
            db.with(|conn| {
                conn.execute(
                    "UPDATE game_versions SET last_known_path=?1 WHERE id=?2",
                    params![found.to_string_lossy().to_string(), version_id],
                )?;
                Ok(())
            })?;
            return Ok(found);
        }
    }

    // 3. locator 回退（仓库自身的盘符/IP/主机名定位）
    let repo_root = crate::transfer::resolve_repo_root(db, loc.repository_id)?;
    Ok(join_rel(&repo_root, &loc.version_rel))
}

/// 遍历所有已登记仓库根，找 `version_uuid` 匹配的版本层根。
/// 只扫两层（`<root>/<游戏>/SakuAlter`、`<root>/<游戏>/<版本>/SakuAlter`），不递归内容。
fn find_by_anchor(db: &Db, version_uuid: &str) -> AppResult<Option<PathBuf>> {
    let roots: Vec<String> = db.with(|conn| {
        let mut stmt =
            conn.prepare("SELECT DISTINCT path_template FROM repository_locators")?;
        let it = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(it.filter_map(|r| r.ok()).collect())
    })?;

    for root in roots {
        let root_path = PathBuf::from(&root);
        let Ok(game_dirs) = std::fs::read_dir(extend_path(&root_path)) else {
            continue;
        };
        for game in game_dirs.flatten() {
            let game_path = game.path();
            if !game_path.is_dir() {
                continue;
            }
            // 紧凑型：<root>/<游戏>/SakuAlter
            if anchor_matches(&game_path, version_uuid) {
                return Ok(Some(game_path));
            }
            // 标准型：<root>/<游戏>/<版本>/SakuAlter
            let Ok(ver_dirs) = std::fs::read_dir(extend_path(&game_path)) else {
                continue;
            };
            for ver in ver_dirs.flatten() {
                let ver_path = ver.path();
                if ver_path.is_dir() && anchor_matches(&ver_path, version_uuid) {
                    return Ok(Some(ver_path));
                }
            }
        }
    }
    Ok(None)
}

fn anchor_matches(version_root: &Path, version_uuid: &str) -> bool {
    read_anchor(&anchor_path(version_root))
        .map(|a| a.version_uuid == version_uuid)
        .unwrap_or(false)
}

/// 扫描前加载「版本相对路径 → version_uuid」映射（供增量跳过用）。
/// 只包含 DB 里已存在且 uuid 非空的版本。
pub fn load_anchor_map(db: &Db, repository_id: i64) -> HashMap<String, String> {
    db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT v.relative_path, v.version_uuid FROM game_versions v
             JOIN games g ON g.id = v.game_id
             WHERE g.repository_id=?1 AND v.version_uuid IS NOT NULL AND v.version_uuid<>''",
        )?;
        let it = stmt.query_map(params![repository_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(it
            .filter_map(|r| r.ok())
            .collect::<HashMap<String, String>>())
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sakugame_anchor_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ensure_anchor_skips_without_alter_dir() {
        let dir = temp_dir("skip");
        let sync = ensure_anchor(&dir, "g-1", "v-1").unwrap();
        assert_eq!(sync.state, "skipped");
        assert!(!alter_dir(&dir).exists(), "不得创建 SakuAlter 目录");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_anchor_creates_then_keeps() {
        let dir = temp_dir("create");
        std::fs::create_dir_all(alter_dir(&dir)).unwrap();

        let first = ensure_anchor(&dir, "g-1", "v-1").unwrap();
        assert_eq!(first.state, "created");
        assert!(!first.hash.is_empty());
        let anchor = read_anchor(&anchor_path(&dir)).expect("锚点应存在");
        assert_eq!(anchor.version_uuid, "v-1");

        let second = ensure_anchor(&dir, "g-1", "v-1").unwrap();
        assert_eq!(second.state, "ok");
        assert_eq!(second.hash, first.hash, "重复调用不应改写");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_anchor_rewrites_on_uuid_drift() {
        let dir = temp_dir("rewrite");
        std::fs::create_dir_all(alter_dir(&dir)).unwrap();
        ensure_anchor(&dir, "g-1", "v-1").unwrap();

        let sync = ensure_anchor(&dir, "g-1", "v-2").unwrap();
        assert_eq!(sync.state, "rewritten");
        assert_eq!(read_anchor(&anchor_path(&dir)).unwrap().version_uuid, "v-2");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_file_matches_hash_bytes() {
        let dir = temp_dir("hash");
        let f = dir.join("a.bin");
        std::fs::write(&f, b"hello-sakugame").unwrap();
        assert_eq!(
            hash_file(&f).unwrap(),
            hash_bytes(b"hello-sakugame"),
            "文件 hash 与字节 hash 必须同口径"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
