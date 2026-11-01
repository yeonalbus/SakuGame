//! 真实仓库扫描验收（ACC-02-08 的自动化部分）。
//!
//! 默认被 #[ignore] 跳过；手动运行：
//! ```text
//! cargo test --test real_repo_scan -- --ignored --nocapture
//! ```
//! 可用环境变量覆盖仓库路径：`SAKUGAME_REPO=E:\SomeLibrary`

use std::path::Path;
use std::time::Instant;

use sakugame_lib::models::GameType;
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

    let folder = r
        .games
        .iter()
        .filter(|g| g.game_type == GameType::Folder)
        .count();
    let archive = r.games.len() - folder;

    println!("=== 真实仓库扫描结果 ===");
    println!("仓库路径: {root}");
    println!("耗时: {:?}", start.elapsed());
    println!("档案总数: {}（folder={folder}, archive={archive}）", r.games.len());
    println!("警告数: {}", r.warnings);
    println!("取消: {}", r.cancelled);
    println!("--- 前 15 条 ---");
    for g in r.games.iter().take(15) {
        println!(
            "  {:<24} {:?}  {:.1} MB  {}",
            g.display_name,
            g.game_type,
            g.size_bytes as f64 / 1_048_576.0,
            g.relative_path
        );
    }

    assert!(!r.cancelled, "不应被取消");
    assert!(!r.games.is_empty(), "应至少产出档案");
}
