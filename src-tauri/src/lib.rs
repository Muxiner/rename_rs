//! 批量重命名工具 — Tauri v2 后端核心
//!
//! 提供三个 Tauri Command：
//! - get_preview: 纯计算新旧文件名映射，不修改磁盘
//! - execute_rename: 批量执行重命名，支持取消、实时进度推送
//! - cancel_operation: 取消正在执行的任务
//!
//! 架构铁律：
//! - 所有文件 I/O 必须通过 tokio::task::spawn_blocking 放到阻塞线程池
//! - 全项目零 unwrap()，所有错误通过 Result + ? 传播
//! - execute_rename 阶段重新计算映射，不信任前端传入的文件名
//!
//! 模块划分：
//! - models:   数据结构与枚举定义
//! - engine:   文件遍历、名称计算、冲突检测（纯逻辑，无 Tauri 依赖）
//! - commands: Tauri 异步命令（对前端暴露的接口）
//! - state:    全局共享状态（取消令牌）

mod commands;
mod engine;
mod models;
mod state;

use std::sync::{atomic::AtomicBool, Arc};

use crate::state::CancelFlag;

/// Tauri 应用启动：注册插件、状态、命令处理器
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 注册系统对话框插件（文件夹选择）
        .plugin(tauri_plugin_dialog::init())
        // 注册全局取消标志（Arc<AtomicBool>，线程安全）
        .manage(CancelFlag(Arc::new(AtomicBool::new(false))))
        // 注册三个 Tauri 命令
        .invoke_handler(tauri::generate_handler![
            commands::get_preview,
            commands::execute_rename,
            commands::cancel_operation,
        ])
        .run(tauri::generate_context!())
        .expect("启动应用失败");
}
