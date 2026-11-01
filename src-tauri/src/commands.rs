//! Tauri 命令层：仓库配置、档案读写、扫描控制。
//! 依据：DSPlans/MVPv1扫描器规格.md §7（IPC 协议）、§8（幂等重扫）、§10（错误处理）

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OptionalExtension};
use tauri::{AppHandle, Emitter, State};

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::locator;
use crate::models::{
    GameDetail, GameEntry, GameRow, GameType, LocatorKind, NewRepository, RepositoryLocator,
    RepositoryRow, ScanPhase, ScanProgress, ScanSummary,
};
use crate::scanner::{self, ScanConfig, ScanControl};
use crate::state::{ActiveScan, AppState, ScanSlot};

// ---------------------------------------------------------------------------
// 仓库
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_repositories(state: State<'_, AppState>) -> AppResult<Vec<RepositoryRow>> {
    state.db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, share_name, relative_root, active_locator, last_scan_at, is_offline
             FROM repositories ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RepositoryRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    share_name: r.get(2)?,
                    relative_root: r.get(3)?,
                    active_locator: r.get(4)?,
                    last_scan_at: r.get(5)?,
                    is_offline: r.get::<_, i64>(6)? != 0,
                    locators: Vec::new(),
                    resolved_path: None,
                })
            })?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();

        let mut out = Vec::with_capacity(rows.len());
        for mut row in rows {
            row.locators = load_locators(conn, row.id)?;
            // 不在此处做网络探测（会拖慢列表）；active_locator 对应的模板作为乐观路径
            row.resolved_path = row
                .active_locator
                .as_ref()
                .and_then(|kind| {
                    row.locators
                        .iter()
                        .find(|l| &l.kind == kind)
                        .map(|l| l.path_template.clone())
                });
            out.push(row);
        }
        Ok(out)
    })
}

#[tauri::command]
pub fn check_repository(state: State<'_, AppState>, repository_id: i64) -> AppResult<RepositoryRow> {
    state.db.with(|conn| {
        let mut repo = load_repository(conn, repository_id)?;
        repo.locators = load_locators(conn, repository_id)?;
        // D16：按可达性探测（盘符 → IP → 主机名）
        if let Some((kind, path)) = locator::resolve(&repo.locators) {
            conn.execute(
                "UPDATE repositories SET active_locator=?1, is_offline=0 WHERE id=?2",
                params![kind.as_str(), repository_id],
            )?;
            conn.execute(
                "UPDATE repository_locators SET last_ok_at=?1 WHERE repository_id=?2 AND kind=?3",
                params![Db::now(), repository_id, kind.as_str()],
            )?;
            repo.active_locator = Some(kind.as_str().to_string());
            repo.is_offline = false;
            repo.resolved_path = Some(path);
        } else {
            conn.execute(
                "UPDATE repositories SET is_offline=1 WHERE id=?1",
                params![repository_id],
            )?;
            repo.is_offline = true;
            repo.resolved_path = None;
        }
        Ok(repo)
    })
}

#[tauri::command]
pub fn add_repository(state: State<'_, AppState>, payload: NewRepository) -> AppResult<i64> {
    validate_new_repository(&payload)?;
    let name = payload
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| {
            payload
                .locators
                .iter()
                .find(|l| l.kind == "drive")
                .map(|l| l.path_template.clone())
        })
        .unwrap_or_else(|| payload.relative_root.clone());

    state.db.with_mut(|conn| {
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO repositories(name, share_name, relative_root, created_at, is_offline)
             VALUES(?1, ?2, ?3, ?4, 0)",
            params![name, payload.share_name, payload.relative_root, Db::now()],
        )?;
        let id = tx.last_insert_rowid();
        for loc in &payload.locators {
            tx.execute(
                "INSERT OR REPLACE INTO repository_locators(repository_id, kind, path_template)
                 VALUES(?1, ?2, ?3)",
                params![id, loc.kind, loc.path_template],
            )?;
        }
        tx.commit()?;
        Ok(id)
    })
}

