//! 档案持久化：幂等重扫的差异比对。
//!
//! 依据 DSPlans/MVPv1扫描器规格.md §8：
//! - 新条目 INSERT
//! - 已存在条目只刷新 size / game_type / status，**绝不触碰**用户字段（D11）
//! - 本次未扫到的条目标记 missing，**不删除**（D11）
//!
//! 从 commands.rs 抽出，便于集成测试直接验证数据保护行为。

use std::collections::HashSet;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::db::Db;
use crate::error::AppResult;
use crate::models::{GameEntry, GameType};

/// 幂等重扫（§8）：返回 (新增数, 更新数, 标记缺失数)
///
/// `mark_missing`：是否对本次未扫到的条目标记 missing。
/// 续扫（断点恢复）时应传 false——避免把尚未处理的条目标成缺失。
pub fn apply_results(
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

fn write_game_files(tx: &Transaction<'_>, game_id: i64, g: &GameEntry) -> AppResult<()> {
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
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ArchiveFile, ArchiveGroup};
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
        let db = Db::open(&path).expect("open db");
        (db, dir)
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

    fn folder_entry(path: &str, size: u64) -> GameEntry {
        GameEntry {
            relative_path: path.to_string(),
            group_key: String::new(),
            game_type: GameType::Folder,
            display_name: path.rsplit('/').next().unwrap_or(path).to_string(),
            size_bytes: size,
            groups: Vec::new(),
        }
    }

    fn archive_entry(path: &str, parts: usize) -> GameEntry {
        let files = (1..=parts)
            .map(|i| ArchiveFile {
                file_name: format!("game.part{i}.rar"),
                size_bytes: 100,
                part_index: Some(i as u32),
            })
            .collect();
        GameEntry {
            relative_path: path.to_string(),
            group_key: "game".to_string(),
            game_type: GameType::Archive,
            display_name: path.rsplit('/').next().unwrap_or(path).to_string(),
            size_bytes: 100 * parts as u64,
            groups: vec![ArchiveGroup {
                base_name: "game".to_string(),
                files,
            }],
        }
    }

    /// ACC-02-30 / T13：重扫必须保留用户手填字段
    #[test]
    fn rescan_preserves_user_fields() {
        let (db, dir) = temp_db("preserve");
        let repo = seed_repo(&db);

        let (added, updated, missing) =
            apply_results(&db, repo, &[folder_entry("A", 100)], true).unwrap();
        assert_eq!((added, updated, missing), (1, 0, 0));

        // 用户改档
        db.with(|c| {
            c.execute(
                "UPDATE games SET display_name='我的游戏', game_code='RJ123456', cover_path='C:\\c.png' WHERE id=1",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        // 重扫：大小变化
        let (added, updated, missing) =
            apply_results(&db, repo, &[folder_entry("A", 200)], true).unwrap();
        assert_eq!((added, updated, missing), (0, 1, 0), "重扫应只更新不新增");

        let (name, code, cover, size): (String, Option<String>, Option<String>, i64) = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT display_name, game_code, cover_path, size_bytes FROM games WHERE id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?)
            })
            .unwrap();

        assert_eq!(name, "我的游戏", "用户改的名称必须保留");
        assert_eq!(code.as_deref(), Some("RJ123456"), "用户填的编号必须保留");
        assert_eq!(cover.as_deref(), Some("C:\\c.png"), "用户配的封面必须保留");
        assert_eq!(size, 200, "大小应刷新");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ACC-02-31 / T14：本次未扫到的条目 → 标记 missing，不删除
    #[test]
    fn missing_marked_not_deleted() {
        let (db, dir) = temp_db("missing");
        let repo = seed_repo(&db);

        apply_results(
            &db,
            repo,
            &[folder_entry("A", 10), folder_entry("B", 20)],
            true,
        )
        .unwrap();

        let (added, updated, missing) =
            apply_results(&db, repo, &[folder_entry("A", 10)], true).unwrap();
        assert_eq!((added, updated, missing), (0, 1, 1));

        let total: i64 = db
            .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM games", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(total, 2, "缺失条目不得被删除");

        let status: String = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT status FROM games WHERE relative_path='B'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(status, "missing");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ACC-02-32 / T15：缺失条目再次扫到 → 自动恢复
    #[test]
    fn missing_recovers_when_rescanned() {
        let (db, dir) = temp_db("recover");
        let repo = seed_repo(&db);

        apply_results(&db, repo, &[folder_entry("A", 10)], true).unwrap();
        apply_results(&db, repo, &[], true).unwrap(); // A 变 missing
        apply_results(&db, repo, &[folder_entry("A", 10)], true).unwrap(); // A 回来了

        let status: String = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT status FROM games WHERE relative_path='A'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(status, "ok");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ACC-02-33：压缩包型分卷清单正确落库，重扫不重复
    #[test]
    fn archive_files_written_and_replaced() {
        let (db, dir) = temp_db("files");
        let repo = seed_repo(&db);

        apply_results(&db, repo, &[archive_entry("G", 2)], true).unwrap();
        let n: i64 = db
            .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM game_files", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 2);

        // 分卷从 2 个变 3 个
        apply_results(&db, repo, &[archive_entry("G", 3)], true).unwrap();
        let n: i64 = db
            .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM game_files", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 3, "应先删后插，不得累积");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 续扫（mark_missing=false）不得把未处理条目标成缺失
    #[test]
    fn resume_does_not_mark_missing() {
        let (db, dir) = temp_db("resume");
        let repo = seed_repo(&db);

        apply_results(
            &db,
            repo,
            &[folder_entry("A", 10), folder_entry("B", 20)],
            true,
        )
        .unwrap();

        // 模拟断点续扫：只处理了 A，不应把 B 标 missing
        let (_, _, missing) =
            apply_results(&db, repo, &[folder_entry("A", 10)], false).unwrap();
        assert_eq!(missing, 0, "续扫不得标记缺失");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ACC-04-03 / ACC-04-04：扫描器不得自动填游戏编号与封面（即使目录名是编号）
    #[test]
    fn scan_never_fills_game_code_or_cover() {
        let (db, dir) = temp_db("code");
        let repo = seed_repo(&db);

        apply_results(&db, repo, &[folder_entry("RJ01005841", 10)], true).unwrap();

        let (code, cover): (Option<String>, Option<String>) = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT game_code, cover_path FROM games",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();

        assert!(code.is_none(), "游戏编号必须为空（禁止自动扫描）");
        assert!(cover.is_none(), "封面必须为空（禁止自动扫描）");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ACC-CON-01 / ACC-CON-02：schema 不得含 exe 字段、exe 表或版本字段（D19 / D20 护栏）
    #[test]
    fn schema_excludes_exe_and_version_fields() {
        let (db, dir) = temp_db("schema");

        db.with(|c| {
            let mut stmt = c.prepare("PRAGMA table_info(games)")?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            assert!(
                !cols.iter().any(|col| col.to_lowercase().contains("exe")),
                "games 表不得有 exe 字段（D19）: {cols:?}"
            );
            assert!(
                !cols.iter().any(|col| col.to_lowercase().contains("version")),
                "games 表不得有版本字段（D20）: {cols:?}"
            );

            let mut stmt = c.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
            let tables: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            assert!(
                !tables.iter().any(|t| t.to_lowercase().contains("exe")),
                "不得存在 exe 候选表（D19）: {tables:?}"
            );
            Ok(())
        })
        .unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
