//! 通用导入导出：CSV/TSV 导入（列映射 + 试运行 + 报告）、长表/宽表导出 + 说明书。
//!
//! 两条硬规则在这里体现得最直接：
//! - **导入的写入走内核**：全部行先检查，再一次性交给内核 `append-many`；
//! - **导出要留痕**：导出后往流水里记一条 `export`，说明"谁在何时把哪张表导到了哪里"。

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use cercis_kit_core::checks::{check_rows, collect_key_values, drop_empty_rows};
use cercis_kit_core::cli;
use cercis_kit_core::error::{KitError, Result};
use cercis_kit_core::kernel::Kernel;
use cercis_kit_core::model::{self, Row};

/// 导入参数。
pub struct ImportOptions {
    pub project: PathBuf,
    pub table: String,
    pub file: PathBuf,
    /// 列映射：`源列=目标列`
    pub maps: Vec<String>,
    /// 忽略源文件里模板没有的列（默认报错）
    pub skip_unknown: bool,
    pub dry_run: bool,
    /// 生成导入报告（Markdown）
    pub report: Option<PathBuf>,
    pub allow_new_columns: bool,
}

/// 导入 CSV/TSV：检查 → 写入内核 → 出报告。
pub fn import(kernel: &Kernel, opts: &ImportOptions) -> Result<Value> {
    if !opts.file.is_file() {
        return Err(KitError::with_hint(
            format!("导入文件不存在：{}", opts.file.display()),
            "确认路径；Excel 请先另存为 CSV",
        ));
    }

    let input = model::read_input(&opts.file.display().to_string())?;
    if input.rows.is_empty() {
        return Err(KitError::with_hint(
            format!("{} 里没有数据行", opts.file.display()),
            "首行应为列名，其后为数据",
        ));
    }

    let schema = kernel.table_schema(&opts.project, &opts.table)?;
    let mapping = build_mapping(&input.headers, &schema, &opts.maps, opts.skip_unknown)?;

    // 按映射重排列名，并记录被跳过的列
    let mut rows: Vec<Row> = Vec::with_capacity(input.rows.len());
    for row in &input.rows {
        let mut out = Row::new();
        for (from, to) in &mapping.pairs {
            if let Some(v) = row.get(from) {
                out.insert(to.clone(), v.clone());
            }
        }
        rows.push(out);
    }
    let (rows, dropped_empty) = drop_empty_rows(rows);
    if rows.is_empty() {
        return Err(KitError::with_hint(
            "映射后没有剩下任何数据行",
            "检查列映射是否把数据列都跳过了",
        ));
    }

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
                "有 {} 处问题，导入已取消（数据未被改动）：\n- {}",
                check.errors.len(),
                check.errors.join("\n- ")
            ),
            "修正源文件后用 --dry-run 再试一次",
        ));
    }
    // 引用完整性：宁可导入失败，也不让断链的数据进来
    let ref_issues =
        cercis_kit_core::refs::check_references(kernel, &opts.project, &schema, &rows)?;
    if !ref_issues.is_empty() {
        return Err(KitError::with_hint(
            format!(
                "引用检查未通过，导入已取消（数据未被改动）：\n- {}",
                ref_issues.join("\n- ")
            ),
            "先把被引用的记录导入项目，或修正源文件里的编号",
        ));
    }

    let source_ref = opts.file.display().to_string();
    let summary = format!(
        "导入 {} 行到表 {}（源：{}）",
        rows.len(),
        opts.table,
        opts.file.display()
    );
    let outcome = kernel.append_many(
        &opts.project,
        &opts.table,
        &rows,
        "import",
        &summary,
        &source_ref,
        opts.allow_new_columns,
        opts.dry_run,
    )?;

    let batch_id = outcome
        .get("batch_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut report_path = None;
    if let Some(path) = &opts.report {
        let text = import_report(opts, &mapping, &input, &rows, dropped_empty, &check, &batch_id);
        std::fs::write(path, text)?;
        report_path = Some(path.display().to_string());
    }

    Ok(json!({
        "ok": true,
        "action": "data.import",
        "table": opts.table,
        "source": source_ref,
        "rows_in_file": input.rows.len(),
        "rows_imported": rows.len(),
        "dropped_empty_rows": dropped_empty,
        "mapping": mapping.pairs.iter().map(|(f, t)| json!({"from": f, "to": t})).collect::<Vec<_>>(),
        "skipped_columns": mapping.skipped,
        "warnings": check.warnings,
        "dry_run": opts.dry_run,
        "batch_id": batch_id,
        "report": report_path,
        "kernel": outcome,
    }))
}

