//! 写入前的行级检查。
//!
//! 原则（需求文档 §7.2 / §7.3）：**结构错误阻止写入，业务规则给警告**。
//! 检查放在写入之前，脏数据就进不了项目；这也让"试运行"能给出真实结论。
//!
//! 类型规则**不在本文件里**：它们来自内核（[`crate::types::TypeCatalog`]），
//! 本模块只负责"逐行逐列地问一遍"。原因见 §5.9 / D36——类型是数据契约的一部分，
//! 只能有一份实现。

use std::collections::BTreeSet;

use crate::kernel::TableSchema;
use crate::model::Row;
use crate::types::{RuleSeverity, TypeCatalog};

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
    types: &TypeCatalog,
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
            // 类型：按内核发布的规则判（不是插件自己那一套）
            match types.check(&field.field_type, value) {
                crate::types::Verdict::Ok => {}
                crate::types::Verdict::Violation { severity, message } => {
                    let text = format!(
                        "第 {line} 行：字段「{}」{}，实际是 {:?}",
                        field_label(field),
                        message,
                        value
                    );
                    if severity == RuleSeverity::Error {
                        out.errors.push(text);
                        continue;
                    } else {
                        out.warnings.push(text);
                        continue;
                    }
                }
            }
            if !field.allowed_values.is_empty()
                && !field.allowed_values.iter().any(|a| a == value)
            {
                // 强度由内核清单给（§5.9.2：允许值是错误级）
                let text = format!(
                    "第 {line} 行：字段「{}」的值 {:?} 不在允许值（{}）内",
                    field_label(field),
                    value,
                    field.allowed_values.join(" / ")
                );
                push_by_severity(&mut out, types.constraint_severity("allowed_values"), text);
            }
            // 正则模式（text）
            if !field.pattern.is_empty() && !full_match(&field.pattern, value) {
                let text = format!(
                    "第 {line} 行：字段「{}」的值 {:?} 不符合约定的格式 /{}/",
                    field_label(field),
                    value,
                    field.pattern
                );
                push_by_severity(&mut out, types.constraint_severity("pattern"), text);
            }
            if let (Some((min, max)), Ok(num)) = (field.range, value.parse::<f64>()) {
                if let Some(lo) = min {
                    if num < lo {
                        let text = format!(
                            "第 {line} 行：字段「{}」的值 {num} 小于建议下限 {lo}",
                            field_label(field)
                        );
                        push_by_severity(&mut out, types.constraint_severity("range"), text);
                    }
                }
                if let Some(hi) = max {
                    if num > hi {
                        let text = format!(
                            "第 {line} 行：字段「{}」的值 {num} 大于建议上限 {hi}",
                            field_label(field)
                        );
                        push_by_severity(&mut out, types.constraint_severity("range"), text);
                    }
                }
            }
            // 小数位数（number）
            if let Some(p) = field.precision {
                if let Some(decimals) = decimal_places(value) {
                    if decimals > p as usize {
                        let text = format!(
                            "第 {line} 行：字段「{}」的小数位数 {decimals} 超过约定的 {p} 位",
                            field_label(field)
                        );
                        push_by_severity(&mut out, types.constraint_severity("precision"), text);
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

/// 按内核给的强度把问题放进"错误"或"提醒"里。
fn push_by_severity(out: &mut RowCheck, severity: RuleSeverity, text: String) {
    match severity {
        RuleSeverity::Error => out.errors.push(text),
        RuleSeverity::Warning => out.warnings.push(text),
    }
}

/// 整格匹配正则（与内核的 `pattern` 语义一致：`^(?:pattern)$`）。
fn full_match(pattern: &str, value: &str) -> bool {
    match regex::Regex::new(&format!("^(?:{pattern})$")) {
        Ok(re) => re.is_match(value),
        // 模板里的正则写错了：不拦数据（内核体检会提醒）
        Err(_) => true,
    }
}

/// 小数位数（没有小数点或不是数字时返回 None）。
fn decimal_places(value: &str) -> Option<usize> {
    let v = value.trim();
    if v.contains(',') {
        return None;
    }
    let frac = v.split_once('.')?.1;
    if frac.is_empty() || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(frac.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{TableField, TableSchema};
    use crate::types::{TypeCatalog, Verdict};

    /// 类型规则来自内核契约快照（真实内核由一致性测试逐值对账）。
    fn types() -> TypeCatalog {
        crate::types::fixture()
    }

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
                    pattern: String::new(),
                    precision: None,
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
                    pattern: String::new(),
                    precision: None,
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
        let check = check_rows(&schema(), &rows, &BTreeSet::new(), false, &types());
        assert!(check.is_ok(), "{:?}", check.errors);
        assert!(check.warnings.is_empty());
    }

    #[test]
    fn blocks_unknown_column_missing_required_and_bad_type() {
        let rows = vec![row(&[("value_num", "abc"), ("typo", "x")])];
        let check = check_rows(&schema(), &rows, &BTreeSet::new(), false, &types());
        assert!(!check.is_ok());
        let joined = check.errors.join(" | ");
        assert!(joined.contains("typo"), "{joined}");
        assert!(joined.contains("必填"), "{joined}");
        // 类型名的说法来自内核清单（number → "数值"）
        assert!(joined.contains("数值"), "{joined}");
    }

    #[test]
    fn blocks_duplicate_primary_key_in_batch_and_project() {
        let existing: BTreeSet<String> = ["R-100"].iter().map(|s| s.to_string()).collect();
        let rows = vec![
            row(&[("record_id", "R-100")]),
            row(&[("record_id", "R-101")]),
            row(&[("record_id", "R-101")]),
        ];
        let check = check_rows(&schema(), &rows, &existing, false, &types());
        assert_eq!(check.errors.len(), 2, "{:?}", check.errors);
        assert!(check.errors[0].contains("已存在"));
        assert!(check.errors[1].contains("重复"));
    }

    #[test]
    fn warns_on_out_of_range_and_allowed_values() {
        let rows = vec![row(&[("record_id", "R-2"), ("value_num", "300")])];
        let check = check_rows(&schema(), &rows, &BTreeSet::new(), false, &types());
        assert!(check.is_ok());
        assert_eq!(check.warnings.len(), 1);
        assert!(check.warnings[0].contains("上限"));
    }

    /// 与内核一致的几条关键判断：这些值过去被插件**错误地**放行过。
    #[test]
    fn type_rules_follow_kernel_contract() {
        let t = types();
        // 科学计数法与千分位不是规范数字
        for bad in ["1e3", "1,200.5", "十二"] {
            assert!(t.check("number", bad).is_error(), "{bad} 应为错误");
        }
        // 日期写法必须 ISO
        assert!(t.check("date", "2026/6/15").is_error());
        assert!(t.check("date", "2026-02-30").is_error());
        assert!(matches!(t.check("date", "2026-06-15"), Verdict::Ok));
        // 布尔与时刻只提醒
        assert!(!t.check("bool", "Y").is_error());
        assert!(!t.check("time", "9:30").is_error());
        // 空值一律放行（必填另有规则）
        assert!(matches!(t.check("number", ""), Verdict::Ok));
    }

    #[test]
    fn allowed_values_blocks_and_range_only_warns() {
        let mut s = schema();
        s.fields[0].allowed_values = vec!["A".into(), "B".into()];
        let rows = vec![row(&[("record_id", "C"), ("value_num", "12")])];
        let check = check_rows(&s, &rows, &BTreeSet::new(), false, &types());
        assert!(!check.is_ok(), "允许值不符应拦下：{:?}", check.errors);
        assert!(check.errors[0].contains("允许值"));
    }

    #[test]
    fn drops_empty_rows() {
        let rows = vec![row(&[("record_id", "")]), row(&[("record_id", "R-3")])];
        let (kept, dropped) = drop_empty_rows(rows);
        assert_eq!(kept.len(), 1);
        assert_eq!(dropped, 1);
    }
}