#[tauri::command]
pub fn update_repository(
    state: State<'_, AppState>,
    repository_id: i64,
    payload: NewRepository,
) -> AppResult<()> {
    if is_scanning(&state) {
        return Err(AppError::ScanInProgress);
    }
    validate_new_repository(&payload)?;
    state.db.with_mut(|conn| {
        let tx = conn.transaction()?;
        let name = payload
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| {
                payload
                    .locators
                    .iter()
                    .find(|l| l.kind == "drive")
                    .map(|l| l.path_template.clone())
            })
            .unwrap_or_else(|| payload.relative_root.clone());
        let affected = tx.execute(
            "UPDATE repositories SET name=?1, share_name=?2, relative_root=?3 WHERE id=?4",
            params![name, payload.share_name, payload.relative_root, repository_id],
        )?;
        if affected == 0 {
            return Err(AppError::RepositoryNotFound(repository_id));
        }
        // 档案只记相对路径（O3），改路径无需触碰档案
        tx.execute(
            "DELETE FROM repository_locators WHERE repository_id=?1",
            params![repository_id],
        )?;
        for loc in &payload.locators {
            tx.execute(
                "INSERT OR REPLACE INTO repository_locators(repository_id, kind, path_template)
                 VALUES(?1, ?2, ?3)",
                params![repository_id, loc.kind, loc.path_template],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
}

#[tauri::command]
pub fn delete_repository(state: State<'_, AppState>, repository_id: i64) -> AppResult<()> {
    if is_scanning(&state) {
        return Err(AppError::ScanInProgress);
    }
    state.db.with(|conn| {
        // 档案随仓库级联删除（O5）
        let affected = conn.execute(
            "DELETE FROM repositories WHERE id=?1",
            params![repository_id],
        )?;
        if affected == 0 {
            return Err(AppError::RepositoryNotFound(repository_id));
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// 档案
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_games(
    state: State<'_, AppState>,
    repository_id: Option<i64>,
) -> AppResult<Vec<GameRow>> {
    state.db.with(|conn| {
        let sql = "SELECT id, repository_id, relative_path, group_key, game_type, display_name,
                          game_code, cover_path, size_bytes, status, updated_at
                   FROM games
                   WHERE (?1 IS NULL OR repository_id = ?1)
                   ORDER BY display_name COLLATE NOCASE ASC";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(params![repository_id], map_game_row)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    })
}

#[tauri::command]
pub fn get_game(state: State<'_, AppState>, game_id: i64) -> AppResult<GameDetail> {
    state.db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, repository_id, relative_path, group_key, game_type, display_name,
                    game_code, cover_path, size_bytes, status, updated_at
             FROM games WHERE id=?1",
        )?;
        let row = stmt
            .query_row(params![game_id], map_game_row)
            .optional()?
            .ok_or(AppError::GameNotFound(game_id))?;

        let abs = absolute_path(conn, &row)?;
        Ok(GameDetail {
            row,
            absolute_path: abs,
        })
    })
}

#[tauri::command]
pub fn update_game(
    state: State<'_, AppState>,
    game_id: i64,
    display_name: String,
    game_code: Option<String>,
    cover_path: Option<String>,
) -> AppResult<()> {
    state.db.with(|conn| {
        let affected = conn.execute(
            "UPDATE games SET display_name=?1, game_code=?2, cover_path=?3, updated_at=?4 WHERE id=?5",
            params![
                display_name,
                game_code.filter(|s| !s.trim().is_empty()),
                cover_path.filter(|s| !s.trim().is_empty()),
                Db::now(),
                game_id
            ],
        )?;
        if affected == 0 {
            return Err(AppError::GameNotFound(game_id));
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// 扫描控制（D9 / D10）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn start_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    repository_id: i64,
) -> AppResult<u64> {
    let db = state.db.clone();
    let slot = state.scan.clone();

    let (job_id, control, last, start_index, root) = {
        let mut guard = slot.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        if guard.active.is_some() {
            return Err(AppError::ScanBusy);
        }

        // 解析可达路径（D16）
        let mut repo = {
            let conn = db
                .conn
                .lock()
                .map_err(|e| AppError::Internal(e.to_string()))?;
            let mut repo = load_repository(&conn, repository_id)?;
            repo.locators = load_locators(&conn, repository_id)?;
            repo
        };

        let resolved = locator::resolve(&repo.locators);
        let Some((kind, path)) = resolved else {
            db.with(|conn| {
                conn.execute(
                    "UPDATE repositories SET is_offline=1 WHERE id=?1",
                    params![repository_id],
                )?;
                Ok(())
            })?;
            return Err(AppError::RepositoryUnreachable);
        };
        db.with(|conn| {
            conn.execute(
                "UPDATE repositories SET active_locator=?1, is_offline=0 WHERE id=?2",
                params![kind.as_str(), repository_id],
            )?;
            conn.execute(
                "UPDATE repository_locators SET last_ok_at=?1 WHERE repository_id=?2 AND kind=?3",
                params![Db::now(), repository_id, kind.as_str()],
            )?;
            Ok(())
        })?;
        repo.active_locator = Some(kind.as_str().to_string());

        // 断点（D10）
        let start_index = load_checkpoint(&db, repository_id)?;

        let job_id = guard.alloc_job_id();
        let control = ScanControl::new();
        let last: Arc<Mutex<Option<ScanProgress>>> = Arc::new(Mutex::new(None));
        guard.active = Some(ActiveScan {
            job_id,
            repository_id,
            control: control.clone(),
            last: last.clone(),
        });
        (job_id, control, last, start_index, path)
    };

    // job 记录
    db.with(|conn| {
        conn.execute(
            "INSERT INTO scan_jobs(repository_id, started_at, status) VALUES(?1, ?2, 'running')",
            params![repository_id, Db::now()],
        )?;
        Ok(())
    })?;

    let app2 = app.clone();
    let db2 = db.clone();
    let slot2 = slot.clone();
    std::thread::spawn(move || {
        run_scan(
            app2, db2, slot2, repository_id, job_id, control, last, start_index, root,
        );
    });

    Ok(job_id)
}

#[tauri::command]
pub fn pause_scan(state: State<'_, AppState>) -> AppResult<()> {
    let guard = state.scan.lock().map_err(|e| AppError::Internal(e.to_string()))?;
    let active = guard.active.as_ref().ok_or(AppError::NoActiveScan)?;
    active.control.set_paused(true);

    // 落断点：已完成数（D10）
    let done = active
        .last
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.done))
        .unwrap_or(0);
    state.db.with(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO scan_checkpoints(repository_id, next_index, total_entries, committed_games, updated_at)
             VALUES(?1, ?2, ?3, 0, ?4)",
            params![active.repository_id, done as i64, done as i64, Db::now()],
        )?;
        conn.execute(
            "UPDATE scan_jobs SET status='paused' WHERE repository_id=?1 AND status='running'",
            params![active.repository_id],
        )?;
        Ok(())
    })?;
    Ok(())
}

#[tauri::command]
pub fn resume_scan(state: State<'_, AppState>) -> AppResult<()> {
    let guard = state.scan.lock().map_err(|e| AppError::Internal(e.to_string()))?;
    let active = guard.active.as_ref().ok_or(AppError::NoActiveScan)?;
    active.control.set_paused(false);
    state.db.with(|conn| {
        conn.execute(
            "UPDATE scan_jobs SET status='running' WHERE repository_id=?1 AND status='paused'",
            params![active.repository_id],
        )?;
        Ok(())
    })?;
    Ok(())
}

#[tauri::command]
pub fn cancel_scan(state: State<'_, AppState>) -> AppResult<()> {
    let guard = state.scan.lock().map_err(|e| AppError::Internal(e.to_string()))?;
    let active = guard.active.as_ref().ok_or(AppError::NoActiveScan)?;
    active.control.cancel();
    // 清除断点（D10：取消 = 从头再来）
    state.db.with(|conn| {
        conn.execute(
            "DELETE FROM scan_checkpoints WHERE repository_id=?1",
            params![active.repository_id],
        )?;
        Ok(())
    })?;
    Ok(())
}

#[tauri::command]
pub fn scan_status(state: State<'_, AppState>) -> AppResult<Option<ScanProgress>> {
    let guard = state.scan.lock().map_err(|e| AppError::Internal(e.to_string()))?;
    let Some(active) = guard.active.as_ref() else {
        return Ok(None);
    };
    let mut p = active
        .last
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or(ScanProgress {
            job_id: active.job_id,
            repository_id: active.repository_id,
            done: 0,
            total: 0,
            found_games: 0,
            current: None,
            phase: ScanPhase::Idle,
            warnings: 0,
        });
    if active.control.signal() == scanner::ControlSignal::Paused {
        p.phase = ScanPhase::Paused;
    }
    Ok(Some(p))
}

// ---------------------------------------------------------------------------
// 扫描执行
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_scan(
    app: AppHandle,
    db: Arc<Db>,
    slot: Arc<Mutex<ScanSlot>>,
    repository_id: i64,
    job_id: u64,
    control: ScanControl,
    last: Arc<Mutex<Option<ScanProgress>>>,
    start_index: u32,
    root: String,
) {
    let config = ScanConfig::default();
    let mut last_emit = Instant::now();
    let mut last_saved = 0u32;

    let result = scanner::scan_repository(
        Path::new(&root),
        &config,
        &control,
        start_index,
        |ev| {
            let progress = ScanProgress {
                job_id,
                repository_id,
                done: ev.done,
                total: ev.total,
                found_games: ev.found,
                current: ev.current.map(|s| s.to_string()),
                phase: ScanPhase::Scanning,
                warnings: 0,
            };
            if let Ok(mut g) = last.lock() {
                *g = Some(progress.clone());
            }
            // 节流 ≥100ms（§7.2）
            if last_emit.elapsed() >= Duration::from_millis(100) {
                let _ = app.emit("scan://progress", &progress);
                last_emit = Instant::now();
            }
            // 每 50 条落一次断点（避免频繁写库）
            if ev.done - last_saved >= 50 {
                let _ = db.with(|conn| {
                    conn.execute(
                        "INSERT OR REPLACE INTO scan_checkpoints(repository_id, next_index, total_entries, committed_games, updated_at)
                         VALUES(?1, ?2, ?3, 0, ?4)",
                        params![repository_id, ev.done as i64, ev.total as i64, Db::now()],
                    )?;
                    Ok(())
                });
                last_saved = ev.done;
            }
        },
    );

    let cancelled = result.cancelled || control.signal() == scanner::ControlSignal::Cancelled;

    if cancelled {
        // 取消：清除断点与未提交草稿（D10）
        let _ = db.with(|conn| {
            conn.execute(
                "DELETE FROM scan_checkpoints WHERE repository_id=?1",
                params![repository_id],
            )?;
            conn.execute(
                "UPDATE scan_jobs SET status='cancelled', finished_at=?1 WHERE repository_id=?2 AND status IN ('running','paused')",
                params![Db::now(), repository_id],
            )?;
            Ok(())
        });
        let _ = app.emit(
            "scan://failed",
            serde_json::json!({ "job_id": job_id, "message": "扫描已取消" }),
        );
    } else {
        match apply_results(&db, repository_id, &result.games, start_index == 0) {
            Ok((added, updated, missing)) => {
                let _ = db.with(|conn| {
                    conn.execute(
                        "DELETE FROM scan_checkpoints WHERE repository_id=?1",
                        params![repository_id],
                    )?;
                    conn.execute(
                        "UPDATE repositories SET last_scan_at=?1, is_offline=0 WHERE id=?2",
                        params![Db::now(), repository_id],
                    )?;
                    conn.execute(
                        "UPDATE scan_jobs SET status='done', finished_at=?1, found_count=?2, warning_count=?3
                         WHERE repository_id=?4 AND status IN ('running','paused')",
                        params![
                            Db::now(),
                            (added + updated) as i64,
                            result.warnings as i64,
                            repository_id
                        ],
                    )?;
                    Ok(())
                });
                let summary = ScanSummary {
                    job_id,
                    repository_id,
                    found_count: result.games.len() as u32,
                    added,
                    updated,
                    missing,
                    warning_count: result.warnings,
                };
                let _ = app.emit("scan://finished", &summary);
            }
            Err(e) => {
                let _ = db.with(|conn| {
                    conn.execute(
                        "UPDATE scan_jobs SET status='failed', finished_at=?1, error_message=?2
                         WHERE repository_id=?3 AND status IN ('running','paused')",
                        params![Db::now(), e.to_string(), repository_id],
                    )?;
                    Ok(())
                });
                let _ = app.emit(
                    "scan://failed",
                    serde_json::json!({ "job_id": job_id, "message": e.to_string() }),
                );
            }
        }
    }

    // 释放活跃槽
    if let Ok(mut guard) = slot.lock() {
        if guard
            .active
            .as_ref()
            .map(|a| a.job_id == job_id)
            .unwrap_or(false)
        {
            guard.active = None;
        }
    }
}

/// 幂等重扫（§8）：新增 / 更新（保留用户字段）/ 标记缺失
fn apply_results(
    db: &Db,
    repository_id: i64,
    games: &[GameEntry],
    mark_missing: bool,
) -> AppResult<(u32, u32, u32)> {
    db.with_mut(|conn| {
        let tx = conn.transaction()?;
        let now = Db::now();
        let mut added = 0u32;
        let mut updated = 0u32;
        let mut seen: HashSet<(String, String)> = HashSet::new();

        for g in games {
            seen.insert((g.relative_path.clone(), g.group_key.clone()));

            let existing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM games WHERE repository_id=?1 AND relative_path=?2 AND group_key=?3",
                    params![repository_id, g.relative_path, g.group_key],
                    |r| r.get(0),
                )
                .optional()?;

            match existing {
                Some(id) => {
                    // 只刷新大小与类型；绝不触碰 display_name / game_code / cover_path（§8）
                    tx.execute(
                        "UPDATE games SET size_bytes=?1, game_type=?2, status='ok', updated_at=?3 WHERE id=?4",
                        params![g.size_bytes as i64, g.game_type.as_str(), now, id],
                    )?;
                    write_game_files(&tx, id, g)?;
                    updated += 1;
                }
                None => {
                    tx.execute(
                        "INSERT INTO games(repository_id, relative_path, group_key, game_type,
                                           display_name, size_bytes, status, created_at, updated_at)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'ok', ?7, ?7)",
                        params![
                            repository_id,
                            g.relative_path,
                            g.group_key,
                            g.game_type.as_str(),
                            g.display_name,
                            g.size_bytes as i64,
                            now
                        ],
                    )?;
                    let id = tx.last_insert_rowid();
                    write_game_files(&tx, id, g)?;
                    added += 1;
                }
            }
        }

        let mut missing = 0u32;
        if mark_missing {
            let mut stmt = tx.prepare(
                "SELECT id, relative_path, group_key FROM games
                 WHERE repository_id=?1 AND status='ok'",
            )?;
            let rows: Vec<(i64, String, String)> = stmt
                .query_map(params![repository_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);
            for (id, rp, gk) in rows {
                if !seen.contains(&(rp, gk)) {
                    tx.execute(
                        "UPDATE games SET status='missing', updated_at=?1 WHERE id=?2",
                        params![now, id],
                    )?;
                    missing += 1;
                }
            }
        }

        tx.commit()?;
        Ok((added, updated, missing))
    })
}

fn write_game_files(tx: &rusqlite::Transaction<'_>, game_id: i64, g: &GameEntry) -> AppResult<()> {
    if g.game_type != GameType::Archive {
        return Ok(());
    }
    tx.execute("DELETE FROM game_files WHERE game_id=?1", params![game_id])?;
    for grp in &g.groups {
        for f in &grp.files {
            tx.execute(
                "INSERT OR REPLACE INTO game_files(game_id, group_key, file_name, part_index, size_bytes)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    game_id,
                    grp.base_name,
                    f.file_name,
                    f.part_index,
                    f.size_bytes as i64
                ],
            )?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn map_game_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<GameRow> {
    Ok(GameRow {
        id: r.get(0)?,
        repository_id: r.get(1)?,
        relative_path: r.get(2)?,
        group_key: r.get(3)?,
        game_type: r.get(4)?,
        display_name: r.get(5)?,
        game_code: r.get(6)?,
        cover_path: r.get(7)?,
        size_bytes: r.get(8)?,
        status: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

fn load_repository(conn: &Connection, id: i64) -> AppResult<RepositoryRow> {
    let mut stmt = conn.prepare(
        "SELECT id, name, share_name, relative_root, active_locator, last_scan_at, is_offline
         FROM repositories WHERE id=?1",
    )?;
    let row = stmt
        .query_row(params![id], |r| {
            Ok(RepositoryRow {
                id: r.get(0)?,
                name: r.get(1)?,
                share_name: r.get(2)?,
                relative_root: r.get(3)?,
                active_locator: r.get(4)?,
                last_scan_at: r.get(5)?,
                is_offline: r.get::<_, i64>(6)? != 0,
                locators: Vec::new(),
                resolved_path: None,
            })
        })
        .optional()?
        .ok_or(AppError::RepositoryNotFound(id))?;
    Ok(row)
}

fn load_locators(conn: &Connection, repository_id: i64) -> AppResult<Vec<RepositoryLocator>> {
    let mut stmt = conn.prepare(
        "SELECT kind, path_template, last_ok_at FROM repository_locators
         WHERE repository_id=?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![repository_id], |r| {
            Ok(RepositoryLocator {
                kind: r.get(0)?,
                path_template: r.get(1)?,
                last_ok_at: r.get(2)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

fn load_checkpoint(db: &Db, repository_id: i64) -> AppResult<u32> {
    db.with(|conn| {
        let v: Option<i64> = conn
            .query_row(
                "SELECT next_index FROM scan_checkpoints WHERE repository_id=?1",
                params![repository_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.unwrap_or(0).max(0) as u32)
    })
}

/// 拼接绝对路径（详情页"本机详情"）
fn absolute_path(conn: &Connection, row: &GameRow) -> AppResult<Option<String>> {
    let repo = load_repository(conn, row.repository_id)?;
    let locators = load_locators(conn, row.repository_id)?;
    let base = repo
        .active_locator
        .as_ref()
        .and_then(|kind| locators.iter().find(|l| &l.kind == kind))
        .or_else(|| locators.first())
        .map(|l| l.path_template.clone());

    Ok(base.map(|b| {
        if row.relative_path.contains('/') || row.relative_path.contains('\\') {
            format!(
                "{}\\{}",
                b.trim_end_matches(['\\', '/']),
                row.relative_path.replace('/', "\\")
            )
        } else {
            format!("{}\\{}", b.trim_end_matches(['\\', '/']), row.relative_path)
        }
    }))
}

fn validate_new_repository(payload: &NewRepository) -> AppResult<()> {
    if payload.locators.is_empty() {
        return Err(AppError::InvalidInput("至少需要一个定位项".into()));
    }
    // D16：盘符必填
    if !payload.locators.iter().any(|l| l.kind == "drive") {
        return Err(AppError::InvalidInput("盘符定位项为必填".into()));
    }
    for l in &payload.locators {
        if LocatorKind::parse(&l.kind).is_none() {
            return Err(AppError::InvalidInput(format!("未知定位项类型: {}", l.kind)));
        }
        if l.path_template.trim().is_empty() {
            return Err(AppError::InvalidInput("定位项路径不能为空".into()));
        }
    }
    if payload.relative_root.trim().is_empty() {
        return Err(AppError::InvalidInput("相对根路径不能为空".into()));
    }
    Ok(())
}

fn is_scanning(state: &State<'_, AppState>) -> bool {
    state
        .scan
        .lock()
        .map(|g| g.active.is_some())
        .unwrap_or(false)
}
