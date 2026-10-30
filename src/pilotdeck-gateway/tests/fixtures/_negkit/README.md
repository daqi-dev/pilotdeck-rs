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

## 统一 runner 契约（#1107 / M6-pr2）

本轮新增 `contract.py`，不迁移旧脚本，也不依赖 `negative_suite` / `negative_process` / `negative_panic` / `_shared`。调用方提供完整 `Case(mutation, test, assertion)` 表、非空 `oracle` 行集合、可选 case 名选择、`guard()` 和 `execute(case, env)`。

- `guard()` 是恢复并核验的上下文管理器；只有 `__enter__` 成功才标记已进入。产品源码调用方必须传 `negative_process.mutating_sources(root, paths)`，保留源码等于 HEAD 的守卫；不能换成快照任意工作区字节。`execute` 由适配器负责每个变异先编译、正控、变异测试及恢复正控；cargo 使用 `wait_for_slot` / `run_owned` 和调用方 target。本契约不替代这些产品源码与子进程所有权工具。
- `execute` 返回文本模式 `subprocess.CompletedProcess`，一次进程只能精确运行一个完整命名 libtest（不用 quiet；建议 `--exact --show-output`）。启动失败和超时须抛异常，不得伪造返回 101。普通 case 异常记 INVALID 后继续跑剩余项；取消及守卫/发布的任意 `BaseException` 使所有已完成和未运行项 INVALID，并清空 hits。
- 判杀要求返回码 101、恰一个命名 FAILED、恰一个同名 panic 和首行专属断言；支持独立消息或 `assertion … failed: #N …`。不接受 stdout、另一测试、源码位置、回溯、断言续行的标记。panic 正文的保留诊断前缀 `#813 timeout:` / `#813 gateway watchdog:` 不算杀死；未知 watchdog 也不能用非绑定断言判杀。左右值或正常断言消息里含 timeout/watchdog 词语不是诊断；回溯和 libtest 报告不属于正文。返回 0 且命名测试通过才是 SURVIVED。此窄形态没有 unnamed thread / 多测试 / 任一命中模式；迁移遇到这些形式必须显式增加判据、自测与等价证据。
- `CARGO_HOME`、`CARGO_TARGET_DIR`、`PDRS_BASELINE` 均须显式绝对路径，CARGO_HOME 与 PDRS_BASELINE 要求目录存在，target 可以待创建；保留调用方值。dev/test debug 默认 `line-tables-only`。运行与发布期间临时使用 Python `default_int_handler`，包括原先继承 SIG_IGN 的情况；退出还原旧 handler。不可捕获的 SIGKILL/断电不属于 Python BaseException 保证，不能将缺失 summary 解释为成功。
- 准入先校验 case / oracle / 环境，再按 #1060 语义接受不存在或空目录，独占创建 `.claim`。claim 永久保留；竞争败方立即拒跑，不探测 PID、不等待、不回收、不改名旧文件。
- 所有行先留在内存，守卫恢复与核验成功后才通过 `summary.json.tmp` → `summary.json` 一次原子发布。守卫/取消/发布错误不留下本次 summary；发布后、返回前取消也按 inode 撤销本次文件，返回对象保留整表 INVALID 诊断。只删除本次 inode，不删除并发外来文件。返回对象为 `RunResult(code, document, error)`。
- 最外层 finally 只根据成功结果和 entered 标志计算退出码：全选中项 KILLED 且发布成功为 0；进入守卫前拒跑为 1；进入后任何未通过（含 SURVIVED、INVALID、取消、恢复/核验/发布失败）为 2。异常类型与 `SystemExit.code` 不决定退出码。抽样退出 0 只代表选中项通过。

summary 顶层包括 `mode: full|sample`、按声明次序的 `selected` 名单、完整表的 `total`、`controls` 与 `counts`。每行**恰为** `mutation / test / assertion / status / reason / hits`，可直接交给本目录比对器。没有失败时缺省放行、空 case/oracle、重复 case、未知/重复/空选择或多行断言的兼容模式。比对器忽略顶层 metadata，因此等价依然不等于全量通过。

```sh
python3 -I -B src/pilotdeck-gateway/tests/fixtures/_negkit/test_contract.py
python3 -I -B src/pilotdeck-gateway/tests/fixtures/_negkit/negative_contract.py /tmp/new-contract-evidence
python3 -I -B src/pilotdeck-gateway/tests/fixtures/_negkit/toy_runner.py /tmp/new-toy-evidence
# --scenario refuse / restore-fail / cancel 分别演示 1 / 2 / 2；默认 pass 演示 0。
```

`toy_runner.py` 只修改自己的临时文本文件，使用有 5 秒上限的子进程观察变异，打印恢复时 summary 尚不存在的事实；没有 TS 或产品源码变异。自测将 toy 的全部模式跑在 10 秒子进程上限内，并比较两次正常 summary 为 EQUIVALENT。

`negative_contract.py` 只将本模块变异编译为内存模块（每个先 `compile()`），逐个跑命名 unittest 的正控→变异→恢复正控；结构化 AssertionError 的完整测试名和最后一段专属消息必须同时匹配。所有变异跑完才汇总，过期锚点、编译失败、无关错误一律 INVALID/非零，不把编译失败冒充判杀；无需提交新文件后绕 HEAD 运行。无参数模式在临时目录运行，供 `negative_anchor_selfcheck.rs` 与 comparator 自测一起进入 CI。每个显式 UNSUPPORTED 登记同时受到现有 anchor checker 的逐项移除、额外信号和过期登记负控覆盖。