/// 导出参数。
pub struct ExportOptions {
    pub project: PathBuf,
    pub table: String,
    /// 输出文件或目录
    pub out: PathBuf,
    /// csv 或 json
    pub format: String,
    /// 宽表：把这一列的值透成列
    pub wide: Option<String>,
    /// 宽表的取值列（配合 --wide）
    pub value: Option<String>,
    /// 同时生成中文说明书（列字典 + 复现命令）
    pub readme: bool,
    /// 筛选：列:运算符:值（可重复）
    pub filters: Vec<String>,
    pub sort: Option<String>,
}

/// 导出：读内核 → 写文件 → 记流水。
pub fn export(kernel: &Kernel, opts: &ExportOptions) -> Result<Value> {
    let rows = kernel.fetch_rows(&opts.project, &opts.table, &opts.filters, opts.sort.as_deref())?;
    let schema = kernel.table_schema(&opts.project, &opts.table)?;
    let headers = resolve_headers(&schema, &rows);

    let (headers, rows, conflicts) = match (&opts.wide, &opts.value) {
        (Some(key), Some(value)) => {
            let (h, r, c) = pivot(&headers, &rows, key, value)?;
            (h, r, c)
        }
        (Some(_), None) => {
            return Err(KitError::with_hint(
                "--wide 需要同时给出取值列：--value <列名>",
                "例如 --wide factor --value value_num",
            ))
        }
        _ => (headers, rows, 0),
    };

    let wide = opts.wide.is_some();
    let file = resolve_out_path(&opts.out, &opts.table, &opts.format, wide);
    if let Some(parent) = file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let format = opts.format.to_lowercase();
    match format.as_str() {
        "csv" => std::fs::write(&file, model::to_csv_text(&headers, &rows)?)?,
        "json" => {
            let payload = json!({
                "schema": "cercis.table/1",
                "project": opts.project.display().to_string(),
                "table": opts.table,
                "template": schema.table,
                "columns": headers,
                "rows": cercis_kit_core::kernel::rows_to_json(&rows),
            });
            std::fs::write(&file, serde_json::to_string_pretty(&payload)?)?;
        }
        other => {
            return Err(KitError::with_hint(
                format!("不支持的导出格式：{other}"),
                "目前支持 csv 与 json",
            ))
        }
    }

    let mut readme_path = None;
    if opts.readme {
        let path = with_suffix(&file, ".README.md");
        std::fs::write(&path, export_readme(opts, &schema, &headers, rows.len()))?;
        readme_path = Some(path.display().to_string());
    }

    // 导出不改数据，但"导出过什么"必须可追溯
    let summary = format!(
        "导出表 {} 的 {} 行到 {}",
        opts.table,
        rows.len(),
        file.display()
    );
    let note = kernel
        .note(
            &opts.project,
            "export",
            &opts.table,
            &file
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            &summary,
            &file.display().to_string(),
        )
        .ok();

    Ok(json!({
        "ok": true,
        "action": "data.export",
        "table": opts.table,
        "format": format,
        "rows": rows.len(),
        "columns": headers,
        "file": file.display().to_string(),
        "readme": readme_path,
        "wide": wide,
        "pivot_conflicts": conflicts,
        "noted": note.is_some(),
    }))
}

/// 列映射结果。
pub struct Mapping {
    /// 源列 → 目标列
    pub pairs: Vec<(String, String)>,
    /// 被跳过的源列
    pub skipped: Vec<String>,
}

