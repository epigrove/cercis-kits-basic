# 基础工具包（basic）

人人可用的基础功能。**它不是一个程序，而是一组独立插件**——
每个插件有自己的 `plugin.yaml`、自己的权限、自己的界面扩展点，
可以单独启停、单独授权、单独升级（需求文档 §14.4 第 2 层）。

```
basic/
├── crates/
│   ├── cercis-kit-core/        共用库：内核桥、行级检查、引用检查、输入解析
│   ├── plugin-project-wizard/  插件 1 · 项目向导
│   ├── plugin-data-entry/      插件 2 · 通用录入
│   ├── plugin-transfer/        插件 3 · 通用导入导出
│   └── plugin-health/          插件 4 · 通用体检
└── plugins/                    随包分发的插件清单（每个插件一个目录）
    ├── project_wizard/plugin.yaml
    ├── data_entry/plugin.yaml
    ├── import_export/plugin.yaml
    └── health_check/plugin.yaml
```

## 第一批插件（S5）

| 插件 | 能力 | 权限 | 入口 |
|------|------|------|------|
| `project_wizard` 项目向导 | 新建项目、选模板、扫描已有文件夹（只读） | 读 + 写 | `cercis-plugin-wizard` |
| `data_entry` 通用录入 | 批量粘贴、单行录入、附件挂接 | 读 + 写 | `cercis-plugin-entry` |
| `import_export` 通用导入导出 | CSV/TSV 导入（映射/试运行/报告）、长表与宽表导出、数据说明书 | 读 + 写 | `cercis-plugin-transfer` |
| `health_check` 通用体检 | 目录骨架、断链、编码、命名冲突、主键重复、孤儿附件 | **只读** | `cercis-plugin-health` |

> 后续批次（打包合并、备份恢复、插件管理器、通用查询界面）按里程碑加入同一目录。

## 共同的规矩

1. **写入只走内核**：插件通过 `cercis` 命令 + `--json` 干活，绕不过内核的
   原子写入、逐次存档与变更流水（需求文档 FR-P05）。
2. **默认只读**：要写入必须在清单里声明 `write`，并由 app 在调用时授予
   `--allow-write`；没授权时插件明确拒绝，而不是"悄悄写进去"。
3. **写前先检查**：列名、必填、类型、主键重复、**引用完整性**都在写入前查完；
   有问题就整批不写，不给项目留半成品。
4. **核心能力平台无关**：界面适配由各插件在 app 端通过扩展点声明，
   同一份逻辑服务桌面/Pad/手机（§14.6）。

## 构建与测试

```bash
cd kits/basic
cargo build          # 产出 4 个插件二进制
cargo test           # 20 个测试：单元 + 端到端（跨进程跑真内核）
                     # 含"清单里声明的命令必须是插件真认的子命令"
```

端到端测试会真的调用内核命令，因此需要内核二进制存在：

```bash
cd ../../kernel && cargo build
```

若插件二进制比源码旧（`cargo test` 不会刷新 `target/debug/` 下的插件），
测试会**跳过并说明原因**，而不是拿旧二进制假装通过。

## 手工试一遍

```bash
# 1. 建项目（项目向导插件）
cercis-plugin-wizard --project /tmp/demo --name 演示 --template example-study --allow-write

# 2. 录一行（录入插件）
cercis-plugin-entry append --project /tmp/demo --table studies \
  --set study_id=ST-001 --set name=演示研究 --allow-write

# 3. 导入 CSV（导入导出插件）
cercis-plugin-transfer import --project /tmp/demo --table studies \
  --file 更多研究.csv --report /tmp/demo/import-report.md --allow-write

# 4. 导出 + 说明书
cercis-plugin-transfer export --project /tmp/demo --table studies \
  --out /tmp/demo/exports --readme --allow-write

# 5. 体检（只读插件）
cercis-plugin-health --project /tmp/demo
```

每个命令加 `--json` 得到机器可读输出——app 与 AI 用的就是这条通道。
