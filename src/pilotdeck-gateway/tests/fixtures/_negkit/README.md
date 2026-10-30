# 负控判定表比对（#1100 / M6-pr1）

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/_negkit/compare_verdicts.py old/summary.json new/summary.json
python3 -I src/pilotdeck-gateway/tests/fixtures/_negkit/compare_verdicts.py old/summary.json new/summary.json --json
python3 -I src/pilotdeck-gateway/tests/fixtures/_negkit/test_compare_verdicts.py
```

仅使用 Python 标准库、只读两个输入文件，不调用 cargo、不导入 runner、不写源码树。

## 输入边界

顶层必须是带 `controls` 数组的 JSON 对象。每行支持以下明确结构：

- `mutation`、`status`、`test`、`assertion`：`negative_suite.run_cases` 及采用 `negative_process` 的独立 runner（如 `negative-output-directory/negative.py`）。
- `name`、`status`、`targets`：`negative_suite.matrix_main`；`targets` 是非空的 `[test, assertion]` 列表。

默认 case 键为 `mutation`，后一结构显式使用 `name`。不猜测其他 case 键，不接受两种身份/绑定同时出现。规范表行是 `(case, status, bindings)`，其中每个 binding 含测试和断言；两种结构可按同一规范表比较。

状态只认精确的 `KILLED / SURVIVED / INVALID / PASS`，没有大小写折叠或别名推断。`COMPILED`、`SKIPPED(pending-reanchor)` 等返回 2。case、测试和断言必须是非空字符串；断言也可为非空字符串列表，按原形状和顺序比较。多个 target 同样保留次序及配对关系；交换测试或断言不会被集合去重掩盖。

`reason`、`compiled`、`counts`、运行目录等非判定表字段不参与等价判定。不接受裸数组、`cases[]`、`results[]`、嵌套 `selections[]` 等其他报告，后续迁移须另加明确适配和负例。重复 JSON 成员、非标准 JSON 数字、缺字段、无效 UTF-8、错误类型均拒绝。

## 判定和输出

| 退出码 | 含义 |
|---|---|
| 0 | 两表非空、无重复、case 集与逐行状态及绑定完全相同 |
| 1 | 有增删、状态/绑定变化、任一侧重复 case 或空表 |
| 2 | 输入不可读或无法按已知结构解读；不输出局部判等结论 |

**等价不等于全量通过**：两侧相同的 INVALID/SURVIVED 仍是等价；两侧相同的抽样结果也只是这两份表等价。比对器不证明 runner 成功、测试覆盖完整、源码相同或运行环境相同。这些由迁移任务的上游验收保证。

人读输出包含全量逐 case 表及六类差异节，丢 case 单独列 `P0 MISSING CASES`。不取重复键的第一条或最后一条，表内保留全部重复行及其 1 起始行号。case 顺序不影响判等，输出按 case 排序。

`--json` 在 stdout 输出一个对象：

- `equivalent`：是否等价；`totals.old/new` 是原始行数（含重复），`totals.union` 是 case 键并集大小；`same` 仅统计两侧各恰好一行且内容相同的 case。
- `differences`：`missing_cases`（含 `missing_from` 和 `severity: P0`）、`status_changes`、`test_changes`、`assertion_changes`、`duplicate_keys`、`empty_tables`。
- `table`：逐 case 的 `old/new` 规范行数组与 `equal`，保留重复行。
- `errors`：不可读/格式错误的 side、path、reason；有错误时退出 2，`totals/differences` 为 null、`same` 为 0、`table` 为空，避免把未读取的表误报成空表。
