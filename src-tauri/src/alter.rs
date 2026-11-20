//! 变更组：注入 / 检测 / 存储（含 `.waiting` 回写包与崩溃恢复）。
//! 依据：DSPlans/Specs/MVPv2.1技术规格.md §6 / §7 / §8

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use tauri::{AppHandle, Emitter};

use crate::anchor::{self, ALTER_DIR, BG_DIR, UD_DIR};
use crate::cache::{self, join_rel};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::models::{
    AlterRef, AlterSummary, CandidateFile, ManifestFile, TransferProgressEvent, TransferStatus,
    WritebackCandidates, WritebackManifest,
};
use crate::scanner::extend_path;

/// 回写包目录（与 v2 的 `.staging` 同级）
pub const WAITING_DIR: &str = ".waiting";
pub const MANIFEST_FILE: &str = "manifest.json";
const MANIFEST_SCHEMA: i64 = 1;

// ---------------------------------------------------------------------------
// 通用工具
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// 相对根，`/` 分隔
    pub rel_path: String,
    pub size_bytes: i64,
    pub mtime_ms: i64,
}

/// 递归列举文件；`exclude_top` 排除顶层同名目录（用于排除 `SakuAlter`）
pub fn scan_files(root: &Path, exclude_top: Option<&str>) -> AppResult<Vec<ScannedFile>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(extend_path(&dir)) else {
            continue;
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if rel.is_empty() {
                if let Some(ex) = exclude_top {
                    if name.eq_ignore_ascii_case(ex) {
                        continue;
                    }
                }
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let child_rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if meta.is_dir() {
                stack.push((entry.path(), child_rel));
            } else {
                out.push(ScannedFile {
                    rel_path: child_rel,
                    size_bytes: meta.len() as i64,
                    mtime_ms: mtime_ms(&meta),
                });
            }
        }
    }
    Ok(out)
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn copy_one(src: &Path, dst: &Path) -> AppResult<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(extend_path(parent))?;
    }
    std::fs::copy(extend_path(src), extend_path(dst))?;
    Ok(())
}

/// 用户路径状态：ok / unset / unreachable
pub fn user_path_state(user_path: Option<&str>) -> &'static str {
    match user_path {
        None => "unset",
        Some(p) if p.trim().is_empty() => "unset",
        Some(p) => {
            if Path::new(p).is_dir() {
                "ok"
            } else {
                "unreachable"
            }
        }
    }
}

pub fn load_user_path(db: &Db, version_id: i64) -> AppResult<Option<String>> {
    db.with(|conn| {
        let v: Option<Option<String>> = conn
            .query_row(
                "SELECT user_path FROM game_versions WHERE id=?1",
                params![version_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.flatten().filter(|s| !s.trim().is_empty()))
    })
}

// ---------------------------------------------------------------------------
// 注入（pull 完成后）
// ---------------------------------------------------------------------------

pub struct InjectOutcome {
    pub bg_files: i64,
    pub ud_files: i64,
    pub ud_state: &'static str,
}

/// 从 SakuAlter 提取 BG/UD 覆盖到版本层根与用户路径，并记录变更组文件 hash。
/// `SakuAlter` 不存在 → 直接跳过（不创建）。
pub fn inject(
    db: &Db,
    version_id: i64,
    version_root: &Path,
    user_path: Option<&str>,
) -> AppResult<InjectOutcome> {
    let alter = anchor::alter_dir(version_root);
    let mut outcome = InjectOutcome {
        bg_files: 0,
        ud_files: 0,
        ud_state: "unset",
    };
    if !alter.is_dir() {
        return Ok(outcome);
    }

    let bg = alter.join(BG_DIR);
    for f in scan_files(&bg, None)? {
        let src = join_rel(&bg, &f.rel_path);
        let dst = join_rel(version_root, &f.rel_path);
        copy_one(&src, &dst)?;
        let hash = anchor::hash_file(&src)?;
        upsert_alter_file(db, version_id, "bg", &f.rel_path, f.size_bytes, &hash)?;
        outcome.bg_files += 1;
    }

    let state = user_path_state(user_path);
    outcome.ud_state = state;
    if state == "ok" {
        let upath = Path::new(user_path.unwrap_or_default());
        let ud = alter.join(UD_DIR);
        for f in scan_files(&ud, None)? {
            let src = join_rel(&ud, &f.rel_path);
            let dst = join_rel(upath, &f.rel_path);
            copy_one(&src, &dst)?;
            let hash = anchor::hash_file(&src)?;
            upsert_alter_file(db, version_id, "ud", &f.rel_path, f.size_bytes, &hash)?;
            outcome.ud_files += 1;
        }
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// 基线（注入完毕后）
// ---------------------------------------------------------------------------

/// 记录 mtime 基线（整体覆盖写）。返回 (bg 文件数, ud 文件数)。
pub fn record_baseline(
    db: &Db,
    version_id: i64,
    version_root: &Path,
    user_path: Option<&str>,
) -> AppResult<(i64, i64)> {
    let bg_files = scan_files(version_root, Some(ALTER_DIR))?;
    let ud_files = if user_path_state(user_path) == "ok" {
        scan_files(Path::new(user_path.unwrap_or_default()), None)?
    } else {
        Vec::new()
    };

    db.with_mut(|conn| {
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM mtime_baseline WHERE version_id=?1",
            params![version_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO mtime_baseline(version_id, scope, rel_path, size_bytes, mtime_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
            )?;
            for f in &bg_files {
                stmt.execute(params![version_id, "bg", f.rel_path, f.size_bytes, f.mtime_ms])?;
            }
            for f in &ud_files {
                stmt.execute(params![version_id, "ud", f.rel_path, f.size_bytes, f.mtime_ms])?;
            }
        }
        tx.commit()?;
        Ok(())
    })?;
    Ok((bg_files.len() as i64, ud_files.len() as i64))
}

// ---------------------------------------------------------------------------
// 变更组 / 基线读写
// ---------------------------------------------------------------------------

fn upsert_alter_file(
    db: &Db,
    version_id: i64,
    scope: &str,
    rel_path: &str,
    size_bytes: i64,
    hash: &str,
) -> AppResult<()> {
    db.with(|conn| {
        conn.execute(
            "INSERT INTO alter_files(version_id, scope, rel_path, size_bytes, hash, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(version_id, scope, rel_path) DO UPDATE SET
                size_bytes=excluded.size_bytes, hash=excluded.hash, updated_at=excluded.updated_at",
            params![version_id, scope, rel_path, size_bytes, hash, Db::now()],
        )?;
        Ok(())
    })
}

pub struct AlterRow {
    pub scope: String,
    pub rel_path: String,
    pub size_bytes: i64,
    pub hash: String,
}

fn load_alter_files(db: &Db, version_id: i64) -> AppResult<Vec<AlterRow>> {
    db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT scope, rel_path, size_bytes, hash FROM alter_files
             WHERE version_id=?1 ORDER BY scope, rel_path",
        )?;
        let it = stmt.query_map(params![version_id], |r| {
            Ok(AlterRow {
                scope: r.get(0)?,
                rel_path: r.get(1)?,
                size_bytes: r.get(2)?,
                hash: r.get(3)?,
            })
        })?;
        Ok(it.filter_map(|r| r.ok()).collect())
    })
}

