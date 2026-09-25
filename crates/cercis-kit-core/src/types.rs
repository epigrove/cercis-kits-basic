//! 类型规则：**从内核读来的**，插件不自己定规则（需求文档 §5.9 / D36）。
//!
//! 内核 `cercis value-types --json` 会给出七种值域，每种都带一条
//! **机器可执行的判定规则**（`matcher`）：
//!
//! | matcher | 含义 | 插件怎么做 |
//! |---|---|---|
//! | `any` | 不检查 | 放行 |
//! | `regex` | 整格匹配（等价 `^(?:pattern)$`） | 用正则匹配整格 |
//! | `enum` | 取值必须在这个集合里 | 查表（可选忽略 ASCII 大小写） |
//! | `builtin` | 正则表达不了的具名检查器 | 见 [`run_builtin`] |
//!
//! 本模块只做"解释规则"这件事——规则本身（什么算合法日期、什么算规范数字）
//! 永远在内核那边改，这里跟随。
//!
//! 为什么不让插件直接抄一份规则表：抄一份就会有第二份真相。历史上基础工具包
//! 的 `type_ok()` 就是抄的，结果它把 `1e3` 当合法数字、把 `2026/6/15` 当合法
//! 日期——与内核不一致，两边对同一批数据的结论自然对不上。

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::Value;

use crate::error::{KitError, Result};
use crate::kernel::Kernel;

/// 违反规则时的强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSeverity {
    /// 结构错误：写入前必须拦下。
    Error,
    /// 提醒：放行，但记进报告。
    Warning,
}

impl RuleSeverity {
    pub fn is_error(self) -> bool {
        self == RuleSeverity::Error
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RuleSeverity::Error => "error",
            RuleSeverity::Warning => "warning",
        }
    }
}

/// 内核发布的判定规则。
#[derive(Debug, Clone)]
pub enum Matcher {
    Any,
    Regex(String),
    Enum {
        values: Vec<String>,
        ascii_case_insensitive: bool,
    },
    Builtin(String),
}

/// 一个值域的规则。
#[derive(Debug, Clone)]
pub struct DomainRule {
    pub name: String,
    pub label_cn: String,
    /// 违反该值域时的强度
    pub severity: RuleSeverity,
    /// 在 CSV 里长什么样（写给人看的提示）
    pub csv_form: String,
    pub examples_ok: Vec<String>,
    pub matcher: Matcher,
    /// 是否参与类型推断
    pub inferable: bool,
}

/// 类型判定结果。
#[derive(Debug, Clone)]
pub enum Verdict {
    Ok,
    Violation {
        severity: RuleSeverity,
        message: String,
    },
}

impl Verdict {
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            Verdict::Violation {
                severity: RuleSeverity::Error,
                ..
            }
        )
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            Verdict::Ok => None,
            Verdict::Violation { message, .. } => Some(message),
        }
    }
}

/// 内核类型清单（进程内的一份解释器）。
#[derive(Debug, Clone)]
pub struct TypeCatalog {
    domains: BTreeMap<String, DomainRule>,
    constraints: BTreeMap<String, ConstraintRule>,
    order: Vec<String>,
    /// 内核版本（写进报告，便于追溯"当时按哪版规则判的"）
    pub core_version: String,
}

/// 一条约束的规则（强度与适用范围都由内核给）。
#[derive(Debug, Clone)]
pub struct ConstraintRule {
    pub name: String,
    pub label_cn: String,
    /// 违反时的强度
    pub severity: RuleSeverity,
    /// 适用于哪些类型（内核给的可读文本）
    pub applies_to: String,
    pub notes: String,
}

impl TypeCatalog {
    /// 从内核取（**生产路径**：规则的唯一来源）。
    pub fn from_kernel(kernel: &Kernel) -> Result<Self> {
        let out = kernel.json(&["value-types".to_string()])?;
        Self::from_contract_json(&out)
    }

