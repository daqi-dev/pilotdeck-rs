---
type: experiment
number: "012"
date: 2026-10-14
title: A5.2 · #19 六格 TS 进程内实测
status: 第 2 轮 FIFO 装置修正、重测与撤回完成；修法与评分口径待人决定
related: [plan/005_phase-a-session-domain, research/008_phase-a-session-domain/gap, research/008_phase-a-session-domain/decision]
---

# exp012 · #19 六格实测（PR #71 第 2 轮）

**本轮 280 次测量，28 个子例 × model/tool 两阶段 × 5 次；56 组全部稳定。快格与 PR HEAD 的 20 组摘要、100 次完整语义观察一致。** 改用真实 FIFO 后，原有 8 个慢子例均与对应快写结果一致；新增 4 个 G append 慢镜像子例也未倒序。当前 raw 全部为本轮重跑产物。

## 撤回上一轮结论

Claude 指出的 P1 成立。**撤回“慢 status 让次序倒转”和“慢 append 时 result 是 T”两条结论。它们是旧装置绕过基线 FIFO 造成的，不能作为生产基线证据。**

旧 `recordStatus` 自行写 JSONL，barrier 没有进入 `JsonlTranscriptWriter.writeTail`，因此允许后调用的 G 先落盘。旧 registry 慢注入在调用 `originalAppend` 之前，T 尚未进入 `FileRunRegistry.mutationChain`，G 就能抢先入队。旧报告的 durable `G → T`、慢格客户端 `S(G) → E(G)`（无 registry 时后接 T）及成功慢写下最终 `E(T)` 都来自这一装置错误，本轮不再出现。

修正后，成功写入的 durable status 均为 `T → G`；registry 成功写入的收尾均为 `S(T) → E(T) → S(G) → E(G)`，result 为 `E(G)`。原 8 个慢子例中唯一 result 为 `E(T)` 的仍是 `catch-append-fails-status-slow`：G 的 error snapshot 写入失败，盘上保留 T；这与相应快写失败子例一致，**原因是 G persist 失败，不是慢写超车**。新增镜像也不支持旧结论。

## 授权边界与复现

