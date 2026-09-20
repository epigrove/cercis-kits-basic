//! 内部数据模型：一行数据、一份输入表、以及 CSV/JSON 的读写。
//!
//! 内核的 CSV 规则是硬性的（UTF-8 无 BOM、LF、列名小写 ASCII 下划线），
//! 本模块负责让**外部文件**（可能带 BOM、可能用 Tab、可能是 JSON）
//! 变成内核能接受的行。

use std::io::Read;
use std::path::Path;

use serde_json::Value;

use crate::error::{err_hint, KitError, Result};

/// 一行数据：列名 → 单元格文本。
pub type Row = std::collections::BTreeMap<String, String>;

/// 读入的一张表（保留原始列顺序，用于导出与报告）。
#[derive(Debug, Clone, Default)]
pub struct InputTable {
    pub headers: Vec<String>,
    pub rows: Vec<Row>,
    /// 来源描述（文件路径或 stdin），写进报告与流水
    pub source: String,
}

/// 从"文件、标准输入或内联 JSON"读取数据。
///
/// 支持的形态：`.csv` / `.tsv`（首行表头）、`.json`（对象数组）、
/// `-`（标准输入，JSON）、以及直接写在参数里的内联 JSON。
pub fn read_input(from: &str) -> Result<InputTable> {
    if from == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        let mut t = parse_json(&buf)?;
        t.source = "<stdin>".to_string();
        return Ok(t);
    }

    let path = Path::new(from);
    if path.is_file() {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        return match ext.as_str() {
            "csv" => read_delimited(path, b','),
            "tsv" | "tab" => read_delimited(path, b'\t'),
            _ => {
                let text = std::fs::read_to_string(path)?;
                let mut t = parse_json(&text)?;
                t.source = from.to_string();
                Ok(t)
            }
        };
    }

    let trimmed = from.trim_start();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        let mut t = parse_json(from)?;
        t.source = "<inline>".to_string();
        return Ok(t);
    }

    err_hint(
        format!("输入文件不存在：{from}"),
        "确认路径；也可以把 JSON 直接写在参数里，或用 - 从标准输入读取",
    )
}

/// 读 CSV/TSV（首行表头；容忍多余/缺失的尾部字段）。
pub fn read_delimited(path: &Path, delimiter: u8) -> Result<InputTable> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .delimiter(delimiter)
        .from_path(path)?;

    let headers: Vec<String> = rdr
        .headers()?
        .iter()
        .map(|s| s.trim_start_matches('\u{feff}').trim().to_string())
        .collect();
    if headers.is_empty() || headers.iter().any(|h| h.is_empty()) {
        return Err(KitError::with_hint(
            format!("{} 的表头为空或存在空列名", path.display()),
            "首行必须是列名，且不能有空列名",
        ));
    }

    let mut rows = Vec::new();
    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|e| {
            KitError::new(format!("{} 第 {} 行解析失败：{e}", path.display(), i + 2))
        })?;
        let mut row = Row::new();
        for (j, col) in headers.iter().enumerate() {
            row.insert(col.clone(), rec.get(j).unwrap_or("").to_string());
        }
        rows.push(row);
    }

    Ok(InputTable {
        headers,
        rows,
        source: path.display().to_string(),
    })
}

/// 解析 JSON（对象数组，或单个对象）。
pub fn parse_json(text: &str) -> Result<InputTable> {
    let value: Value = serde_json::from_str(text)?;
    let items = match value {
        Value::Array(items) => items,
        obj @ Value::Object(_) => vec![obj],
        other => {
            return err_hint(
                format!("需要 JSON 数组或对象，实际是 {other}"),
                "例如 [{\"record_id\":\"R-1\",\"date\":\"2026-03-15\"}]",
            )
        }
    };

    let mut headers: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let obj = item.as_object().ok_or_else(|| {
            KitError::new(format!("第 {} 项不是 JSON 对象（每行必须是一个对象）", i + 1))
        })?;
        let mut row = Row::new();
        for (k, v) in obj {
            if !headers.contains(k) {
                headers.push(k.clone());
            }
            row.insert(k.clone(), json_cell(v));
        }
        rows.push(row);
    }

    Ok(InputTable {
        headers,
        rows,
        source: "<json>".to_string(),
    })
}

/// 解析 `--set 列=值`（可重复）：单行录入用。
pub fn parse_sets(sets: &[String]) -> Result<Row> {
    let mut row = Row::new();
    for s in sets {
        let (k, v) = s.split_once('=').ok_or_else(|| {
            KitError::with_hint(
                format!("--set 需要 列=值 形式，实际：{s}"),
                "例如 --set record_id=R-0001",
            )
        })?;
        let key = k.trim();
        if key.is_empty() {
            return err_hint("--set 的列名不能为空", "例如 --set record_id=R-0001");
        }
        row.insert(key.to_string(), v.trim().to_string());
    }
    Ok(row)
}

/// 单元格值：字符串原样，其余取 JSON 文本。
pub fn json_cell(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 写出 CSV 文本（UTF-8 无 BOM、LF 换行——与内核规则一致）。
pub fn to_csv_text(headers: &[String], rows: &[Row]) -> Result<String> {
    let mut wtr = csv::WriterBuilder::new()
        .delimiter(b',')
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    wtr.write_record(headers)?;
    for row in rows {
        let record: Vec<&str> = headers
            .iter()
            .map(|h| row.get(h).map(|s| s.as_str()).unwrap_or(""))
            .collect();
        wtr.write_record(&record)?;
    }
    let bytes = wtr
        .into_inner()
        .map_err(|e| KitError::new(format!("导出内容无法写出：{e}")))?;
    String::from_utf8(bytes).map_err(|e| KitError::new(format!("导出内容不是合法 UTF-8：{e}")))
}

/// 稳定排序：按给定列的值排序（空值排最后）。
pub fn sort_rows(rows: &mut [Row], key: &str) {
    rows.sort_by(|a, b| {
        let av = a.get(key).map(|s| s.as_str()).unwrap_or("");
        let bv = b.get(key).map(|s| s.as_str()).unwrap_or("");
        match (av.is_empty(), bv.is_empty()) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => av.cmp(bv),
        }
    });
}
