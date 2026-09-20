//! 插件命令行外壳：统一的输出、失败与**写权限闸门**。
//!
//! 权限模型（需求文档 §7.16 FR-P04）：插件默认只读；要写入必须
//! 先在 `plugin.yaml` 里声明，再由 app 在调用时授予 `--allow-write`。
//! 插件自己再检查一次，越权请求会被明确拒绝，而不是"悄悄写进去"。

use serde_json::{json, Value};

use crate::error::{KitError, Result};
use crate::OUTPUT_SCHEMA;

/// 输出结果：`--json` 走机器可读，否则给人看。
pub fn emit(json: bool, payload: &Value, human: impl FnOnce(&Value)) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(payload).unwrap_or_else(|_| "{}".into())
        );
    } else {
        human(payload);
    }
}

/// 报错退出（退出码 1）：人看的走 stderr，机器看的走 stdout。
pub fn fail(json: bool, e: &KitError) -> ! {
    if json {
        let payload = json!({
            "schema": OUTPUT_SCHEMA,
            "ok": false,
            "error": e.message,
            "hint": e.hint_or_default(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into())
        );
    } else {
        eprintln!("错误：{}", e.message);
        if !e.hint.is_empty() {
            eprintln!("建议：{}", e.hint);
        }
    }
    std::process::exit(1);
}

/// 写入闸门：没拿到授权就不许写。
pub fn require_write(allow_write: bool, what: &str) -> Result<()> {
    if allow_write {
        return Ok(());
    }
    Err(KitError::with_hint(
        format!("{what} 是写入操作，但本次调用没有获得写入授权"),
        "在 app 里授予该插件的写入权限；命令行使用请显式加 --allow-write",
    ))
}

/// 包一层统一的成功外壳。
pub fn ok(action: &str, payload: Value) -> Value {
    let mut obj = match payload {
        Value::Object(map) => map,
        other => {
            let mut m = serde_json::Map::new();
            m.insert("data".into(), other);
            m
        }
    };
    obj.insert("schema".into(), Value::String(OUTPUT_SCHEMA.to_string()));
    obj.insert("ok".into(), Value::Bool(true));
    obj.insert("plugin_action".into(), Value::String(action.to_string()));
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_gate_blocks_without_authorization() {
        let e = require_write(false, "录入").unwrap_err();
        assert!(e.message.contains("写入授权"));
        assert!(require_write(true, "录入").is_ok());
    }

    #[test]
    fn ok_wraps_payload_with_schema_and_action() {
        let v = ok("health", serde_json::json!({ "findings": [] }));
        assert_eq!(v["schema"], OUTPUT_SCHEMA);
        assert_eq!(v["ok"], true);
        assert_eq!(v["plugin_action"], "health");
    }
}
