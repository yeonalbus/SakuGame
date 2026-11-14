//! 搬运机：任务创建、调度、复制、进度、校验、rename、暂停/恢复/取消、崩溃恢复。
//! 依据：DSPlans/Specs/MVPv2技术规格.md §6

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{params, OptionalExtension};
use tauri::{AppHandle, Emitter};

use crate::cache::{self, join_rel};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::models::{TransferProgressEvent, TransferStatus, TransferTaskView};
use crate::scanner::extend_path;

/// 全局文件级通道上限
pub const MAX_SLOTS: usize = 32;
/// 进度事件节流
const PROGRESS_INTERVAL_MS: u64 = 200;
/// 单文件重试次数
const FILE_RETRY: usize = 2;
/// 复制块大小
const CHUNK: usize = 1 << 20;

// ---------------------------------------------------------------------------
// 引擎
// ---------------------------------------------------------------------------

pub struct TransferEngine {
    pub db: Arc<Db>,
    /// 运行中任务的控制句柄（key = task_id）
    pub running: Arc<Mutex<HashMap<i64, TaskControl>>>,
    /// 已占用的全局通道数
    pub slots_used: Arc<AtomicUsize>,
    /// 唤醒调度线程
    pub wake: Arc<(Mutex<()>, Condvar)>,
}

pub struct TaskControl {
    pub pause: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    pub concurrency: usize,
}

impl TransferEngine {
    pub fn new(db: Arc<Db>) -> Self {
        TransferEngine {
            db,
            running: Arc::new(Mutex::new(HashMap::new())),
            slots_used: Arc::new(AtomicUsize::new(0)),
            wake: Arc::new((Mutex::new(()), Condvar::new())),
        }
    }

    pub fn notify(&self) {
        let (lock, cv) = &*self.wake;
        let _g = lock.lock().unwrap();
        cv.notify_all();
    }
}

/// 任务行（内部）
pub struct TaskRow {
    pub id: i64,
    pub repository_id: i64,
    pub game_id: i64,
    pub version_id: i64,
    pub repo_no: String,
    pub source_rel: String,
    pub dest_rel: String,
    pub total_bytes: i64,
    pub total_files: i64,
    pub concurrency: i64,
    pub status: String,
}

// ---------------------------------------------------------------------------
// 启动入口
// ---------------------------------------------------------------------------

