//! 重命名引擎 —— 文件遍历、名称计算、冲突检测、两阶段执行与撤销
//!
//! 除 `execute_renames` / `undo_renames` 直接操作文件系统外，其余均为纯逻辑，
//! 不依赖 Tauri 类型，因此可以独立单元测试（见文件末尾 `#[cfg(test)]`）。

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::SystemTime;

use regex::{NoExpand, Regex};
use walkdir::WalkDir;

use crate::models::{
    ExecuteResult, NumberScope, PreviewItem, RenameMode, RenamerError, SortBy, Status, UndoEntry,
};

/// 单个文件名的最大字节数（绝大多数文件系统的上限）
const MAX_FILENAME_BYTES: usize = 255;
/// 自动补零位数的安全上限，避免异常参数造成超大字符串
const MAX_PAD_WIDTH: usize = 128;
/// Windows 系统保留名（主名相同即算）
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 文件遍历阶段使用的内部结构，预览计算时消费
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// 全局唯一 ID（排序后按遍历顺序重新赋值为 0..n）
    pub id: usize,
    /// 文件绝对路径
    pub full_path: PathBuf,
    /// 相对于根目录的父目录路径（如 "subdir/"；根目录为空串）
    pub relative_dir: String,
    /// 文件名（含扩展名）
    pub name: String,
    /// 修改时间（取不到时为 None）
    pub mtime: Option<SystemTime>,
    /// 创建时间（取不到时为 None）
    pub created: Option<SystemTime>,
}

/// 遍历收集文件
///
/// - `recursive=false` 时只遍历根目录下一层
/// - 跳过隐藏文件/隐藏目录（以 `.` 开头），跳过非普通文件（含符号链接）
/// - 扩展名白名单只过滤文件，不影响目录递归
/// - 结果先按相对目录字典序分组，组内按 `sort_by` 排序；id 按最终顺序 0..n
pub fn collect_files(
    root: &Path,
    recursive: bool,
    filter_exts: &Option<HashSet<String>>,
    sort_by: &SortBy,
) -> Result<Vec<FileEntry>, RenamerError> {
    let mut walker = WalkDir::new(root).same_file_system(true);
    if !recursive {
        walker = walker.max_depth(1);
    }

    let mut entries: Vec<FileEntry> = Vec::new();
    for item in walker
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !is_hidden_name(entry.file_name()))
    {
        let entry = item.map_err(|error| RenamerError::ReadDirFailed(error.to_string()))?;
        // 根目录自身（depth 0）
        if entry.depth() == 0 {
            continue;
        }
        // 跳过目录、符号链接等非普通文件
        if !entry.file_type().is_file() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();

        // 扩展名白名单过滤（比较时小写）
        if let Some(exts) = filter_exts {
            let file_ext = Path::new(&name)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !exts.contains(&file_ext) {
                continue;
            }
        }

        let metadata = entry.metadata().ok();
        let mtime = metadata.as_ref().and_then(|meta| meta.modified().ok());
        let created = metadata.as_ref().and_then(|meta| meta.created().ok());

        entries.push(FileEntry {
            id: 0,
            full_path: entry.path().to_path_buf(),
            relative_dir: relative_dir_of(entry.path(), root),
            name,
            mtime,
            created,
        });
    }

    sort_entries(&mut entries, sort_by);
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.id = index;
    }
    Ok(entries)
}

/// 判断名字是否隐藏（以 `.` 开头）
fn is_hidden_name(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// 计算相对父目录路径，末尾带 `/`；根目录返回空串
fn relative_dir_of(path: &Path, root: &Path) -> String {
    let Some(parent) = path.parent() else {
        return String::new();
    };
    match parent.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => format!("{}/", rel.to_string_lossy()),
        _ => String::new(),
    }
}

/// 按目录分组 + 组内排序
fn sort_entries(entries: &mut [FileEntry], sort_by: &SortBy) {
    entries.sort_by(|a, b| {
        a.relative_dir
            .cmp(&b.relative_dir)
            .then_with(|| match sort_by {
                SortBy::Name => natural_cmp(&a.name, &b.name),
                SortBy::Modified => mtime_of(a)
                    .cmp(&mtime_of(b))
                    .then_with(|| natural_cmp(&a.name, &b.name)),
                SortBy::Created => created_of(a)
                    .cmp(&created_of(b))
                    .then_with(|| natural_cmp(&a.name, &b.name)),
            })
    });
}

fn mtime_of(entry: &FileEntry) -> SystemTime {
    entry.mtime.unwrap_or(SystemTime::UNIX_EPOCH)
}