/// 解析列映射：显式 `--map` 优先，其次同名，最后按 `--skip-unknown` 决定跳过或报错。
pub fn build_mapping(
    headers: &[String],
    schema: &cercis_kit_core::kernel::TableSchema,
    maps: &[String],
    skip_unknown: bool,
) -> Result<Mapping> {
    let mut explicit: Vec<(String, String)> = Vec::new();
    for m in maps {
        let (from, to) = m.split_once('=').ok_or_else(|| {
            KitError::with_hint(
                format!("--map 需要 源列=目标列 形式，实际：{m}"),
                "例如 --map 样品编号=sample_id",
            )
        })?;
        explicit.push((from.trim().to_string(), to.trim().to_string()));
        if from.trim().is_empty() || to.trim().is_empty() {
            return Err(KitError::with_hint(
                format!("--map 两侧都不能为空：{m}"),
                "例如 --map 样品编号=sample_id",
            ));
        }
    }

    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    let has_schema = !schema.fields.is_empty();

    for h in headers {
        if let Some((_, to)) = explicit.iter().find(|(from, _)| from == h) {
            pairs.push((h.clone(), to.clone()));
            continue;
        }
        if !has_schema || schema.has_column(h) {
            pairs.push((h.clone(), h.clone()));
            continue;
        }
        if skip_unknown {
            skipped.push(h.clone());
        } else {
            unknown.push(h.clone());
        }
    }

    if !unknown.is_empty() {
        return Err(KitError::with_hint(
            format!(
                "源文件里有模板未定义的列：{}",
                unknown.join("、")
            ),
            "用 --map 源列=目标列 指定对应关系，或用 --skip-unknown 跳过这些列",
        ));
    }

    // 多个源列映射到同一目标列 → 必须让用户明确取舍
    let mut targets: Vec<String> = Vec::new();
    for (from, to) in &pairs {
        if targets.contains(to) {
            return Err(KitError::with_hint(
                format!("多个源列映射到同一目标列 {to}（最近的源列：{from}）"),
                "调整 --map，保证每个目标列只对应一个源列",
            ));
        }
        targets.push(to.clone());
    }

    Ok(Mapping { pairs, skipped })
}

/// 导出用列顺序：模板列在前（保持声明顺序），数据里多出来的列排在后面。
pub fn resolve_headers(schema: &cercis_kit_core::kernel::TableSchema, rows: &[Row]) -> Vec<String> {
    let mut headers: Vec<String> = schema
        .fields
        .iter()
        .map(|f| f.name.clone())
        .filter(|name| rows.iter().any(|r| r.contains_key(name)))
        .collect();
    if headers.is_empty() {
        for row in rows {
            for k in row.keys() {
                if !headers.contains(k) {
                    headers.push(k.clone());
                }
            }
        }
        return headers;
    }
    let mut extras: Vec<String> = Vec::new();
    for row in rows {
        for k in row.keys() {
            if !headers.contains(k) && !extras.contains(k) {
                extras.push(k.clone());
            }
        }
    }
    extras.sort();
    headers.extend(extras);
    headers
}

