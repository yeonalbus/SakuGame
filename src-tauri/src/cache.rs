//! 缓存池：配置、容量记账、预检、对账、清理、池内路径约定。
//! 依据：DSPlans/Specs/MVPv2技术规格.md §5 / §7 / §8 / §10

use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::models::{CacheConfig, CacheUsage, PreflightReason, TransferPreflight};
use crate::scanner::extend_path;

/// 默认配额：10 GiB（只接受整数，前端换算 MB/GB）
pub const DEFAULT_QUOTA_BYTES: i64 = 10 * 1024 * 1024 * 1024;
/// 盘总容量的 10% 作为安全线
pub const SAFETY_PERCENT: i64 = 10;
/// 紧凑型的版本层通用占位符（定义 3.0）
pub const COMPACT_MARKER: &str = "SakuCompact";

const SETTING_ROOT: &str = "cache_pool_root";
const SETTING_QUOTA: &str = "cache_pool_quota_bytes";

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

pub fn load_config(db: &Db) -> AppResult<CacheConfig> {
    let root = db.get_setting(SETTING_ROOT)?.filter(|s| !s.trim().is_empty());
    let quota = db
        .get_setting(SETTING_QUOTA)?
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_QUOTA_BYTES);
    Ok(CacheConfig {
        root,
        quota_bytes: quota,
    })
}

/// 池根绝对路径（未配置即报错）
pub fn pool_root(db: &Db) -> AppResult<PathBuf> {
    load_config(db)?
        .root
        .map(PathBuf::from)
        .ok_or(AppError::CacheNotConfigured)
}

/// 保存配置。校验顺序见技术规格 §5.1。
pub fn save_config(db: &Db, root: Option<&str>, quota_bytes: i64) -> AppResult<()> {
    if quota_bytes <= 0 {
        return Err(AppError::InvalidInput("缓存池容量必须为正整数".into()));
    }
    let current = load_config(db)?;
    let new_root = root.map(str::trim).filter(|s| !s.is_empty());

    let root_changed = match (&current.root, new_root) {
        (None, None) => false,
        (Some(a), Some(b)) => normalize_path(a) != normalize_path(b),
        _ => true,
    };

    if let Some(r) = new_root {
        let p = Path::new(r);
        if !p.is_absolute() {
            return Err(AppError::InvalidInput("缓存池路径必须是绝对路径".into()));
        }
        std::fs::create_dir_all(extend_path(p))
            .map_err(|e| AppError::InvalidInput(format!("缓存池路径不可用: {e}")))?;
        if path_inside_any_repository(db, r)? {
            return Err(AppError::PoolPathInsideRepository);
        }
    }
    if root_changed && new_root.is_some() && has_cache_or_tasks(db)? {
        return Err(AppError::CachePathLocked);
    }

    // 容量校验（缩容不低于已用；扩容后需留出安全线）
    if let Some(r) = new_root {
        let draft = CacheConfig {
            root: Some(r.to_string()),
            quota_bytes,
        };
        let u = usage_with(db, &draft)?;
        if quota_bytes < u.used_bytes {
            return Err(AppError::QuotaExceeded {
                required: u.used_bytes - quota_bytes,
                available: 0,
            });
        }
        let grow = quota_bytes - u.used_bytes;
        let room = (u.disk_free_bytes - u.safety_bytes).max(0);
        if grow > room {
            return Err(AppError::DiskExceeded {
                required: grow,
                available: room,
            });
        }
    }

    match new_root {
        Some(r) => db.set_setting(SETTING_ROOT, r)?,
        None => db.set_setting(SETTING_ROOT, "")?,
    }
    db.set_setting(SETTING_QUOTA, &quota_bytes.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 卷容量
// ---------------------------------------------------------------------------

/// 返回 (卷总容量, 卷剩余)。路径必须已存在。
#[cfg(windows)]
pub fn disk_space(path: &Path) -> AppResult<(i64, i64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free_to_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            &mut total,
            &mut total_free,
        )
    };
    if ok == 0 {
        return Err(AppError::Io(std::io::Error::last_os_error()));
    }
    Ok((total as i64, free_to_caller as i64))
}

