//! 应用全局状态

use std::sync::{
    atomic::AtomicBool,
    Arc,
};

/// 取消令牌，用 Arc<AtomicBool> 在线程间安全共享
pub struct CancelFlag(pub Arc<AtomicBool>);
