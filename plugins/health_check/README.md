# 通用体检（health_check）

```bash
cercis-plugin-health --project <项目>          # 给人看
cercis-plugin-health --project <项目> --json   # 给 app / AI 看
```

查六类问题，每条结论都带位置与处理方向：

| 类别 | 代号 | 级别 |
|------|------|------|
| 目录骨架（缺目录、残留临时文件） | `project.structure` | 提醒 |
| 引用断链、字段规则 | `validate.error` / `validate.warning` | 问题 / 提醒 |
| CSV 编码：非 UTF-8 / BOM / CRLF / 末尾无换行 | `encoding.*` | 问题 / 提醒 / 提示 |
| 命名冲突：重复列名、列名不规范、仅大小写不同的文件名 | `table.*` / `naming.case_conflict` | 问题 / 提醒 |
| 主键：空、重复 | `table.duplicate_primary_key` / `table.empty_primary_key` | 问题 |
| 附件：引用了却不在（断链）、在但没人引用（孤儿） | `media.missing` / `media.orphan` | 问题 / 提醒 |

**本插件是只读的**：清单里 `write: false`，拿不到写入授权，
所以"体检不会改数据"由权限模型保证，而不是靠自觉。
