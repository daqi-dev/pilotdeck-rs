# 负控判定表比对（#1100 / M6-pr1）

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/_negkit/compare_verdicts.py old/summary.json new/summary.json
python3 -I src/pilotdeck-gateway/tests/fixtures/_negkit/compare_verdicts.py old/summary.json new/summary.json --json
python3 -I src/pilotdeck-gateway/tests/fixtures/_negkit/test_compare_verdicts.py
python3 -I -B src/pilotdeck-gateway/tests/fixtures/_negkit/negative_compare_verdicts.py /tmp/compare-verdicts-negative-new
```

仅使用 Python 标准库、只读两个输入文件，不调用 cargo、不导入 runner、不写源码树。

## 输入边界

顶层必须是带 `controls` 数组的 JSON 对象。以下五份宿主原始 JSON 已原字节保存在 `testdata/`，构成真实输出适配清单（路径均相对本目录）：

| 实际产出方 | 行结构（另有元数据） | 固定样本 | 行数 |
|---|---|---|---|
| `negative_suite.run_cases`，调用方 `mcp-http-encoding/negative.py` | `mutation/status/test/assertion` | [run-cases.json](testdata/run-cases.json) | 10 |
| `negative_suite.matrix_main`，调用方 `model-catalog-cwd/negative_controls.py` | `name/status/targets` | [matrix-main.json](testdata/matrix-main.json) | 15 |
| `negative-output-directory/negative.py` | `mutation/status/test/assertion` | [negative-output-directory.json](testdata/negative-output-directory.json) | 5 |
| `gateway-entry-guard/negative_controls.py` | `mutation/status/test`，缺 assertion | [gateway-entry-guard.json](testdata/gateway-entry-guard.json) | 50 |
| `sdk-run/negative_controls.py` | `name/status/test`，缺 assertion | [sdk-run.json](testdata/sdk-run.json) | 49 |

后三个脚本使用 `negative_process`；该共享模块只提供进程/源码保护工具，**不定义 summary 格式**，不能把某个消费者已覆盖说成所有消费者已覆盖。`targets` 必须是非空的 `[test, assertion]` 列表。

`mutation/test` 以 `mutation` 为 case 键；`name/test` 和 `name/targets` 以 `name` 为键。两种身份字段不能同时出现；`targets` 不能和顶层 `test/assertion` 混用。case 键保留大小写、首尾空白和 Unicode 码点，`alpha`/`Alpha`、`alpha`/`alpha `、NFC/NFD 均为不同 case，不做 strip/lower/Unicode 归一。

规范表行是 `(case, schema, status, bindings, hits)`。`schema` 精确保留 `mutation/test`、`name/test` 或 `name/targets`；两侧身份字段或绑定容器形态不同即报告“格式漂移”并退出 1，当前没有跨格式判等开关。`mutation/test` 与 `name/test` **仅在 assertion 字段缺失时**按下列已登记规则从 reason 提取绑定；显式 assertion 优先，若字段存在仍按原形状严格检查和比较。提取不到时断言标记为 `""`，JSON 的 `unbound_markers` 和人读“无绑定标记”节逐侧列出 case/test；该提示不改变等价判定，缺失与非空标记仍有差异。`name/test` 带显式 assertion 的分支由合成输入覆盖，暂无独立宿主样本，不声称真实 runner 已覆盖该形态。

reason 提取规则依据 `gateway-entry-guard/negative_controls.py` 的 `classify` 与恢复/守卫分支：成功判杀直接以 `message` 作为 reason。仅识别以 `#[0-9]+ ` 后接非空白字符开头的字符串；保留整个绑定消息，包括冒号、内部 `; evidence ...` 等分号文本。仅剥离该产出方追加的 `; restored positive failed`（后接字符串结尾或另一个 `; ` 诊断）、`; restored: `、`; source guard: ` 及其之后的诊断尾部。不扫描字符串内部的 issue 标记，也不把无该前缀的路径/耗时/自由文本当绑定。

`sdk-run` 使用 `negative_suite.judge` 的通用 receipt 说明，reason 不含绑定标记，因而显示“无绑定标记”；比对器不能从这类旧报告恢复未写出的 assertion。`shared-runner-failclosed` 等其他产出方的固定包装前缀（如 `failed outside the bound assertion: ...`）和不同恢复后缀未登记，不能据本规则声称其绑定完整可比；需真实样本另行适配。

`hits` 若存在必须为非空字符串组成的列表，允许空列表；原顺序和重复次数都保留，缺失规范化为 JSON null，与 `[]` 不同。所有已识别行形态统一比较 hits；targets 不变而 hits 缩水、换序、增删、出现/缺失都会退出 1。`negative_suite.judge(any_cell=True)` 允许部分 target 命中，所以不要求 hits 等于全部 targets。

状态只认精确的 `KILLED / SURVIVED / INVALID / PASS`，没有大小写折叠或别名推断。`COMPILED`、`SKIPPED(pending-reanchor)` 等返回 2。case、测试和显式断言必须是非空字符串；断言也可为非空字符串列表，按原形状和顺序比较。显式 `null`、空串、纯空白、空列表均拒绝，不能冒充缺失；`targets` 中不允许缺标记。多个 target 保留次序及配对关系。

