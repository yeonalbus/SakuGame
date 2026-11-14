//! 应用全局状态。

use std::sync::{Arc, Mutex};

use crate::db::Db;
use crate::models::ScanProgress;
use crate::scanner::ScanControl;
use crate::transfer::TransferEngine;

pub struct AppState {
    pub db: Arc<Db>,
    /// 扫描槽：需要被扫描线程共享，故为 Arc<Mutex<_>>
    pub scan: Arc<Mutex<ScanSlot>>,
    /// 搬运机（v2）
    pub transfer: Arc<TransferEngine>,
}

impl AppState {
    pub fn new(db: Db) -> Self {
        let db = Arc::new(db);
        let transfer = Arc::new(TransferEngine::new(db.clone()));
        AppState {
            db,
            scan: Arc::new(Mutex::new(ScanSlot::default())),
            transfer,
        }
    }
}

#[derive(Default)]
pub struct ScanSlot {
    pub active: Option<ActiveScan>,
    pub next_job_id: u64,
}

pub struct ActiveScan {
    pub job_id: u64,
    pub repository_id: i64,
    pub control: ScanControl,
    /// 最近一次进度（暂停时用于落断点）
    pub last: Arc<Mutex<Option<ScanProgress>>>,
}

impl ScanSlot {
    pub fn alloc_job_id(&mut self) -> u64 {
        self.next_job_id += 1;
        self.next_job_id
    }
}
