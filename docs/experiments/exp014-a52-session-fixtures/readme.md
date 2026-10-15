---
type: experiment
number: "014"
date: 2026-10-15
title: A5.2a · #19 六格复核与独立失败对照
status: 基线实测完成；D4 第二步差异与评分口径待人决定
related: [plan/005_phase-a-session-domain, experiments/exp012-a5-19-six-cells, research/008_phase-a-session-domain/gap]
---

# A5.2a 六格实测

**36 个子例 × model/tool 两阶段 × k=5，共 360 次测量，72 组全部一致；9 个独立失败对照命中预注册规则。** 本表只陈述 cc7c4e6 基线的受控实测，没有 Rust 结果，没有接受差异、产品修法或评分决定。[results.json](results.json) 留存逐子例摘要、规则、expected/executed 集合和原始证据 SHA-256；完整证据在本 worktree 的 [`_tmp/a52/final-run/`](../../../_tmp/a52/final-run/)。

本次从 main 已有的 [exp012](../exp012-a5-19-six-cells/readme.md) 和真实 FIFO 装置继续工作，保留原 28 个子例，新增 8 个子例。未覆盖旧实验或其 raw。新入口为 [run.py](../../../scripts/a5-19/run.py)，观察规则为 [observation_rules.mjs](../../../scripts/a5-19/observation_rules.mjs)，测量主体为 [six_cells.mts](../../../scripts/a5-19/six_cells.mts)。

## 复现和位置选择

```sh
python3 scripts/a5-19/run.py --output _tmp/a52/reproduction
```