只测量、修装置和报告；未改产品、state、resources 或冻结尺子，未提交、未发 issue 评论。原任务依据仍为 [计划 005 A5.2](../../plan/005_phase-a-session-domain.md)、[gap §四](../../research/008_phase-a-session-domain/gap.md#四19宿主竞态的可定位部分与不可冒认部分)和 [D4](../../research/008_phase-a-session-domain/decision.md)。本轮根据 PR #71 第 2 轮任务处理装置 P1 和 GLM 的 G append 慢补测，不重新定修法、接受差异或评分口径。

现行入口：[run.py](../../../scripts/a5-19/run.py)，内部调用 [six_cells.mts](../../../scripts/a5-19/six_cells.mts)。后者要求 launcher 预先设置隔离环境，不能再直接运行。Node **v22.22.0**；只读基线 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck` @ `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。本轮工作树 HEAD 为 `c5ab36fb2884892366adf7923305fc0fe5130c46`，脚本 hash、524 个实际导入 TS 源文件 hash 和全部摘要见 [manifest.json](raw/manifest.json)。manifest 的每个来源仍采用对象分行写 `path`、`sha256`，保留 gitleaks 误报修法。

```sh
python3 scripts/a5-19/run.py \
  --baseline /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  --output _tmp/20261016-exp012-replay
```

上述入口从仓库根目录运行，输出目录必须未存在。launcher 在 Node 启动前隔离 HOME/XDG/TMPDIR/PILOT_HOME 并设两个 SKIP_BROWSER_OPEN，进程内夹具禁止 listen。它复跑当前 A5.2a 装置，当前规则和扩展对照见 [exp014](../exp014-a52-session-fixtures/readme.md)；不将新跑结果冒认为历史 exp012 原件。

2026-10-15 P2 清账实际用上述入口复跑，exit 0：`DONE 360 measurements; 0 unstable variant/phase groups; 524 unchanged baseline source files.` 当前夹具新增的对照使次数为 360；历史第 2 轮 280 次原始证据继续保留。原始输出保存在受保护的旧工作树 `_build/wt-p2c/_tmp/exp012-replay/`：[启动审计](../../../_build/wt-p2c/_tmp/exp012-replay/launch.json)、[运行日志](../../../_build/wt-p2c/_tmp/exp012-replay/run.log)，测量在同目录 `raw/`，不 listen。这是历史复跑，当前接续未将它冒认为新 HEAD 的重测；后续 #112 的改装须重新录制。

历史第 2 轮两条命令均 exit 0；最终输出 `DONE 280 measurements; 0 unstable variant/phase groups; 524 unchanged baseline source files.` 本轮日志为 `_tmp/20261014-a5-19/check-r2.log`、`run-r2.log`，快格对比记录为该目录的 `fast-comparison-r2.json`。Node 的 TypeScript / SQLite ExperimentalWarning 不算失败。脚本在导入基线前把 `PILOT_HOME` 指向新的 `_tmp/20261014-a5-19/run-*/home`。无 listen、模型请求、实际工具执行、sidecar 子进程或二进制下载；只读 git 子进程用于记录身份。

## 装置与真实串行化边界

1. 实际构造基线 `InProcessGateway → SessionRouter → AgentSession → TurnRunner → 注入 loop`，以及真实 `JsonlTranscriptWriter → JsonlSessionEventStore → JsonlSessionPersistence` 和可选的 `FileRunRegistry`。通过 `registerHooks` 解析原 `.ts`，只用 Node 内置 transform 加载；不复制、补丁或改写基线源码。
2. **同一会话的普通 transcript 和 gateway status 共用一个真实 writer。** 外层 `recordAgentStatusMessage` 只记 `status.attempt` 并同步调用原方法。延迟 / EIO 只发生在其底层 `eventStore.append` 内，此时真实 `writeTail` 已入队。未命中注入的操作调用原 `JsonlSessionEventStore.append`，经过真实持久化链落盘。普通 transcript 不再用自制 store。底层 `now/uuid` 是确定性夹具，原始 transcript sequence、entryId、parentEntryId 全保留。
3. status 慢时，先等 T 的 `status.block`，释放 loop unknown，再等 G 的 `status.attempt`；确认 G 尚未进入底层 store 后才释放 T。**不再等待 G 已发布**，因为 G 必须排在 T 后；沿用旧等待条件会在真实 FIFO 内死锁。
4. registry 的慢 / EIO **都在真实 `persist` 内、mutationChain 和文件锁内，内存变更之后、atomic snapshot 写盘之前**。外层 append 只记录并立即调用原方法。T 慢或 G 慢都在所选目标的第一个 persist（一般为 S；T status 失败时为 E）持有同一 gate；后续同目标 error persist 经过已释放的 gate。控制器等 G error append 已调用但还未进入 persist，确认没有越过阻塞操作，再释放 gate。失败子例只对指定 error persist 抛一次 EIO，保留失败时内存和磁盘记录。
5. `status.attempt/store.enter` 和 `registry.append.attempt/persist.enter` 分开记录。装置逐次检查调用顺序与内部进入顺序一致，且底层 append / persist 不并发；慢子例另有 `control.status.fifo.checked` / `control.registry.fifo.checked`。断言约束注入边界及观察完整性，不预设生产应返回哪套帧或 result。
6. deadline 捕获指定 `timeoutMs=123456` 的原 callback 后手动触发；其 abort/status/publish/close 逻辑不变。loop 在 model 或 tool 标记后停在 Promise，unknown 分支抛真实 `AgentLoopResultUnknownError`。未执行真实模型、工具、Module ledger 或 sidecar；固定时钟和 gate 仅用于构造可复现交错。15 秒 watchdog 用于发现装置挂起，不用 sleep 猜顺序。
7. 客户端通过真实 async iterator 收帧，前缀后暂停消费以保持外层 completion fence current。enqueue/close、fence 方法都只作同步被动记录；after-complete 另测消费到 done 后再释放 unknown。每个子例串行，model/tool 各 5 次。
8. durable 原文现在是**完整真实 transcript 文件**，`durableStatus.entries` 从独立读盘结果筛选 `type=agent_status_message`，不再来自额外 status 文件；无 status 只表示没有 status entry，不表示整个 transcript 为空。registry 同时直接读盘并用 fresh instance 的 get/events 复核。重复比较保留完整帧、status entry 和 registry event/record，仅忽略 record 的墙钟时间/PID、status detail 随机 errorId；未改冻结评分器。

## 六格新结果

均为观测值，核心六格为 timeout-first/current-handle。`T=turn_timeout`、`G=gateway_submit_failed`、`U=result_unknown`、`F=turn_failed`、`I=agent_invalid_state`；`S(x)=agent_status`、`E(x)=error`、`C(x)=turn_completed(finishReason=x)`。

只省略正常前缀：model 为 `turn_started → input_accepted → model_request_started`，tool 多一个 `tool_call_started`。`∅` 表示未收到收尾，观察器仍有完整前缀。

| 六格 | 客户端收尾帧 | durable status | registry 磁盘结果 | 重复 |
|---|---|---|---|---|
| 无 registry / 快 | S(T) → E(T) | T → G | 不适用 | 核心两阶段各 5/5 |
| 无 registry / 慢 | S(T) → E(T) | T → G | 不适用 | 两阶段各 5/5 |
| 无 registry / 失败 | T status 失败：E(T)；G status 失败：S(T) → E(T) | 前者仅 G；后者仅 T | 不适用 | 2 子例 × 2 阶段，各 5/5 |
| 有 registry / 快 | ∅（仅前缀） | T → G | S(T) → E(T) → S(G) → E(G)；failed / E(G) | 核心两阶段各 5/5 |
| 有 registry / 慢 | ∅（仅前缀） | T → G | 5 个子例均 S(T) → E(T) → S(G) → E(G)；failed / E(G) | 5 子例 × 2 阶段，各 5/5 |
| 有 registry / 失败 | 全部 ∅（仅前缀） | T status 失败仅 G；G status 失败仅 T；append 失败均 T → G | G error append 失败为 failed / E(T)，其余 failed / E(G)；详见逐项表 | 10 子例 × 2 阶段，各 5/5 |

### 每个 writer 子例

以下 seq/revision/lastSeq 为 model；tool 因多一个前缀全部加 1，其余顺序和结果相同。每行两阶段分别 5/5 一致。名称中的 `append-slow` 保留旧名，表示 T 慢；新增 `catch-append-slow` 表示 G 慢，共覆盖正常 status、T status 慢、T status 失败、G status 失败四个镜像。镜像改变阻塞目标，**不绕过 FIFO 强行让 G 先调用**。

| 六格 / 子例（raw variant.name） | 客户端收尾 | durable status | 磁盘 registry 收尾 events（model seq） | state / result | revision / lastSeq |
|---|---|---|---|---|---|
| no-registry-fast / `fast` | S(T) → E(T) | T → G | 不适用 | 不适用 | 不适用 |
| no-registry-slow / `status-slow` | S(T) → E(T) | T → G | 不适用 | 不适用 | 不适用 |
| no-registry-failure / `timeout-status-fails` | E(T) | G | 不适用 | 不适用 | 不适用 |
| no-registry-failure / `catch-status-fails` | S(T) → E(T) | T | 不适用 | 不适用 | 不适用 |
| registry-fast / `fast` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `status-slow-append-fast` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `status-fast-append-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `both-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `status-fast-catch-append-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-slow / `status-slow-catch-append-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-failure / `timeout-status-fails-append-fast` | ∅ | G | 4:E(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-status-fails-append-fast` | ∅ | T | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-failure / `timeout-status-fails-append-slow` | ∅ | G | 4:E(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-status-fails-append-slow` | ∅ | T | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-failure / `timeout-status-fails-catch-append-slow` | ∅ | G | 4:E(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-status-fails-catch-append-slow` | ∅ | T | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 |
| registry-failure / `timeout-append-fails-status-fast` | ∅ | T → G | 4:S(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-append-fails-status-fast` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) | failed / E(T) | 7 / 6 |
| registry-failure / `timeout-append-fails-status-slow` | ∅ | T → G | 4:S(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 |
| registry-failure / `catch-append-fails-status-slow` | ∅ | T → G | 4:S(T) → 5:E(T) → 6:S(G) | failed / E(T) | 7 / 6 |

### 次序与 complete 边界补测

这些子例放在 fast 格，不替代慢 / 失败格；两拓扑、两阶段均各 5/5，与 PR HEAD 的语义结果一致。

| 六格 / 子例（raw variant.name） | 客户端收尾 | durable status | 磁盘 registry 收尾 events（model seq） | state / result | revision / lastSeq |
|---|---|---|---|---|---|
| no-registry-fast / `unknown-first` | S(F) → E(I) → C(model_error) | F | 不适用 | 不适用 | 不适用 |
| no-registry-fast / `known-final-first` | C(completed) | ∅ | 不适用 | 不适用 | 不适用 |
| no-registry-fast / `after-complete` | S(T) → E(T) | T | 不适用 | 不适用 | 不适用 |
| no-registry-fast / `known-final-before-timeout-pending` | S(T) → E(T) | T | 不适用 | 不适用 | 不适用 |
| registry-fast / `unknown-first` | S(F) → E(I) → C(model_error) | F | 4:S(F) → 5:E(I) → 6:C(model_error) | completed / C(model_error) | 7 / 6 |
| registry-fast / `known-final-first` | C(completed) | ∅ | 4:C(completed) | completed / C(completed) | 5 / 4 |
| registry-fast / `after-complete` | ∅ | T | 4:S(T) → 5:E(T) | failed / E(T) | 6 / 5 |
| registry-fast / `known-final-before-timeout-pending` | ∅ | T | 4:S(T) → 5:E(T) | failed / E(T) | 6 / 5 |

- unknown-first：未 abort 的 unknown 由真实 TurnRunner 投影为 F/I/C(model_error)；timer 已清除，后续触发请求被跳过。registry 的 completed 是其对 turn_completed 的分类，result finishReason 仍为 model_error，不代表成功。
- known-final-first：已知结果已 return、AgentSession 已结束，gateway 发布完成帧并清 timer；transcript 没有 status entry。
- known-final-before-timeout-pending：loop 已 yield 已知完成，但其 return 被阻塞，TurnRunner 尚未向外发布完成；timeout 最终仍生效。此例不能冒充真实 Module known final 仲裁。
- after-complete：客户端已 done 且 fence.complete 后才释放 unknown；current=false，未新增 G status / 帧 / registry event。

## 独立证据与自检

六格 raw 分别为 [无 registry 快](raw/no-registry-fast.json) 50 次、[慢](raw/no-registry-slow.json) 10 次、[失败](raw/no-registry-failure.json) 20 次；[有 registry 快](raw/registry-fast.json) 50 次、[慢](raw/registry-slow.json) 50 次、[失败](raw/registry-failure.json) 100 次。raw 嵌入完整 transcript / registry 原文、未归一化 payload、完整 timeline 和错误堆栈，不依赖保留 `_tmp` 才能复核。

- 56 组 × 5 全部稳定；各要求的 gate / 故障均触发；读盘 status 与 commit journal、registry 与 fresh reader、客户端帧与真实 open queue 发布逐项一致。
- 六个观察副本破坏对照均检出：删除 timeout 帧、删除 durable status、漏掉 registry event、隐藏 result 的 T→G 覆盖，以及分别倒置 status.store.enter / registry.persist.enter 顺序。对照不写生产存储。
- 两个 fast 格全部 20 组摘要与 HEAD 完全一致；100 次完整语义观察逐项比较帧、status payload、registry events/record 也一致。新真实 transcript 的 sequence/entry 元数据没有与旧自制 status 序号混比；两者格式差异不算帧或持久 status 内容变化。
- `git diff --check` 通过；`gitleaks dir --no-banner --no-color --redact docs/experiments/exp012-a5-19-six-cells` 扫描约 8.62 MB，exit 0 / no leaks found，日志为 `_tmp/20261014-a5-19/gitleaks-r2.log`。
- 524 个导入基线文件前后 SHA-256 一致；基线 tracked 源码无变化（既有 untracked `tools/agent-loop-parity/__pycache__/`）。脚本和 manifest hash 相符；路径 / hash 分行格式保持。

可直接核查以下本轮 `model/1` timeline：

| raw / 子例 | 关键 order 与证据 |
|---|---|
| registry-fast / fast | 41 首次 close；45 current=true；46–49 G status 调用/落盘；53–66 依次 persist S(T),E(T),S(G),E(G)；71 才 complete。客户端无收尾，盘上 result=E(G)。 |
| registry-slow / both-slow | 38 T 在 store 内阻塞；43 G 已调用；44 核验 G 未进入 store；45 释放 T，47 T commit，49 G 进入。55 G error append 已调用；58 T persist 阻塞，59 核验 G 未越过；60 释放；71/75 才进入 G status/error persist。 |
| registry-slow / status-fast-catch-append-slow | 57–58 T error persist 已完成；61–62 G status persist 进入并阻塞；63 核验 G error 仍在队列；64 释放，69 才进入 G error persist；result=E(G)。 |

## 与机制分析的关系和边界

仍支持：close 不等于 complete；close 后 current=true 的 catch 仍可写 G，complete 后旧 catch 被挡住；registry append 后才 enqueue，timeout 的同步 close 可令自身帧丢失；failed record 仍接受后续 append，成功的后到 error 可覆盖 result。这里“后到”是**真实 FIFO 调用次序**，不能用队外 barrier 人造倒序证明。

新增修正：本轮真实 FIFO 中，慢 status 不让 durable 倒序，慢 T/G append 不让最终 result 从 G 变 T。status 失败后写队列可继续；G status 写失败仍尝试发布 S(G)/E(G)，因此 registry 可有 G、durable 却无 G。G error persist 失败时最终盘上保留 E(T)，这也说明不能把“总是 G”推广到所有故障。registry 失败触发的 U 在本轮受控 close 后被丢弃，原慢 status 下客户端能见 S(G)/E(U) 的证据随装置错误一并撤回。

1. **未测真实调度比例和端到端归因。** 手动 timer、消费暂停和 loop gate 只能证明指定交错稳定；不能量化生产丢帧概率或宣称 #19 历史 sidecar 差异全部定位。
2. **故障切面有限。** 现已使用真实 writer / event store / 文件持久化和 registry，但 status EIO 在底层 store append 前，registry EIO 在 snapshot 写入前；未测部分 I/O、rename 中途、进程崩溃、跨进程锁竞争或断电 fsync。独立读盘不等于 crash consistency。
3. **未定产品修复和最终评分。** 不把候选 Rust oracle 当基线断言，不回算 #19 或阶段 C 分数；真实 sidecar、cold restart、双连接等 A5.2 其余范围未完成，不关闭 #19。