/// 创建任务：先算体积（无锁），再在锁内做容量判定 + 预留 + 入队。
pub fn start_transfer(
    engine: &TransferEngine,
    repository_root: &Path,
    version_id: i64,
) -> AppResult<i64> {
    let db = &engine.db;
    let meta = cache::load_version_meta(db, version_id)?;

    if meta.version_status != "ok" {
        return Err(AppError::InvalidInput("该版本已缺失，请重新扫描".into()));
    }
    if meta.game_type == "archive" {
        return Err(AppError::ArchiveUnsupported);
    }
    if cache::is_cached(db, version_id)? {
        return Err(AppError::InvalidInput("该版本已在缓存池中".into()));
    }
    if cache::has_active_task(db, version_id)? {
        return Err(AppError::TransferBusy);
    }

    // 递归求和（源可达性在此暴露）
    let src = join_rel(repository_root, &meta.version_rel);
    let (total_bytes, total_files) = cache::sum_tree(&src)?;
    let concurrency = pick_concurrency(total_files);

    // 容量判定 + 预留（与调度器共用一把锁）
    let _guard = engine.running.lock().unwrap();
    let cfg = cache::load_config(db)?;
    if cfg.root.is_none() {
        return Err(AppError::CacheNotConfigured);
    }
    let u = cache::usage_with(db, &cfg)?;
    let quota_room = u.quota_bytes - u.used_bytes;
    if total_bytes > quota_room {
        return Err(AppError::QuotaExceeded {
            required: total_bytes - quota_room,
            available: quota_room.max(0),
        });
    }
    if total_bytes > u.available_bytes {
        return Err(AppError::DiskExceeded {
            required: total_bytes - u.available_bytes,
            available: u.available_bytes.max(0),
        });
    }

    let dest_rel = cache::dest_rel(&meta);
    let task_id = db.with(|conn| {
        conn.execute(
            "INSERT INTO transfer_tasks(repository_id, game_id, version_id, repo_no, source_rel,
                dest_rel, total_bytes, total_files, written_bytes, written_files, concurrency,
                status, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0,0,?9,'queued',?10,?10)",
            params![
                meta.repository_id,
                meta.game_id,
                version_id,
                cache::repo_no(meta.repo_slot),
                meta.version_rel,
                dest_rel,
                total_bytes,
                total_files,
                concurrency,
                Db::now()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })?;
    engine.notify();
    Ok(task_id)
}

/// 文件数 → 并发档位（技术规格 §6.3）
pub fn pick_concurrency(total_files: i64) -> i64 {
    if total_files <= 200 {
        4
    } else if total_files <= 2000 {
        8
    } else if total_files <= 10000 {
        16
    } else {
        32
    }
}

// ---------------------------------------------------------------------------
// 调度
// ---------------------------------------------------------------------------

pub fn spawn_scheduler(engine: Arc<TransferEngine>, app: AppHandle) {
    std::thread::spawn(move || loop {
        let queued = match load_by_status(&engine.db, "queued") {
            Ok(v) => v,
            Err(_) => Vec::new(),
        };
        for task in queued {
            let need = task.concurrency.max(1) as usize;
            let used = engine.slots_used.load(Ordering::SeqCst);
            if used + need > MAX_SLOTS {
                continue;
            }
            if mark_running(&engine.db, task.id).is_err() {
                continue;
            }
            engine.slots_used.fetch_add(need, Ordering::SeqCst);
            spawn_task_thread(engine.clone(), app.clone(), task);
        }
        // 等通知或 500ms 轮询
        let (lock, cv) = &*engine.wake;
        let guard = lock.lock().unwrap();
        let _ = cv.wait_timeout(guard, Duration::from_millis(500));
    });
}

fn mark_running(db: &Db, task_id: i64) -> AppResult<()> {
    db.with(|conn| {
        conn.execute(
            "UPDATE transfer_tasks SET status='running', started_at=?1, updated_at=?1
             WHERE id=?2 AND status='queued'",
            params![Db::now(), task_id],
        )?;
        Ok(())
    })
}

fn load_by_status(db: &Db, status: &str) -> AppResult<Vec<TaskRow>> {
    db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, repository_id, game_id, version_id, repo_no, source_rel, dest_rel,
                    total_bytes, total_files, concurrency, status
             FROM transfer_tasks WHERE status=?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![status], |r| {
            Ok(TaskRow {
                id: r.get(0)?,
                repository_id: r.get(1)?,
                game_id: r.get(2)?,
                version_id: r.get(3)?,
                repo_no: r.get(4)?,
                source_rel: r.get(5)?,
                dest_rel: r.get(6)?,
                total_bytes: r.get(7)?,
                total_files: r.get(8)?,
                concurrency: r.get(9)?,
                status: r.get(10)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

pub fn load_task(db: &Db, task_id: i64) -> AppResult<Option<TaskRow>> {
    db.with(|conn| {
        let row = conn
            .query_row(
                "SELECT id, repository_id, game_id, version_id, repo_no, source_rel, dest_rel,
                        total_bytes, total_files, concurrency, status
                 FROM transfer_tasks WHERE id=?1",
                params![task_id],
                |r| {
                    Ok(TaskRow {
                        id: r.get(0)?,
                        repository_id: r.get(1)?,
                        game_id: r.get(2)?,
                        version_id: r.get(3)?,
                        repo_no: r.get(4)?,
                        source_rel: r.get(5)?,
                        dest_rel: r.get(6)?,
                        total_bytes: r.get(7)?,
                        total_files: r.get(8)?,
                        concurrency: r.get(9)?,
                        status: r.get(10)?,
                    })
                },
            )
            .ok();
        Ok(row)
    })
}

// ---------------------------------------------------------------------------
// 任务线程
// ---------------------------------------------------------------------------

fn spawn_task_thread(engine: Arc<TransferEngine>, app: AppHandle, task: TaskRow) {
    let pause = Arc::new(AtomicBool::new(false));
    let cancel = Arc::new(AtomicBool::new(false));
    let concurrency = task.concurrency.max(1) as usize;

    {
        let mut map = engine.running.lock().unwrap();
        map.insert(
            task.id,
            TaskControl {
                pause: pause.clone(),
                cancel: cancel.clone(),
                concurrency,
            },
        );
    }

    std::thread::spawn(move || {
        let result = run_task(&engine, Some(&app), &task, &pause, &cancel, concurrency);
        // 释放通道 + 注销
        engine.slots_used.fetch_sub(concurrency, Ordering::SeqCst);
        engine.running.lock().unwrap().remove(&task.id);
        if let Err(e) = &result {
            set_error(&engine.db, task.id, &e.to_string(), None);
            emit_state(Some(&app), task_view(&engine.db, task.id));
        }
        engine.notify();
    });
}

fn run_task(
    engine: &TransferEngine,
    app: Option<&AppHandle>,
    task: &TaskRow,
    pause: &Arc<AtomicBool>,
    cancel: &Arc<AtomicBool>,
    concurrency: usize,
) -> AppResult<()> {
    let db = &engine.db;
    let pool_root = cache::pool_root(db)?;
    let repo_root = resolve_repo_root(db, task.repository_id)?;
    let src = join_rel(&repo_root, &task.source_rel);
    let staging = pool_root.join(".staging").join(task.id.to_string());
    std::fs::create_dir_all(extend_path(&staging))?;

    // 枚举源文件（跳过已存在的同 size 文件由 worker 判断）
    let files = collect_files(&src)?;
    let total_files = files.len() as i64;
    let total_bytes: i64 = files.iter().map(|(_, s)| *s as i64).sum();

    // 修正 DB 的总量（源可能变化）
    if total_files != task.total_files || total_bytes != task.total_bytes {
        db.with(|conn| {
            conn.execute(
                "UPDATE transfer_tasks SET total_bytes=?1, total_files=?2, updated_at=?3 WHERE id=?4",
                params![total_bytes, total_files, Db::now(), task.id],
            )?;
            Ok(())
        })?;
    }

    let queue = Arc::new(Mutex::new(VecDeque::from(files)));
    let written = Arc::new(AtomicI64::new(0));
    let written_files = Arc::new(AtomicI64::new(0));
    let failed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..concurrency {
        let q = queue.clone();
        let w = written.clone();
        let wf = written_files.clone();
        let f = failed.clone();
        let d = done.clone();
        let pf = pause.clone();
        let cf = cancel.clone();
        let staging2 = staging.clone();
        let src2 = src.clone();
        handles.push(std::thread::spawn(move || {
            worker_loop(&q, &w, &wf, &f, &pf, &cf, &src2, &staging2);
            d.fetch_add(1, Ordering::SeqCst);
        }));
    }

    // 节流推进度 + 等 worker 结束
    let mut samples: VecDeque<(Instant, i64)> = VecDeque::new();
    loop {
        std::thread::sleep(Duration::from_millis(PROGRESS_INTERVAL_MS));
        let now = Instant::now();
        let cur = written.load(Ordering::SeqCst);
        samples.push_back((now, cur));
        while samples.len() > 2
            && now.duration_since(samples.front().unwrap().0) > Duration::from_secs(10)
        {
            samples.pop_front();
        }
        let speed = speed_bps(&samples, cur);
        let eta = if speed > 0 {
            let remain = total_bytes - cur;
            Some(remain.max(0) / speed.max(1))
        } else {
            None
        };
        emit_progress(
            app,
            TransferProgressEvent {
                task_id: task.id,
                written_bytes: cur,
                total_bytes,
                speed_bps: speed,
                eta_secs: eta,
                status: TransferStatus::Running,
            },
        );
        persist_progress(db, task.id, cur, written_files.load(Ordering::SeqCst));
        if done.load(Ordering::SeqCst) >= concurrency {
            break;
        }
        if cancel.load(Ordering::SeqCst) {
            // 等 worker 退出
            if done.load(Ordering::SeqCst) >= concurrency {
                break;
            }
        }
    }
    for h in handles {
        let _ = h.join();
    }

    // 取消：删 staging + 删任务行
    if cancel.load(Ordering::SeqCst) {
        let _ = std::fs::remove_dir_all(extend_path(&staging));
        db.with(|conn| {
            conn.execute("DELETE FROM transfer_tasks WHERE id=?1", params![task.id])?;
            Ok(())
        })?;
        emit_state(app, None);
        return Ok(());
    }

    // 暂停：保留 staging，状态 paused
    if pause.load(Ordering::SeqCst) {
        db.with(|conn| {
            conn.execute(
                "UPDATE transfer_tasks SET status='paused', updated_at=?1 WHERE id=?2",
                params![Db::now(), task.id],
            )?;
            Ok(())
        })?;
        emit_state(app, task_view(db, task.id));
        return Ok(());
    }

    // 取消的兜底（pause 与 cancel 同时设置时以 cancel 为准，上面已处理）

    // L1 校验
    let failed_list = failed.lock().unwrap().clone();
    let (staging_bytes, staging_files) = cache::sum_tree(&staging)?;
    let expected_files = total_files - failed_list.len() as i64;
    if staging_files != expected_files || staging_bytes != written.load(Ordering::SeqCst).max(0) {
        let msg = format!(
            "校验失败：期望 {expected_files} 个文件 / {} 字节，实际 {staging_files} 个 / {staging_bytes} 字节",
            written.load(Ordering::SeqCst)
        );
        set_error(db, task.id, &msg, Some(&failed_list));
        emit_state(app, task_view(db, task.id));
        return Ok(());
    }
    if !failed_list.is_empty() {
        set_error(
            db,
            task.id,
            &format!("{} 个文件搬运失败", failed_list.len()),
            Some(&failed_list),
        );
        emit_state(app, task_view(db, task.id));
        return Ok(());
    }

    // 落地：目标已存在则报错，不覆盖
    let dest = join_rel(&pool_root, &task.dest_rel);
    if dest.exists() {
        let msg = "目标路径已存在，请先清理缓存池后再试".to_string();
        set_error(db, task.id, &msg, None);
        let _ = std::fs::remove_dir_all(extend_path(&staging));
        emit_state(app, task_view(db, task.id));
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(extend_path(parent))?;
    }
    std::fs::rename(extend_path(&staging), extend_path(&dest))?;

    // 写缓存记录 + 任务完成（同一事务）
    let now = Db::now();
    db.with_mut(|conn| {
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO cache_entries(repository_id, game_id, version_id, repo_no, dest_rel,
                size_bytes, file_count, status, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'ok',?8)
             ON CONFLICT(version_id) DO UPDATE SET
                dest_rel=excluded.dest_rel, size_bytes=excluded.size_bytes,
                file_count=excluded.file_count, status='ok'",
            params![
                task.repository_id,
                task.game_id,
                task.version_id,
                task.repo_no,
                task.dest_rel,
                total_bytes,
                total_files,
                now
            ],
        )?;
        tx.execute(
            "UPDATE transfer_tasks SET status='done', written_bytes=?1, written_files=?2,
                finished_at=?3, updated_at=?3 WHERE id=?4",
            params![total_bytes, total_files, now, task.id],
        )?;
        tx.commit()?;
        Ok(())
    })?;
    emit_state(app, task_view(db, task.id));
    Ok(())
}

fn worker_loop(
    queue: &Arc<Mutex<VecDeque<(String, u64)>>>,
    written: &Arc<AtomicI64>,
    written_files: &Arc<AtomicI64>,
    failed: &Arc<Mutex<Vec<String>>>,
    pause: &Arc<AtomicBool>,
    cancel: &Arc<AtomicBool>,
    src: &Path,
    staging: &Path,
) {
    loop {
        if cancel.load(Ordering::SeqCst) || pause.load(Ordering::SeqCst) {
            return;
        }
        let job = {
            let mut q = queue.lock().unwrap();
            q.pop_front()
        };
        let Some((rel, size)) = job else { return };

        let src_path = join_rel(src, &rel);
        let dst_path = join_rel(staging, &rel);

        // 恢复续传：已存在且大小一致则跳过（也要计入进度）
        if let Ok(meta) = std::fs::metadata(extend_path(&dst_path)) {
            if meta.len() == size {
                written.fetch_add(size as i64, Ordering::SeqCst);
                written_files.fetch_add(1, Ordering::SeqCst);
                continue;
            }
        }

        let mut ok = false;
        for attempt in 0..=FILE_RETRY {
            match copy_file(&src_path, &dst_path, written, cancel) {
                Ok(n) if n == size => {
                    ok = true;
                    break;
                }
                Ok(_) => {
                    // 字节数不一致，视为失败（继续重试）
                }
                Err(_) => {}
            }
            if attempt < FILE_RETRY {
                std::thread::sleep(Duration::from_millis(200 * (attempt as u64 + 1)));
            }
        }
        if ok {
            written_files.fetch_add(1, Ordering::SeqCst);
        } else {
            failed.lock().unwrap().push(rel.clone());
        }
    }
}

fn copy_file(
    src: &Path,
    dst: &Path,
    written: &Arc<AtomicI64>,
    cancel: &Arc<AtomicBool>,
) -> std::io::Result<u64> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(extend_path(parent))?;
    }
    let mut r = File::open(extend_path(src))?;
    let mut w = File::create(extend_path(dst))?;
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n])?;
        total += n as u64;
        written.fetch_add(n as i64, Ordering::SeqCst);
    }
    w.flush()?;
    Ok(total)
}

