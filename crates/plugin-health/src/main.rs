//! 通用体检：把"这堆文件夹还能不能放心用"一次说清楚。
//!
//! 查六类问题（对应需求文档 §14.4 基础工具包的"通用体检"）：
//! 目录骨架、引用断链与字段规则、CSV 编码、命名冲突、附件孤儿/缺失、空表。
//!
//! 体检**只读**：不改数据、不写流水；发现的问题都带位置与处理建议。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use cercis_kit_core::error::{KitError, Result};
use cercis_kit_core::kernel::{table_schema, Kernel, TableSchema};

/// 一条体检结论。
pub struct Finding {
    /// error（必须处理）/ warn（建议处理）/ info（知道就好）
    pub level: &'static str,
    /// 机器可读的问题代号，便于 app 分类与测试
    pub code: &'static str,
    /// 问题位置（文件或表.列）
    pub target: String,
    pub message_cn: String,
    pub message_en: String,
}

/// 执行体检。
pub fn run(kernel: &Kernel, project: &Path) -> Result<Value> {
    if !project.join("project.yaml").exists() {
        return Err(KitError::with_hint(
            format!("这里不是 Cercis 项目：{}", project.display()),
            "选择含 project.yaml 的项目文件夹",
        ));
    }

    let mut findings: Vec<Finding> = Vec::new();

    // 0. 类型规则：确认内核提供了类型清单（体检与导入都按它判类型，见 §5.9 / D36）
    match cercis_kit_core::types::TypeCatalog::from_kernel(kernel) {
        Ok(catalog) => {
            let names = catalog.names();
            if names.len() != 7 {
                findings.push(Finding {
                    level: "warn",
                    code: "types.contract",
                    target: "内核".to_string(),
                    message_cn: format!(
                        "内核给的类型清单是 {} 项（预期 7 项：{}）——内核与工具包版本可能不匹配",
                        names.len(),
                        names.join(" / ")
                    ),
                    message_en: format!(
                        "Kernel type list has {} entries (expected 7): {}",
                        names.len(),
                        names.join(" / ")
                    ),
                });
            }
        }
        Err(e) => findings.push(Finding {
            level: "warn",
            code: "types.unavailable",
            target: "内核".to_string(),
            message_cn: format!(
                "拿不到内核的类型清单（{}）——类型校验可能对不上，建议升级内核",
                e.message
            ),
            message_en: format!("Kernel type list unavailable: {}", e.message),
        }),
    }

    // 0.5 插件状态（D37/D38/D40）：停用、未生效、键冲突、撤不回去的登记
    if let Ok(found) = kernel.json(&["plugins".into(), project.display().to_string()]) {
        if let Some(list) = found.get("plugins").and_then(|v| v.as_array()) {
            for p in list {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if p.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false) {
                    findings.push(Finding {
                        level: "info",
                        code: "plugin.disabled",
                        target: id.to_string(),
                        message_cn: format!("插件 {id} 已停用（它的登记已回滚）"),
                        message_en: format!("Plugin {id} is disabled"),
                    });
                    continue;
                }
                if !p.get("active").and_then(|v| v.as_bool()).unwrap_or(true) {
                    let reasons = p
                        .get("inactive_reasons")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join("；")
                        })
                        .unwrap_or_default();
                    findings.push(Finding {
                        level: "info",
                        code: "plugin.inactive",
                        target: id.to_string(),
                        message_cn: format!("插件 {id} 已装但未生效：{reasons}"),
                        message_en: format!("Plugin {id} is installed but inactive: {reasons}"),
                    });
                }
            }
        }
        for c in found
            .get("conflicts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let text = c.as_str().unwrap_or("").to_string();
            findings.push(Finding {
                level: "warn",
                code: "plugin.conflict",
                target: "plugins/".to_string(),
                message_cn: text.clone(),
                message_en: text,
            });
        }
        for b in found
            .get("blocked_registrations")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let plugin = b.get("plugin").and_then(|v| v.as_str()).unwrap_or("");
            let target = b.get("target").and_then(|v| v.as_str()).unwrap_or("");
            let reason = b.get("blocked_reason").and_then(|v| v.as_str()).unwrap_or("");
            findings.push(Finding {
                level: "warn",
                code: "plugin.registration_blocked",
                target: format!("{plugin}:{target}"),
                message_cn: format!(
                    "插件 {plugin} 的登记「{target}」撤不回去：{reason}"
                ),
                message_en: format!(
                    "Plugin {plugin} registration '{target}' cannot be reverted: {reason}"
                ),
            });
        }
    }

    // 1. 目录骨架（内核体检）
    if let Ok(doctor) = kernel.doctor(project) {
        if let Some(issues) = doctor.get("issues").and_then(|v| v.as_array()) {
            for i in issues {
                let text = i.as_str().unwrap_or_default().to_string();
                findings.push(Finding {
                    level: "warn",
                    code: "project.structure",
                    target: project.display().to_string(),
                    message_cn: text.clone(),
                    message_en: text,
                });
            }
        }
    }

    // 2. 引用完整性 + 字段规则（内核校验）
    let (valid_ok, errors, warnings) = kernel.validate(project)?;
    for e in &errors {
        findings.push(Finding {
            level: "error",
            code: "validate.error",
            target: "tables".to_string(),
            message_cn: e.clone(),
            message_en: e.clone(),
        });
    }
    for w in &warnings {
        findings.push(Finding {
            level: "warn",
            code: "validate.warning",
            target: "tables".to_string(),
            message_cn: w.clone(),
            message_en: w.clone(),
        });
    }
    let _ = valid_ok;

    // 3. 逐张表：编码、列名、主键、空表
    let template = kernel.template(project)?;
    let schema_names = template_table_names(&template);
    let mut table_data: BTreeMap<String, (Vec<String>, Vec<cercis_kit_core::model::Row>)> = BTreeMap::new();
    let tables_dir = project.join("tables");
    let mut found_tables: BTreeSet<String> = BTreeSet::new();

    if tables_dir.is_dir() {
        for entry in std::fs::read_dir(&tables_dir)? {
            let path = entry?.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if ext != "csv" {
                continue;
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            found_tables.insert(name.clone());

            check_file_encoding(&path, &name, &mut findings);

            match cercis_kit_core::model::read_delimited(&path, b',') {
                Ok(t) => {
                    check_headers(&name, &t.headers, &mut findings);
                    let schema = table_schema(&template, &name);
                    check_primary_keys(&name, &schema, &t.rows, &mut findings);
                    if t.rows.is_empty() && schema_names.contains(&name) {
                        findings.push(Finding {
                            level: "info",
                            code: "table.empty",
                            target: format!("tables/{name}.csv"),
                            message_cn: format!("表 {name} 还没有数据（正常，表已建好）"),
                            message_en: format!("Table {name} is empty (fine — it is ready to use)"),
                        });
                    }
                    table_data.insert(name, (t.headers, t.rows));
                }
                Err(e) => findings.push(Finding {
                    level: "error",
                    code: "table.unreadable",
                    target: format!("tables/{name}.csv"),
                    message_cn: format!("表 {name} 读不出来：{}", e.message),
                    message_en: format!("Table {name} cannot be read: {}", e.message),
                }),
            }
        }
    }

    // 模板里有、项目里没有的表：提醒（可能是没装模板或误删文件）
    for expected in &schema_names {
        if !found_tables.contains(expected) {
            findings.push(Finding {
                level: "warn",
                code: "table.missing",
                target: format!("tables/{expected}.csv"),
                message_cn: format!("模板里有表 {expected}，但项目里没有对应文件"),
                message_en: format!("Template declares table {expected}, but the file is missing"),
            });
        }
    }
    // 项目里有、模板里没有的表：不是错误，但值得知道
    for extra in &found_tables {
        if !schema_names.is_empty() && !schema_names.contains(extra) {
            findings.push(Finding {
                level: "info",
                code: "table.extra",
                target: format!("tables/{extra}.csv"),
                message_cn: format!("表 {extra} 不在当前模板里（自定义表，或用错了模板）"),
                message_en: format!("Table {extra} is not in the current template"),
            });
        }
    }

    // 4. 附件：缺失 / 孤儿 / 命名冲突
    check_media(project, &table_data, &mut findings)?;

    // 汇总
    let errors_n = findings.iter().filter(|f| f.level == "error").count();
    let warns_n = findings.iter().filter(|f| f.level == "warn").count();
    let infos_n = findings.iter().filter(|f| f.level == "info").count();

    Ok(json!({
        "ok": errors_n == 0,
        "action": "health",
        "project": project.display().to_string(),
        "summary": { "errors": errors_n, "warnings": warns_n, "infos": infos_n, "tables": found_tables.len() },
        "findings": findings.iter().map(|f| json!({
            "level": f.level,
            "code": f.code,
            "target": f.target,
            "message_cn": f.message_cn,
            "message_en": f.message_en,
        })).collect::<Vec<_>>(),
    }))
}

