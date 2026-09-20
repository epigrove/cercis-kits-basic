//! 写入前的行级检查。
//!
//! 原则（需求文档 §7.2 / §7.3）：**结构错误阻止写入，业务规则给警告**。
//! 检查放在写入之前，脏数据就进不了项目；这也让"试运行"能给出真实结论。

use std::collections::BTreeSet;

use crate::kernel::TableSchema;
use crate::model::Row;

/// 一批行的检查结果。
#[derive(Debug, Default)]
pub struct RowCheck {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// 模板未定义的列
    pub unknown_columns: Vec<String>,
    /// 全空行（会被跳过，不算错误）
    pub empty_rows: usize,
}

impl RowCheck {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// 检查一批行是否可以写入。
///
/// - `existing_pks`：项目里已有的主键值（用于查重）
/// - `allow_new_columns`：是否允许模板之外的列（默认不允许，防止手滑写脏列）
pub fn check_rows(
    schema: &TableSchema,
    rows: &[Row],
    existing_pks: &BTreeSet<String>,
    allow_new_columns: bool,
) -> RowCheck {
    let mut out = RowCheck::default();
    let schema_columns: BTreeSet<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
    let has_schema = !schema.fields.is_empty();

    // 1. 列名：模板之外的列（默认阻止）
    let mut unknown: Vec<String> = Vec::new();
    for row in rows {
        for key in row.keys() {
            if has_schema && !schema_columns.contains(key.as_str()) && !unknown.contains(key) {
                unknown.push(key.clone());
            }
        }
    }
    if !unknown.is_empty() && !allow_new_columns {
        out.errors.push(format!(
            "模板 {} 中未定义的列：{}（改列名、去掉这些列，或显式允许新增列）",
            if schema.table.is_empty() {
                "（无）"
            } else {
                &schema.table
            },
            unknown.join("、")
        ));
    }
    out.unknown_columns = unknown;

    // 2. 必填、类型、取值范围
    for (i, row) in rows.iter().enumerate() {
        let line = i + 1;
        if row.values().all(|v| v.trim().is_empty()) {
            out.empty_rows += 1;
            continue;
        }
        for field in &schema.fields {
            let value = row.get(&field.name).map(|s| s.trim()).unwrap_or("");
            if field.required && value.is_empty() {
                out.errors.push(format!(
                    "第 {line} 行：必填字段「{}」为空",
                    field_label(field)
                ));
                continue;
            }
            if value.is_empty() {
                continue;
            }
            if !type_ok(&field.field_type, value) {
                out.errors.push(format!(
                    "第 {line} 行：字段「{}」应为 {} 类型，实际是 {:?}",
                    field_label(field),
                    field.field_type,
                    value
                ));
                continue;
            }
            if !field.allowed_values.is_empty()
                && !field.allowed_values.iter().any(|a| a == value)
            {
                out.warnings.push(format!(
                    "第 {line} 行：字段「{}」的值 {:?} 不在允许值（{}）内",
                    field_label(field),
                    value,
                    field.allowed_values.join(" / ")
                ));
            }
            if let (Some((min, max)), Ok(num)) = (field.range, value.parse::<f64>()) {
                if let Some(lo) = min {
                    if num < lo {
                        out.warnings.push(format!(
                            "第 {line} 行：字段「{}」的值 {num} 小于建议下限 {lo}",
                            field_label(field)
                        ));
                    }
                }
                if let Some(hi) = max {
                    if num > hi {
                        out.warnings.push(format!(
                            "第 {line} 行：字段「{}」的值 {num} 大于建议上限 {hi}",
                            field_label(field)
                        ));
                    }
                }
            }
        }
    }

    // 3. 主键：空、批内重复、与已有数据重复
    if schema.has_primary_key() {
        let key = schema.primary_key.as_str();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for (i, row) in rows.iter().enumerate() {
            let value = row.get(key).map(|s| s.trim()).unwrap_or("");
            if value.is_empty() {
                continue; // 必填检查已经报过
            }
            if !seen.insert(value.to_string()) {
                out.errors
                    .push(format!("第 {} 行：主键 {key}={value} 在本批中重复", i + 1));
            }
            if existing_pks.contains(value) {
                out.errors.push(format!(
                    "第 {} 行：主键 {key}={value} 在项目中已存在（数据只增不改，不能覆盖）",
                    i + 1
                ));
            }
        }
    }

    out
}

/// 排除全空行（Excel 尾部常有空行）。
pub fn drop_empty_rows(rows: Vec<Row>) -> (Vec<Row>, usize) {
    let before = rows.len();
    let kept: Vec<Row> = rows
        .into_iter()
        .filter(|r| !r.values().all(|v| v.trim().is_empty()))
        .collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

/// 从多行里取出主键列的值集合（用于与项目已有数据查重）。
pub fn collect_key_values(rows: &[Row], key: &str) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|r| r.get(key))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn field_label(field: &crate::kernel::TableField) -> String {
    if field.label_cn.is_empty() {
        field.name.clone()
    } else {
        format!("{}（{}）", field.label_cn, field.name)
    }
}

/// 类型是否匹配（空值一律放过，必填由必填规则管）。
fn type_ok(field_type: &str, value: &str) -> bool {
    match field_type {
        "number" => value.parse::<f64>().is_ok(),
        "integer" => value.parse::<i64>().is_ok(),
        "bool" => matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "false" | "0" | "1" | "是" | "否"
        ),
        "date" => is_iso_date(value),
        "datetime" => is_iso_date(value) || value.contains('T') || value.contains(' '),
        _ => true,
    }
}

