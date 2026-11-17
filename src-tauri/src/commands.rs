//! Tauri 命令 —— 对前端暴露的五个接口
//!
//! - `get_preview`：纯计算新旧文件名映射，不修改磁盘
//! - `execute_rename`：执行前重新遍历 + 重算映射，只处理勾选且当前状态 ok 的项
//! - `cancel_operation`：设置取消标志，执行线程轮询后中止
//! - `undo_last_rename`：逆序撤销最近一次成功批次
//! - `get_undo_status`：查询撤销是否可用
//!
//! 所有文件 I/O 都放在 `tokio::task::spawn_blocking` 的阻塞线程池中，
//! 避免阻塞 UI 主线程。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter};

use crate::engine;
use crate::models::*;
use crate::state::{CancelFlag, RenameHistory};

/// 命令1：预览变更（纯计算，不碰磁盘）
#[tauri::command]
pub async fn get_preview(
    root_path: String,
    recursive: bool,
    mode: RenameMode,
    filter_exts: Option<String>,
    sort_by: SortBy,
) -> Result<Vec<PreviewItem>, RenamerError> {
    let mode_clone = mode.clone();
    let root_path_clone = root_path.clone();
    let filter = parse_filter(&filter_exts);

    let preview = tokio::task::spawn_blocking(move || {
        let root = Path::new(&root_path_clone);
        if !root.is_dir() {
            return Err(RenamerError::DirNotFound(root_path_clone.clone()));
        }
        let entries = engine::collect_files(root, recursive, &filter, &sort_by)?;
        engine::compute_preview(&entries, &mode_clone, root)
    })
    .await
    .map_err(|error| RenamerError::ReadDirFailed(format!("后台任务异常: {error}")))??;

    Ok(preview)
}

/// 命令2：执行重命名（批量操作、实时进度、支持取消）
///
/// 关键设计：
/// - 执行前重新计算预览映射，不信任前端传入的文件名
/// - 仅处理用户勾选且当前状态为 ok 的文件；`ids` 去重
/// - 单个文件失败不中止整体任务
/// - 通过 emit `rename-progress` 事件推送实时进度（total = 去重后 ids 数）
/// - 支持 `AtomicBool` 取消：取消时统一返回 `RenamerError::Cancelled`
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn execute_rename(
    app: AppHandle,
    cancel: tauri::State<'_, CancelFlag>,
    history: tauri::State<'_, RenameHistory>,
    root_path: String,
    recursive: bool,
    mode: RenameMode,
    filter_exts: Option<String>,
    sort_by: SortBy,
    ids: Vec<usize>,
) -> Result<Vec<ExecuteResult>, RenamerError> {
    // 重置取消标志（上一次操作的取消不应影响本次）
    cancel.0.store(false, Ordering::SeqCst);
    let cancel_flag = cancel.0.clone();

    // ids 去重且保持前端传入顺序
    let mut requested: Vec<usize> = Vec::with_capacity(ids.len());
    let mut seen: HashSet<usize> = HashSet::new();
    for id in ids {
        if seen.insert(id) {
            requested.push(id);
        }
    }

    let mode_clone = mode.clone();
    let root_path_clone = root_path.clone();
    let filter = parse_filter(&filter_exts);
    let app_handle = app.clone();

    let outcome = tokio::task::spawn_blocking(move || {
        let root = Path::new(&root_path_clone);
        if !root.is_dir() {
            return Err(RenamerError::DirNotFound(root_path_clone.clone()));
        }

        // 执行前重新遍历 + 重算（文件系统可能已变化，不能信任前端数据）
        let entries = engine::collect_files(root, recursive, &filter, &sort_by)?;
        let preview = engine::compute_preview(&entries, &mode_clone, root)?;
        let by_id: HashMap<usize, &PreviewItem> =
            preview.iter().map(|item| (item.id, item)).collect();

        let total = requested.len();
        let mut results: Vec<ExecuteResult> = Vec::new();
        let mut jobs: Vec<engine::MoveJob> = Vec::new();

        for id in &requested {
            match by_id.get(id) {
                None => results.push(ExecuteResult {
                    id: *id,
                    success: false,
                    message: "跳过: 预览项不存在（文件可能已被移动）".to_string(),
                }),
                Some(item) if item.status != Status::Ok => {
                    let reason = if item.reason.is_empty() {
                        item.status.label().to_string()
                    } else {
                        item.reason.clone()
                    };
                    results.push(ExecuteResult {
                        id: *id,
                        success: false,
                        message: format!("跳过: {reason}"),
                    });
                }
                Some(item) => {
                    let old_path = root.join(&item.relative_path).join(&item.old_name);
                    let new_path = root.join(&item.relative_path).join(&item.new_name);
                    jobs.push(engine::MoveJob {
                        id: item.id,
                        old_path,
                        new_path,
                    });
                }
            }
        }

        // 被跳过的项也计入进度，保证进度条从 1 走到 total
        let skipped = results.len();
        for step in 1..=skipped {
            let _ = app_handle.emit(
                "rename-progress",
                ProgressPayload {
                    current: step,
                    total,
                },
            );
        }

        let base = skipped;
        let app_for_progress = app_handle.clone();
        let mut move_outcome = engine::execute_renames(&jobs, &cancel_flag, |processed| {
            let _ = app_for_progress.emit(
                "rename-progress",
                ProgressPayload {
                    current: base + processed,
                    total,
                },
            );
        })?;
        // 跳过项的失败结果放在前面，保持与用户勾选顺序一致
        let mut all_results = results;
        all_results.append(&mut move_outcome.results);
        move_outcome.results = all_results;

        Ok::<_, RenamerError>(move_outcome)
    })
    .await
    .map_err(|error| RenamerError::ReadDirFailed(format!("后台任务异常: {error}")))??;

    // 成功项写入撤销历史（只保留最近一次批次，覆盖旧的；无成功项时保留旧历史）。
    // 取消场景也写入：已完成的改名必须可撤销。
    if !outcome.undo.is_empty() {
        if let Ok(mut guard) = history.0.lock() {
            *guard = Some(outcome.undo);
        }
    }

    if outcome.cancelled {
        return Err(RenamerError::Cancelled);
    }

    Ok(outcome.results)
}