/// 长表 → 宽表：把 `key_col` 的不同取值透成列，用 `value_col` 填值。
///
/// 行身份 = 除这两列以外的其余列。同一身份+同一键出现多行时，
/// **保留最后一个非空值**并计入 `conflicts`（不静默丢数据，报告里能看见）。
pub fn pivot(
    headers: &[String],
    rows: &[Row],
    key_col: &str,
    value_col: &str,
) -> Result<(Vec<String>, Vec<Row>, usize)> {
    if !headers.iter().any(|h| h == key_col) {
        return Err(KitError::with_hint(
            format!("宽表键列不存在：{key_col}"),
            format!("可用列：{}", headers.join("、")),
        ));
    }
    if !headers.iter().any(|h| h == value_col) {
        return Err(KitError::with_hint(
            format!("宽表取值列不存在：{value_col}"),
            format!("可用列：{}", headers.join("、")),
        ));
    }

    let index_cols: Vec<String> = headers
        .iter()
        .filter(|h| *h != key_col && *h != value_col)
        .cloned()
        .collect();

    let mut keys: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get(key_col))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    keys.sort();
    keys.dedup();

    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::BTreeMap<String, Row> = std::collections::BTreeMap::new();
    let mut conflicts = 0usize;

    for row in rows {
        let id: Vec<String> = index_cols
            .iter()
            .map(|c| row.get(c).cloned().unwrap_or_default())
            .collect();
        let group_key = id.join("\u{1}");
        if !groups.contains_key(&group_key) {
            order.push(group_key.clone());
            let mut out = Row::new();
            for (c, v) in index_cols.iter().zip(id.iter()) {
                out.insert(c.clone(), v.clone());
            }
            // 宽表是矩形：所有透视列先在，缺的地方留空（导出 CSV/JSON 都是完整的）
            for k in &keys {
                out.insert(k.clone(), String::new());
            }
            groups.insert(group_key.clone(), out);
        }
        let pivot_key = row.get(key_col).map(|s| s.trim().to_string()).unwrap_or_default();
        if pivot_key.is_empty() {
            continue;
        }
        let value = row.get(value_col).cloned().unwrap_or_default();
        if value.trim().is_empty() {
            continue;
        }
        let group = groups.get_mut(&group_key).expect("组已建立");
        if let Some(existing) = group.get(&pivot_key) {
            if !existing.trim().is_empty() {
                conflicts += 1;
            }
        }
        group.insert(pivot_key, value);
    }

    let mut out_headers = index_cols;
    out_headers.extend(keys.clone());
    let out_rows: Vec<Row> = order
        .iter()
        .filter_map(|k| groups.get(k).cloned())
        .collect();
    Ok((out_headers, out_rows, conflicts))
}

/// 决定导出文件路径：给目录就按表名命名。
fn resolve_out_path(out: &Path, table: &str, format: &str, wide: bool) -> PathBuf {
    let ext = if format.eq_ignore_ascii_case("json") {
        "json"
    } else {
        "csv"
    };
    let name = if wide {
        format!("{table}-wide.{ext}")
    } else {
        format!("{table}.{ext}")
    };
    let looks_like_file = out
        .extension()
        .map(|e| {
            let e = e.to_string_lossy().to_lowercase();
            e == "csv" || e == "json"
        })
        .unwrap_or(false);
    if looks_like_file || out.is_file() {
        out.to_path_buf()
    } else {
        out.join(name)
    }
}

fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "export".to_string());
    file.with_file_name(format!("{name}{suffix}"))
}

fn import_report(
    opts: &ImportOptions,
    mapping: &Mapping,
    input: &model::InputTable,
    rows: &[Row],
    dropped_empty: usize,
    check: &cercis_kit_core::checks::RowCheck,
    batch_id: &str,
) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut out = String::new();
    out.push_str(&format!("# 导入报告：{}\n\n", opts.table));
    out.push_str(&format!("- 时间：{now}\n"));
    out.push_str(&format!("- 项目：{}\n", opts.project.display()));
    out.push_str(&format!("- 源文件：{}\n", opts.file.display()));
    out.push_str(&format!("- 源文件指纹：{}\n", file_sha256(&opts.file)));
    out.push_str(&format!(
        "- 行数：源 {} 行 → 导入 {} 行（跳过空行 {}）\n",
        input.rows.len(),
        rows.len(),
        dropped_empty
    ));
    out.push_str(&format!("- 内核批次号：{batch_id}\n"));
    out.push_str(&format!(
        "- 状态：{}\n\n",
        if opts.dry_run { "试运行（未写入）" } else { "已写入" }
    ));

    out.push_str("## 列映射\n\n| 源列 | 目标列 |\n|---|---|\n");
    for (from, to) in &mapping.pairs {
        let mark = if from == to { "" } else { " ← 映射" };
        out.push_str(&format!("| {from} | {to}{mark} |\n"));
    }
    if !mapping.skipped.is_empty() {
        out.push_str(&format!(
            "\n跳过的源列：{}\n",
            mapping.skipped.join("、")
        ));
    }

    if !check.warnings.is_empty() {
        out.push_str("\n## 警告\n\n");
        for w in &check.warnings {
            out.push_str(&format!("- {w}\n"));
        }
    }

    out.push_str("\n## 复现命令\n\n```bash\n");
    // 命令名从 Cargo 取（CARGO_BIN_NAME），避免改名后说明书里的命令失效
    out.push_str(&format!(
        "{} import --project {} --table {} --file {} --allow-write\n",
        env!("CARGO_BIN_NAME"),
        opts.project.display(),
        opts.table,
        opts.file.display()
    ));
    out.push_str("```\n");
    out
}

