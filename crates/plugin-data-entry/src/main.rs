//! 通用录入：批量粘贴多行、单行录入、附件挂接。
//!
//! 录入是**写入**，因此：
//! - 必须先做行级检查（列名、必填、主键重复），问题在写入前就拦住；
//! - 写入必须走内核通道（`append-many`），由内核保证原子与留痕；
//! - 附件挂接时"复制文件"由本工具包做，"写索引行"仍走内核。

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use cercis_kit_core::checks::{check_rows, collect_key_values, drop_empty_rows};
use cercis_kit_core::cli;
use cercis_kit_core::error::{KitError, Result};
use cercis_kit_core::kernel::Kernel;
use cercis_kit_core::model::{self, Row};

/// 录入参数。
pub struct AppendOptions {
    pub project: PathBuf,
    pub table: String,
    /// 数据来源：CSV/TSV/JSON 文件、`-`（标准输入）或内联 JSON
    pub from: Option<String>,
    /// 单行录入：`--set 列=值`
    pub sets: Vec<String>,
    pub dry_run: bool,
    pub allow_new_columns: bool,
}

/// 录入多行（一次事务、一条流水）。
pub fn append(kernel: &Kernel, opts: &AppendOptions) -> Result<Value> {
    let mut headers: Vec<String> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut source = String::new();

    if let Some(from) = &opts.from {
        let input = model::read_input(from)?;
        source = input.source.clone();
        headers = input.headers;
        rows = input.rows;
    }
    if !opts.sets.is_empty() {
        let single = model::parse_sets(&opts.sets)?;
        for k in single.keys() {
            if !headers.contains(k) {
                headers.push(k.clone());
            }
        }
        rows.push(single);
    }
    if rows.is_empty() {
        return Err(KitError::with_hint(
            "没有要录入的数据",
            "用 --from 给文件（CSV/TSV/JSON），或用 --set 列=值 录入一行",
        ));
    }

    let (rows, dropped_empty) = drop_empty_rows(rows);
    if rows.is_empty() {
        return Err(KitError::with_hint(
            "所有行都是空行，没有可录入的数据",
            "检查文件是否只含空行",
        ));
    }

    let schema = kernel.table_schema(&opts.project, &opts.table)?;
    let existing = if schema.has_primary_key() {
        let existing_rows = kernel.fetch_rows(&opts.project, &opts.table, &[], None)?;
        collect_key_values(&existing_rows, &schema.primary_key)
    } else {
        Default::default()
    };
    let check = check_rows(&schema, &rows, &existing, opts.allow_new_columns);
    if !check.is_ok() {
        return Err(KitError::with_hint(
            format!(
                "有 {} 处问题，录入已取消（数据未被改动）：\n- {}",
                check.errors.len(),
                check.errors.join("\n- ")
            ),
            "修正后用 --dry-run 再试一次",
        ));
    }
    // 引用完整性：模板声明了 reference 的列，值必须指向真实存在的记录
    let ref_issues =
        cercis_kit_core::refs::check_references(kernel, &opts.project, &opts.table, &schema, &rows)?;
    if !ref_issues.is_empty() {
        return Err(KitError::with_hint(
            format!(
                "引用检查未通过，录入已取消（数据未被改动）：\n- {}",
                ref_issues.join("\n- ")
            ),
            "先录入被引用的记录（例如先建研究、再录实验单元），或修正编号",
        ));
    }

    let summary = format!(
        "录入 {} 行到表 {}（工具包：通用录入）",
        rows.len(),
        opts.table
    );
    let outcome = kernel.append_many(
        &opts.project,
        &opts.table,
        &rows,
        "create",
        &summary,
        &source,
        opts.allow_new_columns,
        opts.dry_run,
    )?;

    Ok(json!({
        "ok": true,
        "action": "entry.append",
        "table": opts.table,
        "rows": rows.len(),
        "dropped_empty_rows": dropped_empty,
        "columns": headers,
        "warnings": check.warnings,
        "dry_run": opts.dry_run,
        "kernel": outcome,
    }))
}

/// 附件挂接参数。
pub struct AttachOptions {
    pub project: PathBuf,
    pub table: String,
    pub file: PathBuf,
    /// 存放相对路径的列名（默认 path）
    pub path_col: String,
    pub sets: Vec<String>,
    pub dry_run: bool,
}

