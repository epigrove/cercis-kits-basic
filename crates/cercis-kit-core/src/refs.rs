//! 写入前的**引用完整性**检查。
//!
//! 模板里 `reference: events.event_id` 这样的声明，含义是"这一列的值必须
//! 指向另一张表里真实存在的记录"。内核在 `validate` 时会检出断裂的引用，
//! 但那时数据已经写进去了；工具包在**写入前**先查一遍，用户就能当场改。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::error::Result;
use crate::kernel::{Kernel, TableSchema};
use crate::model::Row;

/// 检查一批行的引用是否都指向真实存在的记录，返回人类可读的问题列表。
pub fn check_references(
    kernel: &Kernel,
    project: &Path,
    schema: &TableSchema,
    rows: &[Row],
) -> Result<Vec<String>> {
    let mut issues = Vec::new();

    // 按被引用表归组，避免同一张表被反复查询
    let mut wanted: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    for field in &schema.fields {
        if field.reference.trim().is_empty() {
            continue;
        }
        let Some((table, column)) = field.reference.split_once('.') else {
            continue;
        };
        let entry = wanted
            .entry(table.to_string())
            .or_insert_with(|| (column.to_string(), BTreeSet::new()));
        for row in rows {
            if let Some(v) = row.get(&field.name) {
                let v = v.trim();
                if !v.is_empty() {
                    entry.1.insert(v.to_string());
                }
            }
        }
    }

    for (table, (column, values)) in wanted {
        if values.is_empty() {
            continue;
        }
        let existing: BTreeSet<String> = match kernel.fetch_rows(project, &table, &[], None) {
            Ok(rows) => rows
                .iter()
                .filter_map(|r| r.get(&column))
                .map(|s| s.trim().to_string())
                .collect(),
            Err(_) => {
                issues.push(format!(
                    "被引用的表 {table} 读不出来（缺失或损坏），无法确认 {table}.{column} 的引用是否成立"
                ));
                continue;
            }
        };

        let missing: Vec<&String> = values.difference(&existing).collect();
        if !missing.is_empty() {
            let field_label = schema
                .fields
                .iter()
                .find(|f| f.reference == format!("{table}.{column}"))
                .map(|f| format!("{}（{}）", f.label_cn, f.name))
                .unwrap_or_else(|| column.clone());
            let list: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
            issues.push(format!(
                "字段「{field_label}」引用了 {table}.{column} 中不存在的值：{}",
                list.join("、")
            ));
        }
    }

    Ok(issues)
}
