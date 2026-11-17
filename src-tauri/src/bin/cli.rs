//! rename-rust-cli —— 命令行批量重命名工具
//!
//! 与 GUI 版共用同一套命名引擎（`engine` / `models`），规则、冲突检测、两阶段
//! 改名与执行结果完全一致。默认 **只预览、不改磁盘**；加 `--apply` 才真正执行。
//!
//! 支持四种命名模式（find / seq / pattern / suffix）、递归遍历、扩展名过滤、
//! 自然排序，以及 `--json` 便于脚本消费。
//!
//! 示例：
//! ```text
//! rename-rust-cli ~/Pictures --filter jpg,png --find IMG_ --replace PIC_ --apply
//! rename-rust-cli ~/Photos -r -m seq --prefix Trip --pad 3 --apply
//! rename-rust-cli ~/Downloads -m pattern --template '{date}_{name}' --json
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use anyhow::{bail, Context, Result};

use rename_rust_lib::engine::{self, MoveJob};
use rename_rust_lib::models::{
    ExecuteResult, NumberScope, PreviewItem, RenameMode, SortBy, Status,
};

const HELP: &str = "\
rename-rust-cli —— 批量重命名（默认只预览，加 --apply 才执行）

用法:
  rename-rust-cli <目录> [选项]

通用选项:
  -r, --recursive                    包含子目录
      --filter <扩展名>              白名单，逗号分隔（如 png,jpg），留空=全部
      --sort <name|modified|created> 排序方式，默认 name（自然排序）
  -m, --mode <find|seq|pattern|suffix>
                                     命名模式，默认 find
      --apply                        真正执行重命名（默认 dry-run 只预览）
      --json                         以 JSON 输出（预览数组 / 执行结果数组）
  -h, --help                         显示本帮助
  -V, --version                      显示版本

查找替换（-m find）:
      --find <文本>                  查找内容
      --replace <文本>               替换为（默认空）
      --regex                        按正则匹配（替换文本支持 $1 捕获组）
      --ignore-case                  不区分大小写
      --whole-word                   全词匹配
      --first-only                   只替换第一处
      --include-extension            连扩展名一起替换

序号命名（-m seq）:
      --prefix <文本>                前缀，默认 file
      --separator <文本>             连接符，默认 _
      --number-suffix <文本>         数字后的后缀，默认空
      --start <N>                    起始编号，默认 1
      --step <N>                     步长，默认 1
      --pad <N>                      补零位数，0=自动，默认 0
      --scope <dir|global>           编号范围，默认 dir（每个目录独立）

模板命名（-m pattern）:
      --template <模板>              默认 {name}_{index}
                                     占位符: {name} {original} {ext} {index}
                                             {date} {time} {parent}
                                     （--start/--step/--pad/--scope 同样适用）
统一后缀（-m suffix）:
      --target-ext <后缀>            目标后缀，默认 jpg（不带点号）
      --no-force                     不强制转换已有扩展名（只补无扩展名文件）
      --keep-case                    后缀不转小写

退出码:
  0  成功（预览模式始终为 0；执行模式无失败项）
  1  执行时有文件重命名失败
  2  参数错误或运行出错

示例:
  rename-rust-cli ~/Pictures --filter jpg,png --find IMG_ --replace PIC_ --apply
  rename-rust-cli ~/Photos -r -m seq --prefix Trip --start 1 --pad 3 --scope global --apply
  rename-rust-cli ~/Downloads -m pattern --template '{date}_{name}' --apply
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Find,
    Sequential,
    Pattern,
    Suffix,
}

impl Mode {
    fn parse(raw: &str) -> Result<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "find" | "find-replace" | "replace" => Ok(Mode::Find),
            "seq" | "sequential" | "number" => Ok(Mode::Sequential),
            "pattern" | "template" => Ok(Mode::Pattern),
            "suffix" | "ext" => Ok(Mode::Suffix),
            other => bail!("未知模式: {other}（可选 find / seq / pattern / suffix）"),
        }
    }
}

struct Args {
    root: PathBuf,
    recursive: bool,
    filter: Option<String>,
    sort: SortBy,
    mode: Mode,
    apply: bool,
    json: bool,

    // find
    find: String,
    replace: String,
    use_regex: bool,
    case_sensitive: bool,
    whole_word: bool,
    first_only: bool,
    include_extension: bool,

