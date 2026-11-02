//! 档案持久化：两层结构的幂等落库（v1.1）。
//!
//! 依据 DSPlans/MVPv1.1技术规格.md §6（重扫保护矩阵）与 §7（行为细则）：
//! - 扫描只刷新「客观事实」：存在性、大小、类型、状态
//! - **用户裁决永不覆盖**：名称 / 编号 / 封面 / 版本名 / 备注 / 选中的版本 / 选中的可执行文件
//! - 本次未扫到的标 missing，**不删除**
//! - 黑名单优先于一切：命中即整条跳过，不产生 missing
//! - 手动版本（source=manual）**永不删除**，也不参与 missing 计算

use std::collections::{HashMap, HashSet};

use rusqlite::{params, OptionalExtension, Transaction};

use crate::db::Db;
use crate::error::AppResult;
use crate::models::{ArchiveGroup, ExeCandidate, GameEntry, VersionEntry};

/// 一次落库的统计
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub added: u32,
    pub updated: u32,
    pub missing: u32,
}

/// 黑名单快照
#[derive(Debug, Default)]
struct Blocked {
    games: HashSet<String>,
    versions: HashSet<String>,
}

impl Blocked {
    fn is_game(&self, rel: &str) -> bool {
        self.games.contains(rel)
    }

    fn is_version(&self, rel: &str) -> bool {
        self.versions.contains(rel)
    }
}

/// 全量扫描落库（§6）
///
/// `mark_missing`：是否对本次未扫到的条目标记 missing。续扫（断点恢复）时应传 false——
/// 避免把尚未处理的条目标成缺失。
pub fn apply_report(
    db: &Db,
    repository_id: i64,
    games: &[GameEntry],
    mark_missing: bool,
) -> AppResult<ApplyOutcome> {
    db.with_mut(|conn| {
        let tx = conn.transaction()?;
        let now = Db::now();
        let blocked = load_blocked(&tx, repository_id)?;
        let mut out = ApplyOutcome::default();

        let mut seen_games: HashSet<String> = HashSet::new();
        let mut seen_versions: HashMap<String, HashSet<String>> = HashMap::new();

        for g in games {
            if blocked.is_game(&g.relative_path) {
                continue;
            }
            seen_games.insert(g.relative_path.clone());
            let (game_id, is_new) = upsert_game(&tx, repository_id, g, &now)?;
            if is_new {
                out.added += 1;
            } else {
                out.updated += 1;
            }

            let mut seen: HashSet<String> = HashSet::new();
            for v in &g.versions {
                if blocked.is_version(&v.relative_path) {
                    continue;
                }
                seen.insert(v.relative_path.clone());
                upsert_version(&tx, game_id, v, &now)?;
            }
            seen_versions.insert(g.relative_path.clone(), seen);
        }

        if mark_missing {
            out.missing += mark_missing_games(&tx, repository_id, &seen_games, &blocked, &now)?;
            for (game_rel, seen) in &seen_versions {
                let game_id: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM games WHERE repository_id=?1 AND relative_path=?2",
                        params![repository_id, game_rel],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(gid) = game_id {
                    out.missing +=
                        mark_missing_versions(&tx, gid, seen, &blocked, &now)?;
                }
            }
        }

        repair_selected_versions(&tx)?;
        tx.commit()?;
        Ok(out)
    })
}

/// 单档案重扫落库：只处理一个游戏，missing 也只算该游戏的版本。
pub fn apply_single(db: &Db, game_id: i64, entry: &GameEntry) -> AppResult<ApplyOutcome> {
    db.with_mut(|conn| {
        let tx = conn.transaction()?;
        let now = Db::now();

        let repository_id: i64 = tx.query_row(
            "SELECT repository_id FROM games WHERE id=?1",
            params![game_id],
            |r| r.get(0),
        )?;
        let blocked = load_blocked(&tx, repository_id)?;
        let mut out = ApplyOutcome::default();

        // 游戏标识层的相对路径以数据库为准（entry 来自同一路径，但保证不漂移）
        let game_rel: String = tx.query_row(
            "SELECT relative_path FROM games WHERE id=?1",
            params![game_id],
            |r| r.get(0),
        )?;
        if blocked.is_game(&game_rel) {
            return Ok(out);
        }

        let (gid, is_new) = upsert_game(&tx, repository_id, entry, &now)?;
        debug_assert_eq!(gid, game_id);
        if is_new {
            out.added += 1;
        } else {
            out.updated += 1;
        }

        let mut seen: HashSet<String> = HashSet::new();
        for v in &entry.versions {
            if blocked.is_version(&v.relative_path) {
                continue;
            }
            seen.insert(v.relative_path.clone());
            upsert_version(&tx, gid, v, &now)?;
        }
        out.missing += mark_missing_versions(&tx, gid, &seen, &blocked, &now)?;

        repair_selected_versions(&tx)?;
        tx.commit()?;
        Ok(out)
    })
}