/// UTF-8、BOM、CRLF、文件末尾换行——四项直接影响"别人能不能读懂这个文件"。
fn check_file_encoding(path: &Path, table: &str, findings: &mut Vec<Finding>) {
    let target = format!("tables/{table}.csv");
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            findings.push(Finding {
                level: "error",
                code: "table.unreadable",
                target,
                message_cn: format!("读文件失败：{e}"),
                message_en: format!("Cannot read file: {e}"),
            });
            return;
        }
    };

    if std::str::from_utf8(&bytes).is_err() {
        findings.push(Finding {
            level: "error",
            code: "encoding.not_utf8",
            target: target.clone(),
            message_cn: "文件不是 UTF-8 编码（多半是 Excel 另存的 GBK/ANSI）".into(),
            message_en: "File is not UTF-8 encoded (likely saved as GBK/ANSI from Excel)".into(),
        });
        return;
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        findings.push(Finding {
            level: "warn",
            code: "encoding.bom",
            target: target.clone(),
            message_cn: "文件带 UTF-8 BOM（内核规则是无 BOM；建议另存为无 BOM）".into(),
            message_en: "File has a UTF-8 BOM (core expects none)".into(),
        });
    }
    let text = String::from_utf8_lossy(&bytes);
    if text.contains("\r\n") {
        findings.push(Finding {
            level: "warn",
            code: "encoding.crlf",
            target: target.clone(),
            message_cn: "换行是 Windows 风格（CRLF）；内核规则是 LF".into(),
            message_en: "Line endings are CRLF; the core rule is LF".into(),
        });
    }
    if !bytes.is_empty() && !text.ends_with('\n') {
        findings.push(Finding {
            level: "info",
            code: "encoding.no_final_newline",
            target,
            message_cn: "文件末尾没有换行符（部分工具会因此拼接出错行）".into(),
            message_en: "File does not end with a newline".into(),
        });
    }
}