/// ISO 8601 日期（YYYY-MM-DD）。
fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{TableField, TableSchema};

    fn schema() -> TableSchema {
        TableSchema {
            table: "records".into(),
            label_cn: "记录".into(),
            label_en: "Records".into(),
            primary_key: "record_id".into(),
            fields: vec![
                TableField {
                    name: "record_id".into(),
                    label_cn: "记录编号".into(),
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
                    range: Some((Some(0.0), Some(100.0))),
                },
            ],
        }
    }

    fn row(pairs: &[(&str, &str)]) -> Row {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn accepts_good_rows() {
        let rows = vec![row(&[("record_id", "R-1"), ("value_num", "12.5")])];
        let check = check_rows(&schema(), &rows, &BTreeSet::new(), false);
        assert!(check.is_ok(), "{:?}", check.errors);
        assert!(check.warnings.is_empty());
    }

    #[test]
    fn blocks_unknown_column_missing_required_and_bad_type() {
        let rows = vec![row(&[("value_num", "abc"), ("typo", "x")])];
        let check = check_rows(&schema(), &rows, &BTreeSet::new(), false);
        assert!(!check.is_ok());
        let joined = check.errors.join(" | ");
        assert!(joined.contains("typo"), "{joined}");
        assert!(joined.contains("必填"), "{joined}");
        assert!(joined.contains("number"), "{joined}");
    }

    #[test]
    fn blocks_duplicate_primary_key_in_batch_and_project() {
        let existing: BTreeSet<String> = ["R-100"].iter().map(|s| s.to_string()).collect();
        let rows = vec![
            row(&[("record_id", "R-100")]),
            row(&[("record_id", "R-101")]),
            row(&[("record_id", "R-101")]),
        ];
        let check = check_rows(&schema(), &rows, &existing, false);
        assert_eq!(check.errors.len(), 2, "{:?}", check.errors);
        assert!(check.errors[0].contains("已存在"));
        assert!(check.errors[1].contains("重复"));
    }

    #[test]
    fn warns_on_out_of_range_and_allowed_values() {
        let rows = vec![row(&[("record_id", "R-2"), ("value_num", "300")])];
        let check = check_rows(&schema(), &rows, &BTreeSet::new(), false);
        assert!(check.is_ok());
        assert_eq!(check.warnings.len(), 1);
        assert!(check.warnings[0].contains("上限"));
    }

    #[test]
    fn drops_empty_rows() {
        let rows = vec![row(&[("record_id", "")]), row(&[("record_id", "R-3")])];
        let (kept, dropped) = drop_empty_rows(rows);
        assert_eq!(kept.len(), 1);
        assert_eq!(dropped, 1);
    }
}
