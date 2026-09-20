# 通用录入（data_entry）

表格式录入：批量粘贴、单行录入、附件挂接。

```bash
# 单行
cercis-plugin-entry append --project <项目> --table <表> \
  --set 列=值 --set 列=值 --allow-write

# 批量粘贴（CSV/TSV/JSON 文件，或 - 从标准输入）
cercis-plugin-entry append --project <项目> --table <表> --from 粘贴.csv --allow-write

# 先试运行：只检查，不写入
cercis-plugin-entry append --project <项目> --table <表> --from 粘贴.csv --dry-run

# 附件挂接：复制进项目 media/，并写一行索引
cercis-plugin-entry attach --project <项目> --table <附件表> \
  --file 现场照片.jpg --set sample_id=S-001 --allow-write
```

写入前的五道检查：模板列名、必填字段、字段类型、主键重复（批内 + 与已有数据）、
**引用完整性**（引用的记录必须真实存在）。任何一道不过，整批都不写。

附件挂接时，模板里若有 `sha256` / `original_path` / `mode` 列会自动填充；
同名文件不会覆盖：内容相同则复用，内容不同自动加序号。

权限：`read` + `write`。