fn export_readme(
    opts: &ExportOptions,
    schema: &cercis_kit_core::kernel::TableSchema,
    headers: &[String],
    rows: usize,
) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut out = String::new();
    out.push_str(&format!("# 数据说明书：{}\n\n", opts.table));
    out.push_str("本文件由 Cercis 基础工具包生成，用于半年后仍能看懂这批数据。\n\n");
    out.push_str(&format!("- 导出时间：{now}\n"));
    out.push_str(&format!("- 项目：{}\n", opts.project.display()));
    out.push_str(&format!(
        "- 表：{}{}\n",
        opts.table,
        if schema.label_cn.is_empty() {
            String::new()
        } else {
            format!("（{}）", schema.label_cn)
        }
    ));
    out.push_str(&format!("- 行数：{rows}\n"));
    out.push_str(&format!("- 列数：{}\n", headers.len()));
    if let Some(w) = &opts.wide {
        out.push_str(&format!(
            "- 形态：宽表（按「{w}」透视，取值列「{}」）\n",
            opts.value.clone().unwrap_or_default()
        ));
    } else {
        out.push_str("- 形态：长表（一行一条记录）\n");
    }
    if !opts.filters.is_empty() {
        out.push_str(&format!("- 筛选条件：{}\n", opts.filters.join("；")));
    }
    if let Some(s) = &opts.sort {
        out.push_str(&format!("- 排序：{s}\n"));
    }

    out.push_str("\n## 列字典\n\n| 列名 | 中文名 | 英文名 | 类型 | 单位 | 必填 | 引用 |\n|---|---|---|---|---|---|---|\n");
    for h in headers {
        let field = schema.fields.iter().find(|f| &f.name == h);
        match field {
            Some(f) => out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                f.name,
                f.label_cn,
                f.label_en,
                f.field_type,
                f.unit,
                if f.required { "是" } else { "否" },
                f.reference
            )),
            None => out.push_str(&format!("| {h} | （模板未定义） | | | | | |\n")),
        }
    }

    out.push_str("\n## 复现命令\n\n```bash\n");
    out.push_str(&format!(
        "{} export --project {} --table {} --out {} --format {}{}{}\n",
        env!("CARGO_BIN_NAME"),
        opts.project.display(),
        opts.table,
        opts.out.display(),
        opts.format,
        opts.wide
            .as_ref()
            .map(|w| format!(" --wide {w}"))
            .unwrap_or_default(),
        opts.readme_combo()
    ));
    out.push_str("```\n\n");
    out.push_str("> 数据来源：Cercis 项目内的 CSV（唯一事实来源）。\n");
    out
}

impl ExportOptions {
    fn readme_combo(&self) -> String {
        if self.readme {
            " --readme".to_string()
        } else {
            String::new()
        }
    }
}