fn speed_bps(samples: &VecDeque<(Instant, i64)>, current: i64) -> i64 {
    let Some((t0, b0)) = samples.front() else {
        return 0;
    };
    let Some((t1, _)) = samples.back() else {
        return 0;
    };
    let dt = t1.duration_since(*t0).as_secs_f64();
    if dt <= 0.0 {
        return 0;
    }
    ((current - b0) as f64 / dt) as i64
}

fn collect_files(root: &Path) -> AppResult<Vec<(String, u64)>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for entry in std::fs::read_dir(extend_path(&dir))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let meta = entry.metadata()?;
            let rel_path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if meta.is_dir() {
                stack.push((entry.path(), rel_path));
            } else {
                out.push((rel_path, meta.len()));
            }
        }
    }
    Ok(out)
}

fn emit_state(app: Option<&AppHandle>, view: Option<TransferTaskView>) {
    if let Some(a) = app {
        let _ = a.emit("transfer://state", view);
    }
}

fn emit_progress(app: Option<&AppHandle>, ev: TransferProgressEvent) {
    if let Some(a) = app {
        let _ = a.emit("transfer://progress", ev);
    }
}

fn persist_progress(db: &Db, task_id: i64, written: i64, files: i64) {
    let _ = db.with(|conn| {
        conn.execute(
            "UPDATE transfer_tasks SET written_bytes=?1, written_files=?2, updated_at=?3 WHERE id=?4",
            params![written, files, Db::now(), task_id],
        )?;
        Ok(())
    });
}