type BaselineMap = HashMap<(String, String), (i64, i64)>;

fn load_baseline(db: &Db, version_id: i64) -> AppResult<BaselineMap> {
    db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT scope, rel_path, size_bytes, mtime_ms FROM mtime_baseline WHERE version_id=?1",
        )?;
        let it = stmt.query_map(params![version_id], |r| {
            Ok((
                (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                (r.get::<_, i64>(2)?, r.get::<_, i64>(3)?),
            ))
        })?;
        Ok(it.filter_map(|r| r.ok()).collect())
    })
}

// ---------------------------------------------------------------------------
// 检测（移出缓存前）
// ---------------------------------------------------------------------------

/// 递归扫描当前缓存池版本层根与用户路径，与变更组记录 / mtime 基线比对，产出候选集合。
pub fn detect(db: &Db, version_id: i64) -> AppResult<WritebackCandidates> {
    let meta = cache::load_version_meta(db, version_id)?;
    let pool = cache::pool_root(db)?;
    let dest_rel: Option<String> = db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT dest_rel FROM cache_entries WHERE version_id=?1 AND status='ok'",
                params![version_id],
                |r| r.get(0),
            )
            .optional()?)
    })?;
    let dest_rel = dest_rel
        .ok_or_else(|| AppError::InvalidInput("该版本不在缓存池中，无法移出".into()))?;
    let version_root = join_rel(&pool, &dest_rel);
    if !version_root.is_dir() {
        return Err(AppError::InvalidInput(
            "缓存池内目录已丢失，请重新搬运后再试".into(),
        ));
    }

    let user_path = load_user_path(db, version_id)?;
    let up_state = user_path_state(user_path.as_deref());
    let alter_rows = load_alter_files(db, version_id)?;
    let baseline = load_baseline(db, version_id)?;
    let alter_ready = !alter_rows.is_empty();
    let baseline_ready = !baseline.is_empty();

    let bg_now = scan_files(&version_root, Some(ALTER_DIR))?;
    let ud_now = if up_state == "ok" {
        scan_files(Path::new(user_path.as_deref().unwrap_or_default()), None)?
    } else {
        Vec::new()
    };

    let mut map: HashMap<(String, String), CandidateFile> = HashMap::new();
    let mut deleted: Vec<String> = Vec::new();

    // 1) hash 严格口径：变更组文件（用户路径不可达时跳过 UD，避免全量误报为删除）
    for row in &alter_rows {
        if row.scope == "ud" && up_state != "ok" {
            continue;
        }
        let current = find_file(&bg_now, &ud_now, &row.scope, &row.rel_path);
        match current {
            Some(f) => {
                let path = current_path(
                    &version_root,
                    user_path.as_deref(),
                    &row.scope,
                    &row.rel_path,
                );
                let hash = anchor::hash_file(&path)?;
                if hash != row.hash {
                    let entry = map
                        .entry((row.scope.clone(), row.rel_path.clone()))
                        .or_insert_with(|| candidate_from(f, &row.scope, &row.rel_path));
                    if !entry.reason.iter().any(|r| r == "hash") {
                        entry.reason.push("hash".into());
                    }
                    entry.required = true;
                }
            }
            None => deleted.push(format!("{}:{}", row.scope, row.rel_path)),
        }
    }

    // 2) mtime 宽松口径：基线比对（无基线时整段跳过）
    if baseline_ready {
        for ((scope, rel), (b_size, b_mtime)) in &baseline {
            if scope == "ud" && up_state != "ok" {
                continue;
            }
            let current = find_file(&bg_now, &ud_now, scope, rel);
            match current {
                Some(f) => {
                    if f.size_bytes != *b_size || f.mtime_ms != *b_mtime {
                        let entry = map
                            .entry((scope.clone(), rel.clone()))
                            .or_insert_with(|| candidate_from(f, scope, rel));
                        if !entry.reason.iter().any(|r| r == "mtime") {
                            entry.reason.push("mtime".into());
                        }
                    }
                }
                None => deleted.push(format!("{scope}:{rel}")),
            }
        }
        // 3) 基线里没有的当前文件 → 新增
        for f in &bg_now {
            if !baseline.contains_key(&("bg".to_string(), f.rel_path.clone())) {
                let entry = map
                    .entry(("bg".to_string(), f.rel_path.clone()))
                    .or_insert_with(|| candidate_from(f, "bg", &f.rel_path));
                if !entry.reason.iter().any(|r| r == "mtime") {
                    entry.reason.push("mtime".into());
                }
            }
        }
        for f in &ud_now {
            if !baseline.contains_key(&("ud".to_string(), f.rel_path.clone())) {
                let entry = map
                    .entry(("ud".to_string(), f.rel_path.clone()))
                    .or_insert_with(|| candidate_from(f, "ud", &f.rel_path));
                if !entry.reason.iter().any(|r| r == "mtime") {
                    entry.reason.push("mtime".into());
                }
            }
        }
    }

    let mut files: Vec<CandidateFile> = map.into_values().collect();
    files.sort_by(|a, b| {
        a.scope
            .cmp(&b.scope)
            .then_with(|| a.rel_path.cmp(&b.rel_path))
    });
    deleted.sort();
    deleted.dedup();

    Ok(WritebackCandidates {
        version_id,
        game_name: meta.game_name,
        version_label: meta.version_label,
        files,
        deleted,
        user_path: user_path.clone(),
        user_path_state: up_state.to_string(),
        baseline_ready,
        alter_ready,
    })
}

