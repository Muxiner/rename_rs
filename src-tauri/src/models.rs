//! 数据模型 —— 所有结构体、枚举及其默认值
//!
//! 本文件的 serde 格式是前后端契约（见 CONTRACT.md「一、后端数据契约」），
//! 修改字段名/JSON 形状必须同步前端 `src/types.ts`。

use serde::{Deserialize, Serialize};

/// 文件操作状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// 正常，可执行重命名
    #[serde(rename = "ok")]
    Ok,
    /// 冲突（目标文件名已存在、非法名或与其他项重复）
    #[serde(rename = "conflict")]
    Conflict,
    /// 无变化（新旧文件名相同）
    #[serde(rename = "unchanged")]
    Unchanged,
}

impl Status {
    /// 面向用户的中文标签（执行结果里的「跳过」提示使用）
    pub fn label(&self) -> &'static str {
        match self {
            Status::Ok => "可执行",
            Status::Conflict => "冲突",
            Status::Unchanged => "无变化",
        }
    }
}

/// 文件排序方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SortBy {
    /// 按文件名自然排序（natural sort，`img2 < img10`）
    #[default]
    #[serde(rename = "name")]
    Name,
    /// 按修改时间排序（升序）
    #[serde(rename = "modified")]
    Modified,
    /// 按创建时间排序（升序，取不到时回退修改时间）
    #[serde(rename = "created")]
    Created,
}

/// 序号编号范围
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NumberScope {
    /// 每个相对目录独立编号
    #[default]
    #[serde(rename = "per_dir")]
    PerDir,
    /// 全部文件按遍历顺序连续编号
    #[serde(rename = "global")]
    Global,
}