/// 列名：重复列、非规范列名。
fn check_headers(table: &str, headers: &[String], findings: &mut Vec<Finding>) {
    let target = format!("tables/{table}.csv");
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for h in headers {
        if !seen.insert(h.clone()) {
            findings.push(Finding {
                level: "error",
                code: "table.duplicate_column",
                target: target.clone(),
                message_cn: format!("列名重复：{h}（数据会互相覆盖）"),
                message_en: format!("Duplicate column name: {h}"),
            });
        }
        if !is_ascii_snake(h) {
            findings.push(Finding {
                level: "warn",
                code: "table.column_naming",
                target: target.clone(),
                message_cn: format!("列名 {h:?} 不符合规范（应是小写 ASCII 下划线，如 sample_id）"),
                message_en: format!("Column name {h:?} is not lowercase ascii_snake_case"),
            });
        }
    }
}

/// 主键：空值、重复值。
fn check_primary_keys(
    table: &str,
    schema: &TableSchema,
    rows: &[cercis_kit_core::model::Row],
    findings: &mut Vec<Finding>,
) {
    if !schema.has_primary_key() {
        return;
    }
    let key = schema.primary_key.as_str();
    let target = format!("tables/{table}.csv");
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut empties = 0usize;
    for (i, row) in rows.iter().enumerate() {
        let value = row.get(key).map(|s| s.trim()).unwrap_or("");
        if value.is_empty() {
            empties += 1;
            continue;
        }
        if let Some(first) = seen.insert(value.to_string(), i + 1) {
            findings.push(Finding {
                level: "error",
                code: "table.duplicate_primary_key",
                target: target.clone(),
                message_cn: format!(
                    "主键重复：{key}={value}（第 {first} 行与第 {} 行）",
                    i + 1
                ),
                message_en: format!("Duplicate primary key {key}={value} (rows {first} and {})", i + 1),
            });
        }
    }
    if empties > 0 {
        findings.push(Finding {
            level: "error",
            code: "table.empty_primary_key",
            target,
            message_cn: format!("有 {empties} 行的主键 {key} 为空"),
            message_en: format!("{empties} row(s) have an empty primary key {key}"),
        });
    }
}