fn set_error(db: &Db, task_id: i64, msg: &str, failed: Option<&[String]>) {
    let failed_json = failed.map(|f| serde_json::to_string(f).unwrap_or_default());
    let _ = db.with(|conn| {
        conn.execute(
            "UPDATE transfer_tasks SET status='error', error_message=?1, failed_files=?2,
                updated_at=?3 WHERE id=?4",
            params![msg, failed_json, Db::now(), task_id],
        )?;
        Ok(())
    });
}

fn resolve_repo_root(db: &Db, repository_id: i64) -> AppResult<PathBuf> {
    let templates: Vec<String> = db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT path_template FROM repository_locators WHERE repository_id=?1
             ORDER BY CASE kind WHEN 'drive' THEN 0 WHEN 'ip' THEN 1 ELSE 2 END",
        )?;
        let it = stmt.query_map(params![repository_id], |r| r.get::<_, String>(0))?;
        Ok(it.filter_map(|r| r.ok()).collect())
    })?;
    for t in templates {
        let p = Path::new(&t);
        if p.exists() {
            return Ok(p.to_path_buf());
        }
    }
    Err(AppError::RepositoryUnreachable)
}

// ---------------------------------------------------------------------------
// 视图 / 状态操作 / 恢复
// ---------------------------------------------------------------------------

