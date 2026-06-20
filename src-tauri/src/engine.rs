//! 重命名引擎 —— 文件遍历、名称计算、冲突检测等纯逻辑
//!
//! 此处所有函数不涉及 Tauri 相关类型，可独立单元测试。

use regex::Regex;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::models::{PreviewItem, RenameMode, RenamerError, SortBy, Status};

/// 文件遍历阶段使用的内部结构，预览计算时消费
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// 全局唯一 ID
    pub id: usize,
    /// 文件绝对路径
    pub full_path: String,
    /// 相对于根目录的父目录路径（如 "subdir/"）
    pub relative_dir: String,
    /// 文件名（含扩展名）
    pub name: String,
}

/// 遍历收集文件
///
/// 支持递归/非递归遍历、扩展名白名单过滤、按文件名或修改时间排序。
/// 跳过隐藏目录（以 `.` 开头）。
pub fn collect_files(
    root: &Path,
    recursive: bool,
    filter_exts: &Option<HashSet<String>>,
    sort_by: &SortBy,
) -> Result<Vec<FileEntry>, RenamerError> {
    let mut entries: Vec<FileEntry> = Vec::new();
    let mut id_counter: usize = 0;

    // 构建遍历器：非递归时限制深度为 1（仅当前目录）
    let mut builder = walkdir::WalkDir::new(root).same_file_system(true);
    if !recursive {
        builder = builder.max_depth(1);
    }
    // 过滤掉隐藏目录（以 . 开头的目录不进入）
    let walker = builder.into_iter().filter_entry(|e| {
        !e.file_name()
            .to_str()
            .map(|s| s.starts_with('.'))
            .unwrap_or(false)
    });

    for entry in walker {
        let entry = entry.map_err(|e| RenamerError::ReadDirFailed(e.to_string()))?;
        // 跳过非普通文件（目录、符号链接等）
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();

        // 扩展名白名单过滤
        if let Some(ref exts) = filter_exts {
            let file_ext = Path::new(&name)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !exts.contains(&file_ext) {
                continue;
            }
        }

        let full_path = entry.path().to_string_lossy().to_string();
        // 计算相对目录路径（保留子目录结构，末尾带 /）
        let relative_dir = entry
            .path()
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| {
                let s = p.to_string_lossy().to_string();
                if s.is_empty() {
                    String::new()
                } else {
                    format!("{}/", s)
                }
            })
            .unwrap_or_default();

        entries.push(FileEntry {
            id: id_counter,
            full_path,
            relative_dir,
            name,
        });
        id_counter += 1;
    }

    // 排序：先按目录分组，同一目录内按指定方式排序
    sort_entries(&mut entries, sort_by);

    Ok(entries)
}

