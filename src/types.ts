// 前后端共享类型定义，与 Rust 端 models.rs 一一对应

/** 文件操作状态 */
export type Status = "ok" | "conflict" | "unchanged";

/** 文件排序依据 */
export type SortBy = "name" | "modified";

/** 重命名模式标识 */
export type ModeType = "find_replace" | "sequential" | "suffix";

/** 重命名模式（发送给后端的格式） */
export interface RenameMode {
  type: ModeType;
  params: FindReplaceParams | SequentialParams | SuffixParams;
}

/** 模式A 参数：查找替换 */
export interface FindReplaceParams {
  find: string;       // 查找文本或正则
  replace: string;    // 替换文本
  use_regex: boolean; // 是否启用正则
}

/** 模式B 参数：序号命名 */
export interface SequentialParams {
  prefix: string;     // 前缀
  separator: string;  // 前缀与数字间的连接符
  start: number;      // 起始编号
}

/** 模式C 参数：统一后缀 */
export interface SuffixParams {
  target_suffix: string; // 目标后缀（不带点号）
  force_all: boolean;    // 是否强制转换所有扩展名
  lowercase: boolean;    // 是否转小写
}

/** 预览结果条目 */
export interface PreviewItem {
  id: number;            // 文件唯一 ID
  relative_path: string; // 相对目录路径（如 "assets/icons/"）
  old_name: string;      // 原始文件名
  new_name: string;      // 新文件名
  status: Status;        // 操作状态
}

/** 执行结果（每条文件一个） */
export interface ExecuteResult {
  id: number;       // 对应预览项 ID
  success: boolean; // 是否成功
  message: string;  // 成功信息或失败原因
}

/** 重命名进度事件载荷 */
export interface ProgressPayload {
  current: number; // 当前已处理数
  total: number;   // 总数量
}

/** 日志条目（底部日志面板用） */
export interface LogEntry {
  time: string;  // 时间戳（HH:MM:SS）
  text: string;  // 日志内容
  ok: boolean;   // 成功/失败
}