fn created_of(entry: &FileEntry) -> SystemTime {
    entry
        .created
        .or(entry.mtime)
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

// ─── 自然排序 ─────────────────────────────────────────

/// 自然排序（natural sort）：数字段按数值比较，`img2 < img10`、`a1b2 < a1b10`
///
/// 规则：数字段按数值比较，数值相等时短者优先；非数字段不区分大小写比较，
/// 再按原串打破平局。
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a_chunks = split_chunks(a);
    let b_chunks = split_chunks(b);
    for (chunk_a, chunk_b) in a_chunks.iter().zip(b_chunks.iter()) {
        let ordering = match (chunk_a, chunk_b) {
            (Chunk::Number(x), Chunk::Number(y)) => compare_number(x, y),
            (Chunk::Text(x), Chunk::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
            (Chunk::Number(x), Chunk::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
            (Chunk::Text(x), Chunk::Number(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    a_chunks.len().cmp(&b_chunks.len()).then_with(|| a.cmp(b))
}

#[derive(Debug)]
enum Chunk<'a> {
    Number(&'a str),
    Text(&'a str),
}

/// 把字符串切成「数字段 / 非数字段」交替的片段
fn split_chunks(value: &str) -> Vec<Chunk<'_>> {
    let mut chunks = Vec::new();
    let mut current_is_digit: Option<bool> = None;
    let mut start = 0usize;
    for (index, character) in value.char_indices() {
        let is_digit = character.is_ascii_digit();
        match current_is_digit {
            None => {
                current_is_digit = Some(is_digit);
                start = index;
            }
            Some(previous) if previous == is_digit => {}
            Some(previous) => {
                chunks.push(make_chunk(previous, &value[start..index]));
                current_is_digit = Some(is_digit);
                start = index;
            }
        }
    }
    if let Some(is_digit) = current_is_digit {
        chunks.push(make_chunk(is_digit, &value[start..]));
    }
    chunks
}

fn make_chunk(is_digit: bool, text: &str) -> Chunk<'_> {
    if is_digit {
        Chunk::Number(text)
    } else {
        Chunk::Text(text)
    }
}

/// 数字段比较：先比有效数字长度，再比字典序，最后短者优先（如 "2" < "02"）
fn compare_number(x: &str, y: &str) -> Ordering {
    let x_trimmed = x.trim_start_matches('0');
    let y_trimmed = y.trim_start_matches('0');
    let x_digits = if x_trimmed.is_empty() { "0" } else { x_trimmed };
    let y_digits = if y_trimmed.is_empty() { "0" } else { y_trimmed };
    x_digits
        .len()
        .cmp(&y_digits.len())
        .then_with(|| x_digits.cmp(y_digits))
        .then_with(|| x.len().cmp(&y.len()))
}

// ─── 预览计算 ─────────────────────────────────────────

/// 第一遍生成的候选名，第二遍据此判定状态
enum Candidate {
    /// 参与冲突检测的候选新名
    Name(String),
    /// 模式层面就确定为「无变化」的项（保留具体中文 reason）
    Unchanged { new_name: String, reason: String },
    /// 模式层面就确定为「冲突」的项（占位符非法、目标后缀为空等）
    Conflict { new_name: String, reason: String },
}

impl Candidate {
    /// 参与「目标名重复」检测的名字；模式层面已冲突的项不参与
    fn materialized_name(&self) -> Option<&str> {
        match self {
            Candidate::Name(name) => Some(name),
            Candidate::Unchanged { new_name, .. } => Some(new_name),
            Candidate::Conflict { .. } => None,
        }
    }
}

/// 计算预览映射（纯内存计算，绝不修改磁盘）
///
/// 两遍：第一遍生成所有候选新名；第二遍统一做冲突检测并给出中文 reason。
pub fn compute_preview(
    entries: &[FileEntry],
    mode: &RenameMode,
    root: &Path,
) -> Result<Vec<PreviewItem>, RenamerError> {
    let total = entries.len();
    if total == 0 {
        return Ok(Vec::new());
    }

    // 查找替换的正则只编译一次；非法正则会中断整个预览（交给前端 toast）
    let regex = compile_find_regex(mode)?;

    // 按相对目录分组，并计算每个文件的「组内序号 / 全局序号 / 组大小」
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        groups
            .entry(entry.relative_dir.as_str())
            .or_default()
            .push(index);
    }
    let mut global_positions = vec![0usize; total];
    let mut local_positions = vec![0usize; total];
    let mut group_sizes = vec![0usize; total];
    let mut running = 0usize;
    for members in groups.values() {
        let group_size = members.len();
        for (local, &index) in members.iter().enumerate() {
            global_positions[index] = running + local;
            local_positions[index] = local;
            group_sizes[index] = group_size;
        }
        running += group_size;
    }

    let scope = mode_scope(mode);
    let (start, step, configured_pad) = mode_numbering(mode);

    // 第一遍：生成候选名
    let mut candidates: Vec<Candidate> = Vec::with_capacity(total);
    for index in 0..total {
        let position = match scope {
            Some(NumberScope::Global) => global_positions[index],
            _ => local_positions[index],
        };
        let auto_max = match scope {
            Some(NumberScope::Global) => {
                start.saturating_add((total.saturating_sub(1) as u64).saturating_mul(step))
            }
            Some(NumberScope::PerDir) => start
                .saturating_add((group_sizes[index].saturating_sub(1) as u64).saturating_mul(step)),
            None => 0,
        };
        let pad_width = if configured_pad > 0 {
            configured_pad
        } else {
            decimal_digits(auto_max)
        };
        candidates.push(candidate_for(
            &entries[index],
            mode,
            position as u64,
            pad_width,
            regex.as_ref(),
            root,
        ));
    }

    // 统计目标重名（按小写比较，兼容大小写不敏感文件系统）
    let mut target_counts: HashMap<(String, String), usize> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(name) = candidate.materialized_name() {
            let key = (
                entries[index].relative_dir.to_lowercase(),
                name.to_lowercase(),
            );
            *target_counts.entry(key).or_insert(0) += 1;
        }
    }

    // 第二遍：判定状态与中文 reason
    //
    // 冲突检测顺序（契约 2.3）：旧路径相同 → 非法文件名 → 目标已存在 → 目标名重复。
    // 目标路径若恰好是另一个「本次会改名的源文件」（链式 / 互换），不算冲突，
    // 由执行阶段的两阶段临时名处理。多级链式用不动点迭代判定：当某个源文件不再
    // 会被让出时，依赖它让出位置的项也随之变为冲突。
    let mut old_canonical_index: HashMap<PathBuf, usize> = HashMap::new();
    let mut old_raw_index: HashMap<PathBuf, usize> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Ok(canonical) = fs::canonicalize(&entry.full_path) {
            old_canonical_index.entry(canonical).or_insert(index);
        }
        old_raw_index
            .entry(entry.full_path.clone())
            .or_insert(index);
    }

    let mut path_equal = vec![false; total];
    let mut invalid_issues: Vec<Option<&'static str>> = vec![None; total];
    let mut duplicate_issues = vec![false; total];
    let mut moving = vec![false; total];
    for index in 0..total {
        let Candidate::Name(candidate_name) = &candidates[index] else {
            continue;
        };
        let entry = &entries[index];
        let new_path = root.join(&entry.relative_dir).join(candidate_name);
        path_equal[index] = &entry.full_path == &new_path;
        if path_equal[index] {
            continue;
        }
        invalid_issues[index] = validate_filename(candidate_name);
        let key = (
            entry.relative_dir.to_lowercase(),
            candidate_name.to_lowercase(),
        );
        duplicate_issues[index] = target_counts.get(&key).copied().unwrap_or(0) > 1;
        moving[index] = invalid_issues[index].is_none() && !duplicate_issues[index];
    }

    let mut disk_issues: Vec<Option<&'static str>> = vec![None; total];
    loop {
        let mut changed = false;
        for index in 0..total {
            if !moving[index] {
                continue;
            }
            let Candidate::Name(candidate_name) = &candidates[index] else {
                continue;
            };
            let entry = &entries[index];
            let new_path = root.join(&entry.relative_dir).join(candidate_name);
            if let Some(reason) = disk_conflict_with_pending_source(
                &entry.full_path,
                &new_path,
                &old_canonical_index,
                &old_raw_index,
                index,
                &moving,
            ) {
                moving[index] = false;
                disk_issues[index] = Some(reason);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut preview: Vec<PreviewItem> = Vec::with_capacity(total);
    for (index, entry) in entries.iter().enumerate() {
        let (status, new_name, reason) = match &candidates[index] {
            Candidate::Conflict { new_name, reason } => {
                (Status::Conflict, new_name.clone(), reason.clone())
            }
            Candidate::Unchanged { new_name, reason } => {
                (Status::Unchanged, new_name.clone(), reason.clone())
            }
            Candidate::Name(candidate_name) => {
                let old_path = &entry.full_path;
                let new_path = root.join(&entry.relative_dir).join(candidate_name);
                if path_equal[index] {
                    // 完整新路径 == 旧路径 → 无变化
                    (
                        Status::Unchanged,
                        candidate_name.clone(),
                        "文件名无变化".to_string(),
                    )
                } else if let Some(invalid_reason) = invalid_issues[index] {
                    (
                        Status::Conflict,
                        candidate_name.clone(),
                        invalid_reason.to_string(),
                    )
                } else if let Some(conflict_reason) = disk_issues[index] {
                    (
                        Status::Conflict,
                        candidate_name.clone(),
                        conflict_reason.to_string(),
                    )
                } else if duplicate_issues[index] {
                    // 契约顺序：目标已存在（规则3）优先于目标名重复（规则4）
                    match disk_conflict(old_path, &new_path) {
                        Some(conflict_reason) => (
                            Status::Conflict,
                            candidate_name.clone(),
                            conflict_reason.to_string(),
                        ),
                        None => (
                            Status::Conflict,
                            candidate_name.clone(),
                            "与其它文件目标名重复".to_string(),
                        ),
                    }
                } else if moving[index] {
                    (Status::Ok, candidate_name.clone(), String::new())
                } else {
                    // 理论上不可达；保守按冲突处理
                    (
                        Status::Conflict,
                        candidate_name.clone(),
                        "目标文件已存在".to_string(),
                    )
                }
            }
        };
        preview.push(PreviewItem {
            id: entry.id,
            relative_path: entry.relative_dir.clone(),
            old_name: entry.name.clone(),
            new_name,
            status,
            reason,
        });
    }

    Ok(preview)
}

/// 当前模式使用的编号范围（仅序号/模板模式有）
fn mode_scope(mode: &RenameMode) -> Option<NumberScope> {
    match mode {
        RenameMode::Sequential { scope, .. } | RenameMode::Pattern { scope, .. } => Some(*scope),
        _ => None,
    }
}

/// 当前模式的编号参数：(start, step, pad_width)
fn mode_numbering(mode: &RenameMode) -> (u64, u64, usize) {
    match mode {
        RenameMode::Sequential {
            start,
            step,
            pad_width,
            ..
        }
        | RenameMode::Pattern {
            start,
            step,
            pad_width,
            ..
        } => (*start, *step, *pad_width),
        _ => (1, 1, 0),
    }
}

/// 十进制位数（0 算 1 位）
fn decimal_digits(mut value: u64) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

/// 按 start/step/position/pad_width 生成补零编号
fn pad_number(start: u64, step: u64, position: u64, width: usize) -> String {
    let number = start.saturating_add(position.saturating_mul(step));
    let width = width.min(MAX_PAD_WIDTH);
    format!("{number:0width$}", width = width)
}

/// 编译查找替换使用的正则（仅在需要时编译）
///
/// - 普通模式：转义字面量，`case_sensitive=false` 时加 `(?i)`
/// - 正则模式：按用户表达式编译，`whole_word` 时包成 `\b(?:...)\b`
fn compile_find_regex(mode: &RenameMode) -> Result<Option<Regex>, RenamerError> {
    match mode {
        RenameMode::FindReplace {
            find,
            use_regex,
            case_sensitive,
            whole_word,
            ..
        } => {
            if find.is_empty() {
                return Ok(None);
            }
            let mut pattern = if *use_regex {
                find.clone()
            } else {
                regex::escape(find)
            };
            if *whole_word {
                pattern = format!(r"\b(?:{pattern})\b");
            }
            if !*case_sensitive {
                pattern = format!("(?i){pattern}");
            }
            Regex::new(&pattern)
                .map(Some)
                .map_err(|error| RenamerError::InvalidRegex(error.to_string()))
        }
        _ => Ok(None),
    }
}

/// 根据模式为单个文件生成候选名（不涉及磁盘冲突判定）
fn candidate_for(
    entry: &FileEntry,
    mode: &RenameMode,
    position: u64,
    pad_width: usize,
    regex: Option<&Regex>,
    root: &Path,
) -> Candidate {
    let (stem, ext) = split_name(&entry.name);
    match mode {
        // 模式A：查找替换
        RenameMode::FindReplace {
            find,
            replace,
            use_regex,
            include_extension,
            first_only,
            ..
        } => {
            // find 为空 → 新名 = 原名
            if find.is_empty() {
                return Candidate::Name(entry.name.clone());
            }
            let Some(regex) = regex else {
                return Candidate::Name(entry.name.clone());
            };
            let target = if *include_extension {
                entry.name.as_str()
            } else {
                stem.as_str()
            };
            let replaced = if *use_regex {
                if *first_only {
                    regex.replace(target, replace.as_str()).to_string()
                } else {
                    regex.replace_all(target, replace.as_str()).to_string()
                }
            } else if *first_only {
                // 普通模式：替换串按字面量处理，避免 $1 被当成捕获组
                regex
                    .replace(target, NoExpand(replace.as_str()))
                    .to_string()
            } else {
                regex
                    .replace_all(target, NoExpand(replace.as_str()))
                    .to_string()
            };
            if *include_extension || ext.is_empty() {
                Candidate::Name(replaced)
            } else {
                Candidate::Name(format!("{replaced}.{ext}"))
            }
        }

        // 模式B：序号命名
        RenameMode::Sequential {
            prefix,
            separator,
            start,
            step,
            suffix,
            ..
        } => {
            let padded = pad_number(*start, *step, position, pad_width);
            let mut new_name = String::new();
            new_name.push_str(prefix);
            new_name.push_str(separator);
            new_name.push_str(&padded);
            new_name.push_str(suffix);
            if !ext.is_empty() {
                new_name.push('.');
                new_name.push_str(&ext);
            }
            Candidate::Name(new_name)
        }

        // 模式C：模板命名
        RenameMode::Pattern {
            template,
            start,
            step,
            ..
        } => {
            let padded = pad_number(*start, *step, position, pad_width);
            let (date, time) = pattern_datetime(entry.mtime);
            let parent = parent_label(&entry.relative_dir, root);
            match render_pattern(
                template,
                &stem,
                &entry.name,
                &ext,
                &padded,
                &date,
                &time,
                &parent,
            ) {
                Ok(new_name) => Candidate::Name(new_name),
                Err(reason) => Candidate::Conflict {
                    new_name: entry.name.clone(),
                    reason,
                },
            }
        }

        // 模式D：统一后缀
        RenameMode::Suffix {
            target_suffix,
            force_all,
            lowercase,
        } => {
            // 先 trim、去前导点号，再按需转小写
            let trimmed = target_suffix.trim().trim_start_matches('.').to_string();
            let target = if *lowercase {
                trimmed.to_lowercase()
            } else {
                trimmed
            };
            if target.is_empty() {
                return Candidate::Conflict {
                    new_name: entry.name.clone(),
                    reason: "目标后缀为空".to_string(),
                };
            }

            if *force_all {
                Candidate::Name(format!("{stem}.{target}"))
            } else if ext.is_empty() {
                // 只给原本没有扩展名的文件追加
                Candidate::Name(format!("{stem}.{target}"))
            } else if ext.to_lowercase() == target.to_lowercase() {
                if *lowercase && ext != ext.to_lowercase() {
                    // 大小写与目标不同且要求转小写 → 改为小写
                    Candidate::Name(format!("{stem}.{}", ext.to_lowercase()))
                } else {
                    // 保持原大小写
                    Candidate::Unchanged {
                        new_name: entry.name.clone(),
                        reason: "扩展名无变化".to_string(),
                    }
                }
            } else {
                Candidate::Unchanged {
                    new_name: entry.name.clone(),
                    reason: "已有扩展名，未强制转换".to_string(),
                }
            }
        }
    }
}

/// 拆分文件名为主名 + 扩展名（扩展名不含点；无扩展名时为空串）
fn split_name(name: &str) -> (String, String) {
    let path = Path::new(name);
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if ext.is_empty() {
        (name.to_string(), String::new())
    } else {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(name);
        (stem.to_string(), ext.to_string())
    }
}

/// 根目录文件用根文件夹名，子目录文件用所在目录名
fn parent_label(relative_dir: &str, root: &Path) -> String {
    let trimmed = relative_dir.trim_end_matches('/');
    if trimmed.is_empty() {
        root.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_string())
            .unwrap_or_else(|| root.to_string_lossy().to_string())
    } else {
        Path::new(trimmed)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_string())
            .unwrap_or_else(|| trimmed.to_string())
    }
}

/// 渲染模板，返回新文件名或中文冲突原因
///
/// 支持 `{name}` `{original}` `{ext}` `{index}` `{date}` `{time}` `{parent}`，
/// `{{` / `}}` 输出字面量花括号。模板未使用 `{ext}`/`{original}` 时自动补扩展名。
#[allow(clippy::too_many_arguments)]
fn render_pattern(
    template: &str,
    stem: &str,
    original: &str,
    ext: &str,
    index_text: &str,
    date: &str,
    time: &str,
    parent: &str,
) -> Result<String, String> {
    let characters: Vec<char> = template.chars().collect();
    let mut output = String::new();
    let mut used_ext = false;
    let mut used_original = false;
    let mut cursor = 0usize;

    while cursor < characters.len() {
        match characters[cursor] {
            '{' => {
                // `{{` → 字面量 `{`
                if cursor + 1 < characters.len() && characters[cursor + 1] == '{' {
                    output.push('{');
                    cursor += 2;
                    continue;
                }
                let close = characters[cursor + 1..]
                    .iter()
                    .position(|character| *character == '}')
                    .map(|offset| cursor + 1 + offset);
                match close {
                    Some(close) => {
                        let placeholder: String = characters[cursor + 1..close].iter().collect();
                        match placeholder.as_str() {
                            "name" => output.push_str(stem),
                            "original" => {
                                used_original = true;
                                output.push_str(original);
                            }
                            "ext" => {
                                used_ext = true;
                                output.push_str(ext);
                            }
                            "index" => output.push_str(index_text),
                            "date" => output.push_str(date),
                            "time" => output.push_str(time),
                            "parent" => output.push_str(parent),
                            other => return Err(format!("未知占位符: {{{other}}}")),
                        }
                        cursor = close + 1;
                    }
                    None => {
                        output.push('{');
                        cursor += 1;
                    }
                }
            }
            '}' => {
                // `}}` → 字面量 `}`；单个 `}` 原样保留
                if cursor + 1 < characters.len() && characters[cursor + 1] == '}' {
                    cursor += 2;
                } else {
                    cursor += 1;
                }
                output.push('}');
            }
            character => {
                output.push(character);
                cursor += 1;
            }
        }
    }

    // 自动补扩展名：模板没有 {ext} 也没有 {original}，且原文件有扩展名
    if !used_ext && !used_original && !ext.is_empty() {
        output.push('.');
        output.push_str(ext);
    }
    if output.is_empty() {
        return Err("生成的文件名为空".to_string());
    }
    Ok(output)
}

/// 计算 `{date}`/`{time}`：mtime 的本地时区日期与时间，取不到时用当前时间
fn pattern_datetime(mtime: Option<SystemTime>) -> (String, String) {
    let base = mtime.unwrap_or_else(SystemTime::now);
    match local_datetime(base).or_else(|| local_datetime(SystemTime::now())) {
        Some(datetime) => (
            datetime.format("%Y%m%d").to_string(),
            datetime.format("%H%M%S").to_string(),
        ),
        // chrono 时间换算失败时用空串兜底，绝不 panic
        None => (String::new(), String::new()),
    }
}

/// SystemTime → 本地时区 DateTime（失败返回 None，调用方有 fallback）
fn local_datetime(time: SystemTime) -> Option<chrono::DateTime<chrono::Local>> {
    use chrono::TimeZone;

    let duration = time.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(duration.as_secs()).ok()?;
    chrono::Local
        .timestamp_opt(seconds, duration.subsec_nanos())
        .single()
}

/// 校验文件名，返回中文冲突原因
pub fn validate_filename(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("生成的文件名为空");
    }
    // Windows/macOS/Linux 通用禁用字符 + 控制字符
    let banned = ['<', '>', ':', '"', '|', '?', '*', '/', '\\'];
    if name
        .chars()
        .any(|character| character.is_control() || banned.contains(&character))
    {
        return Some("文件名包含非法字符");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("文件名以点或空格结尾");
    }
    if name.len() > MAX_FILENAME_BYTES {
        return Some("文件名过长");
    }
    if is_reserved_name(name) {
        return Some("文件名是系统保留名");
    }
    None
}

/// Windows 保留名判定（主名相同即算，忽略大小写）
fn is_reserved_name(name: &str) -> bool {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    let upper = stem.to_ascii_uppercase();
    RESERVED_NAMES.contains(&upper.as_str())
}

/// 目标路径已存在时的冲突判定
///
/// 目标 canonicalize 后与源相同 → 仅大小写重命名，允许；目标是目录 → 单独原因。
fn disk_conflict(old_path: &Path, new_path: &Path) -> Option<&'static str> {
    if !new_path.exists() {
        return None;
    }
    if let (Ok(old_canonical), Ok(new_canonical)) =
        (fs::canonicalize(old_path), fs::canonicalize(new_path))
    {
        if old_canonical == new_canonical {
            return None;
        }
    }
    if new_path.is_dir() {
        Some("目标路径是目录")
    } else {
        Some("目标文件已存在")
    }
}

/// 目标已存在时的冲突判定，但目标恰好是另一个「即将改名让出」的源文件时放行
///
/// 链式/互换改名在执行阶段用两阶段临时名处理，因此预览阶段不能把它当冲突。
fn disk_conflict_with_pending_source(
    old_path: &Path,
    new_path: &Path,
    old_canonical_index: &HashMap<PathBuf, usize>,
    old_raw_index: &HashMap<PathBuf, usize>,
    self_index: usize,
    moving: &[bool],
) -> Option<&'static str> {
    if !new_path.exists() {
        return None;
    }
    // 路径字节完全相等（canonicalize 不可用时仍能识别链式/互换）
    if let Some(&owner) = old_raw_index.get(new_path) {
        if owner != self_index && moving.get(owner).copied().unwrap_or(false) {
            return None;
        }
    }
    let new_canonical = fs::canonicalize(new_path).ok();
    if let (Some(new_canonical), Ok(old_canonical)) =
        (new_canonical.as_ref(), fs::canonicalize(old_path))
    {
        // 解析到同一 inode → 仅大小写重命名，允许
        if *new_canonical == old_canonical {
            return None;
        }
    }
    if let Some(new_canonical) = new_canonical.as_ref() {
        if let Some(&owner) = old_canonical_index.get(new_canonical) {
            if owner != self_index && moving.get(owner).copied().unwrap_or(false) {
                return None;
            }
        }
    }
    if new_path.is_dir() {
        Some("目标路径是目录")
    } else {
        Some("目标文件已存在")
    }
}

