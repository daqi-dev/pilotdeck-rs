---
type: experiment
number: "012"
date: 2026-10-14
title: A5.2 · #19 六格 TS 进程内实测
status: 实测与记录完成；修法与评分口径待人决定
related: [plan/005_phase-a-session-domain, research/008_phase-a-session-domain/gap, research/008_phase-a-session-domain/decision]
---

# exp012 · #19 六格实测

**六格已实测；240 次测量，48 个「子例 × 等待阶段」组合均在 5 次重复中得到一致的三面语义观察。** 有 registry / 快写的受控 timeout-first 中，客户端只收到运行前缀；durable status 为 `turn_timeout → gateway_submit_failed`；真实 FileRunRegistry 的 events 包含两套 status/error，最终 result 为 `gateway_submit_failed`。这只是指定交错的基线事实，不是修法、豁免或评分决定。

## 假设与授权边界

测前假设来自 [gap §四](../../research/008_phase-a-session-domain/gap.md#四19宿主竞态的可定位部分与不可冒认部分)：timeout、unknown catch、队列关闭、外层 complete 是不同边界；三份观察可能不一致。实验没有把任何具体帧、状态次数或 registry result 预设为“应当”。脚本只检查装置完整性、原始读取一致性与注入确已发生；结果表由跑完的 JSON 生成。

已读 [计划 005 A5.2](../../plan/005_phase-a-session-domain.md)、[D4 与人的拍板](../../research/008_phase-a-session-domain/decision.md)、CLAUDE.md 的“仲裁要实测”，以及 [#19 正文及全部 6 条评论](https://github.com/zhengdaqi/pilotdeck-rs/issues/19)。其中 [最新人的两步决定](https://github.com/zhengdaqi/pilotdeck-rs/issues/19#issuecomment-6162244998)要求先交证据再定口径。本轮只做这一步；未改产品、state、resources 或冻结尺子，未提交、未发 issue 评论。

## 做法与可复现命令

脚本：[scripts/a5-19/six_cells.mts](../../../scripts/a5-19/six_cells.mts)。本次 Node **v22.22.0**；只读基线 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck` @ `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。工作树 HEAD、脚本 hash、524 个实际导入 TS 文件的 SHA-256、执行目录和所有摘要见 [manifest.json](raw/manifest.json)。导入源码的前后 hash 一致；基线 tracked 文件无改动（只读检查见到的既有 untracked 项为 `tools/agent-loop-parity/__pycache__/`）。

```sh
cd /Users/da/ws/pilotdeck-rs/_build/wt-a5x
node --check scripts/a5-19/six_cells.mts
node scripts/a5-19/six_cells.mts
```

两条命令已成功执行，完整运行 exit 0。脚本自己在 `_tmp/20261014-a5-19/run-*/home` 设置 `PILOT_HOME`，发生在导入任何基线模块之前；每次运行新建目录。没有 listen、服务、真实 sidecar 子进程、模型请求、工具执行或二进制下载。只有只读 `git rev-parse` 子进程用于身份记录。Node 的 ExperimentalWarning（TypeScript 转换与依赖图中的 SQLite）原样保留于 `_tmp/20261014-a5-19/run.log`，不是失败。

1. 用 `registerHooks` 将基线内部 `.js` specifier 解析到原 `.ts`；用 Node 内置 `stripTypeScriptTypes(..., {mode: 'transform'})` 在内存加载。纯 strip 不支持基线的 constructor parameter properties，所以用内置 transform；没有复制源码、生成 JS、改源码文本或安装依赖。
2. 实际构造基线 **InProcessGateway → SessionRouter → AgentSession → TurnRunner → 注入 loop**。loop 在 model 等待标记或 tool_calls_detected 标记后停在手动 Promise；unknown 分支抛基线 `AgentLoopResultUnknownError`，known 分支 yield 已知完成事件并 return。真实 TurnRunner 自己决定 timeout unknown 的重抛和 unknown-first 的失败投影；没有伪造 gateway catch 输出。
3. “模型等待 / 工具等待”是注入 loop 的两种边界，工具分支多一个 `tool_call_started` 前缀帧。这里没有运行真实模型或工具，也没有运行 Module transport / SessionOperationLedger；loop 的 `result_unknown/DEADLINE_EXCEEDED` 日志是受控输入，**不是从真实 sidecar 录制的 final**。
4. deadline 用捕获指定 `timeoutMs=123456` 的真实 callback 手动触发；保留其原始 abort、status、publish、close 逻辑。`now` 固定，故不是测量 123456ms 的墙钟等待。其它 timer 不改；15 秒 watchdog 只用于识别装置挂起。没有靠 sleep 猜交错。
5. `recordAgentStatusMessage` 接到注入的磁盘 JSONL status store，路径经真实 `recordGatewayStatusMessage` / router / session / TurnRunner 到 writer。快=无额外 barrier（同步文件写后 Promise 返回）；慢=只阻塞 timeout status，允许 catch status 先提交；失败=在指定 status 写入前抛 EIO。观察最终必须重读 status 文件，不能拿帧推断。普通 transcript 另存并嵌入 raw。
6. 有 registry 时始终用基线 **FileRunRegistry**，保留 accept、文件锁、refresh、mutation chain、atomic snapshot、终态判断与 seq/revision/result 语义；符合生产 `createLocalGateway.ts:996` 默认选择的类。慢 append 在调用实际 append 前阻塞 timeout status/error 两帧，catch append 可先完成。append 故障是在该实例的 `persist` 内存变更之后、真正 snapshot 写盘之前抛一次 EIO；没有用 mock events/result 替代它。此故障点有实际“内存已变、盘未变”的差异，raw 保留 `memoryRecordsAtFailure`。最终直接读 JSON 文件，再新建 FileRunRegistry 读 get/events，两路核对。
7. 客户端通过真实 `submitTurn` async iterator 收帧，收到前缀后暂停消费，维持外层 finally 尚未 complete；因此可把 unknown 放在 queue.close 之后、complete 之前。被动包裹真实 AsyncQueue 的 enqueue/close 和真实 fence 的 isCurrent/complete，只记录并同步委托原实现；不延迟/改写队列方法或 fence 结果。另有 after-complete 子例先消费到 done，再释放 unknown。
8. 所有子例串行，每个 model/tool 阶段重复 5 次。raw 中保留未归一化帧、文件原文、record/events、错误堆栈和全过程序号。重复比较仅忽略 registry record 的墙钟 `acceptedAt/updatedAt`、`ownerPid`，以及 status detail 的随机 `errorId`；帧顺序、完整 payload、status 顺序、events/seq、state/revision/lastSeq/result 全保留。时间线诊断步骤的微任务交错没有声称逐字节一致；这也不是冻结评分器的归一化规则。

## 观察到的结果：六格

以下均为**已经观测到**的值，核心六格采用 timeout-first/current-handle。简称：`T=turn_timeout`、`G=gateway_submit_failed`、`U=result_unknown`、`F=turn_failed`、`I=agent_invalid_state`；`S(x)=agent_status(event=x)`、`E(x)=error(code=x)`、`C(x)=turn_completed(finishReason=x)`。

表中只省略共同运行前缀：model 为 `turn_started → input_accepted → model_request_started`；tool 另加 `tool_call_started`。`∅` 表示没有收到收尾帧，**不是观察器为空**；raw 包含前缀及完整帧内容。

| 六格 | 实际客户端收尾帧 | durable status 实际顺序 | registry 实际结果 | 五次重复 |
|---|---|---|---|---|
| 无 registry / 快 | S(T) → E(T) | T → G | 不适用 | model、tool 各 5/5 一致 |
| 无 registry / 慢 | S(G) → E(G) → S(T) → E(T) | G → T | 不适用 | model、tool 各 5/5 一致 |
| 无 registry / 失败 | timeout status 失败：E(T)；catch status 失败：S(T) → E(T) | 前者仅 G；后者仅 T | 不适用 | 2 子例 × 2 阶段各 5/5 一致 |
| 有 registry / 快 | ∅，只收到运行前缀 | T → G | events 含 S(T), E(T), S(G), E(G)；state=failed，result=E(G) | model、tool 各 5/5 一致 |
| 有 registry / 慢 | status 慢：S(G) → E(G)；仅 append 慢：∅；二者均慢：S(G) → E(G) | 分别 G → T、T → G、G → T | 三个子例 events 均为 S(G), E(G), S(T), E(T)；state=failed，result=E(T) | 3 子例 × 2 阶段各 5/5 一致 |
| 有 registry / 失败 | 随故障点/次序不同：∅、S(G) → E(G)、或 S(G) → E(U) | 仅 G、仅 T、T → G 或 G → T | state 均 failed，result 为 E(G) 或 E(T)；详见逐子例表 | 8 子例 × 2 阶段各 5/5 一致 |

### timeout-first 的每个 writer 子例

本表的 seq/revision/lastSeq 是 model 阶段；tool 因多一个真实投影前缀，所有收尾 seq、revision、lastSeq 均加 1，其余三面顺序与结果相同。每一行的 model 和 tool 都分别 5/5 一致。故障行的 “status 失败” 是 JSONL writer 故障；“append 失败” 是指定 **error 帧**的真实 registry persist 故障。

| 六格 / 子例（与 raw variant.name 相同） | 实际客户端收尾 | 实际 durable status | 磁盘 registry 收尾 events（seq） | state / result | revision / lastSeq | 重复 |
|---|---|---|---|---|---|---|
| 无 registry / 快 / `fast` | S(T) → E(T) | T → G | 不适用 | 不适用 | 不适用 | 两阶段各 5/5 |
| 无 registry / 慢 / `status-slow` | S(G) → E(G) → S(T) → E(T) | G → T | 不适用 | 不适用 | 不适用 | 两阶段各 5/5 |
| 无 registry / 失败 / `timeout-status-fails` | E(T) | G | 不适用 | 不适用 | 不适用 | 两阶段各 5/5 |
| 无 registry / 失败 / `catch-status-fails` | S(T) → E(T) | T | 不适用 | 不适用 | 不适用 | 两阶段各 5/5 |
| 有 registry / 快 / `fast` | ∅（仅收到前缀） | T → G | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 | 两阶段各 5/5 |
| 有 registry / 慢 / `status-slow-append-fast` | S(G) → E(G) | G → T | 4:S(G) → 5:E(G) → 6:S(T) → 7:E(T) | failed / E(T) | 8 / 7 | 两阶段各 5/5 |
| 有 registry / 慢 / `status-fast-append-slow` | ∅（仅收到前缀） | T → G | 4:S(G) → 5:E(G) → 6:S(T) → 7:E(T) | failed / E(T) | 8 / 7 | 两阶段各 5/5 |
| 有 registry / 慢 / `both-slow` | S(G) → E(G) | G → T | 4:S(G) → 5:E(G) → 6:S(T) → 7:E(T) | failed / E(T) | 8 / 7 | 两阶段各 5/5 |
| 有 registry / 失败 / `timeout-status-fails-append-fast` | ∅（仅收到前缀） | G | 4:E(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 | 两阶段各 5/5 |
| 有 registry / 失败 / `catch-status-fails-append-fast` | ∅（仅收到前缀） | T | 4:S(T) → 5:E(T) → 6:S(G) → 7:E(G) | failed / E(G) | 8 / 7 | 两阶段各 5/5 |
| 有 registry / 失败 / `timeout-status-fails-append-slow` | ∅（仅收到前缀） | G | 4:S(G) → 5:E(G) → 6:E(T) | failed / E(T) | 7 / 6 | 两阶段各 5/5 |
| 有 registry / 失败 / `catch-status-fails-append-slow` | ∅（仅收到前缀） | T | 4:S(G) → 5:E(G) → 6:S(T) → 7:E(T) | failed / E(T) | 8 / 7 | 两阶段各 5/5 |
| 有 registry / 失败 / `timeout-append-fails-status-fast` | ∅（仅收到前缀） | T → G | 4:S(T) → 5:S(G) → 6:E(G) | failed / E(G) | 7 / 6 | 两阶段各 5/5 |
| 有 registry / 失败 / `catch-append-fails-status-fast` | ∅（仅收到前缀） | T → G | 4:S(T) → 5:E(T) → 6:S(G) | failed / E(T) | 7 / 6 | 两阶段各 5/5 |
| 有 registry / 失败 / `timeout-append-fails-status-slow` | S(G) → E(G) | G → T | 4:S(G) → 5:E(G) → 6:S(T) | failed / E(G) | 7 / 6 | 两阶段各 5/5 |
| 有 registry / 失败 / `catch-append-fails-status-slow` | S(G) → E(U) | G → T | 4:S(G) → 5:S(T) → 6:E(T) | failed / E(T) | 7 / 6 | 两阶段各 5/5 |

### 次序与 complete 边界补测

这些子例放在对应 fast 格的 raw 内，分别覆盖两种 registry 拓扑、model/tool 阶段，各重复 5 次；不替代慢/失败格。

| 拓扑 / 实际控制次序 | 实际客户端收尾 | 实际 durable status | registry events（model seq） | state / result | 重复 |
|---|---|---|---|---|---|
| 无 registry / `unknown-first` | S(F) → E(I) → C(model_error) | F | 不适用 | 不适用 | 两阶段各 5/5 |
| 无 registry / `known-final-first` | C(completed) | ∅（文件为空） | 不适用 | 不适用 | 两阶段各 5/5 |
| 无 registry / `after-complete` | S(T) → E(T) | T | 不适用 | 不适用 | 两阶段各 5/5 |
| 无 registry / `known-final-before-timeout-pending` | S(T) → E(T) | T | 不适用 | 不适用 | 两阶段各 5/5 |
| 有 registry / `unknown-first` | S(F) → E(I) → C(model_error) | F | 4:S(F) → 5:E(I) → 6:C(model_error) | completed / C(model_error) | 两阶段各 5/5 |
| 有 registry / `known-final-first` | C(completed) | ∅（文件为空） | 4:C(completed) | completed / C(completed) | 两阶段各 5/5 |
| 有 registry / `after-complete` | ∅（仅收到前缀） | T | 4:S(T) → 5:E(T) | failed / E(T) | 两阶段各 5/5 |
| 有 registry / `known-final-before-timeout-pending` | ∅（仅收到前缀） | T | 4:S(T) → 5:E(T) | failed / E(T) | 两阶段各 5/5 |

- **unknown-first**：loop 先交 unknown，尚未触发本地 timeout。真实 TurnRunner 记录 `turn_failed`、投影 `agent_invalid_state` 和 `turn_completed(model_error)`；timer 已被清除，后续手动触发请求被跳过。有 registry 的 `state=completed` 是其对 `turn_completed` 的实际分类，**不能解读为该轮成功**；result 的 finishReason 是 model_error。
- **known-final-first**：loop 的已知结果已 return、AgentSession 已结束，gateway 发布完成帧并清 timer；没有 status 写入，磁盘 status 文件实际为空，raw 显式解释空值。
- **known-final-before-timeout-pending**：loop 已 yield 已知 `turn_completed`，但受控阻塞其 return；TurnRunner 尚未把完成事件向外发布。这时触发 timeout，最终观察仍为 timeout，不能把“loop 先产生完成事件”直接等同于“gateway 已接受完成终态”。此例不冒充真实 Module known final 仲裁。
- **after-complete**：先等外层 fence.complete 与客户端 done，再释放 unknown。实际 `isCurrent=false`，没有新增 G status、G 帧或 G registry event。它与核心快写格的 close-before-complete 有明确不同。

## 独立证据与自检

每个 observation 有 `frames`（客户端实际消费）、`durableStatus.rawText/entries`（独立读文件）、`registry.rawText/record/events`（直接读盘 + 新实例 get/events），以及按发生顺序编号的 `timeline`。每次 append 成功后还直接读盘保存 `diskRecordAfterAppend`；故障点同时保留 `memoryRecordsAtFailure` 和 `diskRecordAtFailure`，可以独立复核中间 result 的覆盖和失败时的内存/磁盘差别。registry 未配置时明确 `applicable=false`；已知完成分支 status 空文件有 `emptyReason`。不会把写入尝试或失败时的内存候选当成 durable 成功。

- 无 registry 快格：[no-registry-fast.json](raw/no-registry-fast.json)，50 次（核心 + 四种次序补测）。
- 无 registry 慢格：[no-registry-slow.json](raw/no-registry-slow.json)，10 次。
- 无 registry 失败格：[no-registry-failure.json](raw/no-registry-failure.json)，20 次。
- 有 registry 快格：[registry-fast.json](raw/registry-fast.json)，50 次。
- 有 registry 慢格：[registry-slow.json](raw/registry-slow.json)，30 次。
- 有 registry 失败格：[registry-failure.json](raw/registry-failure.json)，80 次。

装置检查均通过：48 组 × 5；三面完整性；所有要求的 barrier / 故障确实触发；status 原文与提交日志一致；registry 原文与 fresh reader 一致；帧与真实 open queue 发布一致；导入基线 hash 前后一致。四个独立破坏对照也均被检出：删除实际 timeout error 帧、删掉一个 durable status、漏读一个 registry event、把已读 result 的 G 覆盖隐藏成 T。对照只破坏观察副本，不写产品或真实存储。

可直接复核 `registry-fast.json` 的 `fast/model/1`：timeline order 29 为首次 queue.close，33 的 fence.current=true，34–35 写入并提交 G；39–46 四次 append 成功之后才 enqueue，此时 closed=true；50 才 fence.complete。对应盘上 seq 4–7 是 S(T), E(T), S(G), E(G)，result=E(G)。这些顺序号来自本次 raw，不是源码推测。

## 与静态分析的一致处、不一致处及新增边界

| 静态说法 / 待核问题 | 实测关系 |
|---|---|
| handle 在外层 complete 才失效，close 不等于 complete | 一致：close 后/current=true 仍写 G；after-complete/current=false 不写 G。 |
| 无 registry 直接 enqueue；有 registry append 后才 enqueue，timeout 同步 close 可能丢自身帧 | 一致：核心快格无 registry 收到 S(T),E(T)；有 registry 两帧在盘上存在、客户端缺失。 |
| FileRunRegistry failed 不拒绝后到 append，后到 error 可覆盖 result | 一致：有 registry 快格 result 从 T 被后到 G 覆盖；timeout append 被阻塞的子例则 G 先落盘、T 后覆盖，说明“最终总是 G”同样不成立。 |
| 慢 status 可让 catch 与 timeout 次序倒转 | 一致：status 慢时实际 durable 为 G→T，且 catch 收尾帧在关闭前发布。 |
| “queue 关闭只隐藏帧，所以 registry/result 不变” | 与这种概括不一致；gap §四、D4 已指出其不足。本次实证 events/result 仍变化。并非推翻 gap 当前八步机制。 |
| “unknown-first 也一定走 gateway_submit_failed” | 不符合本次真实 TurnRunner 观察：未 abort 时 unknown 被普通失败投影为 F/I/C(model_error)。gap 原文已将此列为待测，而非固定结论。 |
| status 写失败是否还会发布；registry 写失败是否一定看见 result_unknown | 新增实证：catch status 失败后仍尝试发布 S(G),E(G)，registry 可记录 G 而 durable 无 G（本组客户端对应帧在关闭后丢失）；registry 失败的 U 仅在 queue 关闭前出现的子例可见，关闭后的 U 同样丢失。 |

本次没有发现推翻 gap §四已陈述机制的结果；实际输出补全了它明确留待实测的空白。没有把候选 Rust oracle 当作基线断言，也没有据此回算 #19 或阶段 C 的分数。

## 结论边界与最没把握的三处

1. **真实调度比例与端到端归因未测。** 消费暂停、手动 timer 和 loop 结果 barrier 刻意构造可重复交错；5/5 只证明这个交错稳定，不能证明生产帧缺失概率，不能据此说 #19 两条真实 sidecar 场景的所有差异已经定位。需要后续真实 Module/sidecar 和客户端交叉确认；本轮没有这项证据。
2. **故障模型有明确切面。** status 是注入的文件 store，不是全量生产 transcript writer；registry 使用生产类，但故障只注入在 snapshot 写盘前，不覆盖 write/rename 中途、部分 I/O、进程崩溃、跨进程锁竞争或断电 fsync 保证。重读证明本次可读取持久内容，不证明 crash consistency。known-final-pending 也只测 loop/TurnRunner 边界，不是 Module ledger 对账。
3. **“bug②”与最终评分仍没有替人定标。** 已复现三面分歧及多个收尾组合；上游 operation-deadline.spec.ts 的简单 fakeSession 没有注入相同 unknown 交错，不能只用本结果宣称它的原测试必失败。精确修复范围、帧/落盘/registry 各面的接受差异和故障评分口径，仍按 D4 第二步由人看结果决定。

本实验完成的是 #67 中授权的 **A5.2 六格先行证据**，不宣称 A5.2 的真实 sidecar、cold-restart、双连接夹具或 A5 全部完成，不关闭 #19。