/// 把文件复制进项目并写一行索引（模板里若有相应列则自动填充）。
pub fn attach(kernel: &Kernel, opts: &AttachOptions) -> Result<Value> {
    if !opts.file.is_file() {
        return Err(KitError::with_hint(
            format!("要挂接的文件不存在：{}", opts.file.display()),
            "确认路径",
        ));
    }
    let schema = kernel.table_schema(&opts.project, &opts.table)?;
    let bytes = std::fs::read(&opts.file)?;
    let sha256 = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        hex::encode(h.finalize())
    };

    let original_name = opts
        .file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".to_string());
    let safe_name = sanitize_file_name(&original_name);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let year = chrono::Local::now().format("%Y").to_string();

    // 同名文件：内容相同则复用，内容不同则加序号，绝不覆盖
    let mut rel = format!("media/{year}/{today}/{safe_name}");
    let mut counter = 1;
    loop {
        let abs = opts.project.join(&rel);
        if !abs.exists() {
            break;
        }
        let same = std::fs::read(&abs)
            .map(|b| {
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(&b);
                hex::encode(h.finalize()) == sha256
            })
            .unwrap_or(false);
        if same {
            break;
        }
        counter += 1;
        rel = format!("media/{year}/{today}/{counter}-{safe_name}");
    }
    let dest = opts.project.join(&rel);
    let already_there = dest.exists();

    // 组装索引行：用户给的 --set + 自动填充（只填模板里确实存在的列）
    let mut row = model::parse_sets(&opts.sets)?;
    let path_col = if opts.path_col.is_empty() {
        "path".to_string()
    } else {
        opts.path_col.clone()
    };
    row.insert(path_col.clone(), rel.clone());
    if schema.has_column("sha256") {
        row.insert("sha256".into(), sha256.clone());
    }
    if schema.has_column("original_path") {
        row.insert(
            "original_path".into(),
            opts.file.display().to_string(),
        );
    }
    if schema.has_column("mode") && !row.contains_key("mode") {
        row.insert("mode".into(), "imported".into());
    }
    if schema.has_column("caption") && !row.contains_key("caption") {
        row.insert("caption".into(), String::new());
    }

    let existing = if schema.has_primary_key() {
        let existing_rows = kernel.fetch_rows(&opts.project, &opts.table, &[], None)?;
        collect_key_values(&existing_rows, &schema.primary_key)
    } else {
        Default::default()
    };
    let check = check_rows(&schema, std::slice::from_ref(&row), &existing, false);
    if !check.is_ok() {
        return Err(KitError::with_hint(
            format!("挂接前检查未通过：\n- {}", check.errors.join("\n- ")),
            "补上缺少的字段（例如用 --set 指定主键与关联对象）",
        ));
    }
    let ref_issues =
        cercis_kit_core::refs::check_references(kernel, &opts.project, &opts.table, &schema, std::slice::from_ref(&row))?;
    if !ref_issues.is_empty() {
        return Err(KitError::with_hint(
            format!("挂接前引用检查未通过：\n- {}", ref_issues.join("\n- ")),
            "确认关联对象（如样本、事件）已经录入",
        ));
    }

    if !opts.dry_run && !already_there {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&opts.file, &dest)?;
    }

    let summary = format!(
        "挂接附件 {} 到表 {}（{} 字节）",
        rel,
        opts.table,
        bytes.len()
    );
    let outcome = if opts.dry_run {
        Value::Null
    } else {
        kernel.append_many(
            &opts.project,
            &opts.table,
            std::slice::from_ref(&row),
            "create",
            &summary,
            &rel,
            false,
            false,
        )?
    };

    Ok(json!({
        "ok": true,
        "action": "entry.attach",
        "table": opts.table,
        "file": rel,
        "bytes": bytes.len(),
        "sha256": sha256,
        "reused_existing": already_there,
        "row": row,
        "warnings": check.warnings,
        "dry_run": opts.dry_run,
        "kernel": outcome,
    }))
}

/// 文件名安全化：去掉路径分隔符与首尾空白，空格换成 `-`。
fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '_',
            ' ' => '-',
            other => other,
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').trim().to_string();
    if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned
    }
}

/// 供 app 预览用：把一个路径显示成"项目内相对路径"。
pub fn relative_display(project: &Path, path: &Path) -> String {
    path.strip_prefix(project)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_file_names() {
        assert_eq!(sanitize_file_name("sample 01.jpg"), "sample-01.jpg");
        assert_eq!(sanitize_file_name("a/b\\c.png"), "a_b_c.png");
        assert_eq!(sanitize_file_name("   "), "attachment");
    }

    #[test]
    fn relative_paths_render_inside_project() {
        let project = Path::new("/tmp/proj");
        let file = Path::new("/tmp/proj/media/2026/x.jpg");
        assert_eq!(relative_display(project, file), "media/2026/x.jpg");
    }
}

/// 命令行入口（app 也用同一条命令调用本插件）。
#[derive(Parser, Debug)]
#[command(
    name = "cercis-plugin-entry",
    version,
    about = "通用录入：批量粘贴 · 单行录入 · 附件挂接"
)]
struct Cli {
    /// 机器可读输出（app 与脚本使用）
    #[arg(long, global = true)]
    json: bool,
    /// 内核可执行文件（默认 CERCIS_BIN 或 PATH 里的 cercis）
    #[arg(long, global = true)]
    cercis: Option<PathBuf>,
    /// 允许写入（由 app 按插件权限授予；试运行不需要）
    #[arg(long, global = true)]
    allow_write: bool,
    #[command(subcommand)]
    command: EntryCommand,
}