// ---------------------------------------------------------------------------
// 内部实现
// ---------------------------------------------------------------------------

fn load_blocked(tx: &Transaction<'_>, repository_id: i64) -> AppResult<Blocked> {
    let mut stmt =
        tx.prepare("SELECT kind, relative_path FROM blocklist WHERE repository_id=?1")?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![repository_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let mut blocked = Blocked::default();
    for (kind, rel) in rows {
        if kind == "game" {
            blocked.games.insert(rel);
        } else {
            blocked.versions.insert(rel);
        }
    }
    Ok(blocked)
}

/// 返回 (game_id, 是否新增)
fn upsert_game(
    tx: &Transaction<'_>,
    repository_id: i64,
    g: &GameEntry,
    now: &str,
) -> AppResult<(i64, bool)> {
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM games WHERE repository_id=?1 AND relative_path=?2",
            params![repository_id, g.relative_path],
            |r| r.get(0),
        )
        .optional()?;

    match existing {
        Some(id) => {
            // 只刷新客观字段；display_name / game_code / cover_path / selected_version_id 不动
            tx.execute(
                "UPDATE games SET layout=?1, size_bytes=?2, status='ok', updated_at=?3 WHERE id=?4",
                params![g.layout.as_str(), g.size_bytes as i64, now, id],
            )?;
            Ok((id, false))
        }
        None => {
            tx.execute(
                "INSERT INTO games(repository_id, relative_path, layout, display_name,
                                   size_bytes, status, created_at, updated_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, 'ok', ?6, ?6)",
                params![
                    repository_id,
                    g.relative_path,
                    g.layout.as_str(),
                    g.display_name,
                    g.size_bytes as i64,
                    now
                ],
            )?;
            Ok((tx.last_insert_rowid(), true))
        }
    }
}

fn upsert_version(
    tx: &Transaction<'_>,
    game_id: i64,
    v: &VersionEntry,
    now: &str,
) -> AppResult<i64> {
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM game_versions WHERE game_id=?1 AND relative_path=?2",
            params![game_id, v.relative_path],
            |r| r.get(0),
        )
        .optional()?;

    let version_id = match existing {
        Some(id) => {
            // version_label 跟随目录原名刷新；display_name / note / source 不动
            tx.execute(
                "UPDATE game_versions SET version_label=?1, game_type=?2, size_bytes=?3,
                    status='ok', updated_at=?4 WHERE id=?5",
                params![
                    v.version_label,
                    v.game_type.as_str(),
                    v.size_bytes as i64,
                    now,
                    id
                ],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO game_versions(game_id, relative_path, version_label, display_name,
                                           game_type, size_bytes, source, status, created_at, updated_at)
                 VALUES(?1, ?2, ?3, ?3, ?4, ?5, 'scan', 'ok', ?6, ?6)",
                params![
                    game_id,
                    v.relative_path,
                    v.version_label,
                    v.game_type.as_str(),
                    v.size_bytes as i64,
                    now
                ],
            )?;
            tx.last_insert_rowid()
        }
    };

    replace_scan_executables(tx, version_id, &v.executables, now)?;
    replace_version_files(tx, version_id, &v.archive_groups)?;
    Ok(version_id)
}

