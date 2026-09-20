//! 项目向导：新建项目、装载模板、扫描已有文件夹。
//!
//! 红线（需求文档 §7.12 FR-L）：扫描已有文件夹**只读**，
//! 绝不移动、不改名、不删除用户的原文件——要迁入必须由用户明确选择。

use std::path::{Path, PathBuf};

use clap::Parser;
use serde_json::{json, Value};

use cercis_kit_core::cli;
use cercis_kit_core::error::{KitError, Result};
use cercis_kit_core::kernel::Kernel;

/// 向导参数。
pub struct WizardOptions {
    pub project: PathBuf,
    pub name: String,
    /// 模板名；留空表示只建项目（内核默认通用模板）
    pub template: String,
    pub user: String,
    pub record_device: bool,
    /// 可选：扫描一个已有文件夹，给出"可导入清单"（只读）
    pub scan: Option<PathBuf>,
}

/// 执行向导。
pub fn run(kernel: &Kernel, opts: &WizardOptions) -> Result<Value> {
    if opts.name.trim().is_empty() {
        return Err(KitError::with_hint(
            "项目名称不能为空",
            "例如 --name 我的项目",
        ));
    }
    if opts.project.join("project.yaml").exists() {
        return Err(KitError::with_hint(
            format!("该目录已经是一个 Cercis 项目：{}", opts.project.display()),
            "换个空目录，或直接打开它",
        ));
    }

    // 1. 建项目（内核命令：目录骨架 + project.yaml + 通用模板）
    let mut init_args = vec![
        "init".to_string(),
        opts.project.display().to_string(),
        "--name".to_string(),
        opts.name.clone(),
        "--user".to_string(),
        if opts.user.is_empty() {
            "local-user".to_string()
        } else {
            opts.user.clone()
        },
    ];
    if opts.record_device {
        init_args.push("--record-device".to_string());
    }
    let created = kernel.json(&init_args)?;

    // 2. 装模板（表结构与字段语义来自模板，内核不认识任何学科词）
    let mut template_name = String::from("generic");
    let mut created_tables: Vec<String> = Vec::new();
    if !opts.template.trim().is_empty() {
        let installed = kernel.json(&[
            "install-template".to_string(),
            opts.project.display().to_string(),
            "--name".to_string(),
            opts.template.clone(),
        ])?;
        template_name = installed
            .get("template")
            .and_then(|v| v.as_str())
            .unwrap_or(&opts.template)
            .to_string();
        created_tables = installed
            .get("created_tables")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|i| i.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
    }

    // 3. 生成 AI 材料（AGENTS.md / manifest.json / ai_contract.json）
    let _ = kernel.json(&[
        "agents".to_string(),
        opts.project.display().to_string(),
    ]);

    // 4. 可选：扫描已有文件夹（只读）
    let scan = match &opts.scan {
        Some(dir) => Some(scan_folder(dir)?),
        None => None,
    };

    Ok(json!({
        "ok": true,
        "project": opts.project.display().to_string(),
        "name": created.get("name").and_then(|v| v.as_str()).unwrap_or(&opts.name),
        "template": template_name,
        "created_tables": created_tables,
        "scan": scan,
        "next": [
            "用 app 打开这个项目文件夹",
            "录入数据：cercis-plugin-entry append --project <目录> --table <表> --set 列=值 --allow-write",
            "体检：cercis-plugin-health --project <目录>"
        ],
    }))
}

/// 扫描已有文件夹，列出可以导入的表与媒体文件（**只读**）。
pub fn scan_folder(dir: &Path) -> Result<Value> {
    if !dir.is_dir() {
        return Err(KitError::with_hint(
            format!("要扫描的文件夹不存在：{}", dir.display()),
            "给一个存在的目录路径",
        ));
    }

    let mut tables = Vec::new();
    let mut media = Vec::new();
    let mut unsupported = Vec::new();
    walk(dir, dir, 0, &mut tables, &mut media, &mut unsupported)?;

    Ok(json!({
        "root": dir.display().to_string(),
        "tables": tables,
        "media": media,
        "unsupported": unsupported,
        "note": "只读扫描：未移动、未修改任何原文件",
    }))
}

const MAX_DEPTH: usize = 4;
const MEDIA_EXT: [&str; 10] = [
    "jpg", "jpeg", "png", "tif", "tiff", "heic", "mp4", "mov", "wav", "pdf",
];

fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    tables: &mut Vec<Value>,
    media: &mut Vec<Value>,
    unsupported: &mut Vec<String>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, depth + 1, tables, media, unsupported)?;
            continue;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();

        match ext.as_str() {
            "csv" | "tsv" => {
                let delimiter = if ext == "tsv" { b'\t' } else { b',' };
                let meta = peek_table(&path, delimiter);
                tables.push(json!({
                    "file": rel,
                    "suggested_table": suggest_table_name(&path),
                    "rows": meta.0,
                    "columns": meta.1,
                    "error": meta.2,
                }));
            }
            "xlsx" | "xls" => unsupported.push(format!(
                "{rel}（Excel 需要科研工具包的 xlsx 向导；本批基础工具包只做 CSV/TSV）"
            )),
            other if MEDIA_EXT.contains(&other) => {
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                media.push(json!({ "file": rel, "bytes": bytes }));
            }
            _ => {}
        }
    }
    Ok(())
}