/// 附件：引用了却不存在（断链）、存在却没人引用（孤儿）、同名文件冲突。
fn check_media(
    project: &Path,
    tables: &BTreeMap<String, (Vec<String>, Vec<cercis_kit_core::model::Row>)>,
    findings: &mut Vec<Finding>,
) -> Result<()> {
    // 收集被引用的项目内相对路径
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for (table, (headers, rows)) in tables {
        for row in rows {
            for (col, value) in row {
                let v = value.trim();
                if v.is_empty() {
                    continue;
                }
                let looks_like_path = v.starts_with("media/")
                    || v.starts_with("./media/")
                    || col == "path"
                    || col.ends_with("_path");
                if looks_like_path && (v.contains('/') || v.contains('.')) {
                    referenced.insert(v.trim_start_matches("./").to_string());
                }
            }
        }
        let _ = headers;
        let _ = table;
    }

    // 断链：引用了但文件不在
    for rel in &referenced {
        let abs = project.join(rel);
        if !abs.exists() {
            findings.push(Finding {
                level: "error",
                code: "media.missing",
                target: rel.clone(),
                message_cn: format!("表里引用了 {rel}，但项目里找不到这个文件"),
                message_en: format!("{rel} is referenced but missing on disk"),
            });
        }
    }

    // 项目里的实际文件
    let media_root = project.join("media");
    let mut files: Vec<String> = Vec::new();
    if media_root.is_dir() {
        collect_media_files(&media_root, project, &mut files)?;
    }

    // 孤儿：文件在，但没有任何一行引用它
    for rel in &files {
        if !referenced.contains(rel) {
            findings.push(Finding {
                level: "warn",
                code: "media.orphan",
                target: rel.clone(),
                message_cn: format!("{rel} 没有被任何表引用（孤儿文件）"),
                message_en: format!("{rel} is not referenced by any table (orphan)"),
            });
        }
    }

    // 命名冲突：只在大小写上不同的同名文件（跨平台同步会互相覆盖）
    let mut by_lower: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rel in &files {
        by_lower
            .entry(rel.to_lowercase())
            .or_default()
            .push(rel.clone());
    }
    for (_, group) in by_lower.iter().filter(|(_, v)| v.len() > 1) {
        findings.push(Finding {
            level: "warn",
            code: "naming.case_conflict",
            target: group[0].clone(),
            message_cn: format!("这些文件名只差大小写，跨平台同步会冲突：{}", group.join("、")),
            message_en: format!("Files differ only by case and may collide: {}", group.join(", ")),
        });
    }

    Ok(())
}

