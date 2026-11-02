//! 真实仓库扫描验收（v1.1：两层枚举）。
//!
//! 默认被 #[ignore] 跳过；手动运行：
//! ```text
//! cargo test --test real_repo_scan -- --ignored --nocapture
//! ```
//! 可用环境变量覆盖仓库路径：`SAKUGAME_REPO=E:\SomeLibrary`

use std::path::Path;
use std::time::Instant;

use sakugame_lib::models::{GameLayout, GameType};
use sakugame_lib::scanner::{scan_repository, ScanConfig, ScanControl};

#[test]
#[ignore = "需要真实仓库；手动运行 cargo test --test real_repo_scan -- --ignored --nocapture"]
fn scan_real_repository() {
    let root = std::env::var("SAKUGAME_REPO").unwrap_or_else(|_| "Z:\\Game".to_string());
    if !Path::new(&root).exists() {
        eprintln!("跳过：仓库不存在 {root}");
        return;
    }

    let cfg = ScanConfig::default();
    let ctl = ScanControl::new();
    let start = Instant::now();
    let mut last_bucket = 0u32;

    let r = scan_repository(Path::new(&root), &cfg, &ctl, 0, |ev| {
        let bucket = ev.done / 40;
        if bucket != last_bucket {
            last_bucket = bucket;
            eprintln!("进度 {}/{}  已发现 {}", ev.done, ev.total, ev.found);
        }
    });

    let games = &r.report.games;
    let folder_versions = games
        .iter()
        .flat_map(|g| g.versions.iter())
        .filter(|v| v.game_type == GameType::Folder)
        .count();
    let archive_versions = games
        .iter()
        .flat_map(|g| g.versions.iter())
        .filter(|v| v.game_type == GameType::Archive)
        .count();
    let compact = games
        .iter()
        .filter(|g| g.layout == GameLayout::Compact)
        .count();
    let standard = games.len() - compact;
    let zero_version = games.iter().filter(|g| g.versions.is_empty()).count();

    println!("=== 真实仓库扫描结果（v1.1 两层枚举）===");
    println!("仓库路径: {root}");
    println!("耗时: {:?}", start.elapsed());
    println!(
        "档案总数: {}（紧凑={compact}, 标准={standard}, 其中 0 版本={zero_version}）",
        games.len()
    );
    println!("版本总数: {}（folder={folder_versions}, archive={archive_versions}）", folder_versions + archive_versions);
    println!(
        "告警: 根下散落文件={}, 截断={}, 枚举失败={}",
        r.report.warnings.root_loose_files,
        r.report.warnings.truncated_with_version_dirs,
        r.report.warnings.enum_failed
    );
    println!("取消: {}", r.cancelled);
    println!("--- 前 15 条 ---");
    for g in games.iter().take(15) {
        let types: Vec<&str> = g.versions.iter().map(|v| v.game_type.as_str()).collect();
        println!(
            "  {:<24} {:<8} {:.1} MB  {} 版本={:?}",
            g.display_name,
            g.layout.as_str(),
            g.size_bytes as f64 / 1_048_576.0,
            g.relative_path,
            types
        );
    }

    assert!(!r.cancelled, "不应被取消");
    assert!(!games.is_empty(), "应至少产出档案");
}
