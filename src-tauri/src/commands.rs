//! Tauri 命令 —— 对外暴露的三个异步接口

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter};

use crate::engine;
use crate::models::*;
use crate::state::CancelFlag;

/// 命令1：预览变更（纯计算，不碰磁盘）
///
/// 在 spawn_blocking 中完成文件遍历和名称计算，
/// 避免阻塞主线程导致 UI 冻结。
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

    // 解析扩展名白名单：逗号分隔，去前导点号，转小写
    let filter: Option<HashSet<String>> = parse_filter(&filter_exts);

    // 文件 I/O 通过 spawn_blocking 发送到阻塞线程池
    let preview = tokio::task::spawn_blocking(move || {
        let root = Path::new(&root_path_clone);
        if !root.is_dir() {
            return Err(RenamerError::DirNotFound(root_path_clone));
        }
        let entries = engine::collect_files(root, recursive, &filter, &sort_by)?;
        engine::compute_preview(&entries, &mode_clone, root)
    })
    .await
    .map_err(|_| RenamerError::Cancelled)??;

    Ok(preview)
}

/// 命令2：执行重命名（批量操作、实时进度、支持取消）
///
/// 关键设计：
/// - 执行前重新计算预览映射，不信任前端传入的文件名
/// - 仅处理用户勾选且状态为 ok 的文件
/// - 单个文件失败不中止整体任务
/// - 通过 emit "rename-progress" 事件推送实时进度
/// - 支持 CancellationToken（Arc<AtomicBool>）中途取消
#[tauri::command]
pub async fn execute_rename(
    app: AppHandle,
    cancel: tauri::State<'_, CancelFlag>,
    root_path: String,
    recursive: bool,
    mode: RenameMode,
    filter_exts: Option<String>,
    sort_by: SortBy,
    ids: Vec<usize>,
) -> Result<Vec<ExecuteResult>, RenamerError> {
    // 重置取消标志
    cancel.0.store(false, Ordering::SeqCst);

    let mode_clone = mode.clone();
    let root_path_clone = root_path.clone();
    let cancel_flag = cancel.0.clone();
    let ids_set: HashSet<usize> = ids.into_iter().collect();

    let filter: Option<HashSet<String>> = parse_filter(&filter_exts);

    let results = tokio::task::spawn_blocking(move || {
        let root = Path::new(&root_path_clone);
        if !root.is_dir() {
            return Err(RenamerError::DirNotFound(root_path_clone));
        }
        // 重新计算预览（防止前端数据被篡改或文件系统变更）
        let entries = engine::collect_files(root, recursive, &filter, &sort_by)?;
        let preview = engine::compute_preview(&entries, &mode_clone, root)?;

        let mut results: Vec<ExecuteResult> = Vec::new();
        let total = ids_set.len();

        for item in &preview {
            // 轮询取消标志
            if cancel_flag.load(Ordering::SeqCst) {
                return Err(RenamerError::Cancelled);
            }

            // 仅处理用户勾选的 ID
            if !ids_set.contains(&item.id) {
                continue;
            }
            // 跳过非 ok 状态的文件
            if item.status != Status::Ok {
                results.push(ExecuteResult {
                    id: item.id,
                    success: false,
                    message: format!("跳过: 状态为 {:?}", item.status),
                });
                continue;
            }

            let old_path = Path::new(&root_path_clone)
                .join(&item.relative_path)
                .join(&item.old_name);
            let new_path = Path::new(&root_path_clone)
                .join(&item.relative_path)
                .join(&item.new_name);

            // 执行文件系统重命名
            match std::fs::rename(&old_path, &new_path) {
                Ok(()) => {
                    results.push(ExecuteResult {
                        id: item.id,
                        success: true,
                        message: format!("{} ✓", item.new_name),
                    });
                }
                Err(e) => {
                    // 细粒度错误处理：PermissionDenied / AlreadyExists 分别提供中文消息
                    let msg = match e.kind() {
                        std::io::ErrorKind::PermissionDenied => {
                            format!("权限不足: {}", e)
                        }
                        std::io::ErrorKind::AlreadyExists => {
                            format!("目标已存在: {}", e)
                        }
                        _ => format!("重命名失败: {}", e),
                    };
                    results.push(ExecuteResult {
                        id: item.id,
                        success: false,
                        message: msg,
                    });
                }
            }

            // 推送实时进度事件
            let _ = app.emit(
                "rename-progress",
                ProgressPayload {
                    current: results.len(),
                    total,
                },
            );
        }

        Ok(results)
    })
    .await
    .map_err(|_| RenamerError::Cancelled)??;

    Ok(results)
}

/// 命令3：取消当前正在执行的重命名操作
#[tauri::command]
pub fn cancel_operation(cancel: tauri::State<'_, CancelFlag>) {
    cancel.0.store(true, Ordering::SeqCst);
}

/// 解析扩展名过滤字符串为 HashSet（逗号分隔、去点号、小写）
fn parse_filter(filter_exts: &Option<String>) -> Option<HashSet<String>> {
    filter_exts
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().trim_start_matches('.').to_lowercase())
                .filter(|x| !x.is_empty())
                .collect()
        })
}