fn find_file<'a>(
    bg_now: &'a [ScannedFile],
    ud_now: &'a [ScannedFile],
    scope: &str,
    rel: &str,
) -> Option<&'a ScannedFile> {
    if scope == "bg" {
        bg_now.iter().find(|f| f.rel_path == rel)
    } else {
        ud_now.iter().find(|f| f.rel_path == rel)
    }
}

fn current_path(
    version_root: &Path,
    user_path: Option<&str>,
    scope: &str,
    rel: &str,
) -> PathBuf {
    if scope == "bg" {
        join_rel(version_root, rel)
    } else {
        join_rel(Path::new(user_path.unwrap_or_default()), rel)
    }
}

fn candidate_from(f: &ScannedFile, scope: &str, rel: &str) -> CandidateFile {
    CandidateFile {
        scope: scope.to_string(),
        rel_path: rel.to_string(),
        size_bytes: f.size_bytes,
        mtime_ms: f.mtime_ms,
        reason: Vec::new(),
        required: false,
    }
}

// ---------------------------------------------------------------------------
// 回写包（.waiting）
// ---------------------------------------------------------------------------

pub fn waiting_root(pool: &Path) -> PathBuf {
    pool.join(WAITING_DIR)
}

pub fn waiting_dir(pool: &Path, task_id: i64) -> PathBuf {
    waiting_root(pool).join(task_id.to_string())
}

/// 检测完成后落盘「候选 + 空选择」，供确认与崩溃留痕
pub fn save_plan(db: &Db, task_id: i64, version_id: i64, candidates: &WritebackCandidates) -> AppResult<()> {
    let json = serde_json::to_string(candidates)
        .map_err(|e| AppError::Internal(format!("序列化候选失败: {e}")))?;
    db.with(|conn| {
        conn.execute(
            "INSERT INTO writeback_packages(task_id, version_id, stage, manifest, created_at, updated_at)
             VALUES(?1, ?2, 'detected', ?3, ?4, ?4)
             ON CONFLICT(task_id) DO UPDATE SET manifest=excluded.manifest, stage='detected',
                updated_at=excluded.updated_at",
            params![task_id, version_id, json, Db::now()],
        )?;
        Ok(())
    })
}

