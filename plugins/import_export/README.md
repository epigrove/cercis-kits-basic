# 通用导入导出（import_export）

## 导入

```bash
cercis-plugin-transfer import --project <项目> --table <表> --file 数据.csv \
  --map 样品编号=sample_id --map 测定值=value_num \
  --report 导入报告.md --allow-write

# 试运行：只检查不写入
cercis-plugin-transfer import --project <项目> --table <表> --file 数据.csv --dry-run
```

- 列映射：`--map 源列=目标列`；没写映射时按同名对应。
- 模板未定义的列默认**报错**（`--skip-unknown` 才跳过）——宁可让用户看见，也不默默丢列。
- 导入前查：列名、必填、类型、主键重复、引用完整性；有问题整批不写。
- 一次导入 = 一次事务 + 一条流水：整批共享 `batch_id` 与 `source_ref`（源文件路径）。
- `--report` 生成 Markdown 报告：源文件指纹、列映射、行数、警告、复现命令。

## 导出

```bash
# 长表 + 说明书
cercis-plugin-transfer export --project <项目> --table <表> \
  --out 导出目录 --format csv --readme --allow-write

# 宽表（把某列的值透成列）
cercis-plugin-transfer export --project <项目> --table observations \
  --out 导出目录 --wide trait_id --value value_num --readme --allow-write

# 带筛选与排序
cercis-plugin-transfer export --project <项目> --table observations \
  --out 导出目录 --filter value_num:gt:10 --sort measured_at:desc --allow-write
```

- 导出**不改数据**，但会往变更流水里记一条 `export`（导出过什么、导到哪），
  因此同样需要写入授权。
- 宽表固定为矩形：所有透视列都在，缺的格子为空；同一格出现多次时保留最后
  一个非空值，并把冲突数写进结果（不静默丢数据）。
- 说明书（`.README.md`）含列字典（中英文名、类型、单位、必填、引用）与复现命令，
  目标是"半年后还看得懂"。

权限：`read` + `write`。
