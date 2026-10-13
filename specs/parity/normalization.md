# 归一化口径草稿（待批）

适用基线 `PilotDeck@cc7c4e6` 的 `tools/agent-loop-parity/trace.py` 与回捞的 G03 v1。本文描述代码现状；不增加为 Rust 放行的规则。实现入口为 `scripts/parity/g03_compare.py`，原版模块通过 importlib 以 `pdrs_original_trace` 注册并加载，未复制、未修改。

## 路径对齐

默认不替换路径。显式 `--path-align OLD=NEW` 可重复指定：解析 JSON 后，按参数顺序对两侧所有字符串值执行文字替换（包括字符串内部的模型 system prompt / JSON arguments），不改键名；再调用原版 `canonicalize`。它是文字替换，不检查路径边界，调用者须给出完整且足够具体的实验目录前缀。原始 trace 不改写。

exp001 的两遍输出目录不同，须把 `…/parity-run4` 对齐到 `…/parity-run1`；本次 ts5 五遍从同一个 `cur` 输出路径复制，prompt 路径一致，定 S 时没有传路径对齐。不得用此参数掩盖非工作目录的业务路径变化。

## ID 与时间字段

原版 `trace.py:172` 递归遍历字典（键排序）与列表（保留顺序），按 `_VOLATILE_KEYS` 去掉时间 / 进程 / 请求等 envelope 字段：`timestamp, startedAt, completedAt, createdAt, updatedAt, durationMs, mtimeMs, messageId, requestId, streamId, runId, operationId, idempotencyKey, connectionGeneration, moduleInstanceId, processId, pid`。`code` 与 `structuredResult` 为 null 时省略；base64 image 的派生 `bytes` 省略，其他内容保留。

加载一份 trace 时共用一个 generated-ID 映射。UUID 形式的 `subagentId/subagent_id, compactionId, itemId, turnId` 按 `(ID 类别, 原值)` 映射成首次出现顺序编号的占位符，同类同值的引用关系保留；两份 trace 的映射独立。timeline 的 `id/blockId/previousId` 在匹配 UUID 加后缀时保留后缀并映射 UUID；特定 subagent session/path 字段与 accepted/settled notice 字符串也用同一映射。

**现状的限制**：原版不是对所有 ID 做一一映射。匹配 `_VOLATILE_ID` 的 `id/callId/toolCallId` 都塌缩成 `<generated-id>`，非空 `handoff_id` 也固定塌缩；匹配之外的值原样保留。因此这些字段的多个不同原值可能相互混淆，不能声称全部引用关系均被保住。当前 G03 原样继承，不在本 issue 偷改。

字符串 `arguments` 仅在能解析 JSON 且 canonicalize 改了内容时重新编码；未发生归一化变化时保留原始文字和空白。包含 duration/session/path 标记的完整 JSON 字符串或末尾 JSON 行可按原版规则重新编码；任意普通 prose 不普遍解析重写。

## 顺序与 seq

原始 JSONL 行序及 `sequence` 保留；健康检查要求从 0 起连续整数，用以拒绝丢行、重复号和截断。它不把两侧的 `sequence` 当语义值逐号比较。原版 `_semantic_record` **不投影 envelope `sequence` / transport source**，保留 `logicalSequence`、`phase`；嵌套字段（例如模型请求体里的 `seq`）不在 volatile 集合中，仍比较。`operation.terminal.lastAppliedSequence` 属于 proof，而 proof 不进入流内语义值比较。这些界限必须和“保留 seq”一起理解，不能声称传输帧已被完整对拍。

G03 按 parent/child × A（端口）/B（落盘完成）/C（gateway 可见）/H（harness 控制）/L（sidecar 生命周期）拆流；actor 判断仅为 `agentScope == child`，其余为 parent。无 toolName 的 tool.result 从同 toolCallId 最近的 tool.finish 继承通道，沿用旧实现。各流由原版 `_diff_semantic_records` 比：以 `(kind, agentScope)` 的 LCS 对齐，再比完整投影值；增删、重复、改值、流内重排都会留下差异。跨流先后只由 E 规则与 `_partial_order_differences` 约束。

原版 `_canonicalize_parallel_tool_lifecycle` 对连续的 tool lifecycle 组有既存例外：超过一个互异 callId，组内全部 `concurrencySafe == true`、全部 sideEffectCount 为 0、所有 identity 都来自组内 tool.call 时，按 call 出现顺序与 call/start/finish/result 阶段排序。其余流内顺序保留。`harness.proof` 与 `operation.terminal` 不进语义流，但用于生产证据、身份、E6–E8 和健康检查。G03 与原版严格 comparator 都去掉 `requestEvidence` 后再比语义；不据此允许预算漂移。

## 模型请求体与落盘

`model.request` 投影字段为 `modelView, messages, systemPrompt, tools, metadata, attempt`；modelView 内所有非 volatile 字段保留，包括模型标识、参数、prompt、消息、工具 schema 及 metadata。modelView 不同作为一个原子差异报告（完整值仍附在比较器 JSON），其他字段继续递归比较。没有再排序消息、删 system prompt、忽略工具说明、模糊数字或引入 budget evidence 豁免。

本次只比原版 trace 的落盘投影（durable.state/status/steer/compaction 等），不声称逐字节比较真实磁盘全部文件。E7 原版明示不可观测的边界仍不可观测：transcript turn_result commit 先于可见 turn_completed 的持久化时钟没有记入 trace。

## 判决与适用范围

两侧均为 sidecar，故 E6 / E7 的 sidecar 操作终态要求两侧都执行。旧 native→sidecar 的 deadline declared extension 不适用于 sidecar→sidecar；相同 sidecar 的 deadline 自比应能 PASS，不接受一侧 native 的已声明字段差异。

每场景须通道比较、所有适用 E 规则、原版 sidecar oracle / production proof 与输入健康检查均通过。E1v 原先标注 informative；依照本任务“全部适用 E 规则进入总判决”的要求，其 FAIL 也纳入。新 round 的 `run.json` 存在时，必须有匹配 side 与 expected sha256 的 trace 身份 proof；旧 TS 原料没有该二进制账本，依赖上游 stdio/handshake/module proof，身份可证范围较窄。

整个目录对拍还须基线 scenarios.json 中 pairs 含 pilotdeck 的 53 个场景齐全，且无 BLOCKED。缺场景、空 trace、非法 JSON、无终态均拒绝。两边同错由各自的场景 oracle 拒绝；基线场景本来预期的失败仍是合法结果。