#[cfg(not(windows))]
pub fn disk_space(_path: &Path) -> AppResult<(i64, i64)> {
    Err(AppError::Internal("缓存池容量探测仅支持 Windows".into()))
}

// ---------------------------------------------------------------------------
// 容量记账
// ---------------------------------------------------------------------------

/// 已用 = 已完成缓存 + 在途任务（含预留），见技术规格 §5.2
pub fn used_bytes(db: &Db) -> AppResult<i64> {
    db.with(|conn| {
        let cached: i64 = conn.query_row(
            "SELECT COALESCE(SUM(size_bytes),0) FROM cache_entries WHERE status='ok'",
            [],
            |r| r.get(0),
        )?;
        let inflight: i64 = conn.query_row(
            "SELECT COALESCE(SUM(total_bytes),0) FROM transfer_tasks
             WHERE status IN ('queued','running','paused')",
            [],
            |r| r.get(0),
        )?;
        Ok(cached + inflight)
    })
}

pub fn usage_with(db: &Db, cfg: &CacheConfig) -> AppResult<CacheUsage> {
    let used = used_bytes(db)?;
    let (total, free) = match &cfg.root {
        Some(r) => disk_space(Path::new(r))?,
        None => (0, 0),
    };
    let safety = total * SAFETY_PERCENT / 100;
    Ok(CacheUsage {
        quota_bytes: cfg.quota_bytes,
        used_bytes: used,
        free_bytes: cfg.quota_bytes - used,
        disk_total_bytes: total,
        disk_free_bytes: free,
        safety_bytes: safety,
        available_bytes: free - safety,
    })
}

pub fn usage(db: &Db) -> AppResult<CacheUsage> {
    let cfg = load_config(db)?;
    usage_with(db, &cfg)
}

// ---------------------------------------------------------------------------
// 预检
// ---------------------------------------------------------------------------

pub struct VersionMeta {
    pub game_id: i64,
    pub repository_id: i64,
    pub game_rel: String,
    pub version_rel: String,
    pub game_type: String,
    pub layout: String,
    pub version_status: String,
    pub game_name: String,
    pub version_label: String,
}

pub fn load_version_meta(db: &Db, version_id: i64) -> AppResult<VersionMeta> {
    db.with(|conn| {
        conn.query_row(
            "SELECT g.id, g.repository_id, g.relative_path, v.relative_path, v.game_type,
                    g.layout, v.status, g.display_name, v.version_label
             FROM game_versions v JOIN games g ON g.id = v.game_id WHERE v.id=?1",
            params![version_id],
            |r| {
                Ok(VersionMeta {
                    game_id: r.get(0)?,
                    repository_id: r.get(1)?,
                    game_rel: r.get(2)?,
                    version_rel: r.get(3)?,
                    game_type: r.get(4)?,
                    layout: r.get(5)?,
                    version_status: r.get(6)?,
                    game_name: r.get(7)?,
                    version_label: r.get(8)?,
                })
            },
        )
        .optional()?
        .ok_or(AppError::VersionNotFound(version_id))
    })
}

/// 预检：纯读，不预留。`repository_root` 为仓库根绝对路径。
pub fn preflight(
    db: &Db,
    version_id: i64,
    repository_root: &Path,
) -> AppResult<TransferPreflight> {
    let cfg = load_config(db)?;
    let meta = load_version_meta(db, version_id)?;
    let u = usage_with(db, &cfg)?;
    let quota_room = u.quota_bytes - u.used_bytes;

    let mut required = 0i64;
    let mut reason = None;

    if cfg.root.is_none() {
        reason = Some(PreflightReason::PoolNotConfigured);
    } else if meta.version_status != "ok" {
        reason = Some(PreflightReason::InvalidVersion);
    } else if meta.game_type == "archive" {
        reason = Some(PreflightReason::ArchiveUnsupported);
    } else if is_cached(db, version_id)? {
        reason = Some(PreflightReason::AlreadyCached);
    } else {
        let src = join_rel(repository_root, &meta.version_rel);
        required = sum_tree(&src)?.0;
        if required > quota_room {
            reason = Some(PreflightReason::QuotaExceeded);
        } else if required > u.available_bytes {
            reason = Some(PreflightReason::DiskExceeded);
        }
    }

    Ok(TransferPreflight {
        ok: reason.is_none(),
        reason,
        required_bytes: required,
        available_quota_bytes: quota_room,
        available_disk_bytes: u.available_bytes,
    })
}