输出目录必须不存在，每次启动保留独立原始证据。Node v22.22.0，基线固定为 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`；可用 `--baseline` 指向同 commit 的只读目录。无需 npm install、编译基线、listen 或 sidecar 子进程，使用基线已有 node_modules。Node 内置 TypeScript transform 只负责加载原源码；524 个实际导入 TS 文件前后 SHA-256 一致。

沿用现有 `scripts/a5-19/`，理由是此处已有独立 TS-only 进程内测量装置及 FIFO 修正历史；共享装置避免另复制一套生产模拟。`scripts/gateway-parity/common.py` 的 `apparatus_hashes()` 会拒绝其目录下未登记的 `.py/.mjs/.mts`，把本次新文件放进建议目录会触发冻结白名单。**本次没有修改冻结目录、登记 hash 或接入冻结评分器。** 两套 check_frozen 通过，既有白名单集合不变；后续若接入 E1，应交协调者按 #67 的 A5 独立尺子流程评审并登记。当前 `session_domain.py --suite calibrate` 仍为 NOT_IMPLEMENTED，不能用本夹具宣称完整 E1/A5.2 通过。

Python 在启动 Node **之前**把 HOME、XDG_CONFIG_HOME、XDG_CACHE_HOME、XDG_DATA_HOME、TMPDIR、PILOT_HOME 分别指向 output 内新的临时目录，并设置 `PILOTDECK_SKIP_BROWSER_OPEN=1`、`SKIP_BROWSER_OPEN=1`。只继承 PATH/语言设置，清除用户 provider、sidecar 和 Node 启动注入变量。Node 在动态导入基线前验证隔离，拦截 TCP Server.listen；未尝试绑定端口。launch.json 记录父/子 PID、实际环境及退出状态，Node 已回收。没有端口查杀、用户进程清理或用户 home 写入。

## 注入和三个独立观察面

实际链为基线 `InProcessGateway → SessionRouter → AgentSession → TurnRunner → 注入 loop`。model/tool 是 loop 的等待位置标记；工具阶段实际输出 `tool_call_started`，但没有模型请求、真实工具副作用、Module ledger 或 sidecar。这是宿主受控交错复现，不是端到端归因。

1. **实时帧**：通过真实 submitTurn async iterator 逐帧消费，完整 payload 与顺序保留。queue.enqueue/close 仅同步被动记录并立即调用原方法；分别记录 open 发布和 close 后被丢弃的入队尝试。FRAME_PUBLICATIONS 独立比对客户端帧与 open 发布 journal，不能因 registry 有 event 就算客户端收到。
2. **durable status**：普通 transcript 与 status 共用真实 `JsonlTranscriptWriter → JsonlSessionEventStore → JsonlSessionPersistence`。在底层 eventStore.append 注入 gate/EIO，操作已经进入真实 writeTail。外层 record 方法同步调用原方法，仅记录 attempt。从实际 transcript.jsonl 重新读取完整原文，筛选 status entries，独立比较成功 commit 的完整 entry、次数、顺序与 FIFO 接纳次序。没有把写入尝试算落盘，也没有用帧反推 status。
3. **registry**：显式把 `new FileRunRegistry(tempRegistryPath)` 注入 gateway 的 runRegistry option；这是 `createLocalGateway.ts:996` 默认使用的生产类，未用 mock append 替代。append 外层同步委托，gate/EIO 在真实 persist 内、mutationChain 和文件锁内、内存变更之后、atomic snapshot 写盘之前。实际成功操作调用原 persist；失败时保存内存与当时磁盘 record。收尾后直接读取 registry.json，并用 fresh FileRunRegistry 的 get/events 独立复核，记录 events、seq、state、revision、lastSeq、完整 result 与原文。无 registry 格明确不适用。

timeout 使用固定 gateway now 与 `timeoutMs=123456` 的原 callback；捕获后显式触发，不等墙钟。Session/storage、timer 和 consumer 暂停都由可控 Promise/barrier 决定。原 queue.close、append .then 与外层 finally complete 仍由基线执行。慢 T status 时确认 G 已调用同 writer、尚未进入 store，再释放 T；慢 append 时确认 G error 已调用、尚未进入 persist，再释放 gate，不能绕过 FIFO 造倒序。G status 慢另阻塞 G 的底层写入。15 秒 watchdog 仅发现装置挂起，不参与交错。

原始 timeline 每条同步写入各测量目录的 timeline.ndjson；失败或 watchdog 另写 incomplete.json，不丢弃当时 transcript/registry。全组重复比较保留帧原始语义 payload、status entry、registry event/seq/state/revision/result，以及 close 后丢弃的入队序列；仅忽略 status detail 的随机 errorId 和 registry record 的墙钟时间/PID。原值一直保留。非稳定组单列 unstableGroups 和 observedAlternatives，本次为空，没有删除或跳过子例。

## 六格结果

`T=turn_timeout`，`G=gateway_submit_failed`，`U=result_unknown`；`S(x)=agent_status`，`E(x)=error`，`C(x)=turn_completed(finishReason=x)`。表中只省略正常前缀：model 为 `turn_started → input_accepted → model_request_started`；tool 再多一个 `tool_call_started`。`∅` 表示没有收到收尾，原始帧仍有完整前缀。`T → G` 均表示各一条成功持久化 status。

| 六格（核心 timeout-first/current-handle） | 实时收尾帧 | durable status | registry 实际落盘 | 子例与重复 |
|---|---|---|---|---|
| 无 registry / 快 | S(T) → E(T) | T → G | 未配置，不适用 | 核心 1 子例；两阶段各 5/5 |
| 无 registry / 慢 | S(T) → E(T) | T → G | 未配置，不适用 | T/G status 各慢，2 子例；两阶段各 5/5 |
| 无 registry / 失败 | T status 失败：E(T)；G status 失败：S(T) → E(T) | 前者仅 G；后者仅 T | 未配置，不适用 | 2 子例；两阶段各 5/5 |
| 有 registry / 快 | ∅ | T → G | S(T) → E(T) → S(G) → E(G)；failed，result=E(G) | 核心 1 子例；两阶段各 5/5 |
| 有 registry / 慢 | ∅ | T → G | 同快格，真实 FIFO 未倒序；failed，result=E(G) | 7 个快慢/双慢及 T/G 阻塞子例；两阶段各 5/5 |
| 有 registry / 失败 | ∅ | 随 status 故障仅 T、仅 G 或 T → G | G error append 失败保留 result=E(T)；其他子例 result=E(G)，详见逐项表 | 15 个独立/交叉故障子例；两阶段各 5/5 |

有 registry 的 timeout 自身 status/error 都在同步 close 后才尝试 enqueue；迟到 G 同样被丢弃。故障触发的 U 也在 close 后被丢弃。以上是本次指定交错的观察值，不推断生产调度概率或保证所有故障都会如此。

**重连可从 registry 回放**：同 runId 的重复 submitTurn 接纳会读取 registry events（基线 `InProcessGateway.ts:913`），也可通过 run_events 查询；上表的 `∅` 只指原 submitTurn 实时流，不表示事件无法恢复，也不表示真实 Web bridge 已自动发出恢复请求。

## 逐项 writer 实测

下面每行采用 model 的实际 seq；tool 多一个前缀，registry seq、revision、lastSeq 全部加 1，其余行为相同。全部两阶段各 5/5 一致。快格的其他次序另表，不计作慢/失败格替代。

| 六格 / 子例 | 实时收尾 | durable status | registry 磁盘收尾 events（seq） | state / result | revision / lastSeq |
|---|---|---|---|---|---|
| no-registry-fast / `fast` | S(T) → E(T) | T → G | 不适用 | N/A / N/A | N/A / N/A |
| no-registry-slow / `status-slow` | S(T) → E(T) | T → G | 不适用 | N/A / N/A | N/A / N/A |
| no-registry-slow / `catch-status-slow` | S(T) → E(T) | T → G | 不适用 | N/A / N/A | N/A / N/A |
| no-registry-failure / `timeout-status-fails` | E(T) | G | 不适用 | N/A / N/A | N/A / N/A |
| no-registry-failure / `catch-status-fails` | S(T) → E(T) | T | 不适用 | N/A / N/A | N/A / N/A |
| registry-fast / `fast` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `status-slow-append-fast` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `status-fast-append-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `both-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `status-fast-catch-append-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `status-slow-catch-append-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `catch-status-slow-append-fast` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `catch-status-slow-append-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-failure / `timeout-status-fails-append-fast` | ∅ | G | 4:E(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-status-fails-append-fast` | ∅ | T | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-failure / `timeout-status-fails-append-slow` | ∅ | G | 4:E(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-status-fails-append-slow` | ∅ | T | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-failure / `timeout-status-fails-catch-append-slow` | ∅ | G | 4:E(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-status-fails-catch-append-slow` | ∅ | T | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-failure / `timeout-append-fails-status-fast` | ∅ | T → G | 4:S(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `timeout-status-event-append-fails` | ∅ | T → G | 4:E(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-status-event-append-fails` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-append-fails-status-fast` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) | failed / E(T) | 7 / 6 |
| registry-failure / `timeout-append-fails-status-slow` | ∅ | T → G | 4:S(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-append-fails-status-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) | failed / E(T) | 7 / 6 |
| registry-failure / `timeout-status-and-catch-append-fail` | ∅ | G | 4:E(T) → 5:S(G) | failed / E(T) | 6 / 5 |
| registry-failure / `catch-status-and-timeout-append-fail` | ∅ | T | 4:S(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `timeout-append-fails-catch-status-slow` | ∅ | T → G | 4:S(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |

## unknown / known 与旧 handle 边界

| 拓扑 / 子例 | 实时收尾 | durable status | registry 磁盘收尾 events（seq） | state / result | revision / lastSeq |
|---|---|---|---|---|---|
| no-registry-fast / `unknown-first` | S(F) → E(I) → C(model_error) | F | 不适用 | N/A / N/A | N/A / N/A |
| no-registry-fast / `known-final-first` | C(completed) | ∅ | 不适用 | N/A / N/A | N/A / N/A |
| no-registry-fast / `after-complete` | S(T) → E(T) | T | 不适用 | N/A / N/A | N/A / N/A |
| no-registry-fast / `known-final-before-timeout-pending` | S(T) → E(T) | T | 不适用 | N/A / N/A | N/A / N/A |
| registry-fast / `unknown-first` | S(F) → E(I) → C(model_error) | F | 4:S(F) → 5:E(I) → 6:C(model_error) | completed / C(model_error) | 7 / 6 |
| registry-fast / `known-final-first` | C(completed) | ∅ | 4:C(completed) | completed / C(completed) | 5 / 4 |
| registry-fast / `after-complete` | ∅ | T | 4:S(T) → 5:E(T) | failed / E(T) | 6 / 5 |
| registry-fast / `known-final-before-timeout-pending` | ∅ | T | 4:S(T) → 5:E(T) | failed / E(T) | 6 / 5 |

- unknown-first：未 timeout abort 的真实 unknown 被 TurnRunner 投影成 `turn_failed / agent_invalid_state / C(model_error)`；timer 已清除，后续触发请求被跳过。registry 的 state=completed 不表示成功，result 的 finishReason=model_error。
- known-final-first：已知完成结束 session generator，基线清 timer，只有 C(completed)，没有 durable status。
- after-complete：消费到客户端 done，确认真实 fence.complete 后才释放 unknown；随后 current=false，没有新增 G status/frame/registry event。这是运行期旧 handle 检查，未混入接纳前 releasePreflight。
- known-final-before-timeout-pending：loop 已 yield 完成，但 return barrier 未释放；timeout 生效，只留下 timeout status/result。此例说明 loop 已知输出尚未到外层终态发布边界，不能冒充真实 Module ledger 的 known final 仲裁。

支持 gap §四中的受控机制：queue.close 不等于 handle complete；仍 current 的 catch 可追加 G；FileRunRegistry 的 failed 允许后续 append，并可覆盖 result。**bug②的整体归因仍证据不足**：未实测真实 sidecar 的模型/工具调用、历史 #19 差异的全部原因、自然调度比例或冷重启。没有把受控稳定性写成生产稳定性。

## 预注册失败对照

`preregistration.json` 在任何测量前落盘，登记完整 case 集合与观察规则；这些规则检查证据一致性，不预填 TS 应返回什么终态。每个对照使用实际测量结果的独立副本破坏 observer，必须捕获**指定 rule ID**；普通异常、超时或非零退出码不能算命中。不会修改生产文件。

| 破坏对照 | 预注册 / 实际命中规则 | 结果 |
|---|---|---|
| `drop-timeout-frame` | `FRAME_PUBLICATIONS` / `FRAME_PUBLICATIONS` | 检出 |
| `omit-registry-events` | `REGISTRY_EVENTS` / `REGISTRY_EVENTS` | 检出 |
| `ignore-result-overwrite` | `REGISTRY_RESULT` / `REGISTRY_RESULT` | 检出 |
| `attempt-as-success` | `STATUS_COMMITS` / `STATUS_COMMITS` | 检出 |
| `registry-attempt-as-success` | `REGISTRY_COMMITS` / `REGISTRY_COMMITS` | 检出 |
| `both-sides-bad` | `FRAME_PUBLICATIONS` / `FRAME_PUBLICATIONS` | 检出 |
| `drop-durable-status` | `STATUS_RAW` / `STATUS_RAW` | 检出 |
| `reverse-status.store.enter` | `STATUS_FIFO` / `STATUS_FIFO` | 检出 |
| `reverse-registry.persist.enter` | `REGISTRY_FIFO` / `REGISTRY_FIFO` | 检出 |

“把尝试当成功”对照同时伪造 decoded status 与其声称的 rawText，仍被独立 commit journal 拒绝。registry 版本同样伪造 events/raw snapshot/record，仍被成功 append journal 拒绝。“两侧同坏”把同一坏帧副本传给两侧，先分别核对其独立原始证据再比较，不能因两边相等通过。

## 验证和交接边界

完整命令、原始输出、资源与失败尝试记录见 [`_tmp/RESULT.md`](../../../_tmp/RESULT.md) 和 [`_tmp/a52/`](../../../_tmp/a52/)。本次实测全部 360 次完成，expectedCases=executedCases，无空组、隐式 skip 或非稳定组。全 workspace fmt/clippy/test、两套冻结检查、ledger check 与 diff 检查最终均通过；test 为 809 passed / 0 failed，另有源码既定的 2 个 release-only resource tests ignored。命令、失败缓存尝试与最终日志由 RESULT 记录。

没有提交，没有改 state/specs/冻结脚本/基线。D4 第二步仍由人依据实测决定差异与评分；未改 accepted-diffs 或回算阶段 C 分数。双连接、多会话、cold-restart 留给 A5.2b；真实 sidecar 与完整 E1 定标也未宣称完成。故障仅切在 status 底层 append 前及 registry snapshot persist 前，未测部分写入、rename 途中、断电、跨进程锁或 fsync/crash consistency。协调者移除 worktree 前须归档 `_tmp/a52/`，否则本文件指向的完整原始证据会随 worktree 丢失。

## 2026-10-16 · #112 append promise 保真复算

`registry.append` 观察层改为直接调用原方法并返回同一 Promise；旁路 `then(success, failure)` 只记提交/失败，不向调用者替换 Promise、不重抛、不新增入场前 await。pending 集合跟踪原 Promise，生产 mutationChain/FIFO 入场与同步 throw 保持原调用路径。

重新执行72组、360次（registry270次），无不稳定组，9项负控与原件相同。360条完整投影逐字段一致：实时帧、durable status entries、registry record/events/result，以及 close 后 enqueue payload；沿用原 signature 仅排除 registry acceptedAt/updatedAt/ownerPid 和 detail.errorId。72组summary也与已发布 results.json 和归档 final-run 精确一致。原始证据与比较脚本：`_tmp/20261016-a5-112/20261016-append-regression/`、`20261016-regression-comparison.json`、`20261016-compare.py`；未改原 results.json 或冻结尺子。

初轮独立评审后补观察callback故障闸门和逐资源cleanup；最终重新运行`_tmp/20261016-a5-112/20261016-review-append-final/`，72组360次/registry270/9负控仍通过，360条完整投影逐字段与历史一致。manifest现同时绑定append-observer.mjs及cleanup-observer.mjs。观察分支异常必在drain非零失败，原promise/同步throw不变；runtime-controls.mjs验证这些边界。最终比较脚本/结果为`20261016-review-compare.py`和`20261016-review-regression-comparison.json`。