/// 刷新扫描来源的可执行文件候选；保留用户选择与手填项（§6）
fn replace_scan_executables(
    tx: &Transaction<'_>,
    version_id: i64,
    exes: &[ExeCandidate],
    now: &str,
) -> AppResult<()> {
    let mut stmt = tx.prepare(
        "SELECT id, relative_path, is_selected, source FROM game_version_executables
         WHERE version_id=?1",
    )?;
    let rows: Vec<(i64, String, bool, String)> = stmt
        .query_map(params![version_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0, r.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let new_paths: HashSet<&str> = exes.iter().map(|e| e.relative_path.as_str()).collect();

    // 1) 不再出现的扫描行：选中的保留并标失效（绝不自动改选），其余删除
    for (id, path, selected, source) in &rows {
        if source == "scan" && !new_paths.contains(path.as_str()) {
            if *selected {
                tx.execute(
                    "UPDATE game_version_executables SET invalid=1 WHERE id=?1",
                    params![id],
                )?;
            } else {
                tx.execute(
                    "DELETE FROM game_version_executables WHERE id=?1",
                    params![id],
                )?;
            }
        }
    }

    // 2) 新候选：已存在则刷新大小并清除失效标记，否则插入
    for e in exes {
        match rows.iter().find(|(_, p, _, _)| p == &e.relative_path) {
            Some((id, _, _, _)) => {
                tx.execute(
                    "UPDATE game_version_executables SET size_bytes=?1, invalid=0 WHERE id=?2",
                    params![e.size_bytes as i64, id],
                )?;
            }
            None => {
                // 同路径的 manual 行已存在时忽略，避免重复展示
                tx.execute(
                    "INSERT OR IGNORE INTO game_version_executables(version_id, relative_path,
                        file_name, size_bytes, is_selected, source, invalid, created_at)
                     VALUES(?1, ?2, ?3, ?4, 0, 'scan', 0, ?5)",
                    params![
                        version_id,
                        e.relative_path,
                        e.file_name,
                        e.size_bytes as i64,
                        now
                    ],
                )?;
            }
        }
    }
    Ok(())
}

fn replace_version_files(
    tx: &Transaction<'_>,
    version_id: i64,
    groups: &[ArchiveGroup],
) -> AppResult<()> {
    tx.execute(
        "DELETE FROM game_version_files WHERE version_id=?1",
        params![version_id],
    )?;
    for g in groups {
        for f in &g.files {
            tx.execute(
                "INSERT OR REPLACE INTO game_version_files(version_id, group_key, file_name,
                    part_index, size_bytes)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    version_id,
                    g.base_name,
                    f.file_name,
                    f.part_index,
                    f.size_bytes as i64
                ],
            )?;
        }
    }
    Ok(())
}

