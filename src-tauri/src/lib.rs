//! 批量重命名工具 — Tauri v2 后端核心
//!
//! 提供五个 Tauri Command：
//! - get_preview: 纯计算新旧文件名映射，不修改磁盘
//! - execute_rename: 批量执行重命名，支持取消、实时进度推送、两阶段改名
//! - cancel_operation: 取消正在执行的任务
//! - undo_last_rename: 撤销最近一次成功批次
//! - get_undo_status: 查询撤销是否可用
//!
//! 架构约定：
//! - 所有文件 I/O 必须通过 tokio::task::spawn_blocking 放到阻塞线程池
//! - 全项目零 unwrap()/expect()，所有错误通过 Result + ? 传播
//! - execute_rename 阶段重新计算映射，不信任前端传入的文件名
//!
//! 模块划分：
//! - models:   数据结构与枚举定义
//! - engine:   文件遍历、名称计算、冲突检测、执行与撤销（纯逻辑，无 Tauri 依赖）
//! - commands: Tauri 异步命令（对前端暴露的接口）
//! - state:    全局共享状态（取消令牌、撤销历史）

// engine / models 同时供 GUI 与命令行工具复用（CLI 入口见 src/bin/cli.rs）
mod commands;
pub mod engine;
pub mod models;
mod state;

use crate::state::{CancelFlag, RenameHistory};

/// Tauri 应用启动：注册插件、状态、命令处理器
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let result = tauri::Builder::default()
        // 注册系统对话框插件（文件夹选择）
        .plugin(tauri_plugin_dialog::init())
        // 注册全局状态：取消标志 + 撤销历史
        .manage(CancelFlag::new())
        .manage(RenameHistory::new())
        // 注册五个 Tauri 命令
        .invoke_handler(tauri::generate_handler![
            commands::get_preview,
            commands::execute_rename,
            commands::cancel_operation,
            commands::undo_last_rename,
            commands::get_undo_status,
        ])
        .run(tauri::generate_context!());

    // 禁止 expect：启动失败时记录到 stderr，不 panic
    if let Err(error) = result {
        eprintln!("启动应用失败: {error}");
    }
}