fn collect_media_files(root: &Path, project: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_media_files(&path, project, out)?;
        } else {
            out.push(
                path.strip_prefix(project)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn template_table_names(template: &Value) -> BTreeSet<String> {
    template
        .get("tables")
        .and_then(|v| v.as_array())
        .map(|tables| {
            tables
                .iter()
                .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 列名规范：小写 ASCII 下划线，首字符是字母。
fn is_ascii_snake(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// 供 app 展示：把体检结论汇总成一句话。
pub fn summary_line(report: &Value) -> String {
    let s = report.get("summary").cloned().unwrap_or_default();
    let errors = s.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
    let warnings = s.get("warnings").and_then(|v| v.as_u64()).unwrap_or(0);
    let infos = s.get("infos").and_then(|v| v.as_u64()).unwrap_or(0);
    if errors == 0 && warnings == 0 {
        format!("体检通过（{} 项提示）", infos)
    } else {
        format!("{errors} 个问题、{warnings} 个提醒、{infos} 项提示")
    }
}

/// 供测试与调用方使用：项目路径的规范化形式。
pub fn normalize(project: &Path) -> PathBuf {
    project.to_path_buf()
}

/// 命令行入口（app 也用同一条命令调用本插件）。
///
/// 本插件**只读**：清单里不声明写入权限，因此不会被授予写权限，
/// 也就无法（也不需要）改动任何数据。
#[derive(clap::Parser, Debug)]
#[command(
    name = "cercis-plugin-health",
    version,
    about = "通用体检：目录骨架 · 断链 · 编码 · 命名冲突 · 主键重复 · 孤儿附件"
)]
struct Cli {
    /// 机器可读输出（app 与脚本使用）
    #[arg(long)]
    json: bool,
    /// 内核可执行文件（默认 CERCIS_BIN 或 PATH 里的 cercis）
    #[arg(long)]
    cercis: Option<PathBuf>,
    /// 项目目录
    #[arg(long)]
    project: PathBuf,
}

fn main() {
    use clap::Parser;
    let cli = Cli::parse();
    let kernel = match Kernel::locate(cli.cercis.as_deref()) {
        Ok(k) => k,
        Err(e) => cercis_kit_core::cli::fail(cli.json, &e),
    };

    match run(&kernel, &cli.project) {
        Ok(value) => cercis_kit_core::cli::emit(cli.json, &value, human),
        Err(e) => cercis_kit_core::cli::fail(cli.json, &e),
    }
}

fn human(v: &Value) {
    let summary = v["summary"].clone();
    println!(
        "体检结果：{}",
        summary_line(v)
    );
    let findings = v["findings"].as_array().cloned().unwrap_or_default();
    if findings.is_empty() {
        println!("未发现问题。");
        return;
    }
    println!();
    for f in &findings {
        let level = match f["level"].as_str().unwrap_or("info") {
            "error" => "问题",
            "warn" => "提醒",
            _ => "提示",
        };
        println!(
            "[{level}] {}  {}",
            f["target"].as_str().unwrap_or(""),
            f["message_cn"].as_str().unwrap_or("")
        );
    }
    let errors = summary["errors"].as_u64().unwrap_or(0);
    if errors > 0 {
        println!("\n有 {errors} 个问题需要处理；修好后重新体检即可。");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_column_naming_rules() {
        assert!(is_ascii_snake("sample_id"));
        assert!(is_ascii_snake("a1"));
        assert!(!is_ascii_snake("SampleID"));
        assert!(!is_ascii_snake("1abc"));
        assert!(!is_ascii_snake("样品"));
    }

    #[test]
    fn finds_media_orphans_and_broken_links() {
        let dir = std::env::temp_dir().join("cercis-health-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("media/2026")).unwrap();
        std::fs::write(dir.join("media/2026/used.jpg"), b"x").unwrap();
        std::fs::write(dir.join("media/2026/orphan.jpg"), b"x").unwrap();

        let mut tables: BTreeMap<String, (Vec<String>, Vec<cercis_kit_core::model::Row>)> = BTreeMap::new();
        let mut row = cercis_kit_core::model::Row::new();
        row.insert("path".into(), "media/2026/used.jpg".into());
        row.insert("thumb_path".into(), "media/2026/missing.jpg".into());
        tables.insert("media_table".into(), (vec!["path".into()], vec![row]));

        let mut findings = Vec::new();
        check_media(&dir, &tables, &mut findings).unwrap();
        let codes: Vec<&str> = findings.iter().map(|f| f.code).collect();
        assert!(codes.contains(&"media.missing"));
        assert!(codes.contains(&"media.orphan"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