/// 命令3：取消当前正在执行的重命名操作
#[tauri::command]
pub fn cancel_operation(cancel: tauri::State<'_, CancelFlag>) {
    cancel.0.store(true, Ordering::SeqCst);
}

/// 命令4：撤销最近一次成功批次
///
/// 取出历史后清空；按逆序把 `to_path` 改回 `from_path`，
/// 若与其它待撤销项交叉则自动走两阶段临时名。
#[tauri::command]
pub async fn undo_last_rename(
    history: tauri::State<'_, RenameHistory>,
) -> Result<Vec<ExecuteResult>, RenamerError> {
    let entries = match history.0.lock() {
        Ok(mut guard) => guard.take(),
        Err(error) => {
            return Err(RenamerError::ReadDirFailed(format!(
                "撤销历史状态异常: {error}"
            )))
        }
    };
    let entries = match entries {
        Some(entries) if !entries.is_empty() => entries,
        _ => return Err(RenamerError::NoUndo),
    };

    let results = tokio::task::spawn_blocking(move || engine::undo_renames(&entries))
        .await
        .map_err(|error| RenamerError::ReadDirFailed(format!("后台任务异常: {error}")))?;

    Ok(results)
}

/// 命令5：查询撤销状态（启动时前端用于初始化按钮）
#[tauri::command]
pub fn get_undo_status(history: tauri::State<'_, RenameHistory>) -> UndoStatus {
    match history.0.lock() {
        Ok(guard) => {
            let count = guard
                .as_ref()
                .map(|entries| entries.len())
                .unwrap_or_default();
            UndoStatus {
                available: count > 0,
                count,
            }
        }
        Err(_) => UndoStatus {
            available: false,
            count: 0,
        },
    }
}

/// 解析扩展名过滤字符串为 HashSet（逗号分隔、去点号、小写；空白视为不过滤）
fn parse_filter(filter_exts: &Option<String>) -> Option<HashSet<String>> {
    filter_exts
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .split(',')
                .map(|ext| ext.trim().trim_start_matches('.').to_lowercase())
                .filter(|ext| !ext.is_empty())
                .collect()
        })
}