/// 读取已落盘的检测候选（供 IPC 展示；不重扫）
pub fn plan_view(db: &Db, task_id: i64) -> AppResult<WritebackCandidates> {
    load_plan(db, task_id)?.ok_or_else(|| {
        AppError::WritebackNotConfirmable("尚未完成检测，请稍候或重新移出缓存".into())
    })
}

fn load_plan(db: &Db, task_id: i64) -> AppResult<Option<WritebackCandidates>> {
    let raw: Option<String> = db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT manifest FROM writeback_packages WHERE task_id=?1 AND stage='detected'",
                params![task_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    })?;
    match raw {
        None => Ok(None),
        Some(s) => serde_json::from_str::<WritebackCandidates>(&s)
            .map(Some)
            .map_err(|e| AppError::Internal(format!("回写候选反序列化失败: {e}"))),
    }
}

/// 确认回写：校验勾选（必须包含全部 required），落选择集，任务回到队列
pub fn confirm(db: &Db, task_id: i64, selection: &[AlterRef]) -> AppResult<()> {
    let plan = load_plan(db, task_id)?.ok_or_else(|| {
        AppError::WritebackNotConfirmable("找不到待确认的检测结果，请重新移出缓存".into())
    })?;

    let mut picked: Vec<&CandidateFile> = Vec::new();
    for sel in selection {
        let hit = plan
            .files
            .iter()
            .find(|f| f.scope == sel.scope && f.rel_path == sel.rel_path)
            .ok_or_else(|| {
                AppError::InvalidInput(format!("文件不在候选列表中: {}:{}", sel.scope, sel.rel_path))
            })?;
        if !picked.iter().any(|p| p.scope == hit.scope && p.rel_path == hit.rel_path) {
            picked.push(hit);
        }
    }
    // required 必须全选（缺失则补上，前端理应已勾选）
    for f in plan.files.iter().filter(|f| f.required) {
        if !picked.iter().any(|p| p.scope == f.scope && p.rel_path == f.rel_path) {
            picked.push(f);
        }
    }

    let total_files = picked.len() as i64;
    let total_bytes: i64 = picked.iter().map(|f| f.size_bytes).sum();
    // 确认后按实际选中文件数重新定档（检测阶段用的是全游戏基线规模，偏保守）
    let concurrency = crate::transfer::pick_concurrency(total_files);
    let manifest = build_manifest(&plan, &picked)?;
    let json = serde_json::to_string(&manifest)
        .map_err(|e| AppError::Internal(format!("序列化回写清单失败: {e}")))?;

    db.with_mut(|conn| {
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE writeback_packages SET stage='confirmed', manifest=?1, updated_at=?2
             WHERE task_id=?3",
            params![json, Db::now(), task_id],
        )?;
        tx.execute(
            "UPDATE transfer_tasks SET status='queued', stage='extract', total_bytes=?1,
                total_files=?2, concurrency=?3, written_bytes=0, written_files=0, updated_at=?4
             WHERE id=?5 AND status='waiting_confirm'",
            params![total_bytes, total_files, concurrency, Db::now(), task_id],
        )?;
        tx.commit()?;
        Ok(())
    })?;
    Ok(())
}

fn build_manifest(plan: &WritebackCandidates, picked: &[&CandidateFile]) -> AppResult<WritebackManifest> {
    let mut files = Vec::new();
    for f in picked {
        files.push(ManifestFile {
            scope: f.scope.clone(),
            rel_path: f.rel_path.clone(),
            // 回写时重算（确认到执行之间文件可能又变了），此处仅占位
            hash: String::new(),
            size_bytes: f.size_bytes,
        });
    }
    Ok(WritebackManifest {
        schema: MANIFEST_SCHEMA,
        task_id: 0,
        version_id: plan.version_id,
        repository_id: 0,
        game_rel: String::new(),
        version_rel: String::new(),
        user_path: plan.user_path.clone(),
        created_at: Db::now(),
        files,
    })
}

