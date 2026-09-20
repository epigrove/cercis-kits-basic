//! S5 验收：**一组独立插件**串起完整链路
//! （项目向导 → 通用录入 → 通用导入 → 内核查询 → 通用导出 → 通用体检）。
//!
//! 这个测试刻意跨进程运行真正的插件二进制与真正的内核命令，
//! 因为 S5 要验收的正是"插件之间、插件与内核之间"能不能接上。
//! 数据全部为合成数据。
//!
//! 若内核或插件二进制尚未构建（例如只跑 `cargo test -p cercis-kit-core`），
//! 测试会打印一行说明并跳过，而不是假装通过。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

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

fn plugin_bin(name: &str) -> Option<PathBuf> {
    let p = workspace_root().join("target/debug").join(name);
    p.is_file().then(|| p.canonicalize().unwrap_or(p))
}

/// 二进制是否比源码旧。
///
/// `cargo test` 只保证测试目标是最新的；`target/debug/<插件>` 由 `cargo build` 产出。
/// 过期的二进制会让验收测试得出**错误结论**（该拦的没拦住、该有的没生效），
/// 所以这里宁可跳过，也不拿旧二进制假装通过。
fn is_stale(bin: &Path) -> bool {
    let Ok(bin_time) = std::fs::metadata(bin).and_then(|m| m.modified()) else {
        return true;
    };
    newest_source_time(&workspace_root().join("crates")).is_some_and(|t| t > bin_time)
}

fn newest_source_time(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(t) = newest_source_time(&path) {
                newest = Some(newest.map_or(t, |n| n.max(t)));
            }
            continue;
        }
        let is_source = path
            .extension()
            .map(|e| e == "rs")
            .unwrap_or(false)
            || path.file_name().map(|n| n == "Cargo.toml").unwrap_or(false);
        if !is_source {
            continue;
        }
        if let Ok(t) = std::fs::metadata(&path).and_then(|m| m.modified()) {
            newest = Some(newest.map_or(t, |n| n.max(t)));
        }
    }
    newest
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cercis-s5-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// 运行一个命令，返回（退出码, stdout 文本）。
fn run(bin: &Path, args: &[String]) -> (i32, String) {
    let output = Command::new(bin)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("无法运行 {}：{e}", bin.display()));
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
    )
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("输出不是 JSON：{e}\n{text}"))
}

/// 跑一个插件命令，断言成功并返回 JSON。
fn plugin_ok(bin: &Path, kernel: &Path, args: &[&str]) -> Value {
    let mut all: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    all.push("--json".into());
    all.push("--cercis".into());
    all.push(kernel.display().to_string());
    let (code, out) = run(bin, &all);
    assert_eq!(code, 0, "插件执行失败：{}\n{out}", args.join(" "));
    let v = json_of(&out);
    assert_eq!(v["ok"], true, "插件返回未成功：{out}");
    v
}

fn kernel_json(kernel: &Path, args: &[&str]) -> Value {
    let mut all: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    all.push("--json".into());
    let (code, out) = run(kernel, &all);
    assert_eq!(code, 0, "内核执行失败：{}\n{out}", args.join(" "));
    json_of(&out)
}

fn skip(reason: &str) -> bool {
    eprintln!("[跳过] {reason}");
    true
}

