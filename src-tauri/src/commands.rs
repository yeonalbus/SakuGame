//! Tauri 命令层：仓库配置、档案 / 版本读写、扫描控制、黑名单。
//! 依据：DSPlans/MVPv1.1技术规格.md §7（行为细则）、§8（IPC 协议）

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OptionalExtension};
use tauri::{AppHandle, Emitter, State};

use crate::cache;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::locator;
use crate::models::{
    BlocklistRow, CacheConfig, CacheUsage, ExecutableRow, ExeCandidate, GameDetail, GameRow,
    GameVersionRow, LocatorKind, NewRepository, RepositoryDeletePreview, RepositoryLocator,
    RepositoryRow, ScanPhase, ScanProgress, ScanScope, ScanSummary, TransferPreflight,
    TransferTaskView, UnblockOutcome, VersionState,
};
use crate::scanner::{self, ScanConfig, ScanControl};
use crate::state::{ActiveScan, AppState, ScanSlot};
use crate::store;
use crate::transfer;

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
            row.resolved_path = row.active_locator.as_ref().and_then(|kind| {
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
    let name = repository_display_name(&payload);
    // 分配缓存池槽位号（最小可用号，删库后空号复用）
    let slot = cache::alloc_slot(&state.db)?;

    state.db.with_mut(|conn| {
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO repositories(name, share_name, relative_root, created_at, is_offline, cache_slot)
             VALUES(?1, ?2, ?3, ?4, 0, ?5)",
            params![name, payload.share_name, payload.relative_root, Db::now(), slot],
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
        let affected = tx.execute(
            "UPDATE repositories SET name=?1, share_name=?2, relative_root=?3 WHERE id=?4",
            params![
                repository_display_name(&payload),
                payload.share_name,
                payload.relative_root,
                repository_id
            ],
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
    let db = &state.db;
    // v2：有在途搬运任务时拒绝（否则任务会往正在被删的路径里写）
    let active: i64 = db.with(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM transfer_tasks WHERE repository_id=?1
             AND status IN ('queued','running','paused')",
            params![repository_id],
            |r| r.get(0),
        )?)
    })?;
    if active > 0 {
        return Err(AppError::InvalidInput(
            "该仓库还有搬运任务，请先取消任务再删除".into(),
        ));
    }
    // v2：连带清理该仓库在缓存池中的目录（R{槽位号}）
    let slot: Option<i64> = db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT COALESCE(cache_slot, id) FROM repositories WHERE id=?1",
                params![repository_id],
                |r| r.get(0),
            )
            .optional()?)
    })?;
    if let (Some(slot), Ok(pool)) = (slot, cache::pool_root(db)) {
        let dir = cache::repo_dir(&pool, slot);
        if dir.exists() {
            std::fs::remove_dir_all(scanner::extend_path(&dir))?;
        }
    }
    db.with(|conn| {
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

const GAME_ROW_SQL: &str = "SELECT g.id, g.repository_id, g.relative_path, g.layout, g.display_name,
        g.game_code, g.cover_path, g.size_bytes, g.status, g.selected_version_id, g.updated_at,
        (SELECT COUNT(*) FROM game_versions v WHERE v.game_id = g.id),
        (SELECT v.display_name FROM game_versions v WHERE v.id = g.selected_version_id)
   FROM games g";

#[tauri::command]
pub fn list_games(
    state: State<'_, AppState>,
    repository_id: Option<i64>,
) -> AppResult<Vec<GameRow>> {
    state.db.with(|conn| {
        let sql = format!(
            "{GAME_ROW_SQL}
             WHERE (?1 IS NULL OR g.repository_id = ?1)
             ORDER BY g.display_name COLLATE NOCASE ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
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
        let sql = format!("{GAME_ROW_SQL} WHERE g.id=?1");
        let row = conn
            .prepare(&sql)?
            .query_row(params![game_id], map_game_row)
            .optional()?
            .ok_or(AppError::GameNotFound(game_id))?;

        let versions = load_versions(conn, game_id)?;
        let abs = absolute_path(conn, &row)?;
        Ok(GameDetail {
            row,
            versions,
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

#[tauri::command]
pub fn update_version(
    state: State<'_, AppState>,
    version_id: i64,
    display_name: String,
    note: Option<String>,
) -> AppResult<()> {
    let name = display_name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::InvalidInput("版本名称不能为空".into()));
    }
    state.db.with(|conn| {
        let affected = conn.execute(
            "UPDATE game_versions SET display_name=?1, note=?2, updated_at=?3 WHERE id=?4",
            params![
                name,
                note.filter(|s| !s.trim().is_empty()),
                Db::now(),
                version_id
            ],
        )?;
        if affected == 0 {
            return Err(AppError::VersionNotFound(version_id));
        }
        Ok(())
    })
}

#[tauri::command]
pub fn set_selected_version(
    state: State<'_, AppState>,
    game_id: i64,
    version_id: i64,
) -> AppResult<()> {
    state.db.with(|conn| {
        let belongs: Option<i64> = conn
            .query_row(
                "SELECT id FROM game_versions WHERE id=?1 AND game_id=?2",
                params![version_id, game_id],
                |r| r.get(0),
            )
            .optional()?;
        if belongs.is_none() {
            return Err(AppError::VersionNotFound(version_id));
        }
        conn.execute(
            "UPDATE games SET selected_version_id=?1, updated_at=?2 WHERE id=?3",
            params![version_id, Db::now(), game_id],
        )?;
        Ok(())
    })
}

#[tauri::command]
pub fn set_selected_executable(
    state: State<'_, AppState>,
    version_id: i64,
    executable_id: i64,
) -> AppResult<()> {
    state.db.with_mut(|conn| {
        let tx = conn.transaction()?;
        let game_id: Option<i64> = tx
            .query_row(
                "SELECT game_id FROM game_versions WHERE id=?1",
                params![version_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(game_id) = game_id else {
            return Err(AppError::VersionNotFound(version_id));
        };
        let exists: Option<i64> = tx
            .query_row(
                "SELECT id FROM game_version_executables WHERE id=?1 AND version_id=?2",
                params![executable_id, version_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(AppError::ExecutableNotFound(executable_id));
        }
        let now = Db::now();
        // 启动项在每个版本内唯一（各版本可各自保留自己的启动项配置）
        tx.execute(
            "UPDATE game_version_executables SET is_selected=0 WHERE version_id=?1",
            params![version_id],
        )?;
        tx.execute(
            "UPDATE game_version_executables SET is_selected=1, invalid=0 WHERE id=?1",
            params![executable_id],
        )?;
        // 设置启动项同时把该版本设为当前版本
        tx.execute(
            "UPDATE games SET selected_version_id=?1, updated_at=?2 WHERE id=?3",
            params![version_id, now, game_id],
        )?;
        tx.commit()?;
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// 版本（手动）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_executable_candidates(
    state: State<'_, AppState>,
    repository_id: i64,
    relative_path: String,
    max_depth: Option<u32>,
) -> AppResult<Vec<ExeCandidate>> {
    let root = resolve_repository(&state.db, repository_id)?;
    let rel = normalize_rel_arg(&relative_path);
    let base = join_rel(Path::new(&root), &rel);
    if !base.is_dir() {
        return Err(AppError::InvalidInput(format!("目录不存在：{rel}")));
    }
    let mut out = Vec::new();
    collect_exes(&base, &rel, 0, max_depth.unwrap_or(2), &mut out);
    out.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(out)
}

#[tauri::command]
pub fn create_manual_version(
    state: State<'_, AppState>,
    game_id: i64,
    relative_path: String,
    executable_relative_path: String,
    display_name: Option<String>,
) -> AppResult<i64> {
    let rel = normalize_rel_arg(&relative_path);
    let exe_rel = normalize_rel_arg(&executable_relative_path);

    let (repository_id, game_rel): (i64, String) = state.db.with(|conn| {
        conn.query_row(
            "SELECT repository_id, relative_path FROM games WHERE id=?1",
            params![game_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(AppError::GameNotFound(game_id))
    })?;

    // 目录必须是该游戏目录自身或其子目录
    if rel != game_rel && !rel.starts_with(&format!("{game_rel}/")) {
        return Err(AppError::InvalidInput(
            "版本目录必须位于该游戏目录之下".into(),
        ));
    }
    // 启动项必须位于该版本目录之下（允许更深层，用于兜底不规范结构）
    if !exe_rel.starts_with(&format!("{rel}/")) {
        return Err(AppError::InvalidInput(
            "可执行文件必须位于该版本目录之下".into(),
        ));
    }

    let root = resolve_repository(&state.db, repository_id)?;
    let dir = join_rel(Path::new(&root), &rel);
    if !dir.is_dir() {
        return Err(AppError::InvalidInput(format!("目录不存在：{rel}")));
    }
    let exe_path = join_rel(Path::new(&root), &exe_rel);
    if !exe_path.is_file() {
        return Err(AppError::InvalidInput(format!("可执行文件不存在：{exe_rel}")));
    }

    let label = rel.rsplit('/').next().unwrap_or(&rel).to_string();
    let shown = display_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| label.clone());
    let file_name = exe_rel.rsplit('/').next().unwrap_or(&exe_rel).to_string();
    let size = std::fs::metadata(&exe_path).map(|m| m.len()).unwrap_or(0);

    state.db.with_mut(|conn| {
        let tx = conn.transaction()?;
        let now = Db::now();
        // 同路径的已存在版本直接复用（避免重复）
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM game_versions WHERE game_id=?1 AND relative_path=?2",
                params![game_id, rel],
                |r| r.get(0),
            )
            .optional()?;

        let version_id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE game_versions SET source='manual', status='ok', updated_at=?1 WHERE id=?2",
                    params![now, id],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO game_versions(game_id, relative_path, version_label, display_name,
                        note, game_type, size_bytes, source, status, created_at, updated_at)
                     VALUES(?1, ?2, ?3, ?4, NULL, 'folder', 0, 'manual', 'ok', ?5, ?5)",
                    params![game_id, rel, label, shown, now],
                )?;
                tx.last_insert_rowid()
            }
        };

        tx.execute(
            "INSERT OR REPLACE INTO game_version_executables(version_id, relative_path, file_name,
                size_bytes, is_selected, source, invalid, created_at)
             VALUES(?1, ?2, ?3, ?4, 0, 'manual', 0, ?5)",
            params![version_id, exe_rel, file_name, size as i64, now],
        )?;
        tx.execute(
            "UPDATE game_version_executables SET is_selected=0 WHERE version_id=?1",
            params![version_id],
        )?;
        tx.execute(
            "UPDATE game_version_executables SET is_selected=1 WHERE version_id=?1 AND relative_path=?2",
            params![version_id, exe_rel],
        )?;
        tx.execute(
            "UPDATE games SET selected_version_id=?1, updated_at=?2 WHERE id=?3",
            params![version_id, now, game_id],
        )?;
        tx.commit()?;
        Ok(version_id)
    })
}

#[tauri::command]
pub fn add_manual_executable(
    state: State<'_, AppState>,
    version_id: i64,
    relative_path: String,
) -> AppResult<i64> {
    let exe_rel = normalize_rel_arg(&relative_path);
    let (game_id, repository_id, version_rel): (i64, i64, String) = state.db.with(|conn| {
        conn.query_row(
            "SELECT v.game_id, g.repository_id, v.relative_path
             FROM game_versions v JOIN games g ON g.id = v.game_id WHERE v.id=?1",
            params![version_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(AppError::VersionNotFound(version_id))
    })?;
    let _ = game_id;

    if !exe_rel.starts_with(&format!("{version_rel}/")) {
        return Err(AppError::InvalidInput(
            "可执行文件必须位于该版本目录之下".into(),
        ));
    }
    let root = resolve_repository(&state.db, repository_id)?;
    let exe_path = join_rel(Path::new(&root), &exe_rel);
    if !exe_path.is_file() {
        return Err(AppError::InvalidInput(format!("可执行文件不存在：{exe_rel}")));
    }
    let file_name = exe_rel.rsplit('/').next().unwrap_or(&exe_rel).to_string();
    let size = std::fs::metadata(&exe_path).map(|m| m.len()).unwrap_or(0);

    state.db.with(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO game_version_executables(version_id, relative_path, file_name,
                size_bytes, is_selected, source, invalid, created_at)
             VALUES(?1, ?2, ?3, ?4, 0, 'manual', 0, ?5)",
            params![version_id, exe_rel, file_name, size as i64, Db::now()],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

// ---------------------------------------------------------------------------
// 删除（黑名单）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn delete_version(state: State<'_, AppState>, version_id: i64) -> AppResult<()> {
    state.db.with_mut(|conn| {
        let tx = conn.transaction()?;
        let row: Option<(i64, String)> = tx
            .query_row(
                "SELECT v.game_id, v.relative_path FROM game_versions v WHERE v.id=?1",
                params![version_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((game_id, version_rel)) = row else {
            return Err(AppError::VersionNotFound(version_id));
        };

        let repository_id: i64 = tx.query_row(
            "SELECT repository_id FROM games WHERE id=?1",
            params![game_id],
            |r| r.get(0),
        )?;

        let now = Db::now();
        tx.execute(
            "INSERT OR REPLACE INTO blocklist(repository_id, kind, relative_path, created_at)
             VALUES(?1, 'version', ?2, ?3)",
            params![repository_id, version_rel, now],
        )?;
        tx.execute(
            "DELETE FROM game_versions WHERE id=?1",
            params![version_id],
        )?;

        // 删除的是当前选中版本 → 回退到剩余第一个，无则清空
        let selected: Option<i64> = tx
            .query_row(
                "SELECT selected_version_id FROM games WHERE id=?1",
                params![game_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if selected == Some(version_id) {
            let next: Option<i64> = tx
                .query_row(
                    "SELECT id FROM game_versions WHERE game_id=?1
                     ORDER BY version_label COLLATE NOCASE ASC LIMIT 1",
                    params![game_id],
                    |r| r.get(0),
                )
                .optional()?;
            tx.execute(
                "UPDATE games SET selected_version_id=?1 WHERE id=?2",
                params![next, game_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
}

#[tauri::command]
pub fn delete_game(state: State<'_, AppState>, game_id: i64) -> AppResult<()> {
    state.db.with_mut(|conn| {
        let tx = conn.transaction()?;
        let row: Option<(i64, String)> = tx
            .query_row(
                "SELECT repository_id, relative_path FROM games WHERE id=?1",
                params![game_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((repository_id, game_rel)) = row else {
            return Err(AppError::GameNotFound(game_id));
        };

        let now = Db::now();
        // 只写一条档案记录：档案被拉黑后，其下版本目录自然不会参与扫描。
        // 已被单独拉黑的版本记录保持原样（便于恢复档案时排除这些版本）。
        tx.execute(
            "INSERT OR REPLACE INTO blocklist(repository_id, kind, relative_path, created_at)
             VALUES(?1, 'game', ?2, ?3)",
            params![repository_id, game_rel, now],
        )?;
        tx.execute("DELETE FROM games WHERE id=?1", params![game_id])?;
        tx.commit()?;
        Ok(())
    })
}

#[tauri::command]
pub fn list_blocklist(state: State<'_, AppState>) -> AppResult<Vec<BlocklistRow>> {
    state.db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, repository_id, kind, relative_path, created_at
             FROM blocklist ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(BlocklistRow {
                    id: r.get(0)?,
                    repository_id: r.get(1)?,
                    kind: r.get(2)?,
                    relative_path: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    })
}

/// 取消拉黑。返回是否已**立即**恢复（不再等下次扫描）。
#[tauri::command]
pub fn unblock(state: State<'_, AppState>, blocklist_id: i64) -> AppResult<UnblockOutcome> {
    let (kind, relative_path, repository_id): (String, String, i64) = state.db.with(|conn| {
        conn.query_row(
            "SELECT kind, relative_path, repository_id FROM blocklist WHERE id=?1",
            params![blocklist_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(AppError::BlocklistNotFound(blocklist_id))
    })?;

    // 先解除拉黑（即使后续恢复失败，记录也应删除）
    state.db.with(|conn| {
        conn.execute("DELETE FROM blocklist WHERE id=?1", params![blocklist_id])?;
        Ok(())
    })?;

    if kind == "game" {
        return Ok(try_restore(&state.db, repository_id, &relative_path));
    }

    // 版本：档案未被拉黑才有地方恢复
    let game_rel = relative_path
        .split('/')
        .next()
        .unwrap_or(&relative_path)
        .to_string();
    let game_exists: Option<i64> = state.db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT id FROM games WHERE repository_id=?1 AND relative_path=?2",
                params![repository_id, game_rel],
                |r| r.get(0),
            )
            .optional()?)
    })?;
    if game_exists.is_none() {
        return Ok(UnblockOutcome {
            restored: false,
            message: "该档案当前处于删除状态；恢复档案后此版本会一并生效".into(),
        });
    }
    Ok(try_restore(&state.db, repository_id, &game_rel))
}

/// 立即恢复一个游戏（单档案枚举 + 落库，走与「档案重扫」相同的保护规则）
fn try_restore(db: &Db, repository_id: i64, game_rel: &str) -> UnblockOutcome {
    match restore_inner(db, repository_id, game_rel) {
        Ok(message) => UnblockOutcome {
            restored: true,
            message,
        },
        Err(message) => UnblockOutcome {
            restored: false,
            message,
        },
    }
}

fn restore_inner(db: &Db, repository_id: i64, game_rel: &str) -> Result<String, String> {
    let root = resolve_repository(db, repository_id)
        .map_err(|_| "仓库当前不可达，将在下次扫描时恢复".to_string())?;
    if !join_rel(Path::new(&root), game_rel).is_dir() {
        return Err("仓库中已找不到该目录，无法恢复".to_string());
    }

    let config = load_scan_config(db);
    let (entry, _warnings) = scanner::scan_one_game(Path::new(&root), game_rel, &config);

    let existing: Option<i64> = db
        .with(|conn| {
            Ok(conn
                .query_row(
                    "SELECT id FROM games WHERE repository_id=?1 AND relative_path=?2",
                    params![repository_id, game_rel],
                    |r| r.get(0),
                )
                .optional()?)
        })
        .map_err(|e| e.to_string())?;

    match existing {
        // 档案仍在（恢复某个版本）→ 只刷新该档案
        Some(game_id) => store::apply_single(db, game_id, &entry)
            .map(|_| "已恢复到档案".to_string())
            .map_err(|e| format!("恢复失败：{e}")),
        // 档案已被删除（恢复整个档案）→ 重新建档
        None => store::apply_report(db, repository_id, std::slice::from_ref(&entry), false)
            .map(|_| "已恢复到主页".to_string())
            .map_err(|e| format!("恢复失败：{e}")),
    }
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
    let root = resolve_repository(&db, repository_id)?;
    let start_index: u32 = db.with(|conn| {
        let v: Option<i64> = conn
            .query_row(
                "SELECT next_index FROM scan_checkpoints WHERE repository_id=?1",
                params![repository_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.unwrap_or(0).max(0) as u32)
    })?;

    let (job_id, control, last) = begin_job(&db, &slot, repository_id)?;
    insert_job_row(&db, repository_id)?;

    let app2 = app.clone();
    let db2 = db.clone();
    let slot2 = slot.clone();
    std::thread::spawn(move || {
        run_full_scan(
            app2, db2, slot2, repository_id, job_id, control, last, start_index, root,
        );
    });
    Ok(job_id)
}

#[tauri::command]
pub fn scan_single_game(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: i64,
) -> AppResult<u64> {
    let db = state.db.clone();
    let slot = state.scan.clone();

    let (repository_id, game_rel): (i64, String) = db.with(|conn| {
        conn.query_row(
            "SELECT repository_id, relative_path FROM games WHERE id=?1",
            params![game_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(AppError::GameNotFound(game_id))
    })?;
    let root = resolve_repository(&db, repository_id)?;

    let (job_id, control, last) = begin_job(&db, &slot, repository_id)?;
    insert_job_row(&db, repository_id)?;

    let app2 = app.clone();
    let db2 = db.clone();
    let slot2 = slot.clone();
    std::thread::spawn(move || {
        run_single_scan(
            app2, db2, slot2, repository_id, game_id, job_id, control, last, root, game_rel,
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
            scope: ScanScope::Full,
        });
    if active.control.signal() == scanner::ControlSignal::Paused {
        p.phase = ScanPhase::Paused;
    }
    Ok(Some(p))
}

// ---------------------------------------------------------------------------
// 扫描执行
// ---------------------------------------------------------------------------

fn begin_job(
    db: &Db,
    slot: &Arc<Mutex<ScanSlot>>,
    repository_id: i64,
) -> AppResult<(u64, ScanControl, Arc<Mutex<Option<ScanProgress>>>)> {
    let _ = db;
    let mut guard = slot.lock().map_err(|e| AppError::Internal(e.to_string()))?;
    if guard.active.is_some() {
        return Err(AppError::ScanBusy);
    }
    let job_id = guard.alloc_job_id();
    let control = ScanControl::new();
    let last: Arc<Mutex<Option<ScanProgress>>> = Arc::new(Mutex::new(None));
    guard.active = Some(ActiveScan {
        job_id,
        repository_id,
        control: control.clone(),
        last: last.clone(),
    });
    Ok((job_id, control, last))
}

fn insert_job_row(db: &Db, repository_id: i64) -> AppResult<()> {
    db.with(|conn| {
        conn.execute(
            "INSERT INTO scan_jobs(repository_id, started_at, status) VALUES(?1, ?2, 'running')",
            params![repository_id, Db::now()],
        )?;
        Ok(())
    })
}

fn release_job(slot: &Arc<Mutex<ScanSlot>>, job_id: u64) {
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

fn persist_progress(last: &Arc<Mutex<Option<ScanProgress>>>, p: &ScanProgress) {
    if let Ok(mut g) = last.lock() {
        *g = Some(p.clone());
    }
}

fn save_checkpoint_every(db: &Db, repository_id: i64, done: u32, total: u32) {
    let _ = db.with(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO scan_checkpoints(repository_id, next_index, total_entries, committed_games, updated_at)
             VALUES(?1, ?2, ?3, 0, ?4)",
            params![repository_id, done as i64, total as i64, Db::now()],
        )?;
        Ok(())
    });
}

fn finish_cancelled(app: &AppHandle, db: &Db, repository_id: i64, job_id: u64) {
    let _ = db.with(|conn| {
        conn.execute(
            "DELETE FROM scan_checkpoints WHERE repository_id=?1",
            params![repository_id],
        )?;
        conn.execute(
            "UPDATE scan_jobs SET status='cancelled', finished_at=?1
             WHERE repository_id=?2 AND status IN ('running','paused')",
            params![Db::now(), repository_id],
        )?;
        Ok(())
    });
    let _ = app.emit(
        "scan://failed",
        serde_json::json!({ "job_id": job_id, "message": "扫描已取消" }),
    );
}

fn finish_failed(app: &AppHandle, db: &Db, repository_id: i64, job_id: u64, err: &str) {
    let _ = db.with(|conn| {
        conn.execute(
            "UPDATE scan_jobs SET status='failed', finished_at=?1, error_message=?2
             WHERE repository_id=?3 AND status IN ('running','paused')",
            params![Db::now(), err, repository_id],
        )?;
        Ok(())
    });
    let _ = app.emit(
        "scan://failed",
        serde_json::json!({ "job_id": job_id, "message": err }),
    );
}

#[allow(clippy::too_many_arguments)]
fn run_full_scan(
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
    let config = load_scan_config(&db);
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
                scope: ScanScope::Full,
            };
            persist_progress(&last, &progress);
            // 节流 ≥100ms（§7.2）
            if last_emit.elapsed() >= Duration::from_millis(100) {
                let _ = app.emit("scan://progress", &progress);
                last_emit = Instant::now();
            }
            // 每 50 条落一次断点（避免频繁写库）
            if ev.done - last_saved >= 50 {
                save_checkpoint_every(&db, repository_id, ev.done, ev.total);
                last_saved = ev.done;
            }
        },
    );

    let cancelled =
        result.cancelled || control.signal() == scanner::ControlSignal::Cancelled;
    if cancelled {
        finish_cancelled(&app, &db, repository_id, job_id);
        release_job(&slot, job_id);
        return;
    }

    let warnings = result.report.warnings;
    match store::apply_report(
        &db,
        repository_id,
        &result.report.games,
        start_index == 0,
    ) {
        Ok(outcome) => {
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
                        (outcome.added + outcome.updated) as i64,
                        warnings.total() as i64,
                        repository_id
                    ],
                )?;
                Ok(())
            });
            let summary = ScanSummary {
                job_id,
                repository_id,
                found_count: result.report.games.len() as u32,
                added: outcome.added,
                updated: outcome.updated,
                missing: outcome.missing,
                warning_count: warnings.total(),
                root_loose_files: warnings.root_loose_files,
                truncated_with_version_dirs: warnings.truncated_with_version_dirs,
                enum_failed: warnings.enum_failed,
                scope: ScanScope::Full,
            };
            let _ = app.emit("scan://finished", &summary);
        }
        Err(e) => finish_failed(&app, &db, repository_id, job_id, &e.to_string()),
    }
    release_job(&slot, job_id);
}

#[allow(clippy::too_many_arguments)]
fn run_single_scan(
    app: AppHandle,
    db: Arc<Db>,
    slot: Arc<Mutex<ScanSlot>>,
    repository_id: i64,
    game_id: i64,
    job_id: u64,
    control: ScanControl,
    last: Arc<Mutex<Option<ScanProgress>>>,
    root: String,
    game_rel: String,
) {
    let config = load_scan_config(&db);

    let progress = ScanProgress {
        job_id,
        repository_id,
        done: 0,
        total: 1,
        found_games: 0,
        current: Some(game_rel.clone()),
        phase: ScanPhase::Scanning,
        warnings: 0,
        scope: ScanScope::Single,
    };
    persist_progress(&last, &progress);
    let _ = app.emit("scan://progress", &progress);

    if control.signal() == scanner::ControlSignal::Cancelled {
        finish_cancelled(&app, &db, repository_id, job_id);
        release_job(&slot, job_id);
        return;
    }

    let (game, warnings) = scanner::scan_one_game(Path::new(&root), &game_rel, &config);
    let direction = root_display(&root);

    // 目录已不存在 → 标记 missing 并明确报错，不静默
    let dir = join_rel(Path::new(&root), &game_rel);
    if !dir.is_dir() {
        let _ = db.with(|conn| {
            conn.execute(
                "UPDATE games SET status='missing', updated_at=?1 WHERE id=?2",
                params![Db::now(), game_id],
            )?;
            Ok(())
        });
        finish_failed(
            &app,
            &db,
            repository_id,
            job_id,
            &format!("目录已不存在：{direction}\\{game_rel}"),
        );
        release_job(&slot, job_id);
        return;
    }

    match store::apply_single(&db, game_id, &game) {
        Ok(outcome) => {
            let _ = db.with(|conn| {
                conn.execute(
                    "UPDATE repositories SET last_scan_at=?1, is_offline=0 WHERE id=?2",
                    params![Db::now(), repository_id],
                )?;
                conn.execute(
                    "UPDATE scan_jobs SET status='done', finished_at=?1, found_count=1, warning_count=?2
                     WHERE repository_id=?3 AND status IN ('running','paused')",
                    params![Db::now(), warnings.total() as i64, repository_id],
                )?;
                Ok(())
            });
            let summary = ScanSummary {
                job_id,
                repository_id,
                found_count: 1,
                added: outcome.added,
                updated: outcome.updated,
                missing: outcome.missing,
                warning_count: warnings.total(),
                root_loose_files: warnings.root_loose_files,
                truncated_with_version_dirs: warnings.truncated_with_version_dirs,
                enum_failed: warnings.enum_failed,
                scope: ScanScope::Single,
            };
            let _ = app.emit("scan://finished", &summary);
        }
        Err(e) => finish_failed(&app, &db, repository_id, job_id, &e.to_string()),
    }
    release_job(&slot, job_id);
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn map_game_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<GameRow> {
    Ok(GameRow {
        id: r.get(0)?,
        repository_id: r.get(1)?,
        relative_path: r.get(2)?,
        layout: r.get(3)?,
        display_name: r.get(4)?,
        game_code: r.get(5)?,
        cover_path: r.get(6)?,
        size_bytes: r.get(7)?,
        status: r.get(8)?,
        selected_version_id: r.get(9)?,
        updated_at: r.get(10)?,
        version_count: r.get(11)?,
        selected_version_label: r.get(12)?,
    })
}

fn load_versions(conn: &Connection, game_id: i64) -> AppResult<Vec<GameVersionRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, game_id, relative_path, version_label, display_name, note, game_type,
                size_bytes, source, status, updated_at
         FROM game_versions WHERE game_id=?1
         ORDER BY version_label COLLATE NOCASE ASC",
    )?;
    let mut rows: Vec<GameVersionRow> = stmt
        .query_map(params![game_id], |r| {
            Ok(GameVersionRow {
                id: r.get(0)?,
                game_id: r.get(1)?,
                relative_path: r.get(2)?,
                version_label: r.get(3)?,
                display_name: r.get(4)?,
                note: r.get(5)?,
                game_type: r.get(6)?,
                size_bytes: r.get(7)?,
                source: r.get(8)?,
                status: r.get(9)?,
                updated_at: r.get(10)?,
                executables: Vec::new(),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    for v in &mut rows {
        v.executables = load_executables(conn, v.id)?;
    }
    Ok(rows)
}

fn load_executables(conn: &Connection, version_id: i64) -> AppResult<Vec<ExecutableRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, relative_path, file_name, size_bytes, is_selected, source, invalid
         FROM game_version_executables WHERE version_id=?1
         ORDER BY is_selected DESC, invalid ASC, file_name COLLATE NOCASE ASC",
    )?;
    let rows = stmt
        .query_map(params![version_id], |r| {
            Ok(ExecutableRow {
                id: r.get(0)?,
                relative_path: r.get(1)?,
                file_name: r.get(2)?,
                size_bytes: r.get(3)?,
                is_selected: r.get::<_, i64>(4)? != 0,
                source: r.get(5)?,
                invalid: r.get::<_, i64>(6)? != 0,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
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

/// 解析仓库可达路径（D16：盘符 → IP → 主机名，按可达性跳过）
fn resolve_repository(db: &Db, repository_id: i64) -> AppResult<String> {
    let locators = db.with(|conn| load_locators(conn, repository_id))?;
    match locator::resolve(&locators) {
        Some((kind, path)) => {
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
            Ok(path)
        }
        None => {
            db.with(|conn| {
                conn.execute(
                    "UPDATE repositories SET is_offline=1 WHERE id=?1",
                    params![repository_id],
                )?;
                Ok(())
            })?;
            Err(AppError::RepositoryUnreachable)
        }
    }
}

/// 加载扫描配置（根级目录排除名单来自 settings）
fn load_scan_config(db: &Db) -> ScanConfig {
    let mut config = ScanConfig::default();
    if let Ok(raw) = db.get_setting("root_excluded_dirs") {
        let parsed = scanner::parse_root_excluded(raw);
        if !parsed.is_empty() {
            config.root_excluded_dirs = parsed;
        }
    }
    config
}

fn root_display(root: &str) -> String {
    root.trim_end_matches(['\\', '/']).to_string()
}

/// 相对路径拼接（"/" 分隔）
fn join_rel(root: &Path, rel: &str) -> std::path::PathBuf {
    let mut p = root.to_path_buf();
    for seg in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    p
}

fn normalize_rel_arg(rel: &str) -> String {
    rel.replace('\\', "/")
        .trim_matches('/')
        .to_string()
}

/// 递归收集可执行文件候选（仅用于手动配置，深度受限）
fn collect_exes(
    dir: &Path,
    rel: &str,
    depth: u32,
    max_depth: u32,
    out: &mut Vec<ExeCandidate>,
) {
    let Ok(entries) = std::fs::read_dir(scanner::extend_path(dir)) else {
        return;
    };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            subdirs.push((entry.path(), name));
            continue;
        }
        if !name.to_lowercase().ends_with(".exe") {
            continue;
        }
        out.push(ExeCandidate {
            relative_path: format!("{rel}/{name}"),
            file_name: name,
            size_bytes: meta.len(),
        });
    }
    if depth >= max_depth {
        return;
    }
    for (path, name) in subdirs {
        collect_exes(&path, &format!("{rel}/{name}"), depth + 1, max_depth, out);
    }
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
        format!(
            "{}\\{}",
            b.trim_end_matches(['\\', '/']),
            row.relative_path.replace('/', "\\")
        )
    }))
}

fn repository_display_name(payload: &NewRepository) -> String {
    payload
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
        .unwrap_or_else(|| payload.relative_root.clone())
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

// ---------------------------------------------------------------------------
// 缓存池（v2）
// 依据：DSPlans/Specs/MVPv2技术规格.md §5 / §11
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_cache_config(state: State<'_, AppState>) -> AppResult<(CacheConfig, CacheUsage)> {
    let db = &state.db;
    let cfg = cache::load_config(db)?;
    let usage = cache::usage(db)?;
    Ok((cfg, usage))
}

#[tauri::command]
pub fn set_cache_config(
    state: State<'_, AppState>,
    root: Option<String>,
    quota_bytes: i64,
) -> AppResult<()> {
    cache::save_config(&state.db, root.as_deref(), quota_bytes)
}

#[tauri::command]
pub fn get_cache_usage(state: State<'_, AppState>) -> AppResult<CacheUsage> {
    cache::usage(&state.db)
}

/// 点搬运前的即时判（纯读，不预留）
#[tauri::command]
pub fn preflight_transfer(
    state: State<'_, AppState>,
    version_id: i64,
) -> AppResult<TransferPreflight> {
    let db = &state.db;
    let meta = cache::load_version_meta(db, version_id)?;
    let root = resolve_repository(db, meta.repository_id)?;
    cache::preflight(db, version_id, Path::new(&root))
}

// ---------------------------------------------------------------------------
// 搬运（v2）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn start_transfer(state: State<'_, AppState>, version_id: i64) -> AppResult<i64> {
    let db = &state.db;
    let meta = cache::load_version_meta(db, version_id)?;
    let root = resolve_repository(db, meta.repository_id)?;
    transfer::start_transfer(&state.transfer, Path::new(&root), version_id)
}

#[tauri::command]
pub fn list_transfers(state: State<'_, AppState>) -> AppResult<Vec<TransferTaskView>> {
    transfer::list_tasks(&state.db)
}

/// 详情页按钮态：该版本当前的任务（排队/搬运/暂停/出错）
#[tauri::command]
pub fn get_version_transfer(
    state: State<'_, AppState>,
    version_id: i64,
) -> AppResult<Option<TransferTaskView>> {
    let db = &state.db;
    let task_id: Option<i64> = db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT id FROM transfer_tasks WHERE version_id=?1
                 AND status IN ('queued','running','paused','error') ORDER BY id DESC LIMIT 1",
                params![version_id],
                |r| r.get(0),
            )
            .optional()?)
    })?;
    Ok(task_id.and_then(|id| transfer::task_view(db, id)))
}

#[tauri::command]
pub fn pause_transfer(state: State<'_, AppState>, task_id: i64) -> AppResult<()> {
    transfer::pause_task(&state.transfer, task_id)
}

#[tauri::command]
pub fn resume_transfer(state: State<'_, AppState>, task_id: i64) -> AppResult<()> {
    transfer::resume_task(&state.transfer, task_id)
}

#[tauri::command]
pub fn cancel_transfer(state: State<'_, AppState>, task_id: i64) -> AppResult<()> {
    transfer::cancel_task(&state.transfer, task_id)
}

#[tauri::command]
pub fn retry_transfer(state: State<'_, AppState>, task_id: i64) -> AppResult<i64> {
    transfer::retry_task(&state.transfer, task_id)
}

/// 从缓存池移除（只删池内副本）
#[tauri::command]
pub fn remove_cache(state: State<'_, AppState>, version_id: i64) -> AppResult<()> {
    cache::remove_cache(&state.db, version_id)
}

/// 详情页按钮态机：一次拿该档案下所有版本的缓存与任务状态
#[tauri::command]
pub fn get_version_states(
    state: State<'_, AppState>,
    game_id: i64,
) -> AppResult<Vec<VersionState>> {
    let db = &state.db;
    // 先在一个 with 里收集原始状态（避免在持锁闭包内再调 task_view 造成重入死锁）
    let raw: Vec<(i64, Option<String>, Option<i64>)> = db.with(|conn| {
        let mut stmt =
            conn.prepare("SELECT id FROM game_versions WHERE game_id=?1 ORDER BY id ASC")?;
        let ids: Vec<i64> = stmt
            .query_map(params![game_id], |r| r.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        let mut out = Vec::with_capacity(ids.len());
        for vid in ids {
            let cache: Option<String> = conn
                .query_row(
                    "SELECT status FROM cache_entries WHERE version_id=?1",
                    params![vid],
                    |r| r.get(0),
                )
                .optional()?;
            let task_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM transfer_tasks WHERE version_id=?1
                     AND status IN ('queued','running','paused','error')
                     ORDER BY id DESC LIMIT 1",
                    params![vid],
                    |r| r.get(0),
                )
                .optional()?;
            out.push((vid, cache, task_id));
        }
        Ok(out)
    })?;

    let mut out = Vec::with_capacity(raw.len());
    for (vid, cache, task_id) in raw {
        out.push(VersionState {
            version_id: vid,
            cached: cache.as_deref() == Some("ok"),
            cache_missing: cache.as_deref() == Some("missing"),
            task: task_id.and_then(|t| transfer::task_view(db, t)),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 启动（v2）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn launch_game(state: State<'_, AppState>, game_id: i64) -> AppResult<()> {
    let db = &state.db;
    let version_id = selected_version_id(db, game_id)?.ok_or(AppError::LaunchNoExecutable)?;

    let exe_rel: Option<String> = db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT relative_path FROM game_version_executables
                 WHERE version_id=?1 AND is_selected=1 AND invalid=0",
                params![version_id],
                |r| r.get(0),
            )
            .optional()?)
    })?;
    let exe_rel = exe_rel.ok_or(AppError::LaunchNoExecutable)?;

    let mapped: Option<(String, String)> = db.with(|conn| {
        Ok(conn
            .query_row(
                "SELECT v.relative_path, c.dest_rel FROM game_versions v
                 JOIN cache_entries c ON c.version_id = v.id
                 WHERE v.id=?1 AND c.status='ok'",
                params![version_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?)
    })?;
    let (version_rel, dest_rel) =
        mapped.ok_or_else(|| AppError::LaunchFailed("该版本尚未缓存，请先搬运".into()))?;

    let pool = cache::pool_root(db)?;
    let prefix = format!("{}/", version_rel.trim_end_matches('/'));
    let suffix = exe_rel.strip_prefix(&prefix).unwrap_or(&exe_rel);
    let exe_abs = cache::join_rel(&pool, &dest_rel).join(suffix.replace('/', "\\"));

    let abs_str = exe_abs.to_string_lossy().to_string();
    if abs_str.chars().count() > 260 {
        return Err(AppError::LaunchFailed(
            "可执行文件路径超过系统 260 字符限制，游戏可能无法启动".into(),
        ));
    }
    if !exe_abs.exists() {
        return Err(AppError::LaunchFailed(
            "可执行文件不存在，缓存可能已被清理".into(),
        ));
    }
    let dir = exe_abs.parent().map(|p| p.to_path_buf()).unwrap_or(pool);
    std::process::Command::new(&exe_abs)
        .current_dir(&dir)
        .spawn()
        .map_err(|e| AppError::LaunchFailed(e.to_string()))?;
    Ok(())
}

fn selected_version_id(db: &Db, game_id: i64) -> AppResult<Option<i64>> {
    db.with(|conn| {
        let v: Option<Option<i64>> = conn
            .query_row(
                "SELECT selected_version_id FROM games WHERE id=?1",
                params![game_id],
                |r| r.get(0),
            )
            .optional()?;
        match v {
            None => Err(AppError::GameNotFound(game_id)),
            Some(inner) => Ok(inner),
        }
    })
}

// ---------------------------------------------------------------------------
// 仓库删除预览（v2）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_repository_delete_preview(
    state: State<'_, AppState>,
    repository_id: i64,
) -> AppResult<RepositoryDeletePreview> {
    state.db.with(|conn| {
        let (cache_count, cache_bytes): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes),0) FROM cache_entries
             WHERE repository_id=?1 AND status='ok'",
            params![repository_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let active: i64 = conn.query_row(
            "SELECT COUNT(*) FROM transfer_tasks WHERE repository_id=?1
             AND status IN ('queued','running','paused')",
            params![repository_id],
            |r| r.get(0),
        )?;
        Ok(RepositoryDeletePreview {
            cache_count,
            cache_bytes,
            active_task_count: active,
        })
    })
}