    /// 解析 `cercis value-types --json` 的输出（也用于测试夹具）。
    pub fn from_contract_json(out: &Value) -> Result<Self> {
        let contract = out.get("value_types").unwrap_or(out);
        let list = contract
            .get("domains")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                KitError::with_hint(
                    "内核没有返回类型清单",
                    "确认内核版本支持 `cercis value-types --json`",
                )
            })?;
        let mut domains = BTreeMap::new();
        let mut order = Vec::new();
        for d in list {
            let name = str_of(d, "name");
            if name.is_empty() {
                return Err(KitError::new("类型清单里有一项没有 name"));
            }
            let matcher = parse_matcher(d.get("matcher"))?;
            let severity = match str_of(d, "severity_on_violation").as_str() {
                "warning" => RuleSeverity::Warning,
                _ => RuleSeverity::Error,
            };
            order.push(name.clone());
            domains.insert(
                name.clone(),
                DomainRule {
                    name,
                    label_cn: str_of(d, "label_cn"),
                    severity,
                    csv_form: str_of(d, "csv_form"),
                    examples_ok: d
                        .get("examples_ok")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().map(json_text).collect())
                        .unwrap_or_default(),
                    matcher,
                    inferable: d
                        .get("inferable")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                },
            );
        }
        Ok(Self {
            domains,
            constraints: parse_constraints(contract)?,
            order,
            core_version: str_of(contract, "core_version"),
        })
    }

    /// 七种值域的名字（按内核给的顺序）。
    pub fn names(&self) -> Vec<String> {
        self.order.clone()
    }

    pub fn rule(&self, field_type: &str) -> Option<&DomainRule> {
        self.domains.get(field_type.trim())
    }

    /// 取一条约束的规则（required / range / allowed_values / pattern / precision / unit / reference）。
    pub fn constraint(&self, name: &str) -> Option<&ConstraintRule> {
        self.constraints.get(name)
    }

    /// 违反某条约束时的强度。
    ///
    /// 清单里没有这条约束时返回 [`RuleSeverity::Warning`]：**宁可少拦，不可误拦**——
    /// 插件与内核版本不一致时，先把数据放行、留下提醒，而不是把人挡在门外。
    pub fn constraint_severity(&self, name: &str) -> RuleSeverity {
        self.constraint(name)
            .map(|c| c.severity)
            .unwrap_or(RuleSeverity::Warning)
    }

    /// 约束名字列表（顺序即内核给的顺序）。
    pub fn constraint_names(&self) -> Vec<String> {
        self.constraints.keys().cloned().collect()
    }

    /// 判定一个单元格。
    ///
    /// 空值一律放行（是否必填由模板的 `required` 管，见 [`crate::checks`]）；
    /// 未知类型放行（将来内核加了新类型，老插件不该把数据拦住）。
    pub fn check(&self, field_type: &str, value: &str) -> Verdict {
        let Some(rule) = self.rule(field_type) else {
            return Verdict::Ok;
        };
        let v = value.trim();
        if v.is_empty() {
            return Verdict::Ok;
        }
        let ok = match &rule.matcher {
            Matcher::Any => true,
            Matcher::Regex(pattern) => full_match(pattern, v),
            Matcher::Enum {
                values,
                ascii_case_insensitive,
            } => values.iter().any(|x| {
                if *ascii_case_insensitive {
                    x.eq_ignore_ascii_case(v)
                } else {
                    x == v
                }
            }),
            Matcher::Builtin(checker) => run_builtin(checker, v),
        };
        if ok {
            return Verdict::Ok;
        }
        Verdict::Violation {
            severity: rule.severity,
            message: format!(
                "应为{}（{}）{}",
                if rule.label_cn.is_empty() {
                    rule.name.clone()
                } else {
                    rule.label_cn.clone()
                },
                rule.csv_form,
                rule.examples_ok
                    .first()
                    .map(|e| format!("，例如 {e}"))
                    .unwrap_or_default()
            ),
        }
    }
}

fn parse_matcher(v: Option<&Value>) -> Result<Matcher> {
    let Some(v) = v else {
        return Ok(Matcher::Any);
    };
    let kind = str_of(v, "kind");
    Ok(match kind.as_str() {
        "regex" => Matcher::Regex(str_of(v, "pattern")),
        "enum" => Matcher::Enum {
            values: v
                .get("values")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().map(json_text).collect())
                .unwrap_or_default(),
            ascii_case_insensitive: v
                .get("ascii_case_insensitive")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
        },
        "builtin" => Matcher::Builtin(str_of(v, "checker")),
        _ => Matcher::Any,
    })
}