    // seq / pattern
    start: u64,
    step: u64,
    pad_width: usize,
    scope: NumberScope,
    prefix: String,
    separator: String,
    number_suffix: String,
    template: String,

    // suffix
    target_ext: String,
    force_all: bool,
    lowercase: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            recursive: false,
            filter: None,
            sort: SortBy::Name,
            mode: Mode::Find,
            apply: false,
            json: false,

            find: String::new(),
            replace: String::new(),
            use_regex: false,
            case_sensitive: true,
            whole_word: false,
            first_only: false,
            include_extension: false,

            start: 1,
            step: 1,
            pad_width: 0,
            scope: NumberScope::PerDir,
            prefix: "file".to_string(),
            separator: "_".to_string(),
            number_suffix: String::new(),
            template: "{name}_{index}".to_string(),

            target_ext: "jpg".to_string(),
            force_all: true,
            lowercase: true,
        }
    }
}

impl Args {
    fn to_mode(&self) -> RenameMode {
        match self.mode {
            Mode::Find => RenameMode::FindReplace {
                find: self.find.clone(),
                replace: self.replace.clone(),
                use_regex: self.use_regex,
                case_sensitive: self.case_sensitive,
                whole_word: self.whole_word,
                first_only: self.first_only,
                include_extension: self.include_extension,
            },
            Mode::Sequential => RenameMode::Sequential {
                prefix: self.prefix.clone(),
                separator: self.separator.clone(),
                start: self.start,
                step: self.step,
                pad_width: self.pad_width,
                suffix: self.number_suffix.clone(),
                scope: self.scope,
            },
            Mode::Pattern => RenameMode::Pattern {
                template: self.template.clone(),
                start: self.start,
                step: self.step,
                pad_width: self.pad_width,
                scope: self.scope,
            },
            Mode::Suffix => RenameMode::Suffix {
                target_suffix: self.target_ext.clone(),
                force_all: self.force_all,
                lowercase: self.lowercase,
            },
        }
    }