pub fn task_view(db: &Db, task_id: i64) -> Option<TransferTaskView> {
    db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT t.id, t.repository_id, t.game_id, t.version_id, t.repo_no, g.display_name,
                        v.version_label, t.total_bytes, t.written_bytes, t.total_files,
                        t.written_files, t.status, t.error_message, t.failed_files
                 FROM transfer_tasks t
                 JOIN games g ON g.id = t.game_id
                 JOIN game_versions v ON v.id = t.version_id
                 WHERE t.id=?1",
                params![task_id],
                |r| {
                    let status: String = r.get(11)?;
                    let failed_json: Option<String> = r.get(13)?;
                    Ok(TransferTaskView {
                        task_id: r.get(0)?,
                        repository_id: r.get(1)?,
                        game_id: r.get(2)?,
                        version_id: r.get(3)?,
                        repo_no: r.get(4)?,
                        game_name: r.get(5)?,
                        version_label: r.get(6)?,
                        total_bytes: r.get(7)?,
                        written_bytes: r.get(8)?,
                        total_files: r.get(9)?,
                        written_files: r.get(10)?,
                        status: TransferStatus::parse(&status).unwrap_or(TransferStatus::Queued),
                        speed_bps: 0,
                        eta_secs: None,
                        error: r.get(12)?,
                        failed_files: failed_json
                            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                            .unwrap_or_default(),
                    })
                },
            )
            .optional()?)
    })
    .ok()
    .flatten()
}