pub fn is_cached(db: &Db, version_id: i64) -> AppResult<bool> {
    db.with(|conn| {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM cache_entries WHERE version_id=?1 AND status='ok'",
            params![version_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    })
}

// ---------------------------------------------------------------------------
// 池内路径约定
// ---------------------------------------------------------------------------

/// 仓库编号：`R{id:02}`，不循环（id ≥ 100 自然三位）
pub fn repo_no(id: i64) -> String {
    format!("R{id:02}")
}

/// 池内相对路径（不含池根），见技术规格 §2.1
pub fn dest_rel(meta: &VersionMeta) -> String {
    let game_layer = last_segment(&meta.game_rel);
    if meta.layout == "compact" {
        format!(
            "{}/{}/{}",
            repo_no(meta.repository_id),
            game_layer,
            COMPACT_MARKER
        )
    } else {
        format!(
            "{}/{}/{}",
            repo_no(meta.repository_id),
            game_layer,
            last_segment(&meta.version_rel)
        )
    }
}

/// 仓库编号层目录（删仓库时整体清理）
pub fn repo_dir(root: &Path, repository_id: i64) -> PathBuf {
    root.join(repo_no(repository_id))
}

// ---------------------------------------------------------------------------
// 对账与清理
// ---------------------------------------------------------------------------

/// 启动对账：记录在、目录不在 → missing
pub fn reconcile(db: &Db) -> AppResult<()> {
    let cfg = load_config(db)?;
    let Some(root) = cfg.root else {
        return Ok(());
    };
    let root = PathBuf::from(root);
    let rows: Vec<(i64, String)> = db.with(|conn| {
        let mut stmt = conn.prepare("SELECT id, dest_rel FROM cache_entries WHERE status='ok'")?;
        let it = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        Ok(it.filter_map(|r| r.ok()).collect())
    })?;
    for (id, rel) in rows {
        let dir = join_rel(&root, &rel);
        if !dir.exists() {
            db.with(|conn| {
                conn.execute(
                    "UPDATE cache_entries SET status='missing', last_verified_at=?1 WHERE id=?2",
                    params![Db::now(), id],
                )?;
                Ok(())
            })?;
        }
    }
    Ok(())
}

/// 校验单个版本的缓存记录，返回是否 ok（顺带落库 missing）
pub fn verify_cached(db: &Db, version_id: i64) -> AppResult<Option<bool>> {
    let cfg = load_config(db)?;
    let row: Option<(i64, String, String)> = db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT id, dest_rel, status FROM cache_entries WHERE version_id=?1",
                params![version_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
    })?;
    let Some((id, rel, status)) = row else {
        return Ok(None);
    };
    if status != "ok" {
        return Ok(Some(false));
    }
    let exists = cfg
        .root
        .map(|root| join_rel(Path::new(&root), &rel).exists())
        .unwrap_or(false);
    if !exists {
        db.with(|conn| {
            conn.execute(
                "UPDATE cache_entries SET status='missing', last_verified_at=?1 WHERE id=?2",
                params![Db::now(), id],
            )?;
            Ok(())
        })?;
        return Ok(Some(false));
    }
    Ok(Some(true))
}