#[derive(Subcommand, Debug)]
enum EntryCommand {
    /// 录入多行：来自 CSV/TSV/JSON 文件、标准输入（-）或 --set
    Append(AppendArgs),
    /// 附件挂接：复制文件进项目，并写一行索引
    Attach(AttachArgs),
}

#[derive(clap::Args, Debug)]
struct AppendArgs {
    /// 项目目录
    #[arg(long)]
    project: PathBuf,
    /// 目标表名
    #[arg(long)]
    table: String,
    /// 数据来源：CSV/TSV/JSON 文件、`-`（标准输入）或内联 JSON
    #[arg(long)]
    from: Option<String>,
    /// 单行录入：列=值（可重复）
    #[arg(long = "set", value_name = "列=值")]
    sets: Vec<String>,
    /// 试运行：只检查不写入
    #[arg(long)]
    dry_run: bool,
    /// 允许写入模板未定义的列（默认拒绝）
    #[arg(long)]
    allow_new_columns: bool,
}

#[derive(clap::Args, Debug)]
struct AttachArgs {
    /// 项目目录
    #[arg(long)]
    project: PathBuf,
    /// 存放附件的表（通常是模板里的媒体表）
    #[arg(long)]
    table: String,
    /// 要挂接的文件
    #[arg(long)]
    file: PathBuf,
    /// 存放相对路径的列名
    #[arg(long, default_value = "path")]
    path_col: String,
    /// 其他字段：列=值（可重复），如 --set sample_id=S-001
    #[arg(long = "set", value_name = "列=值")]
    sets: Vec<String>,
    /// 试运行：只检查不复制、不写入
    #[arg(long)]
    dry_run: bool,
}

fn main() {
    let cli = Cli::parse();
    let kernel = match Kernel::locate(cli.cercis.as_deref()) {
        Ok(k) => k,
        Err(e) => cli::fail(cli.json, &e),
    };

    let result = match &cli.command {
        EntryCommand::Append(a) => {
            if !a.dry_run {
                if let Err(e) = cli::require_write(cli.allow_write, "录入数据") {
                    cli::fail(cli.json, &e);
                }
            }
            append(
                &kernel,
                &AppendOptions {
                    project: a.project.clone(),
                    table: a.table.clone(),
                    from: a.from.clone(),
                    sets: a.sets.clone(),
                    dry_run: a.dry_run,
                    allow_new_columns: a.allow_new_columns,
                },
            )
            .map(|v| (v, Human::Append))
        }
        EntryCommand::Attach(a) => {
            if !a.dry_run {
                if let Err(e) = cli::require_write(cli.allow_write, "挂接附件") {
                    cli::fail(cli.json, &e);
                }
            }
            attach(
                &kernel,
                &AttachOptions {
                    project: a.project.clone(),
                    table: a.table.clone(),
                    file: a.file.clone(),
                    path_col: a.path_col.clone(),
                    sets: a.sets.clone(),
                    dry_run: a.dry_run,
                },
            )
            .map(|v| (v, Human::Attach))
        }
    };

    match result {
        Ok((value, kind)) => cli::emit(cli.json, &value, |v| human(kind, v)),
        Err(e) => cli::fail(cli.json, &e),
    }
}

#[derive(Clone, Copy)]
enum Human {
    Append,
    Attach,
}

fn human(kind: Human, v: &Value) {
    match kind {
        Human::Append => {
            let rows = v["rows"].as_u64().unwrap_or(0);
            let table = v["table"].as_str().unwrap_or("");
            if v["dry_run"].as_bool().unwrap_or(false) {
                println!("试运行通过：将录入 {rows} 行到表 {table}（未写入）");
            } else {
                println!("已录入 {rows} 行到表 {table}（一次事务、一条流水）");
            }
            if let Some(dropped) = v["dropped_empty_rows"].as_u64() {
                if dropped > 0 {
                    println!("跳过空行：{dropped}");
                }
            }
            for w in v["warnings"].as_array().into_iter().flatten() {
                println!("  提醒：{}", w.as_str().unwrap_or(""));
            }
            if let Some(batch) = v["kernel"].get("batch_id").and_then(|b| b.as_str()) {
                println!("批次号：{batch}");
            }
        }
        Human::Attach => {
            let file = v["file"].as_str().unwrap_or("");
            let bytes = v["bytes"].as_u64().unwrap_or(0);
            if v["dry_run"].as_bool().unwrap_or(false) {
                println!("试运行：将把附件放到 {file}（{bytes} 字节），未复制、未写入");
            } else if v["reused_existing"].as_bool().unwrap_or(false) {
                println!("附件已存在（内容相同），复用 {file} 并写入索引行");
            } else {
                println!("已挂接附件：{file}（{bytes} 字节）");
            }
            println!("指纹：{}", v["sha256"].as_str().unwrap_or(""));
            for w in v["warnings"].as_array().into_iter().flatten() {
                println!("  提醒：{}", w.as_str().unwrap_or(""));
            }
        }
    }
}
