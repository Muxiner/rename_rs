//! 应用全局状态

use std::sync::{atomic::AtomicBool, Arc, Mutex};

use crate::models::UndoEntry;

/// 取消令牌，用 Arc<AtomicBool> 在线程间安全共享
pub struct CancelFlag(pub Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl Default for CancelFlag {
    fn default() -> Self {
        Self::new()
    }
}

/// 撤销历史：只保留最近一次成功批次的迁移记录
pub struct RenameHistory(pub Mutex<Option<Vec<UndoEntry>>>);

impl RenameHistory {
    pub fn new() -> Self {
        Self(Mutex::new(None))
    }
}

impl Default for RenameHistory {
    fn default() -> Self {
        Self::new()
    }
}