pub fn list_tasks(db: &Db) -> AppResult<Vec<TransferTaskView>> {
    db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT t.id, t.repository_id, t.game_id, t.version_id, t.repo_no, g.display_name,
                    v.version_label, t.total_bytes, t.written_bytes, t.total_files,
                    t.written_files, t.status, t.error_message, t.failed_files
             FROM transfer_tasks t
             JOIN games g ON g.id = t.game_id
             JOIN game_versions v ON v.id = t.version_id
             ORDER BY t.created_at ASC, t.id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let status: String = r.get(11)?;
            let failed_json: Option<String> = r.get(13)?;
            Ok(TransferTaskView {
                task_id: r.get(0)?,
                repository_id: r.get(1)?,
                game_id: r.get(2)?,
                version_id: r.get(3)?,
                repo_no: r.get(4)?,
                game_name: r.get(5)?,
                version_label: r.get(6)?,
                total_bytes: r.get(7)?,
                written_bytes: r.get(8)?,
                total_files: r.get(9)?,
                written_files: r.get(10)?,
                status: TransferStatus::parse(&status).unwrap_or(TransferStatus::Queued),
                speed_bps: 0,
                eta_secs: None,
                error: r.get(12)?,
                failed_files: failed_json
                    .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                    .unwrap_or_default(),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

pub fn pause_task(engine: &TransferEngine, task_id: i64) -> AppResult<()> {
    let map = engine.running.lock().unwrap();
    match map.get(&task_id) {
        Some(c) => {
            c.pause.store(true, Ordering::SeqCst);
            Ok(())
        }
        None => Err(AppError::TransferNotResumable(
            "该任务不在运行中，无法暂停".into(),
        )),
    }
}

pub fn resume_task(engine: &TransferEngine, task_id: i64) -> AppResult<()> {
    engine.db.with(|conn| {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM transfer_tasks WHERE id=?1 AND status='paused'",
            params![task_id],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Err(AppError::TransferNotResumable(
                "只有暂停中的任务可以恢复".into(),
            ));
        }
        conn.execute(
            "UPDATE transfer_tasks SET status='queued', updated_at=?1 WHERE id=?2",
            params![Db::now(), task_id],
        )?;
        Ok(())
    })?;
    engine.notify();
    Ok(())
}

pub fn cancel_task(engine: &TransferEngine, task_id: i64) -> AppResult<()> {
    let map = engine.running.lock().unwrap();
    if let Some(c) = map.get(&task_id) {
        c.cancel.store(true, Ordering::SeqCst);
        return Ok(());
    }
    // 未在运行（queued / paused）：直接清理
    let db = &engine.db;
    let task = load_task(db, task_id)?.ok_or(AppError::TransferNotFound(task_id))?;
    if let Ok(pool) = cache::pool_root(db) {
        let staging = pool.join(".staging").join(task.id.to_string());
        if staging.exists() {
            let _ = std::fs::remove_dir_all(extend_path(&staging));
        }
    }
    db.with(|conn| {
        conn.execute("DELETE FROM transfer_tasks WHERE id=?1", params![task_id])?;
        Ok(())
    })?;
    engine.notify();
    Ok(())
}

/// 重试 = 重建任务（已存在文件由 worker 跳过）
pub fn retry_task(engine: &TransferEngine, task_id: i64) -> AppResult<i64> {
    let db = &engine.db;
    let task = load_task(db, task_id)?.ok_or(AppError::TransferNotFound(task_id))?;
    db.with(|conn| {
        conn.execute(
            "DELETE FROM transfer_tasks WHERE id=?1 AND status='error'",
            params![task_id],
        )?;
        Ok(())
    })?;
    let root = resolve_repo_root(db, task.repository_id)?;
    start_transfer(engine, &root, task.version_id)
}

/// 崩溃恢复：running → paused（启动时调用）
pub fn recover_on_startup(db: &Db) -> AppResult<()> {
    db.with(|conn| {
        conn.execute(
            "UPDATE transfer_tasks SET status='paused', updated_at=?1 WHERE status='running'",
            params![Db::now()],
        )?;
        Ok(())
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_tiers() {
        assert_eq!(pick_concurrency(0), 4);
        assert_eq!(pick_concurrency(200), 4);
        assert_eq!(pick_concurrency(201), 8);
        assert_eq!(pick_concurrency(2000), 8);
        assert_eq!(pick_concurrency(2001), 16);
        assert_eq!(pick_concurrency(10000), 16);
        assert_eq!(pick_concurrency(10001), 32);
    }

    fn temp_base(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sakugame_xfer_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 准备一个最小可搬运环境：临时仓库 + 池 + DB 记录 + 一条 running 任务
    fn setup(tag: &str) -> (PathBuf, PathBuf, TransferEngine, i64) {
        let base = temp_base(tag);
        let repo = base.join("repo");
        let pool = base.join("pool");
        std::fs::create_dir_all(repo.join("RJ001").join("Ver1").join("data")).unwrap();
        std::fs::create_dir_all(&pool).unwrap();
        std::fs::write(repo.join("RJ001/Ver1/game.exe"), b"hello world").unwrap();
        std::fs::write(repo.join("RJ001/Ver1/data/asset.bin"), vec![7u8; 1024]).unwrap();

        let db = Db::open(&base.join("test.db")).unwrap();
        db.with(|conn| {
            conn.execute_batch(&format!(
                "INSERT INTO repositories(id,name,relative_root,created_at) VALUES(1,'r','Game','x');
                 INSERT INTO repository_locators(repository_id,kind,path_template) VALUES(1,'drive','{}');
                 INSERT INTO games(id,repository_id,relative_path,layout,display_name,size_bytes,status,created_at,updated_at)
                   VALUES(1,1,'RJ001','standard','RJ001',0,'ok','x','x');
                 INSERT INTO game_versions(id,game_id,relative_path,version_label,display_name,game_type,size_bytes,source,status,created_at,updated_at)
                   VALUES(1,1,'RJ001/Ver1','Ver1','Ver1','folder',0,'scan','ok','x','x');",
                repo.to_string_lossy().replace('\'', "''")
            ))?;
            Ok(())
        })
        .unwrap();
        db.set_setting("cache_pool_root", &pool.to_string_lossy())
            .unwrap();
        db.set_setting("cache_pool_quota_bytes", &(1024 * 1024 * 1024i64).to_string())
            .unwrap();

        let engine = TransferEngine::new(Arc::new(db));
        let task_id = engine
            .db
            .with(|conn| {
                conn.execute(
                    "INSERT INTO transfer_tasks(repository_id,game_id,version_id,repo_no,source_rel,
                        dest_rel,total_bytes,total_files,written_bytes,written_files,concurrency,
                        status,created_at,updated_at)
                     VALUES(1,1,1,'R01','RJ001/Ver1','R01/RJ001/Ver1',0,0,0,0,4,'running','x','x')",
                    [],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .unwrap();
        (base, pool, engine, task_id)
    }

    fn task_status(engine: &TransferEngine, task_id: i64) -> String {
        engine
            .db
            .with(|conn| {
                Ok(conn.query_row(
                    "SELECT status FROM transfer_tasks WHERE id=?1",
                    params![task_id],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }

    /// 端到端：staging → L1 校验 → rename → 写缓存记录 → 任务 done
    #[test]
    fn end_to_end_transfer_writes_cache_entry() {
        let (base, pool, engine, task_id) = setup("e2e");
        let task = load_task(&engine.db, task_id).unwrap().unwrap();
        let pause = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        run_task(&engine, None, &task, &pause, &cancel, 4).unwrap();

        let (st, err): (String, Option<String>) = engine
            .db
            .with(|conn| {
                Ok(conn.query_row(
                    "SELECT status, error_message FROM transfer_tasks WHERE id=?1",
                    params![task_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(st, "done", "status={st} err={err:?}");

        let dest = pool.join("R01").join("RJ001").join("Ver1");
        assert_eq!(
            std::fs::read(dest.join("game.exe")).unwrap(),
            b"hello world"
        );
        assert_eq!(
            std::fs::metadata(dest.join("data/asset.bin")).unwrap().len(),
            1024
        );
        assert!(
            !pool.join(".staging").join(task_id.to_string()).exists(),
            "staging 应已 rename 到最终路径"
        );

        let (status, size): (String, i64) = engine
            .db
            .with(|conn| {
                Ok(conn.query_row(
                    "SELECT status, size_bytes FROM cache_entries WHERE version_id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(status, "ok");
        assert_eq!(size, 11 + 1024);
        assert_eq!(task_status(&engine, task_id), "done");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 目标路径已存在：不覆盖、不删除，任务转 error
    #[test]
    fn existing_destination_is_rejected() {
        let (base, pool, engine, task_id) = setup("dup");
        std::fs::create_dir_all(pool.join("R01").join("RJ001").join("Ver1")).unwrap();
        let task = load_task(&engine.db, task_id).unwrap().unwrap();
        let pause = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        run_task(&engine, None, &task, &pause, &cancel, 4).unwrap();

        assert_eq!(task_status(&engine, task_id), "error");
        assert!(pool.join("R01/RJ001/Ver1").exists(), "已有目标目录不得被删");
        let _ = std::fs::remove_dir_all(&base);
    }
}