行内 `reason` 除上述绑定提取外忽略，`compiled`、`restoredPass` 是不参与比较的已观测元数据；除此以外的未知行字段返回 2，包括 `anchor/message` 等其他脚本的绑定别名，显式 assertion 同时存在也不能绕过。顶层 `counts` 等元数据不参与等价判定。重复 JSON 成员、非标准 JSON 数字、缺必填字段、无效 UTF-8、错误类型均拒绝。

未适配的报告明确返回 2，后续须另加真实样本和负例：

- `parity-lock-release/negative_controls.py` 自行输出裸数组，每行只有 `mutation/status/reason`，没有测试绑定。它虽使用 `negative_process`，并非 `run_cases/matrix_main` 的输出；本轮不为它放松必填 test。
- `locale-adaptive/negative_controls.py` 的 `name/test/anchor` 与 `sdk-mcp-p2/negative.py` 的 `name/test/message`，断言别名尚未适配。
- `cases[]`、`results[]`、嵌套 `selections[]` 及其他未知容器。

## 真实样本来源与自测

源路径相对宿主主仓 `/Users/da/ws/pilotdeck-rs/_tmp/`。只拷 JSON，不拷日志；下列 SHA256 可核对原字节，保留原有 verdict（包括 INVALID），不把等价当成运行通过。

| testdata 文件 | 宿主原件 | SHA256 |
|---|---|---|
| `run-cases.json` | `20261026-issue-1037/remote-host/neg-mcp-http-encoding_negative/summary.json` | `8b118cd2549fba8178405e3b07c881650ac0fda452767b4cd3c197b8e2c94f42` |
| `matrix-main.json` | `20261026-issue-347/remote-host/neg-model-catalog-cwd_negative_controls/summary.json` | `bd53fbd95420b362ccc8b9f29ea5fc6604b9071557eb72292360fb648342f347` |
| `negative-output-directory.json` | `20261026-issue-1060/remote-host/neg-negative-output-directory_negative/summary.json` | `ffb685822bc42d99f5c87bb1b3ab0293201065eb8e7ff5727f4df03d611cc3aa` |
| `gateway-entry-guard.json` | `20261026-issue-1060/remote-host/neg-gateway-entry-guard_negative_controls/summary.json` | `7db2a9327c9505e9cb7ebd9fe50725a983f1048c51397093c94242597602bb14` |
| `sdk-run.json` | `20261026-issue-1086/remote-host/neg-sdk-run_negative_controls/summary.json` | `48944e460f0e3781cf99cfa475813db8ee6522bb54b62a03dd0935fe29cf2088` |

`test_real_runner_summaries` 对这五份原始文件逐一执行真实 CLI：原样比自身退出 0；删一行、翻一个状态、改测试绑定、改断言标记各退出 1。其中 `gateway-entry-guard` 的缺 assertion 形态另由 `test_real_reason_marker_changes` 直接修改 reason 绑定标记并要求退出 1；保持标记而改变已知诊断尾部要求退出 0。另覆盖真实矩阵 hits 缩水、原 marker 内部分号、未知 reason 的无绑定提示与身份格式漂移。新结构另有缺字段/坏类型/混合绑定负例；三组严格 case 键用例检查 JSON 与人读 missing 节均保留双方键。

`negative_compare_verdicts.py` 在临时目录创建比对器变异副本，原源码树不动。每个变异先通过 Python `compile()`，再跑绑定测试的正控→负控→恢复正控。判杀依据 unittest 的结构化 AssertionError、完整测试名与专属消息（测试类显式固定 `longMessage=True`）；无关异常和超时记 INVALID，不扫描旧日志判杀。全部变异跑完才汇总，任一 SURVIVED/INVALID 非零退出；输出目录须尚不存在。它不运行 cargo，也不另建仓库或构建目录。

## 判定和输出

| 退出码 | 含义 |
|---|---|
| 0 | 两表非空、无重复、case 集与逐行形态、状态、绑定及 hits 完全相同 |
| 1 | 有增删、格式漂移、状态/绑定/hits 变化、任一侧重复 case 或空表 |
| 2 | 输入不可读或无法按已知结构解读；不输出局部判等结论 |

**等价不等于全量通过**：两侧相同的 INVALID/SURVIVED 仍是等价；两侧相同的抽样结果也只是这两份表等价。比对器不证明 runner 成功、测试覆盖完整、源码相同或运行环境相同。这些由迁移任务的上游验收保证。

人读输出包含全量逐 case 表及八类差异节及“无绑定标记”提示节，丢 case 单独列 `P0 MISSING CASES`。不取重复键的第一条或最后一条，表内保留全部重复行及其 1 起始行号。case 顺序不影响判等，输出按 case 排序。

`--json` 在 stdout 输出一个对象：

- `equivalent`：是否等价；`totals.old/new` 是原始行数（含重复），`totals.union` 是 case 键并集大小；`same` 仅统计两侧各恰好一行且内容相同的 case。
- `differences`：`missing_cases`（含 `missing_from` 和 `severity: P0`）、`status_changes`、`test_changes`、`assertion_changes`、`schema_changes`、`hit_changes`、`duplicate_keys`、`empty_tables`。
- `unbound_markers`：未能取得断言标记的 side/case/test，仅提示能力边界，不声称这些行的断言相同。
- `table`：逐 case 的 `old/new` 规范行数组与 `equal`，保留重复行。
- `errors`：不可读/格式错误的 side、path、reason；有错误时退出 2，`totals/differences` 为 null、`same` 为 0、`table` 为空，避免把未读取的表误报成空表。