/// 重命名模式（四种模式）
///
/// JSON 序列化格式: `{ "type": "...", "params": {...} }`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "params")]
pub enum RenameMode {
    /// 模式A：查找替换（支持普通文本和正则）
    #[serde(rename = "find_replace")]
    FindReplace {
        /// 要查找的文本（或正则表达式）
        #[serde(default)]
        find: String,
        /// 替换为的文本
        #[serde(default)]
        replace: String,
        /// 是否启用正则匹配
        #[serde(default)]
        use_regex: bool,
        /// 是否区分大小写（默认 true）
        #[serde(default = "default_true")]
        case_sensitive: bool,
        /// 是否全词匹配
        #[serde(default)]
        whole_word: bool,
        /// 是否只替换第一处
        #[serde(default)]
        first_only: bool,
        /// 是否作用于完整文件名（含扩展名）
        #[serde(default)]
        include_extension: bool,
    },
    /// 模式B：序号命名
    #[serde(rename = "sequential")]
    Sequential {
        /// 文件名前缀，默认 "Pic"
        #[serde(default = "default_prefix")]
        prefix: String,
        /// 前缀与数字之间的连接符，默认 "_"
        #[serde(default = "default_separator")]
        separator: String,
        /// 起始编号，默认 1
        #[serde(default = "default_start")]
        start: u64,
        /// 步长，默认 1
        #[serde(default = "default_step")]
        step: u64,
        /// 补零位数，0 = 自动按编号范围补零
        #[serde(default)]
        pad_width: usize,
        /// 数字后的后缀文本
        #[serde(default)]
        suffix: String,
        /// 编号范围：每个目录独立 / 全局连续
        #[serde(default)]
        scope: NumberScope,
    },
    /// 模式C：模板命名
    #[serde(rename = "pattern")]
    Pattern {
        /// 模板，支持 {name}/{original}/{ext}/{index}/{date}/{time}/{parent}，{{ }} 转义
        #[serde(default)]
        template: String,
        /// 起始编号，默认 1
        #[serde(default = "default_start")]
        start: u64,
        /// 步长，默认 1
        #[serde(default = "default_step")]
        step: u64,
        /// 补零位数，0 = 自动
        #[serde(default)]
        pad_width: usize,
        /// 编号范围
        #[serde(default)]
        scope: NumberScope,
    },
    /// 模式D：统一后缀（修改文件扩展名）
    #[serde(rename = "suffix")]
    Suffix {
        /// 目标扩展名（不带点号）
        #[serde(default)]
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
    /// 文件唯一 ID（按遍历顺序 0..n，执行时用 ID 而非文件名防止篡改）
    pub id: usize,
    /// 相对于根目录的父目录路径（如 "assets/icons/"），根目录为空串
    pub relative_path: String,
    /// 原始完整文件名
    pub old_name: String,
    /// 新完整文件名
    pub new_name: String,
    /// 操作状态：ok / conflict / unchanged
    pub status: Status,
    /// 中文原因：ok 时为空串
    pub reason: String,
}

/// 执行结果，每个文件返回一条
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResult {
    /// 对应预览项 ID
    pub id: usize,
    /// 是否成功
    pub success: bool,
    /// 成功时为新文件名，失败时为中文原因
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

/// 撤销状态，前端用于控制「撤销上次」按钮
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoStatus {
    /// 是否存在可撤销的批次
    pub available: bool,
    /// 可撤销的文件数
    pub count: usize,
}

/// 撤销历史中的单条迁移记录。from=旧路径，to=新路径
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UndoEntry {
    pub from_path: String,
    pub to_path: String,
}

/// 自定义错误类型
///
/// 保留 thiserror，但**序列化必须是纯字符串**：前端直接 `String(e)` 显示。
/// 因此手写 `Serialize`，不使用 derive（否则会是 `{"DirNotFound": ...}` 之类结构）。
#[derive(Debug, thiserror::Error)]
pub enum RenamerError {
    #[error("目录不存在: {0}")]
    DirNotFound(String),
    #[error("读取目录失败: {0}")]
    ReadDirFailed(String),
    #[error("无效正则表达式: {0}")]
    InvalidRegex(String),
    #[error("操作已取消")]
    Cancelled,
    #[error("没有可撤销的操作")]
    NoUndo,
    /// 预留变体：模板不合法（当前未知占位符按单项 conflict 处理，不整体报错）
    #[allow(dead_code)]
    #[error("无效模板: {0}")]
    InvalidTemplate(String),
}

impl Serialize for RenamerError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
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
pub fn default_step() -> u64 {
    1
}
pub fn default_true() -> bool {
    true
}

// ─── 序列化契约测试 ─────────────────────────────────────
//
// 校验 CONTRACT.md「一、后端数据契约」的 JSON 形状：枚举字符串、tag/content、
// 默认值以及 RenamerError 序列化为纯字符串。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_serialization_matches_contract() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(serde_json::to_string(&Status::Ok)?, "\"ok\"");
        assert_eq!(serde_json::to_string(&Status::Conflict)?, "\"conflict\"");
        assert_eq!(serde_json::to_string(&Status::Unchanged)?, "\"unchanged\"");
        assert_eq!(serde_json::to_string(&SortBy::Name)?, "\"name\"");
        assert_eq!(serde_json::to_string(&SortBy::Modified)?, "\"modified\"");
        assert_eq!(serde_json::to_string(&SortBy::Created)?, "\"created\"");
        assert_eq!(serde_json::to_string(&NumberScope::PerDir)?, "\"per_dir\"");
        assert_eq!(serde_json::to_string(&NumberScope::Global)?, "\"global\"");
        Ok(())
    }

    #[test]
    fn find_replace_mode_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let mode: RenameMode =
            serde_json::from_str(r#"{"type":"find_replace","params":{"find":"a","replace":"b"}}"#)?;
        match mode {
            RenameMode::FindReplace {
                find,
                replace,
                use_regex,
                case_sensitive,
                whole_word,
                first_only,
                include_extension,
            } => {
                assert_eq!((find.as_str(), replace.as_str()), ("a", "b"));
                assert!(!use_regex);
                assert!(case_sensitive, "case_sensitive 默认 true");
                assert!(!whole_word);
                assert!(!first_only);
                assert!(!include_extension);
            }
            _ => return Err("模式反序列化类型错误".into()),
        }
        Ok(())
    }