#[test]
fn s5_flow_from_wizard_to_health() {
    let (Some(kernel), Some(wizard), Some(entry), Some(transfer), Some(health)) = (
        kernel_bin(),
        plugin_bin("cercis-plugin-wizard"),
        plugin_bin("cercis-plugin-entry"),
        plugin_bin("cercis-plugin-transfer"),
        plugin_bin("cercis-plugin-health"),
    ) else {
        skip("内核或插件二进制未构建（先在 kits/basic 与 kernel 各跑一次 cargo build）");
        return;
    };
    if is_stale(&wizard) || is_stale(&entry) || is_stale(&transfer) || is_stale(&health) {
        skip("插件二进制比源码旧：先运行 cargo build 再跑测试");
        return;
    }

    let work = tmp("flow");
    let project = work.join("proj");
    let kernel_s = kernel.display().to_string();

    // 1. 项目向导：建项目 + 装模板
    let created = plugin_ok(
        &wizard,
        &kernel,
        &[
            "--project",
            &project.display().to_string(),
            "--name",
            "端到端（合成数据）",
            "--template",
            "example-study",
            "--allow-write",
        ],
    );
    assert_eq!(created["template"], "example-study");
    assert!(project.join("project.yaml").exists());
    assert!(project.join("tables/samples.csv").exists());

    // 2. 通用录入：单行 + 权限闸门
    let appended = plugin_ok(
        &entry,
        &kernel,
        &[
            "append",
            "--project",
            &project.display().to_string(),
            "--table",
            "studies",
            "--set",
            "study_id=ST-001",
            "--set",
            "name=合成示例研究",
            "--allow-write",
        ],
    );
    assert_eq!(appended["rows"], 1);

    // 上级记录先就位：实验单元与事件（后面的样本要引用它们）
    plugin_ok(
        &entry,
        &kernel,
        &[
            "append",
            "--project",
            &project.display().to_string(),
            "--table",
            "units",
            "--set",
            "unit_id=U-001",
            "--set",
            "study_id=ST-001",
            "--allow-write",
        ],
    );
    plugin_ok(
        &entry,
        &kernel,
        &[
            "append",
            "--project",
            &project.display().to_string(),
            "--table",
            "events",
            "--set",
            "event_id=EV-001",
            "--set",
            "study_id=ST-001",
            "--set",
            "date=2026-09-20",
            "--allow-write",
        ],
    );

    // 引用不存在的记录 → 必须被拦住（写入前的引用完整性检查）
    let mut dangling: Vec<String> = vec![
        "append".into(),
        "--project".into(),
        project.display().to_string(),
        "--table".into(),
        "samples".into(),
        "--set".into(),
        "sample_id=S-999".into(),
        "--set".into(),
        "event_id=EV-404".into(),
        "--allow-write".into(),
        "--json".into(),
        "--cercis".into(),
        kernel_s.clone(),
    ];
    let (code, out) = run(&entry, &dangling);
    assert_ne!(code, 0, "断链的引用居然写进去了：{out}");
    assert!(out.contains("EV-404"), "报错应点名断链的值：{out}");
    dangling.clear();

    // 没有写入授权 → 必须被拒绝（FR-P04：越权被拒绝）
    let mut denied: Vec<String> = vec![
        "append".into(),
        "--project".into(),
        project.display().to_string(),
        "--table".into(),
        "studies".into(),
        "--set".into(),
        "study_id=ST-002".into(),
        "--json".into(),
    ];
    denied.push("--cercis".into());
    denied.push(kernel_s.clone());
    let (code, out) = run(&entry, &denied);
    assert_ne!(code, 0, "未授权写入居然成功了：{out}");
    assert!(
        out.contains("写入授权"),
        "拒绝理由应说明是权限问题：{out}"
    );

    // 3. 通用导入：CSV 两行（先试运行，再正式导入）
    let csv = work.join("samples.csv");
    std::fs::write(
        &csv,
        "sample_id,event_id,unit_id,sample_type\nS-001,EV-001,U-001,soil\nS-002,EV-001,U-001,soil\n",
    )
    .unwrap();
    let dry = plugin_ok(
        &transfer,
        &kernel,
        &[
            "import",
            "--project",
            &project.display().to_string(),
            "--table",
            "samples",
            "--file",
            &csv.display().to_string(),
            "--dry-run",
        ],
    );
    assert_eq!(dry["rows_imported"], 2);
    assert_eq!(dry["dry_run"], true);

    let imported = plugin_ok(
        &transfer,
        &kernel,
        &[
            "import",
            "--project",
            &project.display().to_string(),
            "--table",
            "samples",
            "--file",
            &csv.display().to_string(),
            "--report",
            &work.join("import-report.md").display().to_string(),
            "--allow-write",
        ],
    );
    assert_eq!(imported["rows_imported"], 2);
    assert!(work.join("import-report.md").exists());

    // 模板之外的列必须被拦住（否则就是默默写脏数据）
    let bad = work.join("bad.csv");
    std::fs::write(&bad, "sample_id,event_id,unit_id,typo\nS-003,EV-001,ST-001,x\n").unwrap();
    let mut bad_args: Vec<String> = vec![
        "import".into(),
        "--project".into(),
        project.display().to_string(),
        "--table".into(),
        "samples".into(),
        "--file".into(),
        bad.display().to_string(),
        "--allow-write".into(),
        "--json".into(),
        "--cercis".into(),
        kernel_s.clone(),
    ];
    let (code, out) = run(&transfer, &bad_args);
    assert_ne!(code, 0, "未定义的列居然写进去了：{out}");
    bad_args.clear();

    // 4. 内核查询：真实读到 2 行（断链那行已被拦下，没有写进来）
    let queried = kernel_json(
        &kernel,
        &[
            "query",
            &project.display().to_string(),
            "--table",
            "samples",
            "--limit",
            "50",
        ],
    );
    assert_eq!(queried["count"], 2, "查询结果应为 2 行：{queried}");

    // 5. 通用导出：长表 + 宽表 + 说明书
    let out_dir = work.join("out");
    let exported = plugin_ok(
        &transfer,
        &kernel,
        &[
            "export",
            "--project",
            &project.display().to_string(),
            "--table",
            "samples",
            "--out",
            &out_dir.display().to_string(),
            "--readme",
            "--allow-write",
        ],
    );
    assert_eq!(exported["rows"], 2);
    let csv_out = PathBuf::from(exported["file"].as_str().unwrap());
    assert!(csv_out.exists(), "导出文件应存在：{}", csv_out.display());
    let readme = PathBuf::from(exported["readme"].as_str().unwrap());
    assert!(readme.exists(), "说明书应存在：{}", readme.display());
    let readme_text = std::fs::read_to_string(&readme).unwrap();
    assert!(readme_text.contains("列字典"), "说明书应含列字典");
    assert!(readme_text.contains("复现命令"), "说明书应含复现命令");

    // 6. 通用体检：只读，且应报"未引用/断链"这类问题为 0 错误
    let report = plugin_ok(&health, &kernel, &["--project", &project.display().to_string()]);
    assert_eq!(report["action"], "health");
    assert_eq!(
        report["summary"]["errors"], 0,
        "刚建好的项目不该有问题：{report}"
    );

    let _ = std::fs::remove_dir_all(&work);
}