/// 快速看一眼表格：行数、列名、以及解析错误（不读全文件内容以外的动作）。
fn peek_table(path: &Path, delimiter: u8) -> (usize, Vec<String>, Option<String>) {
    match cercis_kit_core::model::read_delimited(path, delimiter) {
        Ok(t) => (t.rows.len(), t.headers, None),
        Err(e) => (0, vec![], Some(e.message)),
    }
}

/// 用文件名猜一个内核可接受的表名（小写 ASCII 下划线）。
pub fn suggest_table_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut out = String::new();
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() || ch == '-' || ch == '.' {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "table".to_string()
    } else if out.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        format!("t_{out}")
    } else {
        out
    }
}

/// 命令行入口（app 也用同一条命令调用本插件）。
#[derive(Parser, Debug)]
#[command(
    name = "cercis-plugin-wizard",
    version,
    about = "项目向导：新建项目 · 选模板 · 扫描已有文件夹（只读）"
)]
struct Cli {
    /// 机器可读输出（app 与脚本使用）
    #[arg(long, global = true)]
    json: bool,
    /// 内核可执行文件（默认 CERCIS_BIN 或 PATH 里的 cercis）
    #[arg(long, global = true)]
    cercis: Option<PathBuf>,
    /// 允许写入：建项目会写文件，需显式授权
    #[arg(long, global = true)]
    allow_write: bool,
    /// 项目目录（不存在则创建）
    #[arg(long)]
    project: PathBuf,
    /// 项目名称
    #[arg(long)]
    name: String,
    /// 模板名：generic / example-study
    #[arg(long, default_value = "generic")]
    template: String,
    /// 用户名（记入变更流水）
    #[arg(long, default_value = "local-user")]
    user: String,
    /// 是否把设备信息写入流水
    #[arg(long)]
    record_device: bool,
    /// 扫描一个已有文件夹，列出可导入的表与媒体（只读，不动原文件）
    #[arg(long)]
    scan: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();
    let kernel = match Kernel::locate(cli.cercis.as_deref()) {
        Ok(k) => k,
        Err(e) => cli::fail(cli.json, &e),
    };
    if let Err(e) = cli::require_write(cli.allow_write, "新建项目") {
        cli::fail(cli.json, &e);
    }

    let opts = WizardOptions {
        project: cli.project.clone(),
        name: cli.name.clone(),
        template: cli.template.clone(),
        user: cli.user.clone(),
        record_device: cli.record_device,
        scan: cli.scan.clone(),
    };

    match run(&kernel, &opts) {
        Ok(value) => cli::emit(cli.json, &value, human),
        Err(e) => cli::fail(cli.json, &e),
    }
}

fn human(v: &Value) {
    println!("已创建项目：{}", v["project"].as_str().unwrap_or(""));
    println!("项目名称：{}", v["name"].as_str().unwrap_or(""));
    println!("模板：{}", v["template"].as_str().unwrap_or(""));
    if let Some(tables) = v["created_tables"].as_array() {
        if !tables.is_empty() {
            let names: Vec<&str> = tables.iter().filter_map(|t| t.as_str()).collect();
            println!("新建表：{}", names.join("、"));
        }
    }
    if let Some(scan) = v.get("scan").filter(|s| !s.is_null()) {
        let tables = scan["tables"].as_array().map(|a| a.len()).unwrap_or(0);
        let media = scan["media"].as_array().map(|a| a.len()).unwrap_or(0);
        println!("\n扫描 {}：", scan["root"].as_str().unwrap_or(""));
        println!("  可导入的表：{tables} 个");
        println!("  媒体文件：{media} 个");
        if tables > 0 {
            println!("  建议的表名与行数：");
            for t in scan["tables"].as_array().into_iter().flatten() {
                println!(
                    "    {} → {}（{} 行，{} 列）",
                    t["file"].as_str().unwrap_or(""),
                    t["suggested_table"].as_str().unwrap_or(""),
                    t["rows"].as_u64().unwrap_or(0),
                    t["columns"].as_array().map(|c| c.len()).unwrap_or(0)
                );
            }
        }
        if let Some(unsupported) = scan["unsupported"].as_array() {
            for u in unsupported {
                println!("  需要别的工具包：{}", u.as_str().unwrap_or(""));
            }
        }
        println!("  （只读扫描：未移动、未修改任何原文件）");
    }
    println!("\n下一步：");
    for step in v["next"].as_array().into_iter().flatten() {
        println!("  - {}", step.as_str().unwrap_or(""));
    }
}