    fn validate(&self) -> Result<()> {
        if !self.root.is_dir() {
            bail!("目录不存在或不是文件夹: {}", self.root.display());
        }
        if self.step == 0 {
            bail!("--step 必须大于 0");
        }
        if self.pad_width > 128 {
            bail!("--pad 不能超过 128");
        }
        match self.mode {
            Mode::Find if self.find.is_empty() => {
                bail!("find 模式需要 --find <查找内容>（--help 查看用法）")
            }
            Mode::Pattern if self.template.is_empty() => bail!("pattern 模式需要非空的 --template"),
            Mode::Suffix if self.target_ext.trim().trim_start_matches('.').is_empty() => {
                bail!("suffix 模式需要非空的 --target-ext")
            }
            _ => {}
        }
        Ok(())
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("错误: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let Some(args) = parse_args(std::env::args().skip(1))? else {
        return Ok(ExitCode::SUCCESS);
    };
    args.validate()?;

    let filter = parse_filter(args.filter.as_deref());
    let mode = args.to_mode();
    let entries = engine::collect_files(&args.root, args.recursive, &filter, &args.sort)?;
    let preview = engine::compute_preview(&entries, &mode, &args.root)?;

    if args.json {
        let payload = if args.apply {
            let results = apply(&args.root, &preview)?;
            serde_json::to_value(&results)?
        } else {
            serde_json::to_value(&preview)?
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
        if args.apply && has_failure(&payload) {
            return Ok(ExitCode::from(1));
        }
        return Ok(ExitCode::SUCCESS);
    }

    print_preview(&preview);
    if !args.apply {
        println!("\n[预览模式] 未修改磁盘；确认无误后加 --apply 执行。");
        return Ok(ExitCode::SUCCESS);
    }

    let results = apply(&args.root, &preview)?;
    print_results(&preview, &results);
    if results.iter().any(|result| !result.success) {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

/// 把预览里状态为 ok 的项真正执行（两阶段改名由引擎内部处理）
fn apply(root: &Path, preview: &[PreviewItem]) -> Result<Vec<ExecuteResult>> {
    let jobs: Vec<MoveJob> = preview
        .iter()
        .filter(|item| item.status == Status::Ok)
        .map(|item| MoveJob {
            id: item.id,
            old_path: root.join(&item.relative_path).join(&item.old_name),
            new_path: root.join(&item.relative_path).join(&item.new_name),
        })
        .collect();

    if jobs.is_empty() {
        return Ok(Vec::new());
    }

    let cancel = AtomicBool::new(false);
    let outcome = engine::execute_renames(&jobs, &cancel, |_| {})?;
    Ok(outcome.results)
}

fn has_failure(payload: &serde_json::Value) -> bool {
    match payload.as_array() {
        Some(items) => items
            .iter()
            .any(|item| item.get("success").and_then(|value| value.as_bool()) == Some(false)),
        None => false,
    }
}

// ─── 输出 ────────────────────────────────────────────────

fn print_preview(preview: &[PreviewItem]) {
    if preview.is_empty() {
        println!("没有找到符合条件的文件。");
        return;
    }

    let mut ok = 0usize;
    let mut conflict = 0usize;
    let mut unchanged = 0usize;
    let mut width_path = 4usize;
    let mut width_old = 4usize;
    let mut width_new = 4usize;
    for item in preview {
        match item.status {
            Status::Ok => ok += 1,
            Status::Conflict => conflict += 1,
            Status::Unchanged => unchanged += 1,
        }
        width_path = width_path.max(display_width(rel_dir(&item.relative_path)));
        width_old = width_old.max(display_width(&item.old_name));
        width_new = width_new.max(display_width(&item.new_name));
    }

    println!(
        "{:<4} {}  {}  →  {}  状态",
        "",
        pad_to("相对路径", width_path),
        pad_to("原文件名", width_old),
        pad_to("新文件名", width_new)
    );
    println!(
        "{:<4} {}  {}     {}  {}",
        "",
        "-".repeat(width_path),
        "-".repeat(width_old),
        "-".repeat(width_new),
        "-".repeat(6)
    );

    for item in preview {
        let (mark, label) = match item.status {
            Status::Ok => ("[OK]", "待执行"),
            Status::Conflict => ("[!!]", "冲突"),
            Status::Unchanged => ("[--]", "无变化"),
        };
        let rel = rel_dir(&item.relative_path);
        let reason = if item.reason.is_empty() {
            String::new()
        } else {
            format!("  ({})", item.reason)
        };
        println!(
            "{mark:<4} {}  {}  →  {}  {label}{reason}",
            pad_to(rel, width_path),
            pad_to(&item.old_name, width_old),
            pad_to(&item.new_name, width_new)
        );
    }

    println!(
        "\n共 {} 个文件：待执行 {} · 冲突 {} · 无变化 {}",
        preview.len(),
        ok,
        conflict,
        unchanged
    );
}

fn print_results(preview: &[PreviewItem], results: &[ExecuteResult]) {
    let names: std::collections::HashMap<usize, &PreviewItem> =
        preview.iter().map(|item| (item.id, item)).collect();

    if results.is_empty() {
        println!("\n没有可执行的文件（没有状态为「待执行」的项）。");
        return;
    }

    println!("\n开始执行 {} 个文件…", results.len());
    let mut ok = 0usize;
    let mut failed = 0usize;
    for result in results {
        if result.success {
            ok += 1;
            let old = names
                .get(&result.id)
                .map(|item| item.old_name.clone())
                .unwrap_or_else(|| format!("#{}", result.id));
            println!("  ✓ {old}  →  {}", result.message);
        } else {
            failed += 1;
            println!("  ✗ #{}: {}", result.id, result.message);
        }
    }
    println!("\n完成：{ok} 成功 · {failed} 失败");
}

// ─── 参数解析 ────────────────────────────────────────────

fn parse_args(raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let argv: Vec<String> = raw.collect();
    let mut args = Args::default();
    let mut root: Option<PathBuf> = None;
    let mut only_positional = false;
    let mut index = 0usize;

    while index < argv.len() {
        let raw_arg = argv[index].clone();
        if only_positional {
            set_root(&mut root, &raw_arg)?;
            index += 1;
            continue;
        }
        if raw_arg == "--" {
            only_positional = true;
            index += 1;
            continue;
        }

        let (flag, inline_value) = split_flag(&raw_arg);
        match flag {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("rename-rust-cli {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "-r" | "--recursive" => args.recursive = true,
            "--apply" => args.apply = true,
            "--json" => args.json = true,
            "-m" | "--mode" => {
                let value = take_value(&argv, &mut index, inline_value, flag)?;
                args.mode = Mode::parse(&value)?;
            }
            "--filter" => args.filter = Some(take_value(&argv, &mut index, inline_value, flag)?),
            "--sort" => {
                let value = take_value(&argv, &mut index, inline_value, flag)?;
                args.sort = parse_sort(&value)?;
            }
            "--find" => args.find = take_value(&argv, &mut index, inline_value, flag)?,
            "--replace" => args.replace = take_value(&argv, &mut index, inline_value, flag)?,
            "--regex" => args.use_regex = true,
            "--ignore-case" => args.case_sensitive = false,
            "--whole-word" => args.whole_word = true,
            "--first-only" => args.first_only = true,
            "--include-extension" => args.include_extension = true,
            "--prefix" => args.prefix = take_value(&argv, &mut index, inline_value, flag)?,
            "--separator" => args.separator = take_value(&argv, &mut index, inline_value, flag)?,
            "--number-suffix" => {
                args.number_suffix = take_value(&argv, &mut index, inline_value, flag)?
            }
            "--template" => args.template = take_value(&argv, &mut index, inline_value, flag)?,
            "--start" => {
                args.start =
                    parse_number(&take_value(&argv, &mut index, inline_value, flag)?, flag)?
            }
            "--step" => {
                args.step = parse_number(&take_value(&argv, &mut index, inline_value, flag)?, flag)?
            }
            "--pad" => {
                let pad = parse_number(&take_value(&argv, &mut index, inline_value, flag)?, flag)?;
                args.pad_width = usize::try_from(pad).context("--pad 数值超出范围")?;
            }
            "--scope" => {
                let value = take_value(&argv, &mut index, inline_value, flag)?;
                args.scope = parse_scope(&value)?;
            }
            "--target-ext" => args.target_ext = take_value(&argv, &mut index, inline_value, flag)?,
            "--no-force" => args.force_all = false,
            "--keep-case" => args.lowercase = false,
            other if other.starts_with('-') => {
                bail!("未知选项: {other}（--help 查看用法）");
            }
            _ => set_root(&mut root, &raw_arg)?,
        }
        index += 1;
    }

    let root = root.ok_or_else(|| anyhow::anyhow!("缺少目录参数（--help 查看用法）"))?;
    args.root = root;
    Ok(Some(args))
}

fn set_root(root: &mut Option<PathBuf>, value: &str) -> Result<()> {
    if root.is_some() {
        bail!("只能指定一个目录（多余参数: {value}）");
    }
    *root = Some(PathBuf::from(value));
    Ok(())
}

/// 支持 `--opt=value`，返回 (flag, Some(value))
fn split_flag(raw: &str) -> (&str, Option<String>) {
    if raw.starts_with("--") {
        if let Some((flag, value)) = raw.split_once('=') {
            return (flag, Some(value.to_string()));
        }
    }
    (raw, None)
}

fn take_value(
    argv: &[String],
    index: &mut usize,
    inline_value: Option<String>,
    flag: &str,
) -> Result<String> {
    if let Some(value) = inline_value {
        return Ok(value);
    }
    *index += 1;
    argv.get(*index)
        .cloned()
        .with_context(|| format!("{flag} 缺少参数值"))
}

fn parse_number(raw: &str, flag: &str) -> Result<u64> {
    raw.parse::<u64>()
        .with_context(|| format!("{flag} 需要非负整数，收到: {raw}"))
}

fn parse_sort(raw: &str) -> Result<SortBy> {
    match raw.to_ascii_lowercase().as_str() {
        "name" => Ok(SortBy::Name),
        "modified" | "mtime" => Ok(SortBy::Modified),
        "created" | "ctime" => Ok(SortBy::Created),
        other => bail!("未知排序方式: {other}（可选 name / modified / created）"),
    }
}

fn parse_scope(raw: &str) -> Result<NumberScope> {
    match raw.to_ascii_lowercase().as_str() {
        "dir" | "per-dir" | "per_dir" | "directory" => Ok(NumberScope::PerDir),
        "global" | "all" => Ok(NumberScope::Global),
        other => bail!("未知编号范围: {other}（可选 dir / global）"),
    }
}

fn parse_filter(filter: Option<&str>) -> Option<HashSet<String>> {
    let filter = filter?.trim();
    if filter.is_empty() {
        return None;
    }
    let extensions: HashSet<String> = filter
        .split(',')
        .map(|ext| ext.trim().trim_start_matches('.').to_lowercase())
        .filter(|ext| !ext.is_empty())
        .collect();
    if extensions.is_empty() {
        None
    } else {
        Some(extensions)
    }
}

fn rel_dir(relative_path: &str) -> &str {
    if relative_path.is_empty() {
        "./"
    } else {
        relative_path
    }
}

/// 近似显示宽度：ASCII 记 1，其他字符（中文等）记 2。
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|character| if character.is_ascii() { 1 } else { 2 })
        .sum()
}

fn pad_to(text: &str, width: usize) -> String {
    let current = display_width(text);
    if current >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(items: &[&str]) -> Result<Option<Args>> {
        parse_args(items.iter().map(|item| item.to_string()))
    }

    #[test]
    fn parses_find_mode_and_flags() -> Result<()> {
        let args = parse(&[
            "/tmp/photos",
            "--filter",
            "png,jpg",
            "--mode",
            "find",
            "--find",
            "IMG_",
            "--replace",
            "PIC_",
            "--regex",
            "--ignore-case",
            "--apply",
        ])?
        .expect("不应是 help/version");
        assert_eq!(args.root, PathBuf::from("/tmp/photos"));
        assert_eq!(args.mode, Mode::Find);
        assert_eq!(args.filter.as_deref(), Some("png,jpg"));
        assert!(args.apply);
        assert!(!args.case_sensitive);
        assert!(args.use_regex);
        assert_eq!(args.find, "IMG_");
        assert_eq!(args.replace, "PIC_");
        Ok(())
    }

    #[test]
    fn parses_sequential_options_with_equals_syntax() -> Result<()> {
        let args = parse(&[
            "/tmp/photos",
            "-m",
            "seq",
            "--prefix=Trip",
            "--start=10",
            "--step=2",
            "--pad=4",
            "--scope",
            "global",
            "--number-suffix",
            "_raw",
        ])?
        .expect("参数有效");
        assert_eq!(args.mode, Mode::Sequential);
        assert_eq!(args.prefix, "Trip");
        assert_eq!((args.start, args.step, args.pad_width), (10, 2, 4));
        assert_eq!(args.scope, NumberScope::Global);
        assert_eq!(args.number_suffix, "_raw");
        Ok(())
    }

    #[test]
    fn defaults_match_documented_values() {
        let args = Args::default();
        assert_eq!(args.mode, Mode::Find);
        assert_eq!(args.prefix, "file");
        assert_eq!(args.separator, "_");
        assert_eq!(args.start, 1);
        assert_eq!(args.step, 1);
        assert_eq!(args.pad_width, 0);
        assert_eq!(args.scope, NumberScope::PerDir);
        assert_eq!(args.target_ext, "jpg");
        assert!(args.force_all);
        assert!(args.lowercase);
        assert!(args.case_sensitive);
    }

    #[test]
    fn help_and_version_return_none() -> Result<()> {
        assert!(parse(&["--help"])?.is_none());
        assert!(parse(&["-V"])?.is_none());
        Ok(())
    }

    #[test]
    fn rejects_unknown_option_and_missing_root() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--find", "a"]).is_err());
        assert!(parse(&["/a", "/b"]).is_err());
        assert!(parse(&["--mode", "bogus", "/a"]).is_err());
    }

    #[test]
    fn parses_sort_and_scope_aliases() -> Result<()> {
        assert_eq!(parse_sort("modified")?, SortBy::Modified);
        assert_eq!(parse_sort("ctime")?, SortBy::Created);
        assert_eq!(parse_scope("per-dir")?, NumberScope::PerDir);
        assert_eq!(parse_scope("global")?, NumberScope::Global);
        assert!(parse_sort("bogus").is_err());
        assert!(parse_scope("bogus").is_err());
        Ok(())
    }

    #[test]
    fn filter_parsing_normalizes_extensions() {
        let filter = parse_filter(Some(" .PNG, jpg ,, .MOV ")).expect("应解析出白名单");
        assert!(filter.contains("png"));
        assert!(filter.contains("jpg"));
        assert!(filter.contains("mov"));
        assert!(parse_filter(Some("  ")).is_none());
        assert!(parse_filter(None).is_none());
    }

    #[test]
    fn display_width_and_padding_are_cjk_aware() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("中文"), 4);
        assert_eq!(pad_to("a", 3), "a  ");
        assert_eq!(pad_to("中文", 4), "中文");
    }
}