fn file_sha256(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&bytes);
            hex::encode(h.finalize())
        }
        Err(_) => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cercis_kit_core::checks::RowCheck;
    use cercis_kit_core::kernel::{TableField, TableSchema};
    use cercis_kit_core::model::InputTable;

    fn row(pairs: &[(&str, &str)]) -> Row {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn schema() -> TableSchema {
        TableSchema {
            table: "observations".into(),
            label_cn: "观测".into(),
            label_en: "Observations".into(),
            primary_key: "obs_id".into(),
            fields: vec![
                TableField {
                    name: "obs_id".into(),
                    label_cn: "观测编号".into(),
                    label_en: String::new(),
                    field_type: "text".into(),
                    unit: String::new(),
                    required: true,
                    reference: String::new(),
                    allowed_values: vec![],
                    range: None,
                },
                TableField {
                    name: "value_num".into(),
                    label_cn: "数值".into(),
                    label_en: String::new(),
                    field_type: "number".into(),
                    unit: String::new(),
                    required: false,
                    reference: String::new(),
                    allowed_values: vec![],
                    range: None,
                },
            ],
        }
    }

    #[test]
    fn maps_by_name_and_explicit_rule() {
        let headers = vec!["观测编号".to_string(), "value_num".to_string()];
        let mapping =
            build_mapping(&headers, &schema(), &["观测编号=obs_id".to_string()], false).unwrap();
        assert_eq!(mapping.pairs[0], ("观测编号".into(), "obs_id".into()));
        assert_eq!(mapping.pairs[1], ("value_num".into(), "value_num".into()));
        assert!(mapping.skipped.is_empty());
    }

    #[test]
    fn unknown_columns_block_unless_skipped() {
        let headers = vec!["obs_id".to_string(), "备注".to_string()];
        assert!(build_mapping(&headers, &schema(), &[], false).is_err());
        let mapping = build_mapping(&headers, &schema(), &[], true).unwrap();
        assert_eq!(mapping.skipped, vec!["备注".to_string()]);
    }

    #[test]
    fn duplicate_target_mapping_is_rejected() {
        let headers = vec!["a".to_string(), "b".to_string()];
        let maps = vec!["a=obs_id".to_string(), "b=obs_id".to_string()];
        assert!(build_mapping(&headers, &schema(), &maps, false).is_err());
    }

    #[test]
    fn pivots_long_to_wide() {
        let headers = vec![
            "plot".to_string(),
            "factor".to_string(),
            "value_num".to_string(),
        ];
        let rows = vec![
            row(&[("plot", "P1"), ("factor", "N"), ("value_num", "10")]),
            row(&[("plot", "P1"), ("factor", "P"), ("value_num", "12")]),
            row(&[("plot", "P2"), ("factor", "N"), ("value_num", "9")]),
        ];
        let (h, r, conflicts) = pivot(&headers, &rows, "factor", "value_num").unwrap();
        assert_eq!(h, vec!["plot", "N", "P"]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0]["N"], "10");
        assert_eq!(r[0]["P"], "12");
        assert_eq!(r[1]["P"], "");
        assert_eq!(conflicts, 0);
    }

    #[test]
    fn pivot_counts_conflicts_instead_of_hiding_them() {
        let headers = vec!["plot".to_string(), "factor".to_string(), "value".to_string()];
        let rows = vec![
            row(&[("plot", "P1"), ("factor", "N"), ("value", "1")]),
            row(&[("plot", "P1"), ("factor", "N"), ("value", "2")]),
        ];
        let (_, r, conflicts) = pivot(&headers, &rows, "factor", "value").unwrap();
        assert_eq!(conflicts, 1);
        assert_eq!(r[0]["N"], "2");
    }

    #[test]
    fn export_path_follows_directory_or_file() {
        let dir = Path::new("/tmp/out");
        assert_eq!(
            resolve_out_path(dir, "observations", "csv", false),
            PathBuf::from("/tmp/out/observations.csv")
        );
        assert_eq!(
            resolve_out_path(Path::new("/tmp/x.json"), "observations", "json", false),
            PathBuf::from("/tmp/x.json")
        );
        assert_eq!(
            resolve_out_path(dir, "observations", "csv", true),
            PathBuf::from("/tmp/out/observations-wide.csv")
        );
    }

    /// 说明书与报告里的"复现命令"必须用**当前**的二进制名。
    ///
    /// 这个是 S6 黄金对照验收抓到的真问题：工具包拆成四个插件后，
    /// 生成的说明书里还写着早已不存在的 `cercis-basic data export …`，
    /// 半年后照着敲会直接失败。
    #[test]
    fn generated_docs_use_current_binary_name() {
        let bin = env!("CARGO_BIN_NAME");

        let export = ExportOptions {
            project: PathBuf::from("/tmp/proj"),
            table: "observations".into(),
            out: PathBuf::from("/tmp/out"),
            format: "csv".into(),
            wide: None,
            value: None,
            readme: true,
            filters: vec![],
            sort: None,
        };
        let schema = TableSchema {
            table: "observations".into(),
            label_cn: "观测".into(),
            label_en: String::new(),
            primary_key: "obs_id".into(),
            fields: vec![],
        };
        let readme = export_readme(&export, &schema, &["obs_id".to_string()], 3);
        assert!(
            readme.contains(&format!("{bin} export --project")),
            "导出说明书的复现命令应使用 {bin}：\n{readme}"
        );
        assert!(
            !readme.contains("cercis-basic"),
            "不应再出现已废弃的命令名 cercis-basic"
        );

        let import = ImportOptions {
            project: PathBuf::from("/tmp/proj"),
            table: "observations".into(),
            file: PathBuf::from("/tmp/data.csv"),
            maps: vec![],
            skip_unknown: false,
            dry_run: false,
            report: None,
            allow_new_columns: false,
        };
        let mapping = Mapping { pairs: vec![], skipped: vec![] };
        let input = InputTable {
            headers: vec!["obs_id".into()],
            rows: vec![],
            source: "synthetic".into(),
        };
        let check = RowCheck::default();
        let report = import_report(&import, &mapping, &input, &[], 0, &check, "BATCH-1");
        assert!(
            report.contains(&format!("{bin} import --project")),
            "导入报告的复现命令应使用 {bin}：\n{report}"
        );
        assert!(
            !report.contains("cercis-basic"),
            "不应再出现已废弃的命令名 cercis-basic"
        );
    }
}

