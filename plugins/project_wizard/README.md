# 项目向导（project_wizard）

新建 Cercis 项目、装载模板、扫描已有文件夹。

```bash
cercis-plugin-wizard --project <新项目目录> --name <项目名> \
  [--template generic|example-study] [--scan <已有文件夹>] --allow-write
```

- `--template`：表结构与字段语义来自模板；内核本身不认识任何学科词。
- `--scan`：**只读**扫描一个已有文件夹，列出可导入的表与媒体文件，
  绝不移动或改名原文件（需求文档 FR-L 红线）。
- 建项目后会顺带生成 `AGENTS.md` 等 AI 材料，方便 AI 冷启动就正确读项目。

权限：`read` + `write`（建项目、装模板会写文件）。