// ─── 执行与撤销 ───────────────────────────────────────

/// 一次文件搬移：旧路径 → 新路径
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveJob {
    pub id: usize,
    pub old_path: PathBuf,
    pub new_path: PathBuf,
}

/// 批量搬移的完整结果
///
/// `cancelled` 为 true 时，`results`/`undo` 只包含取消前**已经完成**的部分，
/// 调用方应把 `undo` 写入撤销历史，再向前端返回「操作已取消」，
/// 避免用户取消后已改名的文件无法撤销。
#[derive(Debug, Default)]
pub struct MoveOutcome {
    pub results: Vec<ExecuteResult>,
    pub undo: Vec<UndoEntry>,
    pub cancelled: bool,
}

fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel
        .map(|flag| flag.load(AtomicOrdering::SeqCst))
        .unwrap_or(false)
}

fn success_result(id: usize, new_path: &Path) -> ExecuteResult {
    let message = new_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .unwrap_or_else(|| new_path.to_string_lossy().to_string());
    ExecuteResult {
        id,
        success: true,
        message,
    }
}

fn failure_result(
    id: usize,
    old_path: &Path,
    new_path: &Path,
    error: &std::io::Error,
) -> ExecuteResult {
    use std::io::ErrorKind;

    let message = match error.kind() {
        ErrorKind::PermissionDenied => {
            format!("权限不足: {} → {}", old_path.display(), new_path.display())
        }
        ErrorKind::AlreadyExists => format!("目标已存在: {}", new_path.display()),
        ErrorKind::NotFound => format!("源文件不存在: {}", old_path.display()),
        _ => format!(
            "重命名失败: {} → {} ({})",
            old_path.display(),
            new_path.display(),
            error
        ),
    };
    ExecuteResult {
        id,
        success: false,
        message,
    }
}