/// 对文件列表排序：先按目录，再按文件名或修改时间
fn sort_entries(entries: &mut [FileEntry], sort_by: &SortBy) {
    match sort_by {
        SortBy::Name => {
            entries.sort_by(|a, b| {
                let dir_cmp = a.relative_dir.cmp(&b.relative_dir);
                if dir_cmp != std::cmp::Ordering::Equal {
                    dir_cmp
                } else {
                    a.name.cmp(&b.name)
                }
            });
        }
        SortBy::Modified => {
            entries.sort_by(|a, b| {
                let dir_cmp = a.relative_dir.cmp(&b.relative_dir);
                if dir_cmp != std::cmp::Ordering::Equal {
                    return dir_cmp;
                }
                let ma = std::fs::metadata(&a.full_path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let mb = std::fs::metadata(&b.full_path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                ma.cmp(&mb)
            });
        }
    }
}

/// 计算预览映射（纯内存计算，绝不修改磁盘）
///
/// 按父目录分组文件，每组独立编号。
/// 冲突检测依次检查：
/// 1. 新旧路径完全相同 → unchanged
/// 2. 新名称与其他项的计算结果重复 → conflict
/// 3. 目标文件已存在于磁盘（排除大小写重命名）→ conflict
/// 4. 新文件名包含非法字符 → conflict
/// 5. 以上均不满足 → ok
pub fn compute_preview(
    entries: &[FileEntry],
    mode: &RenameMode,
    root: &Path,
) -> Result<Vec<PreviewItem>, RenamerError> {
    // 按 relative_dir 分组：同一子目录的文件归为一组
    let mut groups: BTreeMap<String, Vec<&FileEntry>> = BTreeMap::new();
    for e in entries {
        groups.entry(e.relative_dir.clone()).or_default().push(e);
    }

    let mut preview: Vec<PreviewItem> = Vec::new();
    // 全局新文件名集合，用于检测跨组或前后项冲突
    let mut new_names_global: HashSet<String> = HashSet::new();
    // 正则缓存：同一次预览计算中复用已编译的正则
    let mut regex_cache: Option<Regex> = None;

    // 逐组计算
    for (_dir, group_entries) in &groups {
        let group_size = group_entries.len() as u64;
        // 根据组内文件数动态计算补零位数（如 120 个文件 → 3 位）
        let zero_pad_width = if group_size > 0 {
            (group_size as f64).log10().floor() as usize + 1
        } else {
            1
        };

        for (local_idx, entry) in group_entries.iter().enumerate() {
            let new_name = compute_new_name(
                &entry.name,
                mode,
                (local_idx + 1) as u64,
                zero_pad_width,
                &mut regex_cache,
            )?;

            let new_full = root
                .join(&entry.relative_dir)
                .join(&new_name)
                .to_string_lossy()
                .to_string();
            let old_full = &entry.full_path;

            // 冲突检测
            let status = if old_full == &new_full {
                Status::Unchanged
            } else if new_names_global.contains(&new_full) {
                Status::Conflict
            } else if is_genuine_conflict(old_full, &new_full) {
                Status::Conflict
            } else if !is_valid_filename(&new_name) {
                Status::Conflict
            } else {
                Status::Ok
            };

            new_names_global.insert(new_full.clone());
            preview.push(PreviewItem {
                id: entry.id,
                relative_path: entry.relative_dir.clone(),
                old_name: entry.name.clone(),
                new_name,
                status,
            });
        }
    }

    Ok(preview)
}

/// 根据重命名模式和组内索引，计算单个文件的新文件名
///
/// 参数：
/// - original: 原始文件名
/// - mode: 重命名模式
/// - index: 在组内的 1-indexed 序号
/// - zero_pad: 补零位数
/// - regex_cache: 可选的正则编译缓存，避免重复编译
pub fn compute_new_name(
    original: &str,
    mode: &RenameMode,
    index: u64,
    zero_pad: usize,
    regex_cache: &mut Option<Regex>,
) -> Result<String, RenamerError> {
    let path = Path::new(original);
    // 分离主文件名和扩展名
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(original);
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");

    let new_name = match mode {
        // 模式A：查找替换
        RenameMode::FindReplace {
            find,
            replace,
            use_regex,
        } => {
            if *use_regex {
                // 正则模式：编译并缓存正则对象
                if regex_cache.is_none() {
                    *regex_cache = Some(
                        Regex::new(find)
                            .map_err(|e| RenamerError::InvalidRegex(e.to_string()))?,
                    );
                }
                let re = regex_cache.as_ref().unwrap();
                let new_stem = re.replace_all(stem, replace.as_str()).to_string();
                if ext.is_empty() {
                    new_stem
                } else {
                    format!("{}.{}", new_stem, ext)
                }
            } else {
                // 普通文本替换
                let new_stem = stem.replace(find, replace);
                if ext.is_empty() {
                    new_stem
                } else {
                    format!("{}.{}", new_stem, ext)
                }
            }
        }
        // 模式B：序号命名（前缀 + 连接符 + 补零数字）
        RenameMode::Sequential {
            prefix,
            separator,
            start,
        } => {
            let num = start + index - 1;
            let padded = format!("{:0width$}", num, width = zero_pad);
            if ext.is_empty() {
                format!("{}{}{}", prefix, separator, padded)
            } else {
                format!("{}{}{}.{}", prefix, separator, padded, ext)
            }
        }
        // 模式C：统一后缀
        RenameMode::Suffix {
            target_suffix,
            force_all,
            lowercase,
        } => {
            let new_ext = if *lowercase {
                target_suffix.to_lowercase()
            } else {
                target_suffix.clone()
            };

            if !force_all {
                // 不强制转换：仅在无后缀时添加，或后缀已匹配时保持原样
                if ext.is_empty() {
                    format!("{}.{}", stem, new_ext)
                } else if ext.to_lowercase() == new_ext.to_lowercase() {
                    format!("{}.{}", stem, ext) // 相同后缀，保持原始大小写
                } else {
                    format!("{}.{}", stem, new_ext)
                }
            } else {
                format!("{}.{}", stem, new_ext)
            }
        }
    };

    Ok(new_name)
}

/// 检查文件名是否合法（不含操作系统禁用字符）
pub fn is_valid_filename(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // Windows/macOS/Linux 通用禁用字符
    let banned = ['<', '>', ':', '"', '|', '?', '*', '\0', '/', '\\'];
    !name.chars().any(|c| banned.contains(&c))
        && !name.ends_with('.')
        && !name.ends_with(' ')
}

/// 判断目标路径是否与源文件不同（真正的冲突）
///
/// 在大小写不敏感文件系统（如 macOS APFS）上，
/// `a.JPG` → `a.jpg` 时旧路径检测认为 `a.jpg` 已存在，
/// 但实际上它是同一个文件。通过 canonicalize 比对 inode 来解决。
pub fn is_genuine_conflict(old_path: &str, new_path: &str) -> bool {
    let new_p = Path::new(new_path);
    if !new_p.exists() {
        return false; // 目标不存在，无冲突
    }
    match (
        std::fs::canonicalize(old_path),
        std::fs::canonicalize(new_path),
    ) {
        (Ok(o), Ok(n)) if o == n => {
            // 解析到同一 inode → 仅大小写重命名，允许
            false
        }
        _ => {
            // 不同文件或解析失败 → 真正的冲突
            true
        }
    }
}