fn mark_missing_games(
    tx: &Transaction<'_>,
    repository_id: i64,
    seen: &HashSet<String>,
    blocked: &Blocked,
    now: &str,
) -> AppResult<u32> {
    let mut stmt = tx.prepare(
        "SELECT id, relative_path FROM games WHERE repository_id=?1 AND status='ok'",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map(params![repository_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let mut n = 0u32;
    for (id, rel) in rows {
        if blocked.is_game(&rel) || seen.contains(&rel) {
            continue;
        }
        tx.execute(
            "UPDATE games SET status='missing', updated_at=?1 WHERE id=?2",
            params![now, id],
        )?;
        n += 1;
    }
    Ok(n)
}

fn mark_missing_versions(
    tx: &Transaction<'_>,
    game_id: i64,
    seen: &HashSet<String>,
    blocked: &Blocked,
    now: &str,
) -> AppResult<u32> {
    let mut stmt = tx.prepare(
        "SELECT id, relative_path, source FROM game_versions WHERE game_id=?1 AND status='ok'",
    )?;
    let rows: Vec<(i64, String, String)> = stmt
        .query_map(params![game_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let mut n = 0u32;
    for (id, rel, source) in rows {
        // 手动版本永不删除、不参与 missing
        if source == "manual" {
            continue;
        }
        if blocked.is_version(&rel) || seen.contains(&rel) {
            continue;
        }
        tx.execute(
            "UPDATE game_versions SET status='missing', updated_at=?1 WHERE id=?2",
            params![now, id],
        )?;
        n += 1;
    }
    Ok(n)
}

/// 选中版本悬空修复：指向的版本已不存在则置 NULL（§7.3）
fn repair_selected_versions(tx: &Transaction<'_>) -> AppResult<()> {
    tx.execute(
        "UPDATE games SET selected_version_id=NULL
         WHERE selected_version_id IS NOT NULL
           AND selected_version_id NOT IN (SELECT id FROM game_versions WHERE game_id=games.id)",
        [],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ArchiveFile, GameLayout, GameType};
    use std::path::PathBuf;

    fn temp_db(tag: &str) -> (Db, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sakugame_store_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        (Db::open(&path).expect("open db"), dir)
    }

    fn seed_repo(db: &Db) -> i64 {
        db.with(|c| {
            c.execute(
                "INSERT INTO repositories(name, relative_root, created_at) VALUES('r', 'Game', 'now')",
                [],
            )?;
            Ok(c.last_insert_rowid())
        })
        .unwrap()
    }

    fn compact_game(path: &str, exe: &str) -> GameEntry {
        GameEntry {
            relative_path: path.to_string(),
            layout: GameLayout::Compact,
            display_name: path.rsplit('/').next().unwrap_or(path).to_string(),
            size_bytes: 100,
            versions: vec![VersionEntry {
                relative_path: path.to_string(),
                version_label: path.rsplit('/').next().unwrap_or(path).to_string(),
                game_type: GameType::Folder,
                size_bytes: 100,
                executables: vec![ExeCandidate {
                    relative_path: format!("{path}/{exe}"),
                    file_name: exe.to_string(),
                    size_bytes: 100,
                }],
                archive_groups: Vec::new(),
            }],
        }
    }

    fn standard_game(path: &str, versions: &[&str]) -> GameEntry {
        GameEntry {
            relative_path: path.to_string(),
            layout: GameLayout::Standard,
            display_name: path.to_string(),
            size_bytes: 100 * versions.len() as u64,
            versions: versions
                .iter()
                .map(|v| VersionEntry {
                    relative_path: format!("{path}/{v}"),
                    version_label: v.to_string(),
                    game_type: GameType::Folder,
                    size_bytes: 100,
                    executables: vec![ExeCandidate {
                        relative_path: format!("{path}/{v}/game.exe"),
                        file_name: "game.exe".to_string(),
                        size_bytes: 100,
                    }],
                    archive_groups: Vec::new(),
                })
                .collect(),
        }
    }

    fn archive_game(path: &str) -> GameEntry {
        GameEntry {
            relative_path: path.to_string(),
            layout: GameLayout::Compact,
            display_name: path.to_string(),
            size_bytes: 200,
            versions: vec![VersionEntry {
                relative_path: path.to_string(),
                version_label: path.to_string(),
                game_type: GameType::Archive,
                size_bytes: 200,
                executables: Vec::new(),
                archive_groups: vec![ArchiveGroup {
                    base_name: "game".into(),
                    files: vec![ArchiveFile {
                        file_name: "game.part1.rar".into(),
                        size_bytes: 200,
                        part_index: Some(1),
                    }],
                }],
            }],
        }
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.with(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
    }

    /// T-1.1-13 / ACC-13：重扫必须保留全部用户字段
    #[test]
    fn rescan_preserves_user_fields() {
        let (db, dir) = temp_db("preserve");
        let repo = seed_repo(&db);

        apply_report(&db, repo, &[compact_game("A", "game.exe")], true).unwrap();

        // 用户改：游戏名称/编号/封面 + 版本名/备注 + 选中版本 + 选中启动项
        db.with(|c| {
            c.execute("UPDATE games SET display_name='我的游戏', game_code='RJ123', cover_path='C:\\c.png' WHERE id=1", [])?;
            c.execute("UPDATE game_versions SET display_name='正式版', note='1.0 汉化' WHERE id=1", [])?;
            c.execute("UPDATE games SET selected_version_id=(SELECT id FROM game_versions WHERE game_id=1) WHERE id=1", [])?;
            c.execute("UPDATE game_version_executables SET is_selected=1 WHERE id=1", [])?;
            Ok(())
        })
        .unwrap();

        // 重扫（大小变化）
        apply_report(&db, repo, &[compact_game("A", "game.exe")], true).unwrap();

        let (name, code, cover, sel): (String, Option<String>, Option<String>, Option<i64>) = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT display_name, game_code, cover_path, selected_version_id FROM games WHERE id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?)
            })
            .unwrap();
        assert_eq!(name, "我的游戏");
        assert_eq!(code.as_deref(), Some("RJ123"));
        assert_eq!(cover.as_deref(), Some("C:\\c.png"));
        assert!(sel.is_some(), "选中版本不得被重扫清除");

        let (vname, note): (String, Option<String>) = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT display_name, note FROM game_versions WHERE id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(vname, "正式版", "版本显示名不得被覆盖");
        assert_eq!(note.as_deref(), Some("1.0 汉化"));

        let selected: i64 = count(
            &db,
            "SELECT is_selected FROM game_version_executables WHERE version_id=1",
        );
        assert_eq!(selected, 1, "选中的启动项不得被重扫清除");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-1.1-12：本次未扫到的游戏与版本 → missing，不删除
    #[test]
    fn missing_marked_not_deleted() {
        let (db, dir) = temp_db("missing");
        let repo = seed_repo(&db);

        apply_report(
            &db,
            repo,
            &[
                standard_game("B", &["Ver1.0", "Ver2.0"]),
                compact_game("C", "game.exe"),
            ],
            true,
        )
        .unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM games"), 2);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM game_versions"), 3);

        // 只扫到 B 的 Ver1.0
        let out = apply_report(&db, repo, &[standard_game("B", &["Ver1.0"])], true).unwrap();
        assert_eq!(out.missing, 2, "C 游戏 1 条 + Ver2.0 版本 1 条");

        assert_eq!(count(&db, "SELECT COUNT(*) FROM games"), 2, "不得删除");
        let ver2_status: String = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT status FROM game_versions WHERE version_label='Ver2.0'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(ver2_status, "missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-1.1-15：手动版本重扫不删除
    #[test]
    fn manual_version_survives_rescan() {
        let (db, dir) = temp_db("manual");
        let repo = seed_repo(&db);

        apply_report(&db, repo, &[standard_game("D", &["Ver1.0"])], true).unwrap();
        let game_id: i64 = count(&db, "SELECT id FROM games WHERE relative_path='D'");
        let version_id: i64 = count(&db, "SELECT id FROM game_versions WHERE game_id=1");

        db.with(|c| {
            c.execute(
                "UPDATE game_versions SET source='manual' WHERE id=?1",
                params![version_id],
            )?;
            Ok(())
        })
        .unwrap();

        // 重扫：该版本目录消失
        let out = apply_report(&db, repo, &[compact_game("D", "game.exe")], true).unwrap();

        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM game_versions WHERE game_id=1 AND source='manual'"),
            1,
            "手动版本永不删除"
        );
        assert_eq!(out.missing, 0, "手动版本不参与 missing");

        // 游戏本身仍在
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM games"),
            1,
            "game_id={game_id}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-1.1-14：选中的启动项消失 → 标失效，不自动改选
    #[test]
    fn selected_executable_marked_invalid() {
        let (db, dir) = temp_db("invalid");
        let repo = seed_repo(&db);

        apply_report(&db, repo, &[compact_game("E", "old.exe")], true).unwrap();
        db.with(|c| {
            c.execute(
                "UPDATE game_version_executables SET is_selected=1 WHERE file_name='old.exe'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        // 重扫：old.exe 换成 new.exe
        apply_report(&db, repo, &[compact_game("E", "new.exe")], true).unwrap();

        let (per_version, invalid, selected): (i64, i64, i64) = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*),
                            COALESCE(SUM(invalid),0),
                            COALESCE(SUM(is_selected),0)
                     FROM game_version_executables WHERE version_id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .unwrap();

        assert_eq!(per_version, 2, "旧（已失效）+ 新 = 2 行");
        assert_eq!(invalid, 1, "消失的选中项标失效");
        assert_eq!(selected, 1, "选中状态保留，不自动改选");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §7.1：黑名单命中的游戏 / 版本被跳过，且不产生 missing
    #[test]
    fn blocklist_skips_and_hides() {
        let (db, dir) = temp_db("block");
        let repo = seed_repo(&db);

        apply_report(&db, repo, &[standard_game("F", &["Ver1.0", "Ver2.0"])], true).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO blocklist(repository_id, kind, relative_path, created_at)
                 VALUES(?1, 'version', 'F/Ver2.0', 'now')",
                params![repo],
            )?;
            Ok(())
        })
        .unwrap();

        // 重扫：命中黑名单的版本不再落库
        let out = apply_report(&db, repo, &[standard_game("F", &["Ver1.0", "Ver2.0"])], true).unwrap();
        assert_eq!(out.missing, 0, "黑名单条目不产生 missing");

        let paths: Vec<String> = db
            .with(|c| {
                let mut stmt =
                    c.prepare("SELECT relative_path FROM game_versions ORDER BY relative_path")?;
                let v = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .filter_map(|r| r.ok())
                    .collect();
                Ok(v)
            })
            .unwrap();
        assert!(
            paths.contains(&"F/Ver2.0".to_string()),
            "黑名单记录不会删除既有行，仅跳过刷新: {paths:?}"
        );

        // 全新游戏被拉黑 → 不建
        db.with(|c| {
            c.execute(
                "INSERT INTO blocklist(repository_id, kind, relative_path, created_at)
                 VALUES(?1, 'game', 'G', 'now')",
                params![repo],
            )?;
            Ok(())
        })
        .unwrap();
        apply_report(&db, repo, &[compact_game("G", "game.exe")], true).unwrap();
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM games WHERE relative_path='G'"),
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 续扫（mark_missing=false）不得把未处理条目标成缺失
    #[test]
    fn resume_does_not_mark_missing() {
        let (db, dir) = temp_db("resume");
        let repo = seed_repo(&db);

        apply_report(
            &db,
            repo,
            &[compact_game("A", "game.exe"), compact_game("B", "game.exe")],
            true,
        )
        .unwrap();

        let out = apply_report(&db, repo, &[compact_game("A", "game.exe")], false).unwrap();
        assert_eq!(out.missing, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 压缩包型：game_version_files 正确落库且重扫不累积
    #[test]
    fn archive_files_replaced() {
        let (db, dir) = temp_db("files");
        let repo = seed_repo(&db);

        apply_report(&db, repo, &[archive_game("H")], true).unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM game_version_files"), 1);

        apply_report(&db, repo, &[archive_game("H")], true).unwrap();
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM game_version_files"),
            1,
            "应先删后插，不得累积"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T-1.1-20 / ACC-20：同一游戏 2 版本 → 1 条 games 行
    #[test]
    fn one_game_one_row() {
        let (db, dir) = temp_db("onerow");
        let repo = seed_repo(&db);

        apply_report(&db, repo, &[standard_game("I", &["Ver1.0", "Ver2.0"])], true).unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM games"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM game_versions"), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §7.3：选中版本失效 → 悬空修复置 NULL
    #[test]
    fn dangling_selected_version_repaired() {
        let (db, dir) = temp_db("dangling");
        let repo = seed_repo(&db);

        apply_report(&db, repo, &[standard_game("J", &["Ver1.0"])], true).unwrap();
        db.with(|c| {
            c.execute(
                "UPDATE games SET selected_version_id=9999 WHERE relative_path='J'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        apply_report(&db, repo, &[standard_game("J", &["Ver1.0"])], true).unwrap();
        let sel: Option<i64> = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT selected_version_id FROM games WHERE relative_path='J'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert!(sel.is_none(), "悬空的选中版本应被置空");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 手动版本与扫描得到的同路径版本合并不重复
    #[test]
    fn manual_executable_not_duplicated() {
        let (db, dir) = temp_db("dupexe");
        let repo = seed_repo(&db);

        apply_report(&db, repo, &[compact_game("K", "game.exe")], true).unwrap();
        // 用户手动添加同一路径的可执行文件（commands 层用 INSERT OR REPLACE）
        db.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO game_version_executables(version_id, relative_path, file_name,
                    size_bytes, is_selected, source, invalid, created_at)
                 VALUES(1, 'K/game.exe', 'game.exe', 1, 0, 'manual', 0, 'now')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        // 重扫：同一路径不得出现两行
        apply_report(&db, repo, &[compact_game("K", "game.exe")], true).unwrap();
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM game_version_executables WHERE relative_path='K/game.exe'"
            ),
            1,
            "同一路径只应有一行"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
