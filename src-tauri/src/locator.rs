//! 仓库路径定位（D16）：盘符 / IP / 主机名三定位项，按**可达性**依次探测。
//!
//! 关键点（规格 §12.2）：
//! - 不能只看"盘符是否存在"——映射残留但连接已断时需跳过
//! - 主机名档在中文主机名环境下可能不可用（实测），必须能优雅跳过

use std::path::Path;

use crate::models::{LocatorKind, RepositoryLocator};
use crate::scanner::extend_path;

/// 实际可达性探测：尝试打开目录并读首个条目。
/// 不用 Path::exists()——对网络盘它可能返回缓存结果。
pub fn probe_path(path: &str) -> bool {
    let p = extend_path(Path::new(path));
    match std::fs::read_dir(&p) {
        Ok(mut it) => {
            let _ = it.next();
            true
        }
        Err(_) => false,
    }
}

/// 按 盘符 → IP → 主机名 的顺序探测，返回首个可达项
pub fn resolve(locators: &[RepositoryLocator]) -> Option<(LocatorKind, String)> {
    let mut sorted: Vec<&RepositoryLocator> = locators.iter().collect();
    sorted.sort_by_key(|l| {
        LocatorKind::parse(&l.kind)
            .map(|k| k.priority())
            .unwrap_or(9)
    });

    for loc in sorted {
        if probe_path(&loc.path_template) {
            let kind = LocatorKind::parse(&loc.kind).unwrap_or(LocatorKind::Drive);
            return Some((kind, loc.path_template.clone()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_existing_dir() {
        let dir = std::env::temp_dir();
        assert!(probe_path(&dir.to_string_lossy()));
    }

    #[test]
    fn probe_missing_dir() {
        assert!(!probe_path("Q:\\definitely-not-exists\\path"));
    }

    #[test]
    fn resolve_prefers_drive_order() {
        let locators = vec![
            RepositoryLocator {
                kind: "host".into(),
                path_template: "Q:\\nope".into(),
                last_ok_at: None,
            },
            RepositoryLocator {
                kind: "drive".into(),
                path_template: std::env::temp_dir().to_string_lossy().to_string(),
                last_ok_at: None,
            },
        ];
        let got = resolve(&locators).expect("应命中盘符项");
        assert_eq!(got.0, LocatorKind::Drive);
    }
}