    #[test]
    fn sequential_mode_defaults_and_shape() -> Result<(), Box<dyn std::error::Error>> {
        let mode: RenameMode = serde_json::from_str(r#"{"type":"sequential","params":{}}"#)?;
        match &mode {
            RenameMode::Sequential {
                prefix,
                separator,
                start,
                step,
                pad_width,
                suffix,
                scope,
            } => {
                assert_eq!(prefix, "Pic");
                assert_eq!(separator, "_");
                assert_eq!(*start, 1);
                assert_eq!(*step, 1);
                assert_eq!(*pad_width, 0);
                assert!(suffix.is_empty());
                assert_eq!(*scope, NumberScope::PerDir);
            }
            _ => return Err("模式反序列化类型错误".into()),
        }
        let value = serde_json::to_value(&mode)?;
        assert_eq!(value["type"], "sequential");
        assert!(value.get("params").is_some());
        Ok(())
    }

    #[test]
    fn pattern_and_suffix_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let pattern: RenameMode = serde_json::from_str(r#"{"type":"pattern","params":{}}"#)?;
        match &pattern {
            RenameMode::Pattern {
                template,
                start,
                step,
                pad_width,
                scope,
            } => {
                assert!(template.is_empty());
                assert_eq!((*start, *step, *pad_width), (1, 1, 0));
                assert_eq!(*scope, NumberScope::PerDir);
            }
            _ => return Err("模式反序列化类型错误".into()),
        }

        let suffix: RenameMode = serde_json::from_str(r#"{"type":"suffix","params":{}}"#)?;
        match &suffix {
            RenameMode::Suffix {
                target_suffix,
                force_all,
                lowercase,
            } => {
                assert!(target_suffix.is_empty());
                assert!(*force_all, "force_all 默认 true");
                assert!(*lowercase, "lowercase 默认 true");
            }
            _ => return Err("模式反序列化类型错误".into()),
        }
        Ok(())
    }

    #[test]
    fn preview_item_and_other_structs_shape() -> Result<(), Box<dyn std::error::Error>> {
        let item = PreviewItem {
            id: 3,
            relative_path: "sub/".to_string(),
            old_name: "a.jpg".to_string(),
            new_name: "b.jpg".to_string(),
            status: Status::Conflict,
            reason: "目标文件已存在".to_string(),
        };
        let value = serde_json::to_value(&item)?;
        assert_eq!(value["id"], 3);
        assert_eq!(value["relative_path"], "sub/");
        assert_eq!(value["old_name"], "a.jpg");
        assert_eq!(value["new_name"], "b.jpg");
        assert_eq!(value["status"], "conflict");
        assert_eq!(value["reason"], "目标文件已存在");

        let undo_status = UndoStatus {
            available: true,
            count: 2,
        };
        let value = serde_json::to_value(&undo_status)?;
        assert_eq!(value["available"], true);
        assert_eq!(value["count"], 2);

        let progress = ProgressPayload {
            current: 1,
            total: 5,
        };
        let value = serde_json::to_value(&progress)?;
        assert_eq!(value["current"], 1);
        assert_eq!(value["total"], 5);

        let execute = ExecuteResult {
            id: 0,
            success: true,
            message: "b.jpg".to_string(),
        };
        let value = serde_json::to_value(&execute)?;
        assert_eq!(value["success"], true);
        assert_eq!(value["message"], "b.jpg");

        let entry = UndoEntry {
            from_path: "/tmp/a.jpg".to_string(),
            to_path: "/tmp/b.jpg".to_string(),
        };
        let value = serde_json::to_value(&entry)?;
        assert_eq!(value["from_path"], "/tmp/a.jpg");
        assert_eq!(value["to_path"], "/tmp/b.jpg");
        Ok(())
    }

    #[test]
    fn renamer_error_serializes_as_plain_string() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            serde_json::to_string(&RenamerError::NoUndo)?,
            "\"没有可撤销的操作\""
        );
        assert_eq!(
            serde_json::to_string(&RenamerError::Cancelled)?,
            "\"操作已取消\""
        );
        assert_eq!(
            serde_json::to_string(&RenamerError::DirNotFound("/nope".to_string()))?,
            "\"目录不存在: /nope\""
        );
        assert_eq!(
            serde_json::to_string(&RenamerError::InvalidRegex("(".to_string()))?,
            "\"无效正则表达式: (\""
        );
        Ok(())
    }
}
