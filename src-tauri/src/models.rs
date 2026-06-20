//! 数据模型 —— 所有结构体、枚举及其默认值

use serde::{Deserialize, Serialize};

/// 文件操作状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Status {
    /// 正常，可执行重命名
    #[serde(rename = "ok")]
    Ok,
    /// 冲突（目标文件名已存在或与其他项重复）
    #[serde(rename = "conflict")]
    Conflict,
    /// 无变化（新旧文件名相同）
    #[serde(rename = "unchanged")]
    Unchanged,
}

/// 文件排序方式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SortBy {
    /// 按文件名排序
    #[serde(rename = "name")]
    Name,
    /// 按修改时间排序
    #[serde(rename = "modified")]
    Modified,
}

/// 重命名模式（三种 Tab：查找替换 / 序号命名 / 统一后缀）
///
/// JSON 序列化格式: { "type": "...", "params": {...} }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "params")]
pub enum RenameMode {
    /// 模式A：查找替换（支持普通文本和正则）
    #[serde(rename = "find_replace")]
    FindReplace {
        /// 要查找的文本（或正则表达式）
        find: String,
        /// 替换为的文本
        replace: String,
        /// 是否启用正则匹配
        #[serde(default)]
        use_regex: bool,
    },
    /// 模式B：序号命名（各组独立编号、自动补零）
    #[serde(rename = "sequential")]
    Sequential {
        /// 文件名前缀，默认 "file"
        #[serde(default = "default_prefix")]
        prefix: String,
        /// 前缀与数字之间的连接符，默认 "_"
        #[serde(default = "default_separator")]
        separator: String,
        /// 起始编号，默认 1
        #[serde(default = "default_start")]
        start: u64,
    },
    /// 模式C：统一后缀（修改文件扩展名）
    #[serde(rename = "suffix")]
    Suffix {
        /// 目标扩展名（不带点号）
        target_suffix: String,
        /// 是否强制转换所有文件的扩展名
        #[serde(default = "default_true")]
        force_all: bool,
        /// 是否将目标后缀转为小写
        #[serde(default = "default_true")]
        lowercase: bool,
    },
}

/// 预览结果条目，返回给前端表格展示
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewItem {
    /// 文件唯一 ID（执行时用 ID 而非文件名防止篡改）
    pub id: usize,
    /// 相对于根目录的路径（如 "assets/icons/"）
    pub relative_path: String,
    /// 原始完整文件名
    pub old_name: String,
    /// 新完整文件名
    pub new_name: String,
    /// 操作状态：ok / conflict / unchanged
    pub status: Status,
}

/// 执行结果，每个文件返回一条
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResult {
    /// 对应预览项 ID
    pub id: usize,
    /// 是否成功
    pub success: bool,
    /// 成功信息或失败原因
    pub message: String,
}

/// 重命名进度事件载荷，通过 Tauri emit 推送给前端
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressPayload {
    /// 当前已处理数量
    pub current: usize,
    /// 总数量
    pub total: usize,
}

/// 自定义错误类型
///
/// 同时 derive Serialize，利用 Tauri 的 blanket impl
/// `impl<T: Serialize> From<T> for InvokeError` 自动向前端透传
#[derive(Debug, thiserror::Error, Serialize)]
pub enum RenamerError {
    #[error("目录不存在: {0}")]
    DirNotFound(String),
    #[error("读取目录失败: {0}")]
    ReadDirFailed(String),
    #[error("无效正则表达式: {0}")]
    InvalidRegex(String),
    #[error("操作已取消")]
    Cancelled,
}

// ─── 默认值辅助函数 ─────────────────────────────────────

pub fn default_prefix() -> String {
    "Pic".to_string()   
}
pub fn default_separator() -> String {
    "_".to_string()
}
pub fn default_start() -> u64 {
    1
}
pub fn default_true() -> bool {
    true
}