/// 中止回写：任务停在 paused（游戏与存档原样）
pub fn abort(db: &Db, task_id: i64) -> AppResult<()> {
    db.with(|conn| {
        conn.execute(
            "UPDATE transfer_tasks SET status='paused', stage='waiting_confirm', updated_at=?1
             WHERE id=?2 AND status='waiting_confirm'",
            params![Db::now(), task_id],
        )?;
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// 存储（extract → clean → store）
// ---------------------------------------------------------------------------

/// 执行回写任务（由调度线程调用，任务已处于 running）
pub fn run_writeback(app: Option<&AppHandle>, db: &Db, task_id: i64) -> AppResult<()> {
    let pool = cache::pool_root(db)?;
    let (version_id, repository_id, version_rel, game_rel, user_path) = load_task_context(db, task_id)?;
    let manifest = load_confirmed_manifest(db, task_id)?;

    let version_root = join_rel(&pool, &dest_rel_of(db, version_id)?);

    // ---- ① 提取到 .waiting/<task_id>/staged/ ----
    set_stage(db, task_id, "extract")?;
    let wdir = waiting_dir(&pool, task_id);
    let staged = wdir.join("staged");
    if wdir.exists() {
        let _ = std::fs::remove_dir_all(extend_path(&wdir));
    }
    std::fs::create_dir_all(extend_path(&staged))?;

    let mut files: Vec<ManifestFile> = Vec::new();
    let mut written: i64 = 0;
    let mut done: i64 = 0;
    for mf in &manifest.files {
        let src = current_path(&version_root, user_path.as_deref(), &mf.scope, &mf.rel_path);
        if !src.is_file() {
            continue; // 确认后又被外部删除 → 跳过该项
        }
        let dst = staged.join(&mf.scope).join(rel_to_path(&mf.rel_path));
        copy_one(&src, &dst)?;
        let hash = anchor::hash_file(&dst)?;
        let size = std::fs::metadata(extend_path(&dst))
            .map(|m| m.len() as i64)
            .unwrap_or(mf.size_bytes);
        files.push(ManifestFile {
            scope: mf.scope.clone(),
            rel_path: mf.rel_path.clone(),
            hash,
            size_bytes: size,
        });
        written += size;
        done += 1;
        emit_progress(
            app,
            TransferProgressEvent {
                task_id,
                written_bytes: written,
                total_bytes: manifest_total(&manifest),
                speed_bps: 0,
                eta_secs: None,
                status: TransferStatus::Running,
            },
        );
        let _ = db.with(|conn| {
            conn.execute(
                "UPDATE transfer_tasks SET written_bytes=?1, written_files=?2, updated_at=?3 WHERE id=?4",
                params![written, done, Db::now(), task_id],
            )?;
            Ok(())
        });
    }

    let final_manifest = WritebackManifest {
        schema: MANIFEST_SCHEMA,
        task_id,
        version_id,
        repository_id,
        game_rel: game_rel.clone(),
        version_rel: version_rel.clone(),
        user_path: user_path.clone(),
        created_at: Db::now(),
        files,
    };
    write_manifest(&wdir, &final_manifest)?;
    set_package_stage(db, task_id, "extracted")?;

    // ---- ② 从缓存池移除游戏副本 ----
    set_stage(db, task_id, "clean")?;
    let dest_rel = dest_rel_of(db, version_id)?;
    let pool_version_root = join_rel(&pool, &dest_rel);
    if pool_version_root.exists() {
        std::fs::remove_dir_all(extend_path(&pool_version_root))?;
    }
    db.with(|conn| {
        conn.execute("DELETE FROM cache_entries WHERE version_id=?1", params![version_id])?;
        Ok(())
    })?;

    // ---- ③ 写回 HDD SakuAlter ----
    set_stage(db, task_id, "store")?;
    apply_manifest(db, &final_manifest)?;

    // 收尾：删包 + 任务完成
    let _ = std::fs::remove_dir_all(extend_path(&wdir));
    db.with_mut(|conn| {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM writeback_packages WHERE task_id=?1", params![task_id])?;
        tx.execute(
            "UPDATE transfer_tasks SET status='done', stage=NULL, finished_at=?1, updated_at=?1
             WHERE id=?2",
            params![Db::now(), task_id],
        )?;
        tx.commit()?;
        Ok(())
    })?;
    Ok(())
}

fn manifest_total(m: &WritebackManifest) -> i64 {
    m.files.iter().map(|f| f.size_bytes).sum()
}

/// 把回写包内容写回仓库层的 SakuAlter（BG/UD），并更新 alter_files 的 hash
pub fn apply_manifest(db: &Db, manifest: &WritebackManifest) -> AppResult<()> {
    let repo_root = crate::transfer::resolve_repo_root(db, manifest.repository_id)?;
    let version_root = join_rel(&repo_root, &manifest.version_rel);
    let alter = anchor::alter_dir(&version_root);
    let staged_root = manifest_staged_root(db, manifest.task_id)?;

    for f in &manifest.files {
        let src = staged_root.join(&f.scope).join(rel_to_path(&f.rel_path));
        let dst = if f.scope == "bg" {
            join_rel(&alter.join(BG_DIR), &f.rel_path)
        } else {
            join_rel(&alter.join(UD_DIR), &f.rel_path)
        };
        copy_one(&src, &dst)?;
        upsert_alter_file(db, manifest.version_id, &f.scope, &f.rel_path, f.size_bytes, &f.hash)?;
    }

    // 变更组可能首次创建：补/修锚点
    let (game_uuid, version_uuid): (Option<String>, Option<String>) = db.with(|conn| {
        Ok(conn.query_row(
            "SELECT g.game_uuid, v.version_uuid FROM game_versions v JOIN games g ON g.id=v.game_id
             WHERE v.id=?1",
            params![manifest.version_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })?;
    if let (Some(gu), Some(vu)) = (game_uuid, version_uuid) {
        let sync = anchor::ensure_anchor(&version_root, &gu, &vu)?;
        if sync.state != "skipped" {
            db.with(|conn| {
                conn.execute(
                    "UPDATE game_versions SET anchor_hash=?1, last_known_path=?2 WHERE id=?3",
                    params![sync.hash, version_root.to_string_lossy().to_string(), manifest.version_id],
                )?;
                Ok(())
            })?;
        }
    }
    Ok(())
}

fn rel_to_path(rel: &str) -> PathBuf {
    rel.split('/').fold(PathBuf::new(), |acc, seg| acc.join(seg))
}

fn write_manifest(wdir: &Path, manifest: &WritebackManifest) -> AppResult<()> {
    std::fs::create_dir_all(extend_path(wdir))?;
    let json = serde_json::to_vec_pretty(manifest)
        .map_err(|e| AppError::Internal(format!("序列化回写清单失败: {e}")))?;
    std::fs::write(extend_path(&wdir.join(MANIFEST_FILE)), &json)?;
    Ok(())
}

fn read_manifest(wdir: &Path) -> Option<WritebackManifest> {
    let bytes = std::fs::read(extend_path(&wdir.join(MANIFEST_FILE))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn manifest_staged_root(db: &Db, task_id: i64) -> AppResult<PathBuf> {
    let pool = cache::pool_root(db)?;
    Ok(waiting_dir(&pool, task_id).join("staged"))
}

fn load_confirmed_manifest(db: &Db, task_id: i64) -> AppResult<WritebackManifest> {
    let raw: Option<String> = db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT manifest FROM writeback_packages WHERE task_id=?1",
                params![task_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    })?;
    let raw = raw.ok_or_else(|| {
        AppError::WritebackNotConfirmable("找不到回写清单，请重新移出缓存".into())
    })?;
    serde_json::from_str::<WritebackManifest>(&raw)
        .map_err(|e| AppError::Internal(format!("回写清单反序列化失败: {e}")))
}

fn load_task_context(db: &Db, task_id: i64) -> AppResult<(i64, i64, String, String, Option<String>)> {
    db.with(|conn| {
        conn.query_row(
            "SELECT t.version_id, t.repository_id, v.relative_path, g.relative_path, v.user_path
             FROM transfer_tasks t
             JOIN game_versions v ON v.id = t.version_id
             JOIN games g ON g.id = t.game_id
             WHERE t.id=?1",
            params![task_id],
            |r| {
                let vrel: String = r.get(2)?;
                let grel: String = r.get(3)?;
                let up: Option<String> = r.get(4)?;
                Ok((r.get(0)?, r.get(1)?, vrel, grel, up.filter(|s: &String| !s.trim().is_empty())))
            },
        )
        .map_err(|_| AppError::TransferNotFound(task_id))
    })
}

fn dest_rel_of(db: &Db, version_id: i64) -> AppResult<String> {
    db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT dest_rel FROM cache_entries WHERE version_id=?1",
                params![version_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| AppError::InvalidInput("该版本不在缓存池中".into()))?)
    })
}

fn set_stage(db: &Db, task_id: i64, stage: &str) -> AppResult<()> {
    db.with(|conn| {
        conn.execute(
            "UPDATE transfer_tasks SET stage=?1, updated_at=?2 WHERE id=?3",
            params![stage, Db::now(), task_id],
        )?;
        Ok(())
    })
}

fn set_package_stage(db: &Db, task_id: i64, stage: &str) -> AppResult<()> {
    db.with(|conn| {
        conn.execute(
            "UPDATE writeback_packages SET stage=?1, updated_at=?2 WHERE task_id=?3",
            params![stage, Db::now(), task_id],
        )?;
        Ok(())
    })
}

fn emit_progress(app: Option<&AppHandle>, ev: TransferProgressEvent) {
    if let Some(app) = app {
        let _ = app.emit("transfer://progress", &ev);
    }
}

// ---------------------------------------------------------------------------
// 崩溃恢复
// ---------------------------------------------------------------------------

/// 启动时：恢复 `.waiting` 里已提取但未写回的包；无清单的残留直接删；
/// `waiting_confirm` 任务置 cancelled（游戏仍在池内，无任何写入）。
pub fn recover_on_startup(db: &Db, pool: &Path) -> AppResult<u32> {
    let mut recovered = 0u32;

    // 1) 先收尾任务行：待确认 / 已中止 → cancelled（游戏仍在池内，无任何写入）；
    //    已进入写回的 running → error（中断不可续，用户重新移出即可）
    db.with(|conn| {
        conn.execute(
            "UPDATE transfer_tasks SET status='cancelled', updated_at=?1
             WHERE status='waiting_confirm'",
            params![Db::now()],
        )?;
        conn.execute(
            "UPDATE transfer_tasks SET status='error', stage=NULL,
                error_message='回写过程被中断，请重新移出缓存', updated_at=?1
             WHERE direction='writeback' AND status='running'",
            params![Db::now()],
        )?;
        Ok(())
    })?;

    let wroot = waiting_root(pool);
    let Ok(rd) = std::fs::read_dir(extend_path(&wroot)) else {
        return Ok(0);
    };
    for entry in rd.flatten() {
        let wdir = entry.path();
        if !wdir.is_dir() {
            continue;
        }
        let Some(manifest) = read_manifest(&wdir) else {
            let _ = std::fs::remove_dir_all(extend_path(&wdir));
            continue;
        };
        // 写回 HDD
        if let Err(_e) = apply_manifest(db, &manifest) {
            // 写回失败：保留包，下次启动再试
            continue;
        }
        // 补删缓存池残余
        if let Ok(dest_rel) = dest_rel_of(db, manifest.version_id) {
            let p = join_rel(pool, &dest_rel);
            if p.exists() {
                let _ = std::fs::remove_dir_all(extend_path(&p));
            }
            let _ = db.with(|conn| {
                conn.execute(
                    "DELETE FROM cache_entries WHERE version_id=?1",
                    params![manifest.version_id],
                )?;
                Ok(())
            });
        }
        let _ = std::fs::remove_dir_all(extend_path(&wdir));
        let _ = db.with_mut(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM writeback_packages WHERE task_id=?1",
                params![manifest.task_id],
            )?;
            tx.execute(
                "UPDATE transfer_tasks SET status='done', stage=NULL, finished_at=?1, updated_at=?1
                 WHERE id=?2",
                params![Db::now(), manifest.task_id],
            )?;
            tx.commit()?;
            Ok(())
        });
        recovered += 1;
    }

    // 2) 剩余的回写包都是孤儿（对应任务已 cancelled / error），清掉
    let _ = db.with(|conn| {
        conn.execute("DELETE FROM writeback_packages", [])?;
        Ok(())
    });
    Ok(recovered)
}

// ---------------------------------------------------------------------------
// 摘要
// ---------------------------------------------------------------------------

pub fn summary(db: &Db, version_id: i64) -> AppResult<AlterSummary> {
    let user_path = load_user_path(db, version_id)?;
    let (bg_count, ud_count, bytes) = db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT scope, COUNT(*), COALESCE(SUM(size_bytes),0) FROM alter_files
             WHERE version_id=?1 GROUP BY scope",
        )?;
        let rows: Vec<(String, i64, i64)> = stmt
            .query_map(params![version_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        let mut bg = 0;
        let mut ud = 0;
        let mut bytes = 0;
        for (scope, n, b) in rows {
            if scope == "bg" {
                bg = n;
            } else {
                ud = n;
            }
            bytes += b;
        }
        Ok((bg, ud, bytes))
    })?;
    let baseline_ready: i64 = db.with(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM mtime_baseline WHERE version_id=?1",
            params![version_id],
            |r| r.get(0),
        )?)
    })?;

    let version_root = crate::anchor::resolve_version_root(db, version_id).ok();
    let anchor_state = match version_root {
        None => "missing",
        Some(root) => match anchor::read_anchor(&anchor::anchor_path(&root)) {
            None => {
                if anchor::alter_dir(&root).is_dir() {
                    "missing"
                } else {
                    "none"
                }
            }
            Some(a) => {
                let db_uuid: Option<String> = db
                    .with(|conn| {
                        Ok(conn
                            .query_row(
                                "SELECT version_uuid FROM game_versions WHERE id=?1",
                                params![version_id],
                                |r| r.get::<_, Option<String>>(0),
                            )
                            .optional()?)
                    })?
                    .flatten();
                match db_uuid {
                    Some(u) if u == a.version_uuid => "ok",
                    _ => "drift",
                }
            }
        },
    };

    Ok(AlterSummary {
        version_id,
        bg_count,
        ud_count,
        bytes,
        anchor_state: anchor_state.to_string(),
        baseline_ready: baseline_ready > 0,
        user_path_state: user_path_state(user_path.as_deref()).to_string(),
        user_path,
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sakugame_alter_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_files_excludes_alter_dir() {
        let dir = temp_dir("scan");
        std::fs::create_dir_all(dir.join("save")).unwrap();
        std::fs::create_dir_all(dir.join(ALTER_DIR).join(BG_DIR)).unwrap();
        std::fs::write(dir.join("a.dat"), b"1").unwrap();
        std::fs::write(dir.join("save/sys.dat"), b"22").unwrap();
        std::fs::write(dir.join(ALTER_DIR).join(BG_DIR).join("x.dat"), b"333").unwrap();

        let files = scan_files(&dir, Some(ALTER_DIR)).unwrap();
        let rels: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert!(rels.contains(&"a.dat"));
        assert!(rels.contains(&"save/sys.dat"));
        assert!(
            !rels.iter().any(|r| r.starts_with(ALTER_DIR)),
            "SakuAlter 必须被排除: {rels:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_path_state_three_ways() {
        let dir = temp_dir("upath");
        assert_eq!(user_path_state(None), "unset");
        assert_eq!(user_path_state(Some("  ")), "unset");
        assert_eq!(
            user_path_state(Some(dir.to_string_lossy().as_ref())),
            "ok"
        );
        assert_eq!(user_path_state(Some("Q:\\nope\\nope")), "unreachable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rel_to_path_handles_slash() {
        assert_eq!(rel_to_path("save/sys.dat"), PathBuf::from("save").join("sys.dat"));
    }

    /// 建一个最小可用库：仓库 + 档案 + 版本 + 缓存记录（池落点 R01/G/Ver）
    fn seed_db(tag: &str) -> (Db, PathBuf) {
        let base = temp_dir(tag);
        let pool = base.join("pool");
        std::fs::create_dir_all(&pool).unwrap();
        let db = Db::open(&base.join("t.db")).unwrap();
        // 直接写 settings，绕过 save_config 的磁盘空间校验（测试机可用空间随环境变化）
        db.set_setting("cache_pool_root", pool.to_string_lossy().as_ref())
            .unwrap();
        db.set_setting("cache_pool_quota_bytes", &(10_i64 << 30).to_string())
            .unwrap();
        db.with(|c| {
            c.execute_batch(
                "INSERT INTO repositories(id,name,relative_root,created_at,cache_slot)
                   VALUES(1,'R','',datetime('now'),1);
                 INSERT INTO games(id,repository_id,relative_path,layout,display_name,size_bytes,
                     status,created_at,updated_at,game_uuid)
                   VALUES(1,1,'G','standard','G',0,'ok','x','x','g-uuid');
                 INSERT INTO game_versions(id,game_id,relative_path,version_label,display_name,
                     game_type,size_bytes,source,status,created_at,updated_at,version_uuid)
                   VALUES(1,1,'G/Ver','Ver','Ver','folder',0,'scan','ok','x','x','v-uuid');
                 INSERT INTO cache_entries(repository_id,game_id,version_id,repo_no,dest_rel,
                     size_bytes,file_count,status,created_at)
                   VALUES(1,1,1,'R01','R01/G/Ver',0,0,'ok','x');",
            )?;
            Ok(())
        })
        .unwrap();
        (db, base)
    }

    /// T21-20/21/22/23：hash 与 mtime 并集、required 标记、SakuAlter 自身不入候选
    #[test]
    fn detect_merges_hash_and_mtime() {
        let (db, base) = seed_db("detect");
        let vroot = base.join("pool").join("R01").join("G").join("Ver");
        std::fs::create_dir_all(vroot.join(ALTER_DIR).join(BG_DIR)).unwrap();
        std::fs::write(vroot.join("a.dat"), b"AAA").unwrap();
        std::fs::write(vroot.join("extra.dat"), b"EXTRA").unwrap();
        std::fs::write(
            vroot.join(ALTER_DIR).join(BG_DIR).join("save.dat"),
            b"SAVE1",
        )
        .unwrap();

        // 注入：BG/save.dat 覆盖进本体 + 记录 hash
        let out = inject(&db, 1, &vroot, None).unwrap();
        assert_eq!(out.bg_files, 1, "SakuAlter/BG 下 1 个文件应被注入");
        // 基线：不含 SakuAlter
        let (bg, ud) = record_baseline(&db, 1, &vroot, None).unwrap();
        assert_eq!(ud, 0, "未配置用户路径时不应有 UD 基线");
        assert!(bg >= 3, "本体文件（含被注入的 save.dat）应进基线: {bg}");

        // 变更组文件内容变化（hash 变 / mtime 也变）+ 新增文件
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::write(vroot.join("save.dat"), b"SAVE2").unwrap();
        std::fs::write(vroot.join("new.dat"), b"NEW").unwrap();

        let c = detect(&db, 1).unwrap();
        assert!(!c.files.is_empty(), "应检测到变动");
        let save = c
            .files
            .iter()
            .find(|f| f.rel_path == "save.dat")
            .expect("save.dat 应命中");
        assert!(save.reason.iter().any(|r| r == "hash"), "{:?}", save.reason);
        assert!(save.required, "变更组文件 hash 失配必须 required");
        let added = c
            .files
            .iter()
            .find(|f| f.rel_path == "new.dat")
            .expect("new.dat 应命中");
        assert!(added.reason.iter().any(|r| r == "mtime"));
        assert!(!added.required);
        assert!(
            !c.files.iter().any(|f| f.rel_path.starts_with(ALTER_DIR)),
            "SakuAlter 自身不得进入候选"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// T21-11：无 SakuAlter 时注入跳过，基线仍建立
    #[test]
    fn inject_skips_without_alter_dir() {
        let (db, base) = seed_db("noalter");
        let vroot = base.join("pool").join("R01").join("G").join("Ver");
        std::fs::create_dir_all(&vroot).unwrap();
        std::fs::write(vroot.join("a.dat"), b"AAA").unwrap();

        let out = inject(&db, 1, &vroot, None).unwrap();
        assert_eq!(out.bg_files, 0);
        let (bg, _) = record_baseline(&db, 1, &vroot, None).unwrap();
        assert_eq!(bg, 1);

        let _ = std::fs::remove_dir_all(&base);
    }
}
