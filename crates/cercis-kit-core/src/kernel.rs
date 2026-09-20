//! 内核桥：工具包与内核之间**唯一**的通道。
//!
//! 通道形式（需求文档 §14.7 接口 A）：**命令行 + `--json`**。
//! 为什么选它：
//! - 内核与工具包是两份独立发布的代码，走进程边界后不需要同语言、同版本编译；
//! - 工具包**绕不过内核**：它只能调用内核命令，因此写入必然留痕、必然原子；
//! - 人和 AI 用的是同一套接口，出问题时可原样复现命令。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::error::{err, err_hint, KitError, Result};
use crate::model::Row;

/// 内核可执行文件。
pub struct Kernel {
    bin: PathBuf,
}

/// 一次内核调用的结果。
pub struct KernelRun {
    pub code: i32,
    pub value: Option<Value>,
    pub stderr: String,
}

impl Kernel {
    /// 定位内核可执行文件。
    ///
    /// 顺序：显式参数 → `CERCIS_BIN` 环境变量 → PATH 里的 `cercis` →
    /// 当前程序同级目录 → 开发工作区默认位置。
    pub fn locate(explicit: Option<&Path>) -> Result<Self> {
        if let Some(p) = explicit {
            return Self::from_path(p);
        }
        if let Some(p) = std::env::var_os("CERCIS_BIN") {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Ok(Self { bin: p });
            }
        }
        if let Some(p) = find_on_path("cercis") {
            return Ok(Self { bin: p });
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let sibling = dir.join(exe_name("cercis"));
                if sibling.is_file() {
                    return Ok(Self { bin: sibling });
                }
            }
        }
        // 开发工作区：kits/basic/crates/cercis-basic → Cercis/kernel/target/debug/cercis
        let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../kernel/target/debug")
            .join(exe_name("cercis"));
        if dev.is_file() {
            return Ok(Self { bin: dev });
        }
        err_hint(
            "找不到 Cercis 内核（cercis 命令）",
            "用 --cercis <路径> 指定，或设置环境变量 CERCIS_BIN",
        )
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return err_hint(
                format!("指定的内核不存在：{}", path.display()),
                "检查路径是否写错（应是 cercis 可执行文件）",
            );
        }
        Ok(Self {
            bin: path.to_path_buf(),
        })
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// 调用内核命令（自动追加 `--json`），返回退出码与解析后的输出。
    ///
    /// **不因退出码非零而报错**：像 `validate` 那样"发现问题是正常结果"的命令，
    /// 由调用方决定怎么解读。
    pub fn run(&self, args: &[String]) -> Result<KernelRun> {
        let mut all: Vec<String> = args.to_vec();
        all.push("--json".to_string());

        let output = Command::new(&self.bin).args(&all).output().map_err(|e| {
            KitError::with_hint(
                format!("无法运行内核 {}：{e}", self.bin.display()),
                "确认内核可执行且有执行权限",
            )
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let value = serde_json::from_str::<Value>(stdout.trim()).ok();

        Ok(KernelRun {
            code: output.status.code().unwrap_or(-1),
            value,
            stderr,
        })
    }

    /// 调用内核命令，非零退出即视为失败（内核的 JSON 错误信息会被带出来）。
    pub fn json(&self, args: &[String]) -> Result<Value> {
        let run = self.run(args)?;
        if run.code != 0 {
            let detail = run
                .value
                .as_ref()
                .and_then(|v| v.get("error"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    if run.stderr.trim().is_empty() {
                        None
                    } else {
                        Some(run.stderr.trim().to_string())
                    }
                })
                .unwrap_or_else(|| format!("内核退出码 {}", run.code));
            return err(format!("内核命令失败：{detail}"));
        }
        run.value.ok_or_else(|| {
            KitError::new("内核没有返回可解析的 JSON（可能版本不匹配）")
        })
    }

    /// 载入项目模板（表结构与字段语义的唯一来源）。
    pub fn template(&self, project: &Path) -> Result<Value> {
        let out = self.json(&[
            "template".into(),
            project.display().to_string(),
        ])?;
        out.get("template")
            .cloned()
            .ok_or_else(|| KitError::new("内核未返回模板内容"))
    }

    /// 取某张表的列定义（模板里没有这张表时返回空）。
    pub fn table_fields(&self, project: &Path, table: &str) -> Result<Vec<TableField>> {
        let template = self.template(project)?;
        Ok(table_fields(&template, table))
    }

    /// 取某张表的完整结构（字段 + 主键）。
    pub fn table_schema(&self, project: &Path, table: &str) -> Result<TableSchema> {
        let template = self.template(project)?;
        Ok(table_schema(&template, table))
    }

    /// 读取整张表（自动分页；筛选条件由调用方给出）。
    pub fn fetch_rows(
        &self,
        project: &Path,
        table: &str,
        filters: &[String],
        sort: Option<&str>,
    ) -> Result<Vec<Row>> {
        const PAGE: usize = 2000;
        let mut rows = Vec::new();
        let mut offset = 0usize;
        loop {
            let mut args = vec![
                "query".to_string(),
                project.display().to_string(),
                "--table".to_string(),
                table.to_string(),
                "--limit".to_string(),
                PAGE.to_string(),
                "--offset".to_string(),
                offset.to_string(),
            ];
            if let Some(s) = sort {
                args.push("--sort".to_string());
                args.push(s.to_string());
            }
            for f in filters {
                let parts: Vec<&str> = f.splitn(3, ':').collect();
                if parts.len() != 3 {
                    return err_hint(
                        format!("筛选条件格式应为 列:运算符:值，实际：{f}"),
                        "例如 value_num:gt:10",
                    );
                }
                args.push("--filter".to_string());
                args.push(parts[0].to_string());
                args.push(parts[1].to_string());
                args.push(parts[2].to_string());
            }
            let out = self.json(&args)?;
            let page = out
                .get("rows")
                .and_then(|v| v.as_array())
                .ok_or_else(|| KitError::new("内核未返回 rows"))?;
            let count = page.len();
            for item in page {
                rows.push(row_from_json(item)?);
            }
            if count < PAGE {
                break;
            }
            offset += count;
        }
        Ok(rows)
    }

    /// 批量写入（一次事务、一条流水）。
    #[allow(clippy::too_many_arguments)]
    pub fn append_many(
        &self,
        project: &Path,
        table: &str,
        rows: &[Row],
        action: &str,
        summary: &str,
        source_ref: &str,
        allow_new_columns: bool,
        dry_run: bool,
    ) -> Result<Value> {
        let json = serde_json::to_string(&rows_to_json(rows))
            .map_err(|e| KitError::new(format!("行数据无法序列化：{e}")))?;
        let mut args = vec![
            "append-many".to_string(),
            project.display().to_string(),
            "--table".to_string(),
            table.to_string(),
            "--from".to_string(),
            json,
            "--action".to_string(),
            action.to_string(),
            "--summary".to_string(),
            summary.to_string(),
        ];
        if !source_ref.is_empty() {
            args.push("--source-ref".to_string());
            args.push(source_ref.to_string());
        }
        if allow_new_columns {
            args.push("--allow-new-columns".to_string());
        }
        if dry_run {
            args.push("--dry-run".to_string());
        }
        self.json(&args)
    }

    /// 往变更流水里记一条（导出、体检等**不改变数据**的动作也要留痕）。
    pub fn note(
        &self,
        project: &Path,
        action: &str,
        table: &str,
        target_id: &str,
        summary: &str,
        source_ref: &str,
    ) -> Result<Value> {
        let mut args = vec![
            "note".to_string(),
            project.display().to_string(),
            "--action".to_string(),
            action.to_string(),
            "--table".to_string(),
            table.to_string(),
            "--target".to_string(),
            target_id.to_string(),
            "--summary".to_string(),
            summary.to_string(),
        ];
        if !source_ref.is_empty() {
            args.push("--source-ref".to_string());
            args.push(source_ref.to_string());
        }
        self.json(&args)
    }

    /// 项目自检（内核级：目录骨架、残留临时文件）。
    pub fn doctor(&self, project: &Path) -> Result<Value> {
        self.json(&["doctor".into(), project.display().to_string()])
    }

    /// 项目校验（引用完整性 + 字段规则）。`validate` 失败是**结果**，不是错误。
    pub fn validate(&self, project: &Path) -> Result<(bool, Vec<String>, Vec<String>)> {
        let run = self.run(&["validate".into(), project.display().to_string()])?;
        let value = run
            .value
            .ok_or_else(|| KitError::new("内核未返回校验结果"))?;
        let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let strings = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|i| {
                            i.as_str()
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| i.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok((ok, strings("errors"), strings("warnings")))
    }
}

/// 模板里的一个字段。
#[derive(Debug, Clone)]
pub struct TableField {
    pub name: String,
    pub label_cn: String,
    pub label_en: String,
    pub field_type: String,
    pub unit: String,
    pub required: bool,
    pub reference: String,
    /// 允许值（分号分隔的分类字段）
    pub allowed_values: Vec<String>,
    /// 取值范围（数值类）
    pub range: Option<(Option<f64>, Option<f64>)>,
}

/// 某张表的字段定义 + 主键。
#[derive(Debug, Clone, Default)]
pub struct TableSchema {
    pub table: String,
    pub label_cn: String,
    pub label_en: String,
    pub primary_key: String,
    pub fields: Vec<TableField>,
}

impl TableSchema {
    pub fn column_names(&self) -> Vec<String> {
        self.fields.iter().map(|f| f.name.clone()).collect()
    }

    pub fn has_column(&self, name: &str) -> bool {
        self.fields.iter().any(|f| f.name == name)
    }

    /// 主键是否在模板中声明。
    pub fn has_primary_key(&self) -> bool {
        !self.primary_key.is_empty()
    }
}

/// 从模板 JSON 里取出某张表的定义（模板中没有则为空 schema）。
pub fn table_schema(template: &Value, table: &str) -> TableSchema {
    let Some(tables) = template.get("tables").and_then(|v| v.as_array()) else {
        return TableSchema::default();
    };
    let Some(t) = tables
        .iter()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(table))
    else {
        return TableSchema::default();
    };
    TableSchema {
        table: table.to_string(),
        label_cn: str_of(t, "label_cn"),
        label_en: str_of(t, "label_en"),
        primary_key: str_of(t, "primary_key"),
        fields: t
            .get("fields")
            .and_then(|v| v.as_array())
            .map(|fields| {
                fields
                    .iter()
                    .map(|f| TableField {
                        name: str_of(f, "name"),
                        label_cn: str_of(f, "label_cn"),
                        label_en: str_of(f, "label_en"),
                        field_type: str_of(f, "type"),
                        unit: str_of(f, "unit"),
                        required: f
                            .get("required")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        reference: str_of(f, "reference"),
                        allowed_values: str_of(f, "allowed_values")
                            .split(';')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                        range: f.get("range").and_then(|v| v.as_array()).and_then(|a| {
                            if a.len() != 2 {
                                return None;
                            }
                            Some((
                                a[0].as_f64(),
                                a[1].as_f64(),
                            ))
                        }),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// 兼容旧接口：只取字段列表。
pub fn table_fields(template: &Value, table: &str) -> Vec<TableField> {
    table_schema(template, table).fields
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// JSON 对象 → 行（标量转字符串；`null` 视为空串）。
pub fn row_from_json(item: &Value) -> Result<Row> {
    let obj = item
        .as_object()
        .ok_or_else(|| KitError::new("表格行不是 JSON 对象"))?;
    let mut row = Row::new();
    for (k, v) in obj {
        row.insert(k.clone(), json_cell(v));
    }
    Ok(row)
}

/// 单元格值：字符串原样，其余取紧凑 JSON 文本。
pub fn json_cell(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 行 → JSON（保持键顺序稳定：BTreeMap 已排序）。
pub fn rows_to_json(rows: &[Row]) -> Vec<Value> {
    rows.iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            for (k, v) in r {
                obj.insert(k.clone(), Value::String(v.clone()));
            }
            Value::Object(obj)
        })
        .collect()
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let target = exe_name(name);
    std::env::split_paths(&path)
        .map(|dir| dir.join(&target))
        .find(|p| p.is_file())
}
