/**
 * 前后端共享类型定义，与 Rust 端 models.rs **严格一致**（v2 契约）。
 *
 * - 所有字段名保持后端序列化后的 snake_case / camelCase 约定；
 * - `RenameMode` 是带 tag 的枚举：`{ type, params }`；
 * - 预览条目带 `reason`，用于展示冲突 / 无变化原因。
 */

/** 文件操作状态 */
export type Status = "ok" | "conflict" | "unchanged";

/** 文件排序依据（name 为自然排序，不是字典序） */
export type SortBy = "name" | "modified" | "created";

/** 重命名模式标识 */
export type ModeType = "find_replace" | "sequential" | "pattern" | "suffix";

/** 编号范围：每个目录独立 / 全局连续 */
export type NumberScope = "per_dir" | "global";

/** 主题 */
export type Theme = "dark" | "light";

/** 模式 A 参数：查找替换 */
export interface FindReplaceParams {
  find: string;
  replace: string;
  use_regex: boolean;
  case_sensitive: boolean;
  whole_word: boolean;
  first_only: boolean;
  include_extension: boolean;
}

/** 模式 B 参数：序号命名 */
export interface SequentialParams {
  prefix: string;
  separator: string;
  start: number;
  step: number;
  /** 0 = 自动按编号位数补零 */
  pad_width: number;
  suffix: string;
  scope: NumberScope;
}

/** 模式 C 参数：模板命名 */
export interface PatternParams {
  template: string;
  start: number;
  step: number;
  pad_width: number;
  scope: NumberScope;
}

/** 模式 D 参数：统一后缀 */
export interface SuffixParams {
  target_suffix: string;
  force_all: boolean;
  lowercase: boolean;
}

/** 发送给后端的重命名模式（带 tag 的联合类型） */
export type RenameMode =
  | { type: "find_replace"; params: FindReplaceParams }
  | { type: "sequential"; params: SequentialParams }
  | { type: "pattern"; params: PatternParams }
  | { type: "suffix"; params: SuffixParams };

/** 预览结果条目 */
export interface PreviewItem {
  /** 本次遍历内稳定唯一 ID（按遍历顺序 0..n） */
  id: number;
  /** 相对父目录路径，如 "assets/icons/"；根目录为空串 */
  relative_path: string;
  old_name: string;
  new_name: string;
  status: Status;
  /** ok 时为空；conflict/unchanged 时为中文原因 */
  reason: string;
}

/** 执行结果（每条文件一个） */
export interface ExecuteResult {
  /** 对应预览项 ID */
  id: number;
  success: boolean;
  message: string;
}

/** 撤销状态 */
export interface UndoStatus {
  available: boolean;
  count: number;
}

/** 重命名进度事件载荷 */
export interface ProgressPayload {
  current: number;
  total: number;
}

/** 日志级别 */
export type LogKind = "info" | "success" | "error";

/** 日志条目（底部日志面板用） */
export interface LogEntry {
  /** 时间戳（HH:MM:SS） */
  time: string;
  text: string;
  /** 兼容旧字段：成功/失败 */
  ok: boolean;
  kind: LogKind;
}
