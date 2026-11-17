/**
 * Tauri 后端 API 封装层（v2 契约）
 *
 * 每个函数对应一个 Rust Command，负责序列化参数、调用 invoke 并返回结果。
 * 前端组件只 import 这些函数，不直接接触 invoke / event 细节。
 *
 * 注意：在普通浏览器（无 Tauri 环境）中 import 本模块是安全的，
 * 只有实际调用后端命令时才会抛错（由 app.ts 统一捕获并 toast）。
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  ExecuteResult,
  PreviewItem,
  ProgressPayload,
  RenameMode,
  SortBy,
  UndoStatus,
} from "./types";

/** 弹出系统文件夹选择对话框，返回选中路径或 null */
export async function selectFolder(): Promise<string | null> {
  const selected = await open({ directory: true, multiple: false });
  return typeof selected === "string" ? selected : null;
}

/**
 * 调用 get_preview 命令：遍历文件、计算新文件名映射（纯计算，不修改磁盘）
 */
export async function getPreview(
  rootPath: string,
  recursive: boolean,
  mode: RenameMode,
  filterExts: string | null,
  sortBy: SortBy,
): Promise<PreviewItem[]> {
  return invoke<PreviewItem[]>("get_preview", {
    rootPath,
    recursive,
    mode,
    filterExts,
    sortBy,
  });
}

/**
 * 调用 execute_rename 命令：批量执行重命名
 *
 * 后端执行前会重新遍历并重算映射（不信任前端），只处理 ids 中当前状态为 ok 的项；
 * 执行过程中通过 "rename-progress" 事件推送进度。
 */
export async function executeRename(
  rootPath: string,
  recursive: boolean,
  mode: RenameMode,
  filterExts: string | null,
  sortBy: SortBy,
  ids: number[],
): Promise<ExecuteResult[]> {
  return invoke<ExecuteResult[]>("execute_rename", {
    rootPath,
    recursive,
    mode,
    filterExts,
    sortBy,
    ids,
  });
}

/** 调用 cancel_operation 命令：请求取消正在执行的任务 */
export async function cancelOperation(): Promise<void> {
  return invoke("cancel_operation");
}

/** 调用 undo_last_rename 命令：撤销最近一次批量重命名 */
export async function undoLastRename(): Promise<ExecuteResult[]> {
  return invoke<ExecuteResult[]>("undo_last_rename");
}

/** 调用 get_undo_status 命令：查询当前是否有可撤销记录 */
export async function getUndoStatus(): Promise<UndoStatus> {
  return invoke<UndoStatus>("get_undo_status");
}

/**
 * 监听后端推送的 rename-progress 事件
 * @param cb 回调函数，接收 { current, total }
 * @returns 取消监听的函数
 */
export function onProgress(
  cb: (payload: ProgressPayload) => void,
): Promise<UnlistenFn> {
  return listen<ProgressPayload>("rename-progress", (event) => cb(event.payload));
}
