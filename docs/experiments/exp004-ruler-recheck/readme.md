# exp004 · 回捞 G03 v1 尺子并复核

2026-10-14；计划 001 Issue #1 / GitHub #7。基线 PilotDeck@cc7c4e6，Python 3.14.3 / Node v22.22.0。本 issue 不运行 Rust，不自行跑 53 场景；五遍全量 TS 原料由协调者串行提供。

## 假设

旧 G03 v1 的 parent/child × 生产者通道比较能解释基线自己出现的跨通道调度噪声，又能拒绝语义变化、缺场景、截断和两侧同坏。只有对照通过并得到足够大的 TS 稳定集合，才具备交独立审查与批准冻结的条件。

## 做法

先复制旧 verify_a9.py、formal-round.py、formal-bind.mjs，再改本 worktree 副本。三份脚本头部有来源绝对路径与原文件完整 sha256；旧现场未写入。trace.py / run.py 不复制、不修改；比较器以 importlib 私有模块名加载并在 exec_module 前注册 sys.modules。

旧 compare 的 native 左槽改为 sidecar；场景由基线 scenarios.json 中 pairs 含 pilotdeck 的清单取出（53 项），不再 glob。保留 G03 通道划分、原版 `_diff_semantic_records`、原版 `_partial_order_differences` 与 E predicates；把 E 规则的 FAIL 纳入总判决。E10 原来隐含于流内严格比较，E11 隐含于身份数量比较，此次提供显式结果名称；E6、E7 的 sidecar 要求对两侧都执行。E1v 原本标注 informative，此次依任务“全部适用规则”进入总判决。

两侧同坏使用原版 sidecar 场景 oracle 拒绝错误结果；合法的预期失败场景仍按原版 oracle 接受。输入健康检查拒绝空 trace、无终态、场景身份不符、sequence 断裂与非法 JSON。

先用 exp001 两遍样本调通：把 parity-run4 前缀替换为 parity-run1，G03 53/53 PASS；同份全轮自比 53/53 PASS。接着执行离线 controls：逐规则合法 witness 自比 PASS、注入后指定规则 FAIL；再做真实 trace 的模型请求改值、删场景、两类截断，以及两侧空/同错。详细输出在本 worktree `_tmp/controls.json`。

五遍原料 DONE 后运行 `self_compare.py`，计算全部 10 对的交集，以及 r1 基准四对、相邻四对的交集。每轮清单和健康检查齐全，blocked 与 oracleFailures 均为空；保存全部 trace 输入哈希和代码哈希，便于离线复算和发现陈旧证据。

### 实际适配与 trace 边界

上游 adapter 的 `pilotdeck_gateway_impl.mjs:564–571` 会覆盖环境中的 sidecar COMMAND 为 process.execPath。因此 Rust 分支除设指定的生产环境变量外，还沿用旧驱动的生产 YAML module binding（`createLocalGateway.ts:630` 附近 bindingFactory 优先），args 为生产约定的一个 TS sidecarPath 参数；**不通过 spawn 拦截换二进制**。bind.mjs 只观察实际 spawn 的 command/args/pid/文件哈希，追加身份 proof。Rust 分支此 issue 未运行，其实际接入留给 Issue #2。

TS5 / exp001 原料只有上游 stdio/handshake/module 生产 proof，没有二进制哈希账本；本次对这些 TS 原料的身份依据限于基线装置默认 sidecar 与该 proof。新 round 在 trace 追加 sidecar_identity，并在 run.json 断言 side、sha256 和 spawnObserved；比较器读到新 round 的账本时要求该身份 proof。

原版 trace 的 seq 名为 `sequence`，不是抽象帧 seq；语义投影丢掉 envelope sequence，保留 logicalSequence/phase。本次保留原始 sequence 并校验连续性，不把它编造为帧级对拍。原版 ID 归一化也不是全部 ID 的一一映射，部分 toolCallId/id/callId 塌缩为同一个 `<generated-id>`。模型请求体、路径、并发工具例外及 E7 不可观测持久化时钟的完整现状写在 normalization.md；没有另造放宽规则。

旧 deadline declared extension 是 native→sidecar 的定向差异（原版 matcher 要求每个声明被实际命中），不适用于相同 sidecar 自比，已从这条比较路径移除。否则相同 deadline trace 也会被旧 extension matcher 拒绝。

## 结论

| 检查 | 结果 |
|---|---|
| TS 自比有效原料 | k = 5；五轮 53 场景齐全；0 BLOCKED / 0 oracleFailures |
| 默认 10 对全一致的 S | **53 / 53** |
| r1 基准四对的 S | 53 / 53 |
| 相邻四对的 S | 53 / 53 |
| 逐规则及端到端负向对照 | true；要求指定规则命中 |
| 两侧同坏 | true；空 trace、空目录、同一个错误均 FAIL |
| 正向对照 | true；同份真实全轮 PASS，纯跨通道重排 PASS |
| 冻结文件检查 | 已登记哈希通过；新文件未登记，明确跳过提示 |
| 独立审查 | null；协调者填写，不代填通过 |

S 外场景：无。详见 acceptance.md；若存在，每项均登记为「原版不稳定 / 待批」，尚未批准排除。`check_ruler.py` 在机器检查通过但独立审查待定时返回 2，避免把三个机器对照冒充四项验收齐全。

## 改变了哪个取舍

TS sidecar 自比可按回捞后的 G03 v1 通道口径校准；上游严格逐位置模式不再直接用作 Rust 验收尺子。此次默认稳定集合达到冻结下限 45，可把代码、口径草稿和对照证据交协调者审查/批准；尚不冻结新文件，不据此启动 Rust 迁移。新增身份 proof 使后续量分可确认确实启动了预期的 sidecar。

## 结论的边界

对照覆盖有限构造样本，不能证明比较器对所有未知错误都完备；独立审查尚未完成。五遍 TS 稳定仅对这批运行、固定基线与 G03 投影成立，不证明所有负载/版本都稳定。产出的 S 只由 TS 数据得到。

测试表面是 gateway 的 agent-loop mock 装置；落盘只比 trace 投影，未逐字节比真实磁盘全部文件，也未覆盖 ui/server 或真实模型。原版 ID 塌缩、proof 排除、并发工具归一化、不可观测持久化时钟均继承原边界。

没有运行 Rust；Rust 生产 binding 与带路径参数的入口需在 Issue #2 对实际二进制验证。独立评审、草稿批准与 FROZEN 登记由协调者处理。工作文件已就绪，但本次 Git 提交被沙箱禁止写 worktree 外的 gitdir/index.lock，提交消息保留在本 worktree `_tmp/commit-*.txt`。

## 原始数据路径

- `/Users/da/ws/pilotdeck-rs/_tmp/20261014-ts5/r1` … `r5`，完成标记 `progress.txt`；每份取 `*.pilotdeck-current-sidecar.jsonl`，没有取 native trace 定 S。
- `/Users/da/ws/pilotdeck-rs/_tmp/20261014-exp001/parity-run1`、`parity-run4`，仅用于调通与真实负向对照。
- 本 worktree `_tmp/self-compare.json`、`self-compare.md`、`controls.json`、`ruler-check.json`、`exp001-compare.json`；完整输入/代码哈希随 JSON 保存，临时控制副本不保留。
- 新驱动单场景冒烟结果随后记入本 worktree `_tmp/RESULT.md`（`_tmp/20261014-parity/ts-pure-text-smoke/run.json`）。