/// 命令行入口（app 也用同一条命令调用本插件）。
#[derive(Parser, Debug)]
#[command(
    name = "cercis-plugin-transfer",
    version,
    about = "通用导入导出：CSV/TSV 导入（映射 + 试运行 + 报告）· 长表/宽表导出 + 说明书"
)]
struct Cli {
    /// 机器可读输出（app 与脚本使用）
    #[arg(long, global = true)]
    json: bool,
    /// 内核可执行文件（默认 CERCIS_BIN 或 PATH 里的 cercis）
    #[arg(long, global = true)]
    cercis: Option<PathBuf>,
    /// 允许写入（导入会写数据，导出会写导出记录；试运行不需要）
    #[arg(long, global = true)]
    allow_write: bool,
    #[command(subcommand)]
    command: TransferCommand,
}

#[derive(Subcommand, Debug)]
enum TransferCommand {
    /// 导入 CSV/TSV：先检查、再一次性写入，可选生成导入报告
    Import(ImportArgs),
    /// 导出：长表 / 宽表 / JSON，可选生成数据说明书
    Export(ExportArgs),
}

#[derive(clap::Args, Debug)]
struct ImportArgs {
    /// 项目目录
    #[arg(long)]
    project: PathBuf,
    /// 目标表名
    #[arg(long)]
    table: String,
    /// 源文件（CSV/TSV）
    #[arg(long)]
    file: PathBuf,
    /// 列映射：源列=目标列（可重复）
    #[arg(long = "map", value_name = "源列=目标列")]
    maps: Vec<String>,
    /// 跳过源文件里模板没有的列（默认报错，避免默默丢数据）
    #[arg(long)]
    skip_unknown: bool,
    /// 试运行：只检查不写入
    #[arg(long)]
    dry_run: bool,
    /// 生成导入报告（Markdown 文件路径）
    #[arg(long)]
    report: Option<PathBuf>,
    /// 允许写入模板未定义的列（默认拒绝）
    #[arg(long)]
    allow_new_columns: bool,
}