/// 从缓存池移除（只删池内副本，不碰仓库）
pub fn remove_cache(db: &Db, version_id: i64) -> AppResult<()> {
    if has_active_task(db, version_id)? {
        return Err(AppError::CacheRemoveBusy);
    }
    let cfg = load_config(db)?;
    let entry: Option<String> = db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT dest_rel FROM cache_entries WHERE version_id=?1",
                params![version_id],
                |r| r.get(0),
            )
            .optional()?)
    })?;
    let Some(rel) = entry else {
        return Ok(());
    };
    if let Some(root) = cfg.root {
        let dir = join_rel(Path::new(&root), &rel);
        if dir.exists() {
            std::fs::remove_dir_all(extend_path(&dir))
                .map_err(|e| AppError::InvalidInput(format!("删除缓存目录失败: {e}")))?;
        }
    }
    db.with(|conn| {
        conn.execute(
            "DELETE FROM cache_entries WHERE version_id=?1",
            params![version_id],
        )?;
        Ok(())
    })?;
    Ok(())
}

pub fn has_active_task(db: &Db, version_id: i64) -> AppResult<bool> {
    db.with(|conn| {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM transfer_tasks
             WHERE version_id=?1 AND status IN ('queued','running','paused')",
            params![version_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    })
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

fn last_segment(rel: &str) -> String {
    rel.replace('\\', "/")
        .trim_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(rel)
        .to_string()
}

pub fn join_rel(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    p
}

/// 递归统计（字节, 文件数）。跳过失败项会掩盖问题，此处直接报错，由调用方决定提示。
pub fn sum_tree(root: &Path) -> AppResult<(i64, i64)> {
    let mut bytes = 0i64;
    let mut files = 0i64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(extend_path(&dir))?;
        for entry in entries {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += meta.len() as i64;
                files += 1;
            }
        }
    }
    Ok((bytes, files))
}

fn normalize_path(s: &str) -> String {
    s.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn is_inside(child: &str, parent: &str) -> bool {
    let c = normalize_path(child);
    let p = normalize_path(parent);
    if p.is_empty() {
        return false;
    }
    c == p || c.starts_with(&format!("{p}\\"))
}

fn path_inside_any_repository(db: &Db, pool: &str) -> AppResult<bool> {
    let templates: Vec<String> = db.with(|conn| {
        let mut stmt = conn.prepare("SELECT path_template FROM repository_locators")?;
        let it = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(it.filter_map(|r| r.ok()).collect())
    })?;
    Ok(templates.iter().any(|t| is_inside(pool, t)))
}

fn has_cache_or_tasks(db: &Db) -> AppResult<bool> {
    db.with(|conn| {
        let n: i64 = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM cache_entries WHERE status='ok')
                  + (SELECT COUNT(*) FROM transfer_tasks
                     WHERE status IN ('queued','running','paused'))",
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_no_padding() {
        assert_eq!(repo_no(1), "R01");
        assert_eq!(repo_no(42), "R42");
        assert_eq!(repo_no(100), "R100");
    }

    #[test]
    fn path_prefix_logic() {
        assert!(is_inside(r"D:\Cache\Game", r"D:\Cache"));
        assert!(is_inside(r"D:\Cache", r"D:\Cache"));
        assert!(!is_inside(r"D:\Cache2\Game", r"D:\Cache"));
        assert!(is_inside("d:/cache/game", "D:\\CACHE"));
    }

    #[test]
    fn compacet_marker_in_dest() {
        let meta = VersionMeta {
            game_id: 1,
            repository_id: 7,
            game_rel: "St234567".into(),
            version_rel: "St234567".into(),
            game_type: "folder".into(),
            layout: "compact".into(),
            version_status: "ok".into(),
            game_name: "game".into(),
            version_label: "St234567".into(),
        };
        assert_eq!(dest_rel(&meta), "R07/St234567/SakuCompact");
    }

    #[test]
    fn standard_dest_uses_version_layer() {
        let meta = VersionMeta {
            game_id: 1,
            repository_id: 3,
            game_rel: "RJ123456".into(),
            version_rel: "RJ123456/RJV1".into(),
            game_type: "folder".into(),
            layout: "standard".into(),
            version_status: "ok".into(),
            game_name: "RJ123456".into(),
            version_label: "RJV1".into(),
        };
        assert_eq!(dest_rel(&meta), "R03/RJ123456/RJV1");
    }
}