fn parse_constraints(contract: &Value) -> Result<BTreeMap<String, ConstraintRule>> {
    let mut out = BTreeMap::new();
    let Some(list) = contract.get("constraints").and_then(|v| v.as_array()) else {
        return Ok(out);
    };
    for c in list {
        let name = str_of(c, "name");
        if name.is_empty() {
            continue;
        }
        let severity = match str_of(c, "severity").as_str() {
            "error" => RuleSeverity::Error,
            _ => RuleSeverity::Warning,
        };
        out.insert(
            name.clone(),
            ConstraintRule {
                name,
                label_cn: str_of(c, "label_cn"),
                severity,
                applies_to: str_of(c, "applies_to"),
                notes: str_of(c, "notes"),
            },
        );
    }
    Ok(out)
}

/// 具名检查器（正则表达不了的规则）：**与内核 `value.rs::run_builtin` 一一对应**。
///
/// 目前只有两个；一致性测试会拿真实内核逐值对账，改错了跑测试就会红。
pub fn run_builtin(checker: &str, value: &str) -> bool {
    match checker {
        // 真实存在的日历日（2026-02-30 不合法）
        "iso_date" => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok(),
        // ISO 8601 且带时区（允许省略秒）
        "iso8601_tz" => is_iso8601_with_tz(value),
        // 未知检查器放行：宁可少检查一项，也不要拦住好数据
        _ => true,
    }
}

fn is_iso8601_with_tz(s: &str) -> bool {
    if chrono::DateTime::parse_from_rfc3339(s).is_ok() {
        return true;
    }
    const WITH_TZ: [&str; 2] = ["%Y-%m-%dT%H:%M%:z", "%Y-%m-%dT%H:%M:%S%:z"];
    WITH_TZ
        .iter()
        .any(|f| chrono::DateTime::parse_from_str(s, f).is_ok())
}

/// 整格匹配（内核的 `regex` 语义：内部按 `^(?:pattern)$` 执行）。
fn full_match(pattern: &str, value: &str) -> bool {
    match Regex::new(&format!("^(?:{pattern})$")) {
        Ok(re) => re.is_match(value),
        // 模板里的正则写错了：不拦数据（内核体检会提醒"pattern 不是合法正则"）
        Err(_) => true,
    }
}

/// 内核契约快照（由 `cercis value-types --json` 复制而来），单元测试用。
///
/// 一致性测试（`tests/type_consistency.rs`）会拿**真实内核**逐值对账，
/// 因此这个快照过不过期不是靠人看，而是靠测试红。
#[cfg(test)]
pub(crate) fn fixture() -> TypeCatalog {
    let text = include_str!("../tests/fixtures/value-types.json");
    let json: Value = serde_json::from_str(text).expect("夹具 JSON 应能解析");
    TypeCatalog::from_contract_json(&json).expect("夹具应能装成类型清单")
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .map(json_text)
        .unwrap_or_default()
}

fn json_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_reads_seven_domains_in_order() {
        let c = fixture();
        assert_eq!(
            c.names(),
            vec!["text", "integer", "number", "bool", "date", "datetime", "time"]
        );
        assert!(!c.rule("time").unwrap().inferable);
    }

    #[test]
    fn number_and_integer_rules_match_kernel() {
        let c = fixture();
        assert!(c.check("number", "12.5").message().is_none());
        assert!(c.check("number", "1e3").is_error());
        assert!(c.check("number", "1,200.5").is_error());
        assert!(c.check("integer", "12.5").is_error());
        assert!(c.check("integer", "12").message().is_none());
    }

    #[test]
    fn bool_and_time_only_warn() {
        let c = fixture();
        let v = c.check("bool", "Y");
        assert!(!v.is_error(), "布尔写法问题只提醒");
        assert!(matches!(
            v,
            Verdict::Violation {
                severity: RuleSeverity::Warning,
                ..
            }
        ));
        let v = c.check("time", "9:30");
        assert!(!v.is_error());
    }

    #[test]
    fn builtin_checkers_are_real() {
        let c = fixture();
        assert!(c.check("date", "2026-06-15").message().is_none());
        assert!(c.check("date", "2026-02-30").is_error(), "不存在的日历日要拦住");
        assert!(c.check("date", "2026/06/15").is_error());
        assert!(c
            .check("datetime", "2026-06-15T09:30+08:00")
            .message()
            .is_none());
    }

    #[test]
    fn empty_and_unknown_types_pass() {
        let c = fixture();
        assert!(c.check("number", "").message().is_none());
        assert!(c.check("未来类型", "随便").message().is_none());
    }
}
