//! 一致性测试：**插件的判断必须与内核的判断逐值一致**（需求文档 §5.9 / D36）。
//!
//! 为什么非要有这个测试：这次改动的起因就是"同一个值，两个工具两种判断"——
//! 基础工具包过去自己写了一份 `type_ok()`，把 `1e3` 当合法数字、把 `2026/6/15`
//! 当合法日期，跟内核说不到一块去。单元测试只能证明"插件跟自己一致"，
//! 证明不了"插件跟内核一致"，所以这里跨进程跑**真内核**：
//!
//! 1. 拿内核的 `value-types --json` 建清单 → 断言七种值域与文档一致；
//! 2. 用一堆**好值与坏值**逐格过一遍，比较"插件认为哪些格子是错的"与
//!    "内核 `append-many --dry-run` 认为哪些格子是错的"；
//! 3. 顺带检查仓库里那份契约快照有没有过期（内核改规则就必须更新它）。
//!
//! 内核二进制没构建时跳过并说明原因，不假装通过。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use cercis_kit_core::types::{TypeCatalog, Verdict};

fn workspace_root() -> PathBuf {
    // crates/cercis-kit-core → kits/basic
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn kernel_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CERCIS_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let dev = workspace_root().join("../../kernel/target/debug/cercis");
    dev.is_file().then(|| dev.canonicalize().unwrap_or(dev))
}

fn skip(reason: &str) -> bool {
    eprintln!("跳过：{reason}");
    true
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cercis-type-consistency-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn run(bin: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("跑不起来 {}：{e}", bin.display()));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn kernel_json(bin: &Path, args: &[&str]) -> Value {
    let (code, stdout, stderr) = run(bin, args);
    assert_eq!(code, 0, "内核命令失败：{args:?}\n{stderr}");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("内核输出不是 JSON：{e}\n{stdout}");
    })
}

/// 内核 `append-many --dry-run` 说哪些格子是错的 → [(行号, 列名)]
fn kernel_error_cells(dry_run: &Value) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = dry_run
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|e| {
                    (
                        e.get("row").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                        e.get("field")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[test]
fn catalog_matches_kernel_domain_list() {
    let Some(bin) = kernel_bin() else {
        skip("内核二进制未构建（先在 kernel 目录 cargo build）");
        return;
    };
    let catalog = TypeCatalog::from_kernel(&cercis_kit_core::Kernel::from_path(&bin).unwrap())
        .expect("应从内核取到类型清单");
    assert_eq!(
        catalog.names(),
        vec!["text", "integer", "number", "bool", "date", "datetime", "time"],
        "插件认到的值域必须与内核、与需求文档 §5.9.1 三方一致"
    );
    // 强度也不能各说各话
    use cercis_kit_core::RuleSeverity;
    assert_eq!(
        catalog.constraint_severity("allowed_values"),
        RuleSeverity::Error
    );
    assert_eq!(catalog.constraint_severity("range"), RuleSeverity::Warning);
    assert_eq!(
        catalog.constraint_severity("precision"),
        RuleSeverity::Warning
    );
    assert_eq!(catalog.constraint_severity("pattern"), RuleSeverity::Error);
}

/// 仓库里的契约快照（单元测试用）是否还与真内核一致。
#[test]
fn fixture_is_not_stale() {
    let Some(bin) = kernel_bin() else {
        skip("内核二进制未构建");
        return;
    };
    let live = kernel_json(&bin, &["value-types", "--json"]);
    let live_inner = live.get("value_types").cloned().unwrap_or(live);
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/value-types.json")).expect("夹具 JSON");

    let names = |v: &Value| -> Vec<String> {
        v.get("domains")
            .and_then(|d| d.as_array())
            .map(|a| {
                a.iter()
                    .map(|d| {
                        d.get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(names(&fixture), names(&live_inner), "值域清单变了，请更新夹具");

    let matchers = |v: &Value| -> Vec<Value> {
        v.get("domains")
            .and_then(|d| d.as_array())
            .map(|a| {
                a.iter()
                    .map(|d| d.get("matcher").cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        matchers(&fixture),
        matchers(&live_inner),
        "内核的判定规则变了（matcher 不一致）：更新 crates/cercis-kit-core/tests/fixtures/value-types.json"
    );
}

/// 逐值对账：插件说错的格子 == 内核说错的格子。
#[test]
fn plugin_verdicts_agree_with_kernel_on_every_value() {
    let Some(bin) = kernel_bin() else {
        skip("内核二进制未构建");
        return;
    };
    let catalog =
        TypeCatalog::from_kernel(&cercis_kit_core::Kernel::from_path(&bin).unwrap()).expect("类型清单");

    let dir = tmp("cells");
    let project = dir.join("proj");
    let project_s = project.display().to_string();
    let (code, _, stderr) = run(&bin, &["init", &project_s, "--name", "类型一致性", "--json"]);
    assert_eq!(code, 0, "建项目失败：{stderr}");
    let (code, _, stderr) = run(
        &bin,
        &[
            "install-template",
            &project_s,
            "--name",
            "example-study",
            "--json",
        ],
    );
    assert_eq!(code, 0, "装模板失败：{stderr}");

    // 语料：好值 + 各种坏写法
    let numbers = [
        "12", "12.5", "0.05", "-1.5", "1e3", "1,200.5", ".5", "十二", "abc", "",
    ];
    let dates = [
        "2026-06-15",
        "2026/6/15",
        "2026-02-30",
        "2026-13-01",
        "abc",
        "",
    ];

    for (column, field_type, corpus) in [
        ("value_num", "number", &numbers[..]),
        ("measured_at", "date", &dates[..]),
    ] {
        let rows: Vec<Value> = corpus
            .iter()
            .enumerate()
            .map(|(i, v)| {
                json!({
                    "obs_id": format!("O-{i}"),
                    "sample_id": "S-1",
                    "trait_id": "height",
                    column: v,
                })
            })
            .collect();
        let rows_path = dir.join(format!("rows-{column}.json"));
        std::fs::write(&rows_path, serde_json::to_string(&rows).unwrap()).unwrap();

        let dry = kernel_json(
            &bin,
            &[
                "append-many",
                &project_s,
                "--table",
                "observations",
                "--from",
                &rows_path.display().to_string(),
                "--dry-run",
                "--json",
            ],
        );
        let kernel_errors = kernel_error_cells(&dry);

        // 插件侧：按这一列在模板里的类型逐值判
        let mut plugin_errors: Vec<(usize, String)> = Vec::new();
        for (i, v) in corpus.iter().enumerate() {
            if let Verdict::Violation { severity, .. } = catalog.check(field_type, v) {
                if severity.is_error() {
                    plugin_errors.push((i + 1, column.to_string()));
                }
            }
        }
        plugin_errors.sort();
        assert_eq!(
            plugin_errors, kernel_errors,
            "列 {column}：插件与内核对不上了\n插件：{plugin_errors:?}\n内核：{kernel_errors:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
