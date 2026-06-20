/**
 * Tauri 后端 API 封装层
 *
 * 每个函数对应一个 Rust Command，负责序列化参数、调用 invoke 并返回结果。
 * 前端组件只需 import 这些函数，不直接接触 invoke 细节。
 */

import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  RenameMode,
  SortBy,
  PreviewItem,
  ExecuteResult,
  ProgressPayload,
} from "./types";

/** 弹出系统文件夹选择对话框，返回选中路径或 null */
export async function selectFolder(): Promise<string | null> {
  const selected = await open({ directory: true, multiple: false });
  return typeof selected === "string" ? selected : null;
}

/**
 * 调用 get_preview 命令：遍历文件、计算新文件名映射
 * 纯计算，不修改磁盘
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
 * 后端执行前会重新计算预览（防篡改），并通过事件推送进度
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

/** 调用 cancel_operation 命令：取消正在执行的任务 */
export async function cancelOperation(): Promise<void> {
  return invoke("cancel_operation");
}

/**
 * 监听后端推送的 rename-progress 事件
 * @param cb 回调函数，接收 { current, total }
 * @returns 取消监听的函数
 */
export function onProgress(cb: (p: ProgressPayload) => void): Promise<UnlistenFn> {
  return listen<ProgressPayload>("rename-progress", (e) => cb(e.payload));
}