#[derive(clap::Args, Debug)]
struct ExportArgs {
    /// 项目目录
    #[arg(long)]
    project: PathBuf,
    /// 表名
    #[arg(long)]
    table: String,
    /// 输出文件（.csv/.json）或目录
    #[arg(long)]
    out: PathBuf,
    /// 格式：csv / json
    #[arg(long, default_value = "csv")]
    format: String,
    /// 宽表：把这一列的不同取值透成列
    #[arg(long)]
    wide: Option<String>,
    /// 宽表的取值列（配合 --wide）
    #[arg(long)]
    value: Option<String>,
    /// 同时生成数据说明书（列字典 + 复现命令）
    #[arg(long)]
    readme: bool,
    /// 筛选：列:运算符:值（可重复）
    #[arg(long = "filter", value_name = "列:运算符:值")]
    filters: Vec<String>,
    /// 排序：列 或 列:asc|desc
    #[arg(long)]
    sort: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    let kernel = match Kernel::locate(cli.cercis.as_deref()) {
        Ok(k) => k,
        Err(e) => cli::fail(cli.json, &e),
    };

    let result = match &cli.command {
        TransferCommand::Import(a) => {
            if !a.dry_run {
                if let Err(e) = cli::require_write(cli.allow_write, "导入数据") {
                    cli::fail(cli.json, &e);
                }
            }
            import(
                &kernel,
                &ImportOptions {
                    project: a.project.clone(),
                    table: a.table.clone(),
                    file: a.file.clone(),
                    maps: a.maps.clone(),
                    skip_unknown: a.skip_unknown,
                    dry_run: a.dry_run,
                    report: a.report.clone(),
                    allow_new_columns: a.allow_new_columns,
                },
            )
            .map(|v| (v, Human::Import))
        }
        TransferCommand::Export(a) => {
            // 导出不改数据，但会把"导出过什么"写进变更流水，因此同样需要写入授权
            if let Err(e) = cli::require_write(cli.allow_write, "导出（写入导出记录）") {
                cli::fail(cli.json, &e);
            }
            export(
                &kernel,
                &ExportOptions {
                    project: a.project.clone(),
                    table: a.table.clone(),
                    out: a.out.clone(),
                    format: a.format.clone(),
                    wide: a.wide.clone(),
                    value: a.value.clone(),
                    readme: a.readme,
                    filters: a.filters.clone(),
                    sort: a.sort.clone(),
                },
            )
            .map(|v| (v, Human::Export))
        }
    };

    match result {
        Ok((value, kind)) => cli::emit(cli.json, &value, |v| human(kind, v)),
        Err(e) => cli::fail(cli.json, &e),
    }
}

#[derive(Clone, Copy)]
enum Human {
    Import,
    Export,
}

fn human(kind: Human, v: &Value) {
    match kind {
        Human::Import => {
            let rows = v["rows_imported"].as_u64().unwrap_or(0);
            let table = v["table"].as_str().unwrap_or("");
            if v["dry_run"].as_bool().unwrap_or(false) {
                println!("试运行通过：将导入 {rows} 行到表 {table}（未写入）");
            } else {
                println!("已导入 {rows} 行到表 {table}（一次事务、一条流水）");
            }
            if let Some(skipped) = v["skipped_columns"].as_array() {
                if !skipped.is_empty() {
                    let names: Vec<&str> = skipped.iter().filter_map(|s| s.as_str()).collect();
                    println!("跳过的列：{}", names.join("、"));
                }
            }
            for w in v["warnings"].as_array().into_iter().flatten() {
                println!("  提醒：{}", w.as_str().unwrap_or(""));
            }
            if let Some(report) = v["report"].as_str() {
                println!("导入报告：{report}");
            }
        }
        Human::Export => {
            println!(
                "已导出 {} 行 → {}",
                v["rows"].as_u64().unwrap_or(0),
                v["file"].as_str().unwrap_or("")
            );
            if v["wide"].as_bool().unwrap_or(false) {
                println!(
                    "（宽表；同一格出现多次的冲突数：{}）",
                    v["pivot_conflicts"].as_u64().unwrap_or(0)
                );
            }
            if let Some(readme) = v["readme"].as_str() {
                println!("数据说明书：{readme}");
            }
            if v["noted"].as_bool().unwrap_or(false) {
                println!("已在变更流水中记录本次导出");
            }
        }
    }
}