/// 生成同目录下不重复的临时文件名
fn temp_name_for(path: &Path, counter: usize) -> PathBuf {
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let pid = std::process::id();
    let mut attempt = 0usize;
    loop {
        let name = if attempt == 0 {
            format!(".rename-rust-tmp-{pid}-{counter}")
        } else {
            format!(".rename-rust-tmp-{pid}-{counter}-{attempt}")
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
        attempt = attempt.saturating_add(1);
        if attempt > 1000 {
            return parent.join(format!(".rename-rust-tmp-{pid}-{counter}-overflow"));
        }
    }
}

/// 执行一批搬移，返回逐项结果与撤销记录
///
/// 若某些项的 `new_path` 与其它项的 `old_path` 相交（互换 / 链式改名），
/// 整个批次走两阶段：先全部改成临时名，再改成最终名。
pub fn execute_renames<F>(
    jobs: &[MoveJob],
    cancel: &AtomicBool,
    progress: F,
) -> Result<MoveOutcome, RenamerError>
where
    F: FnMut(usize),
{
    perform_moves(jobs, Some(cancel), progress)
}

/// 撤销一批改动（逆序执行 to_path → from_path），返回逐项结果
pub fn undo_renames(entries: &[UndoEntry]) -> Vec<ExecuteResult> {
    let jobs: Vec<MoveJob> = entries
        .iter()
        .rev()
        .enumerate()
        .map(|(index, entry)| MoveJob {
            // 撤销结果用 1 起的序号作为 id
            id: index + 1,
            old_path: PathBuf::from(&entry.to_path),
            new_path: PathBuf::from(&entry.from_path),
        })
        .collect();

    match perform_moves(&jobs, None, |_| {}) {
        Ok(outcome) => outcome.results,
        // perform_moves 在 cancel=None 时不会返回错误；保守返回空结果
        Err(_error) => Vec::new(),
    }
}

fn perform_moves<F>(
    jobs: &[MoveJob],
    cancel: Option<&AtomicBool>,
    progress: F,
) -> Result<MoveOutcome, RenamerError>
where
    F: FnMut(usize),
{
    if jobs.is_empty() {
        return Ok(MoveOutcome::default());
    }
    let old_raw: HashSet<PathBuf> = jobs.iter().map(|job| job.old_path.clone()).collect();
    let old_canonical: HashSet<PathBuf> = jobs
        .iter()
        .filter_map(|job| fs::canonicalize(&job.old_path).ok())
        .collect();
    // 目标路径是否落在本批次某个源文件上（互换 / 链式）→ 两阶段
    let needs_two_phase = jobs.iter().any(|job| {
        if job.new_path == job.old_path {
            return false;
        }
        if old_raw.contains(&job.new_path) {
            return true;
        }
        match fs::canonicalize(&job.new_path) {
            Ok(canonical) => old_canonical.contains(&canonical),
            Err(_) => false,
        }
    });

    if needs_two_phase {
        perform_moves_two_phase(jobs, cancel, progress)
    } else {
        perform_moves_direct(jobs, cancel, progress)
    }
}

/// 无交叉时直接改名
fn perform_moves_direct<F>(
    jobs: &[MoveJob],
    cancel: Option<&AtomicBool>,
    mut progress: F,
) -> Result<MoveOutcome, RenamerError>
where
    F: FnMut(usize),
{
    let mut outcome = MoveOutcome::default();
    let mut current = 0usize;

    for job in jobs {
        if is_cancelled(cancel) {
            // 已完成的部分保留在 outcome 里：调用方仍可写入撤销历史
            outcome.cancelled = true;
            return Ok(outcome);
        }
        // 防御：目标已存在且不是「仅大小写重命名」时拒绝覆盖（防止误删数据）
        if target_occupied_by_other(&job.old_path, &job.new_path) {
            outcome.results.push(ExecuteResult {
                id: job.id,
                success: false,
                message: format!("目标文件已存在: {}", job.new_path.display()),
            });
        } else {
            match fs::rename(&job.old_path, &job.new_path) {
                Ok(()) => {
                    outcome.results.push(success_result(job.id, &job.new_path));
                    outcome.undo.push(UndoEntry {
                        from_path: job.old_path.to_string_lossy().to_string(),
                        to_path: job.new_path.to_string_lossy().to_string(),
                    });
                }
                Err(error) => outcome.results.push(failure_result(
                    job.id,
                    &job.old_path,
                    &job.new_path,
                    &error,
                )),
            }
        }
        current += 1;
        progress(current);
    }

    Ok(outcome)
}

/// 有交叉时走两阶段：第一阶段全部改成临时名，第二阶段改成最终名
fn perform_moves_two_phase<F>(
    jobs: &[MoveJob],
    cancel: Option<&AtomicBool>,
    mut progress: F,
) -> Result<MoveOutcome, RenamerError>
where
    F: FnMut(usize),
{
    let mut results: Vec<Option<ExecuteResult>> = (0..jobs.len()).map(|_| None).collect();
    let mut temps: Vec<Option<PathBuf>> = (0..jobs.len()).map(|_| None).collect();
    let mut current = 0usize;

    // 第一阶段：旧名 → 临时名
    for (index, job) in jobs.iter().enumerate() {
        if is_cancelled(cancel) {
            // 取消：把已经改成临时名的文件尽量恢复原名，然后整体报取消
            rollback_temps(jobs, &temps);
            return Ok(MoveOutcome {
                cancelled: true,
                ..MoveOutcome::default()
            });
        }
        let temp = temp_name_for(&job.old_path, index);
        match fs::rename(&job.old_path, &temp) {
            Ok(()) => temps[index] = Some(temp),
            Err(error) => {
                results[index] = Some(failure_result(job.id, &job.old_path, &temp, &error))
            }
        }
        current += 1;
        progress(current);
    }

    // 第二阶段：临时名 → 最终名
    //
    // 注意：这一阶段不再轮询取消标志。此时一半文件已是临时名，中途放弃会把
    // 临时名留在磁盘上；契约允许「已完成的改名无法回滚」，但留下临时名不可接受，
    // 因此进入第二阶段后先把整批做完（取消从「每个文件前」的语义上只在第一阶段生效）。
    let mut undo = Vec::new();
    for (index, job) in jobs.iter().enumerate() {
        let Some(temp) = temps[index].take() else {
            continue;
        };
        // 防御：最终目标已被外部文件占用时拒绝覆盖
        if job.new_path != job.old_path && job.new_path.exists() {
            let message = format!("目标文件已存在: {}", job.new_path.display());
            let result = ExecuteResult {
                id: job.id,
                success: false,
                message: append_restore_note(message, &temp, &job.old_path),
            };
            results[index] = Some(result);
            continue;
        }
        match fs::rename(&temp, &job.new_path) {
            Ok(()) => {
                results[index] = Some(success_result(job.id, &job.new_path));
                undo.push(UndoEntry {
                    from_path: job.old_path.to_string_lossy().to_string(),
                    to_path: job.new_path.to_string_lossy().to_string(),
                });
            }
            Err(error) => {
                // 尽力把临时名恢复成原名（原名被占用时保留临时名，绝不覆盖）
                let result = failure_result(job.id, &temp, &job.new_path, &error);
                let result = ExecuteResult {
                    message: append_restore_note(result.message, &temp, &job.old_path),
                    ..result
                };
                results[index] = Some(result);
            }
        }
    }

    Ok(MoveOutcome {
        results: results.into_iter().flatten().collect(),
        undo,
        cancelled: false,
    })
}

/// 目标路径是否被「另一个文件」占用（canonical 相同视为同一文件，允许大小写重命名）
fn target_occupied_by_other(old_path: &Path, new_path: &Path) -> bool {
    if new_path == old_path || !new_path.exists() {
        return false;
    }
    match (fs::canonicalize(old_path), fs::canonicalize(new_path)) {
        (Ok(old_canonical), Ok(new_canonical)) => old_canonical != new_canonical,
        _ => true,
    }
}

/// 尽力把临时名恢复成原名；原名被占用时保留临时名以避免覆盖数据
fn restore_temp(temp: &Path, old_path: &Path) -> Option<PathBuf> {
    if old_path.exists() {
        return Some(temp.to_path_buf());
    }
    match fs::rename(temp, old_path) {
        Ok(()) => None,
        Err(_) => Some(temp.to_path_buf()),
    }
}

/// 在失败信息后追加「临时名处理结果」说明
fn append_restore_note(mut message: String, temp: &Path, old_path: &Path) -> String {
    if let Some(left_path) = restore_temp(temp, old_path) {
        message.push_str(&format!(
            "（原名被占用，文件保留在临时路径: {}）",
            left_path.display()
        ));
    } else {
        message.push_str("（已尝试恢复原名）");
    }
    message
}

/// 把第一阶段产生的临时名恢复成原文件名（尽力而为，绝不覆盖）
fn rollback_temps(jobs: &[MoveJob], temps: &[Option<PathBuf>]) {
    for (index, job) in jobs.iter().enumerate() {
        if let Some(temp) = temps[index].as_ref() {
            let _ = restore_temp(temp, &job.old_path);
        }
    }
}

// ─── 单元测试 ─────────────────────────────────────────
//
// 覆盖契约 2.5 要求的场景。测试使用 std::env::temp_dir 下的唯一目录，
// 结束后由 TempDir 的 Drop 清理。

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    /// 唯一临时目录，Drop 时清理
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> std::io::Result<Self> {
            let base = std::env::temp_dir();
            let mut attempt = 0u32;
            loop {
                let path = base.join(format!(
                    "rename-rust-test-{tag}-{}-{attempt}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Ok(Self { path }),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                        attempt = attempt.saturating_add(1);
                        if attempt > 1000 {
                            return Err(error);
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }

        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_file(path: &Path, contents: &str) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)
    }

    fn preview_of(
        dir: &Path,
        mode: &RenameMode,
    ) -> Result<Vec<PreviewItem>, Box<dyn std::error::Error>> {
        preview_of_batch(dir, mode, false)
    }

    fn preview_of_batch(
        dir: &Path,
        mode: &RenameMode,
        recursive: bool,
    ) -> Result<Vec<PreviewItem>, Box<dyn std::error::Error>> {
        let entries = collect_files(dir, recursive, &None, &SortBy::Name)?;
        Ok(compute_preview(&entries, mode, dir)?)
    }

    fn find_item<'a>(
        items: &'a [PreviewItem],
        old_name: &str,
    ) -> Result<&'a PreviewItem, Box<dyn std::error::Error>> {
        items
            .iter()
            .find(|item| item.old_name == old_name)
            .ok_or_else(|| format!("未找到预览项: {old_name}").into())
    }

    fn sequential_mode(scope: NumberScope) -> RenameMode {
        RenameMode::Sequential {
            prefix: "p".to_string(),
            separator: "_".to_string(),
            start: 1,
            step: 1,
            pad_width: 0,
            suffix: String::new(),
            scope,
        }
    }

    fn pattern_mode(template: &str) -> RenameMode {
        RenameMode::Pattern {
            template: template.to_string(),
            start: 1,
            step: 1,
            pad_width: 0,
            scope: NumberScope::PerDir,
        }
    }

    fn find_replace_mode(find: &str, replace: &str) -> RenameMode {
        RenameMode::FindReplace {
            find: find.to_string(),
            replace: replace.to_string(),
            use_regex: false,
            case_sensitive: true,
            whole_word: false,
            first_only: false,
            include_extension: false,
        }
    }

    // ── 自然排序 ──

    #[test]
    fn natural_sort_orders_numeric_segments() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(natural_cmp("img2", "img10"), Ordering::Less);
        assert_eq!(natural_cmp("img10", "img2"), Ordering::Greater);
        assert_eq!(natural_cmp("a1b2", "a1b10"), Ordering::Less);
        assert_eq!(natural_cmp("img2", "img2"), Ordering::Equal);

        let dir = TempDir::new("natural")?;
        for name in ["img10.jpg", "img2.jpg", "img1.jpg", "IMG3.jpg"] {
            write_file(&dir.join(name), "")?;
        }
        let entries = collect_files(&dir.path, false, &None, &SortBy::Name)?;
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["img1.jpg", "img2.jpg", "IMG3.jpg", "img10.jpg"]);
        Ok(())
    }

    // ── 序号命名：补零 / start / step ──

    #[test]
    fn sequential_auto_and_fixed_padding_with_start_step() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = TempDir::new("seq-pad")?;
        for name in ["c.txt", "a.txt", "b.txt"] {
            write_file(&dir.join(name), "")?;
        }

        // 自动补零：1, 11, 21 的最大值 21 为两位
        let mode = RenameMode::Sequential {
            prefix: "p".to_string(),
            separator: "_".to_string(),
            start: 1,
            step: 10,
            pad_width: 0,
            suffix: String::new(),
            scope: NumberScope::PerDir,
        };
        let items = preview_of(&dir.path, &mode)?;
        let names: Vec<&str> = items.iter().map(|item| item.new_name.as_str()).collect();
        assert_eq!(names, vec!["p_01.txt", "p_11.txt", "p_21.txt"]);
        assert!(items.iter().all(|item| item.status == Status::Ok));

        // 固定补零 3 位
        let mode = RenameMode::Sequential {
            prefix: "p".to_string(),
            separator: "_".to_string(),
            start: 5,
            step: 2,
            pad_width: 3,
            suffix: "x".to_string(),
            scope: NumberScope::PerDir,
        };
        let items = preview_of(&dir.path, &mode)?;
        let names: Vec<&str> = items.iter().map(|item| item.new_name.as_str()).collect();
        assert_eq!(names, vec!["p_005x.txt", "p_007x.txt", "p_009x.txt"]);
        Ok(())
    }

    // ── 序号命名：per_dir vs global ──

    #[test]
    fn sequential_scope_per_dir_versus_global() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("seq-scope")?;
        for name in ["a/x.jpg", "a/y.jpg", "b/x.jpg", "b/y.jpg"] {
            write_file(&dir.join(name), "")?;
        }

        let per_dir = preview_of_batch(&dir.path, &sequential_mode(NumberScope::PerDir), true)?;
        let a_x = per_dir
            .iter()
            .find(|item| item.relative_path == "a/" && item.old_name == "x.jpg")
            .ok_or("缺少 a/x.jpg")?;
        let a_y = per_dir
            .iter()
            .find(|item| item.relative_path == "a/" && item.old_name == "y.jpg")
            .ok_or("缺少 a/y.jpg")?;
        let b_x = per_dir
            .iter()
            .find(|item| item.relative_path == "b/" && item.old_name == "x.jpg")
            .ok_or("缺少 b/x.jpg")?;
        assert_eq!(
            (a_x.new_name.as_str(), a_y.new_name.as_str()),
            ("p_1.jpg", "p_2.jpg")
        );
        assert_eq!(b_x.new_name, "p_1.jpg");

        let global = preview_of_batch(&dir.path, &sequential_mode(NumberScope::Global), true)?;
        let a_x = global
            .iter()
            .find(|item| item.relative_path == "a/" && item.old_name == "x.jpg")
            .ok_or("缺少 a/x.jpg")?;
        let a_y = global
            .iter()
            .find(|item| item.relative_path == "a/" && item.old_name == "y.jpg")
            .ok_or("缺少 a/y.jpg")?;
        let b_x = global
            .iter()
            .find(|item| item.relative_path == "b/" && item.old_name == "x.jpg")
            .ok_or("缺少 b/x.jpg")?;
        let b_y = global
            .iter()
            .find(|item| item.relative_path == "b/" && item.old_name == "y.jpg")
            .ok_or("缺少 b/y.jpg")?;
        assert_eq!(
            (
                a_x.new_name.as_str(),
                a_y.new_name.as_str(),
                b_x.new_name.as_str(),
                b_y.new_name.as_str()
            ),
            ("p_1.jpg", "p_2.jpg", "p_3.jpg", "p_4.jpg")
        );
        Ok(())
    }

    // ── 模板命名 ──

    #[test]
    fn pattern_placeholders_unknown_and_auto_extension() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("pattern")?;
        write_file(&dir.join("photo.JPG"), "")?;

        // 占位符齐全
        let items = preview_of(&dir.path, &pattern_mode("{name}-{index}-{ext}"))?;
        assert_eq!(items[0].new_name, "photo-1-JPG");
        assert_eq!(items[0].status, Status::Ok);

        // 未使用 {ext}/{original} → 自动补扩展名
        let items = preview_of(&dir.path, &pattern_mode("renamed-{index}"))?;
        assert_eq!(items[0].new_name, "renamed-1.JPG");

        // {original} 已含扩展名，不再重复补
        let items = preview_of(&dir.path, &pattern_mode("{original}"))?;
        assert_eq!(items[0].new_name, "photo.JPG");
        assert_eq!(items[0].status, Status::Unchanged);
        assert_eq!(items[0].reason, "文件名无变化");

        // {{ }} 转义
        let items = preview_of(&dir.path, &pattern_mode("{{name}}-{index}"))?;
        assert_eq!(items[0].new_name, "{name}-1.JPG");

        // 未知占位符 → 该项 conflict，但不中断整体预览
        let items = preview_of(&dir.path, &pattern_mode("{foo}-{name}"))?;
        assert_eq!(items[0].status, Status::Conflict);
        assert_eq!(items[0].reason, "未知占位符: {foo}");
        assert_eq!(items[0].new_name, "photo.JPG");

        // {parent} 根目录文件使用根文件夹名
        let root_name = dir
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("临时目录没有名字")?
            .to_string();
        let items = preview_of(&dir.path, &pattern_mode("{parent}-{name}"))?;
        assert_eq!(items[0].new_name, format!("{root_name}-photo.JPG"));

        // {date} / {time} 为本地时区格式
        let items = preview_of(&dir.path, &pattern_mode("{date}_{time}_{name}"))?;
        let new_name = &items[0].new_name;
        let (date, rest) = new_name.split_at(8);
        assert_eq!(date.len(), 8);
        assert!(date.chars().all(|character| character.is_ascii_digit()));
        assert_eq!(&rest[..1], "_");
        let (time, tail) = rest[1..].split_at(6);
        assert!(time.chars().all(|character| character.is_ascii_digit()));
        assert_eq!(tail, "_photo.JPG");
        Ok(())
    }

    // ── 统一后缀三种语义 ──

    #[test]
    fn suffix_semantics() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("suffix")?;
        for name in [
            "a.jpg", "b.txt", "c", "d.TXT", "e.PNG", "f.jpg", "g.tar.gz", "h.png",
        ] {
            write_file(&dir.join(name), "")?;
        }

        // force_all = false：只给原本没有扩展名的文件追加
        let mode = RenameMode::Suffix {
            target_suffix: "png".to_string(),
            force_all: false,
            lowercase: true,
        };
        let items = preview_of(&dir.path, &mode)?;
        let a = find_item(&items, "a.jpg")?;
        assert_eq!(a.status, Status::Unchanged);
        assert_eq!(a.reason, "已有扩展名，未强制转换");
        let c = find_item(&items, "c")?;
        assert_eq!(c.new_name, "c.png");
        assert_eq!(c.status, Status::Ok);
        // 与目标相同（忽略大小写）且非全小写 → 转小写
        let e = find_item(&items, "e.PNG")?;
        assert_eq!(e.new_name, "e.png");
        assert_eq!(e.status, Status::Ok);
        // 与目标不同 → 保留
        let f = find_item(&items, "f.jpg")?;
        assert_eq!(f.status, Status::Unchanged);
        assert_eq!(f.reason, "已有扩展名，未强制转换");
        // 与目标相同且已全小写 → 无变化
        let h = find_item(&items, "h.png")?;
        assert_eq!(h.status, Status::Unchanged);
        assert_eq!(h.reason, "扩展名无变化");
        // 不同扩展名且不强制 → 保留
        let d = find_item(&items, "d.TXT")?;
        assert_eq!(d.status, Status::Unchanged);
        assert_eq!(d.new_name, "d.TXT");

        // lowercase = false：同扩展名保持原大小写
        let mode = RenameMode::Suffix {
            target_suffix: "PNG".to_string(),
            force_all: false,
            lowercase: false,
        };
        let items = preview_of(&dir.path, &mode)?;
        assert_eq!(find_item(&items, "e.PNG")?.status, Status::Unchanged);
        assert_eq!(find_item(&items, "e.PNG")?.reason, "扩展名无变化");

        // force_all = true：全部换成目标后缀；目标后缀会 trim / 去点号 / 转小写
        let mode = RenameMode::Suffix {
            target_suffix: "  .WebP  ".to_string(),
            force_all: true,
            lowercase: true,
        };
        let items = preview_of(&dir.path, &mode)?;
        assert_eq!(find_item(&items, "a.jpg")?.new_name, "a.webp");
        assert_eq!(find_item(&items, "c")?.new_name, "c.webp");
        // 只改最后一个扩展名
        assert_eq!(find_item(&items, "g.tar.gz")?.new_name, "g.tar.webp");

        // 目标后缀为空 → 所有项 conflict
        let mode = RenameMode::Suffix {
            target_suffix: " . ".to_string(),
            force_all: true,
            lowercase: true,
        };
        let items = preview_of(&dir.path, &mode)?;
        for item in &items {
            assert_eq!(item.status, Status::Conflict);
            assert_eq!(item.reason, "目标后缀为空");
        }
        Ok(())
    }

    // ── 查找替换：大小写 / 全词 / 首个 / 含扩展名 / 正则 ──

    #[test]
    fn find_replace_options() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("find")?;
        for name in [
            "Hello.JPG",
            "catcat.jpg",
            "a cat.jpg",
            "aaa.txt",
            "img_001.jpg",
        ] {
            write_file(&dir.join(name), "")?;
        }

        // 大小写不敏感
        let mode = RenameMode::FindReplace {
            find: "hello".to_string(),
            replace: "world".to_string(),
            use_regex: false,
            case_sensitive: false,
            whole_word: false,
            first_only: false,
            include_extension: false,
        };
        let items = preview_of(&dir.path, &mode)?;
        assert_eq!(find_item(&items, "Hello.JPG")?.new_name, "world.JPG");

        // 全词匹配：catcat 不匹配，a cat 匹配
        let mode = RenameMode::FindReplace {
            find: "cat".to_string(),
            replace: "dog".to_string(),
            use_regex: false,
            case_sensitive: true,
            whole_word: true,
            first_only: false,
            include_extension: false,
        };
        let items = preview_of(&dir.path, &mode)?;
        assert_eq!(find_item(&items, "catcat.jpg")?.status, Status::Unchanged);
        assert_eq!(find_item(&items, "a cat.jpg")?.new_name, "a dog.jpg");

        // 只替换首个
        let mode = RenameMode::FindReplace {
            find: "a".to_string(),
            replace: "b".to_string(),
            use_regex: false,
            case_sensitive: true,
            whole_word: false,
            first_only: true,
            include_extension: false,
        };
        let items = preview_of(&dir.path, &mode)?;
        assert_eq!(find_item(&items, "aaa.txt")?.new_name, "baa.txt");

        // 含扩展名：作用于完整文件名
        let mode = RenameMode::FindReplace {
            find: "jpg".to_string(),
            replace: "png".to_string(),
            use_regex: false,
            case_sensitive: true,
            whole_word: false,
            first_only: false,
            include_extension: true,
        };
        let items = preview_of(&dir.path, &mode)?;
        assert_eq!(find_item(&items, "catcat.jpg")?.new_name, "catcat.png");

        // 正则捕获组
        let mode = RenameMode::FindReplace {
            find: r"img_(\d+)".to_string(),
            replace: "photo_$1".to_string(),
            use_regex: true,
            case_sensitive: true,
            whole_word: false,
            first_only: false,
            include_extension: false,
        };
        let items = preview_of(&dir.path, &mode)?;
        assert_eq!(find_item(&items, "img_001.jpg")?.new_name, "photo_001.jpg");

        // 非法正则 → 整体返回错误
        let mode = RenameMode::FindReplace {
            find: "(".to_string(),
            replace: "x".to_string(),
            use_regex: true,
            case_sensitive: true,
            whole_word: false,
            first_only: false,
            include_extension: false,
        };
        let invalid_regex_result = preview_of(&dir.path, &mode);
        assert!(
            matches!(
                invalid_regex_result,
                Err(ref error) if error.downcast_ref::<RenamerError>().is_some()
            ),
            "非法正则应当返回 RenamerError"
        );

        // find 为空 → 全部无变化
        let mode = find_replace_mode("", "x");
        let items = preview_of(&dir.path, &mode)?;
        assert!(items.iter().all(|item| item.status == Status::Unchanged));
        assert!(items.iter().all(|item| item.reason == "文件名无变化"));
        Ok(())
    }

    // ── 链式改名的预览：目标恰是另一个待改名源文件时不算冲突 ──

    #[test]
    fn chained_targets_are_not_preview_conflicts() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("preview-chain")?;
        for name in ["1.txt", "2.txt", "3.txt"] {
            write_file(&dir.join(name), "")?;
        }
        // 序号命名 start=2：1→2, 2→3, 3→4（形成改名链，执行阶段两阶段处理）
        let mode = RenameMode::Sequential {
            prefix: String::new(),
            separator: String::new(),
            start: 2,
            step: 1,
            pad_width: 0,
            suffix: String::new(),
            scope: NumberScope::PerDir,
        };
        let items = preview_of(&dir.path, &mode)?;
        let names: Vec<&str> = items.iter().map(|item| item.new_name.as_str()).collect();
        assert_eq!(names, vec!["2.txt", "3.txt", "4.txt"]);
        assert!(
            items.iter().all(|item| item.status == Status::Ok),
            "链式目标不应被视为冲突: {items:?}"
        );
        Ok(())
    }

    // ── 冲突检测原因 ──

    #[test]
    fn conflict_detection_reasons() -> Result<(), Box<dyn std::error::Error>> {
        // 目标文件已存在
        let exists_dir = TempDir::new("conflict-exists")?;
        write_file(&exists_dir.join("a.txt"), "")?;
        write_file(&exists_dir.join("b.txt"), "")?;
        let items = preview_of(&exists_dir.path, &find_replace_mode("a", "b"))?;
        let a = find_item(&items, "a.txt")?;
        assert_eq!(a.status, Status::Conflict);
        assert_eq!(a.reason, "目标文件已存在");

        // 与其它文件目标名重复（小写比较）
        let duplicate_dir = TempDir::new("conflict-duplicate")?;
        write_file(&duplicate_dir.join("a1.txt"), "")?;
        write_file(&duplicate_dir.join("a2.txt"), "")?;
        let mode = RenameMode::FindReplace {
            find: "a[12]".to_string(),
            replace: "x".to_string(),
            use_regex: true,
            case_sensitive: true,
            whole_word: false,
            first_only: false,
            include_extension: false,
        };
        let items = preview_of(&duplicate_dir.path, &mode)?;
        for item in &items {
            assert_eq!(item.status, Status::Conflict);
            assert_eq!(item.reason, "与其它文件目标名重复");
        }

        // 非法字符 / 结尾点 / 保留名 / 过长
        let invalid_dir = TempDir::new("conflict-invalid")?;
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            write_file(&invalid_dir.join(name), "")?;
        }
        let items = preview_of(&invalid_dir.path, &find_replace_mode("a", "bad?"))?;
        let item = find_item(&items, "a.txt")?;
        assert_eq!(item.reason, "文件名包含非法字符");

        let items = preview_of(&invalid_dir.path, &find_replace_mode("b", "name."))?;
        // "name." + ".txt" → "name..txt"，合法；换一个无扩展名文件验证结尾点
        assert_eq!(find_item(&items, "b.txt")?.status, Status::Ok);

        let dots_dir = TempDir::new("conflict-dots")?;
        write_file(&dots_dir.join("x"), "")?;
        write_file(&dots_dir.join("y"), "")?;
        let items = preview_of(&dots_dir.path, &find_replace_mode("x", "bad."))?;
        let item = find_item(&items, "x")?;
        assert_eq!(item.status, Status::Conflict);
        assert_eq!(item.reason, "文件名以点或空格结尾");

        // 保留名
        let reserved_dir = TempDir::new("conflict-reserved")?;
        write_file(&reserved_dir.join("a.txt"), "")?;
        let items = preview_of(&reserved_dir.path, &find_replace_mode("a", "CON"))?;
        let item = find_item(&items, "a.txt")?;
        assert_eq!(item.status, Status::Conflict);
        assert_eq!(item.reason, "文件名是系统保留名");

        // 过长
        let long_dir = TempDir::new("conflict-long")?;
        write_file(&long_dir.join("a.txt"), "")?;
        let long_replacement = "x".repeat(MAX_FILENAME_BYTES + 5);
        let items = preview_of(&long_dir.path, &find_replace_mode("a", &long_replacement))?;
        let item = find_item(&items, "a.txt")?;
        assert_eq!(item.status, Status::Conflict);
        assert_eq!(item.reason, "文件名过长");

        // 目标路径是目录
        let dir_target_dir = TempDir::new("conflict-dir")?;
        write_file(&dir_target_dir.join("a.txt"), "")?;
        fs::create_dir_all(dir_target_dir.join("target.txt"))?;
        let items = preview_of(&dir_target_dir.path, &find_replace_mode("a", "target"))?;
        let item = find_item(&items, "a.txt")?;
        assert_eq!(item.status, Status::Conflict);
        assert_eq!(item.reason, "目标路径是目录");
        Ok(())
    }

    // ── 遍历：隐藏项 / 扩展名过滤 ──

    #[test]
    fn collection_skips_hidden_and_filters_extension() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("collect")?;
        for name in [
            "keep.JPG",
            "skip.txt",
            ".hidden.jpg",
            ".secret/inside.jpg",
            "sub/nested.jpg",
        ] {
            write_file(&dir.join(name), "")?;
        }

        let filter: Option<HashSet<String>> = Some(["jpg".to_string()].into_iter().collect());
        let entries = collect_files(&dir.path, false, &filter, &SortBy::Name)?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "keep.JPG");

        let entries = collect_files(&dir.path, true, &filter, &SortBy::Name)?;
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["keep.JPG", "nested.jpg"]);
        assert_eq!(entries[0].relative_dir, "");
        assert_eq!(entries[1].relative_dir, "sub/");
        Ok(())
    }

    // ── 两阶段执行：互换（a ↔ b）与撤销 ──

    #[test]
    fn two_phase_execute_swaps_names_and_undo_restores() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("swap")?;
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        write_file(&a, "AAA")?;
        write_file(&b, "BBB")?;

        let jobs = vec![
            MoveJob {
                id: 0,
                old_path: a.clone(),
                new_path: b.clone(),
            },
            MoveJob {
                id: 1,
                old_path: b.clone(),
                new_path: a.clone(),
            },
        ];
        let cancel = AtomicBool::new(false);
        let mut progress_values = Vec::new();
        let outcome = execute_renames(&jobs, &cancel, |current| progress_values.push(current))?;
        let (results, undo) = (outcome.results, outcome.undo);
        assert!(results.iter().all(|result| result.success));
        assert_eq!(progress_values, vec![1, 2]);
        assert_eq!(fs::read_to_string(&a)?, "BBB");
        assert_eq!(fs::read_to_string(&b)?, "AAA");
        assert_eq!(undo.len(), 2);

        let undo_results = undo_renames(&undo);
        assert!(
            undo_results.iter().all(|result| result.success),
            "撤销失败: {undo_results:?}"
        );
        assert_eq!(fs::read_to_string(&a)?, "AAA");
        assert_eq!(fs::read_to_string(&b)?, "BBB");
        Ok(())
    }

    // ── 两阶段执行：链式（a → b, b → c）与撤销 ──

    #[test]
    fn two_phase_execute_handles_rename_chain_and_undo() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("chain")?;
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        let c = dir.join("c.txt");
        write_file(&a, "A")?;
        write_file(&b, "B")?;

        let jobs = vec![
            MoveJob {
                id: 0,
                old_path: a.clone(),
                new_path: b.clone(),
            },
            MoveJob {
                id: 1,
                old_path: b.clone(),
                new_path: c.clone(),
            },
        ];
        let outcome = execute_renames(&jobs, &AtomicBool::new(false), |_| {})?;
        let (results, undo) = (outcome.results, outcome.undo);
        assert!(results.iter().all(|result| result.success));
        assert!(!a.exists());
        assert_eq!(fs::read_to_string(&b)?, "A");
        assert_eq!(fs::read_to_string(&c)?, "B");

        let undo_results = undo_renames(&undo);
        assert!(
            undo_results.iter().all(|result| result.success),
            "撤销失败: {undo_results:?}"
        );
        assert_eq!(fs::read_to_string(&a)?, "A");
        assert_eq!(fs::read_to_string(&b)?, "B");
        assert!(!c.exists());
        Ok(())
    }

    // ── 执行：普通直接改名 + 撤销 ──

    #[test]
    fn direct_execute_and_undo_restore() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("direct")?;
        let a = dir.join("a.txt");
        write_file(&a, "A")?;

        let jobs = vec![MoveJob {
            id: 0,
            old_path: a.clone(),
            new_path: dir.join("z.txt"),
        }];
        let outcome = execute_renames(&jobs, &AtomicBool::new(false), |_| {})?;
        let (results, undo) = (outcome.results, outcome.undo);
        assert!(results[0].success);
        assert_eq!(results[0].message, "z.txt");
        assert_eq!(undo.len(), 1);
        assert!(!a.exists());
        assert!(dir.join("z.txt").exists());

        let undo_results = undo_renames(&undo);
        assert!(undo_results[0].success);
        assert!(a.exists());
        assert!(!dir.join("z.txt").exists());
        Ok(())
    }

    // ── 执行：取消 ──

    #[test]
    fn execute_aborts_when_cancelled() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("cancel")?;
        let a = dir.join("a.txt");
        write_file(&a, "A")?;

        let jobs = vec![MoveJob {
            id: 0,
            old_path: a.clone(),
            new_path: dir.join("z.txt"),
        }];
        let cancel = AtomicBool::new(true);
        let outcome = execute_renames(&jobs, &cancel, |_| {})?;
        assert!(outcome.cancelled);
        assert!(
            outcome.undo.is_empty(),
            "取消时尚未完成的改名不应产生撤销记录"
        );
        assert_eq!(fs::read_to_string(&a)?, "A");
        assert!(!dir.join("z.txt").exists());
        Ok(())
    }

    // ── 执行：取消时已完成的部分仍可撤销 ──

    #[test]
    fn cancel_preserves_completed_undo_entries() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("cancel-undo")?;
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        let x = dir.join("x.txt");
        let y = dir.join("y.txt");
        write_file(&a, "A")?;
        write_file(&b, "B")?;

        let jobs = vec![
            MoveJob {
                id: 0,
                old_path: a.clone(),
                new_path: x.clone(),
            },
            MoveJob {
                id: 1,
                old_path: b.clone(),
                new_path: y.clone(),
            },
        ];
        // 第一个文件完成后请求取消，第二个文件应被跳过，但第一个要留下撤销记录
        let cancel = AtomicBool::new(false);
        let outcome = execute_renames(&jobs, &cancel, |current| {
            if current == 1 {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        })?;
        assert!(outcome.cancelled);
        assert_eq!(outcome.results.len(), 1);
        assert!(outcome.results[0].success);
        assert_eq!(outcome.undo.len(), 1);
        assert!(x.exists());
        assert!(b.exists());
        assert!(!y.exists());

        // 取消后历史里的成功项可以正常撤销
        let undo_results = undo_renames(&outcome.undo);
        assert!(undo_results.iter().all(|result| result.success));
        assert!(a.exists());
        assert!(!x.exists());
        Ok(())
    }

    // ── 执行：单个文件失败不中断 ──

    #[test]
    fn single_failure_does_not_abort_batch() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new("partial")?;
        let a = dir.join("a.txt");
        let missing = dir.join("missing.txt");
        let b = dir.join("b.txt");
        write_file(&a, "A")?;
        write_file(&b, "B")?;

        let jobs = vec![
            MoveJob {
                id: 0,
                old_path: a.clone(),
                new_path: dir.join("x.txt"),
            },
            MoveJob {
                id: 1,
                old_path: missing,
                new_path: dir.join("y.txt"),
            },
            MoveJob {
                id: 2,
                old_path: b.clone(),
                new_path: dir.join("w.txt"),
            },
        ];
        let outcome = execute_renames(&jobs, &AtomicBool::new(false), |_| {})?;
        let (results, undo) = (outcome.results, outcome.undo);
        assert_eq!(results.len(), 3);
        assert!(results[0].success);
        assert!(!results[1].success);
        assert!(results[1].message.contains("源文件不存在"));
        assert!(results[2].success);
        assert_eq!(undo.len(), 2);
        assert!(dir.join("x.txt").exists());
        assert!(dir.join("w.txt").exists());
        Ok(())
    }

    #[test]
    fn execute_refuses_to_overwrite_foreign_target() -> Result<(), Box<dyn std::error::Error>> {
        // 模拟「链式改名只勾选了前一项」：a→b 但 b 没有一起改名，不能覆盖 b
        let dir = TempDir::new("overwrite-guard")?;
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        write_file(&a, "AAA")?;
        write_file(&b, "BBB")?;

        let jobs = vec![MoveJob {
            id: 0,
            old_path: a.clone(),
            new_path: b.clone(),
        }];
        let outcome = execute_renames(&jobs, &AtomicBool::new(false), |_| {})?;
        let (results, undo) = (outcome.results, outcome.undo);
        assert!(!results[0].success);
        assert!(results[0].message.contains("目标文件已存在"));
        assert!(undo.is_empty());
        assert_eq!(fs::read_to_string(&a)?, "AAA");
        assert_eq!(fs::read_to_string(&b)?, "BBB");
        Ok(())
    }

    #[test]
    fn invalid_filename_validation_matrix() {
        assert_eq!(validate_filename(""), Some("生成的文件名为空"));
        assert_eq!(validate_filename("a/b.txt"), Some("文件名包含非法字符"));
        assert_eq!(validate_filename("a?b.txt"), Some("文件名包含非法字符"));
        assert_eq!(validate_filename("bad."), Some("文件名以点或空格结尾"));
        assert_eq!(validate_filename("bad "), Some("文件名以点或空格结尾"));
        assert_eq!(validate_filename("CON.txt"), Some("文件名是系统保留名"));
        assert_eq!(validate_filename("lpt9"), Some("文件名是系统保留名"));
        assert_eq!(validate_filename("normal.txt"), None);
        assert_eq!(
            validate_filename(&"x".repeat(MAX_FILENAME_BYTES + 1)),
            Some("文件名过长")
        );
    }
}