/// 清单里声明的每个命令，都必须是插件真的认识的子命令。
///
/// 这条测试是给"清单写 `entry append`、入口却只吃 `append`"这类
/// 清单与实现不一致的问题兜底的——否则 app 会在运行时才报错。
#[test]
fn declared_commands_match_each_plugin_cli() {
    let manifests = workspace_root().join("plugins");
    let mut checked = 0usize;

    for entry in std::fs::read_dir(&manifests).expect("插件清单目录应存在") {
        let dir = entry.unwrap().path();
        let manifest_path = dir.join("plugin.yaml");
        if !manifest_path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&text)
            .unwrap_or_else(|e| panic!("{} 不是合法 YAML：{e}", manifest_path.display()));

        let entrypoint = doc["entrypoint"].as_str().unwrap_or_default().to_string();
        let bin = workspace_root().join("target/debug").join(&entrypoint);
        if !bin.is_file() {
            skip(&format!("{entrypoint} 未构建，跳过命令一致性检查"));
            return;
        }

        let commands: Vec<String> = doc["ui"]
            .as_sequence()
            .map(|list| {
                list.iter()
                    .filter(|u| u["kind"].as_str() == Some("command"))
                    .map(|u| u["command"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !commands.is_empty(),
            "{entrypoint} 的清单里至少要声明一个 command 扩展点"
        );

        for command in commands {
            let mut args: Vec<String> = command
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            args.push("--help".into());
            let output = Command::new(&bin).args(&args).output().unwrap();
            let err = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "清单声明 `{entrypoint} {command}`，但入口不认这个命令：\n{err}"
            );
            checked += 1;
        }
    }

    assert!(checked >= 4, "应至少检查 4 个命令，实际 {checked}");
}

#[test]
fn plugin_manifests_are_accepted_by_kernel() {
    let Some(kernel) = kernel_bin() else {
        skip("内核二进制未构建");
        return;
    };
    let manifests = workspace_root().join("plugins");
    if !manifests.is_dir() {
        skip("未找到插件清单目录");
        return;
    }

    let work = tmp("manifests");
    let project = work.join("proj");
    let kernel_json_out = kernel_json(
        &kernel,
        &[
            "init",
            &project.display().to_string(),
            "--name",
            "清单校验（合成数据）",
        ],
    );
    assert_eq!(kernel_json_out["ok"], true);

    // 把工具包里的插件目录整个装进项目的 plugins/（真实分发形态）
    let dest = project.join("plugins");
    std::fs::create_dir_all(&dest).unwrap();
    for entry in std::fs::read_dir(&manifests).unwrap() {
        let entry = entry.unwrap();
        if !entry.path().is_dir() {
            continue;
        }
        let target = dest.join(entry.file_name());
        std::fs::create_dir_all(&target).unwrap();
        std::fs::copy(entry.path().join("plugin.yaml"), target.join("plugin.yaml")).unwrap();
    }

    let found = kernel_json(&kernel, &["plugins", &project.display().to_string()]);
    let plugins = found["plugins"].as_array().unwrap();
    let ids: Vec<&str> = plugins
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    for expected in [
        "project_wizard",
        "data_entry",
        "import_export",
        "health_check",
    ] {
        assert!(ids.contains(&expected), "内核未识别插件 {expected}：{ids:?}");
    }

    // 项目内插件默认未信任（D27 / FR-P31）
    for p in plugins {
        assert_eq!(p["trusted"], false, "项目内插件默认不应被信任：{p}");
        assert_eq!(p["tier"], "basic");
    }

    // 体检插件是只读的：权限摘要里不应出现"写入数据"
    let health = plugins
        .iter()
        .find(|p| p["id"] == "health_check")
        .unwrap();
    assert!(
        !health["permissions"]
            .as_str()
            .unwrap_or_default()
            .contains("写入"),
        "只读插件不该声明写入权限：{health}"
    );

    let _ = std::fs::remove_dir_all(&work);
}
