---
type: research
number: "009"
date: 2026-10-31
title: A19-HOST 逐格期望登记：D4-B 失败面推导决策卡
tags: [phase-a, decision, session, timeout, a19-host]
status: #1177 登记候选；失败面推导为 AI 代批，待人复核；修复本身在 A7 未实现
related: [research/008_phase-a-session-domain/decision, specs/gateway-parity/diffs/A19-HOST-001]
---

# 009 · 决策卡（A19-HOST 期望登记的失败面推导）

**建议：D4-B 只给出成功收尾面的口径；T 侧写失败各格按「如实呈现 + TS 实测的失败形状」推导并全部登记（72/72 无留白），其中 `registry-timeout-error-append-failed` 的 `state=running` / `result=(absent)` 是最弱推导，需人复核确认。** 依据：`state/decisions-20261014.md`「D4 第二步 / D6 拍板」（人批 2026-10-15）、`docs/experiments/exp014-a52-session-fixtures/results.json`（360 观测，含 `droppedEnqueues` 逐项）、`B/src/gateway/client/InProcessGateway.ts:1384-1439,1690-1870`、`B/src/gateway/run/RunRegistry.ts:150-320`。`B/` 为只读 TS 基线根。

以下按 `CLAUDE.md` 三档填写。**「自决·待复核」只表示本登记采用该推导（AI 代批，待人复核），不表示 Rust 已实现、native 轮已按此判分或人已批准该失败面口径。** 人批 D4-B 覆盖的是成功收尾面的措辞（「实时一套 timeout status + error；durable 只记 T；registry result 保持 T；result_unknown 责任记录照旧保留；写失败如实报错」）；失败面如何落到逐格期望，是本卡的判断题。发车后由协调者按 `CLAUDE.md` 登记到当日 state 决策文件，本轮不写 state。

记号：S(T)=`agent_status:turn_timeout`，E(T)=`error:turn_timeout`，U=`error:result_unknown`，S(G)/E(G)=`gateway_submit_failed` 状态/错误帧；n=该 case TS 侧后缀首序号（model=4 / tool=5）。

## D1 · 通用帧序规则（Rust 发布 TS 丢弃的帧）

| 卡项 | 内容 |
|---|---|
| 卡在哪 | TS 在 registry 拓扑下 `enqueueGatewayEvent` 等 append 完成才入队，队列提前关闭，实时流收不到收尾帧（bug ①）；但被丢弃的 enqueue 在 `droppedEnqueues` 里保留了按时间顺序的原始顺序。 |
| 谁受影响 | 28 个 registry 超时格的 rust frames 期望。 |
| 能否撤回 | 登记可撤（重开冻结事件），无数据迁移。 |
| 动不动尺子 / 意向 | 只登记，不动判分语义；帧序证据是实测字段。 |
| 选项与后果 | A 从 TS 源码逐步重推：可行但重复劳动。B **以 `droppedEnqueues` 顺序为准，去掉 G 两帧、去掉 G 侧追加失败引发的 U**：直接以实测责任记录为据；U 的产生点在 TS 里只有 registry append 失败一处，G 侧 append 失败产生的 U 不该出现（G 本身被抑制）。C 只登记稳定子集留失败格空白：违背「每格写明确切期望输出」。 |
| 建议与分级 | **选 B，自决·待复核**。交叉验证：4 个非超时先行 order 变体 dropped 为空且 frames 已发布，规则与其一致。 |
| 原则 → 断言 | 修复不得发明 TS 从未入队的帧 → rust frames ⊆ TS `droppedEnqueues` ∪ TS 已发布帧；负控 `expectations-rust-surface` 删帧必须 FAIL。 |

## D2 · T 状态（durable）写失败：不产生 U，帧照 TS 缺省

| 卡项 | 内容 |
|---|---|
| 卡在哪 | `settleTimeout` 里 `recordAgentStatusMessage` 抛错被吞后 S(T) 帧不入队（TS 实测 `timeout-status-fails`：frames=[E(T)]）。D4-B 未说 durable 写失败要不要补 U 或补帧。 |
| 谁受影响 | `no-registry-timeout-status-failed`（2 case）、`registry-timeout-status-failed`（8 case）。 |
| 能否撤回 | 登记可撤。 |
| 动不动尺子 / 意向 | 「写失败如实报错，不伪称成功」：durable 没写进去就登记为空，不在实时流伪造一帧补偿（U 的语义是 registry 责任记录，不是 durable 写失败的通用补偿）。 |
| 选项与后果 | A 补 U 或补 S(T) 帧：发明 TS 没有的帧，且伪称状态已知。B **照 TS 的失败形状：frames=[E(T)]、durable=[]；registry 侧 E(T) 正常追加（events=[n:E(T)]，state=failed，result=E(T)）**：与 TS 唯一不同的是没有迟到的 G。C 整格留空：违背 72/72。 |
| 建议与分级 | **选 B，自决·待复核**。证据：TS 同格实测 frames=[E(T)]、events 首项 `n:error:turn_timeout`（失败 append 不占 seq）。 |
| 原则 → 断言 | durable 写失败不得产生任何 registry 侧补偿 → 该类 registry events 不含 `agent_status:turn_timeout`；负控 `expectations-rust-surface` 必须杀掉任何帧改动。 |

## D3 · E(T) registry 追加失败：U 保留，registry 停在 running、result 缺席（最弱推导）

| 卡项 | 内容 |
|---|---|
| 卡在哪 | D4-B 说「result_unknown 责任记录照旧保留」「registry result 保持 T」：E(T) append 失败时，U 照发（责任记录），但磁盘上 registry 从未收到终结事件。 |
| 谁受影响 | `registry-timeout-error-append-failed`（8 case：timeout-append-fails-status-fast/slow、timeout-append-fails-catch-status-slow、catch-status-and-timeout-append-fail）。 |
| 能否撤回 | 登记可撤。 |
| 动不动尺子 / 意向 | 「写失败如实报错」：append 失败就不能登记 result=E(T)（那等于伪称写成功）；state 是磁盘事实。 |
| 选项与后果 | A **frames=[S(T),U]，durable=[T]，events=[n:S(T)]，state=running，result=(absent)**：磁盘如实（只有 S(T) 落盘，record 无终结）、责任记录保留（U 在实时流）。B state=failed / result=E(T)：与「未写成功」矛盾。C result=null 改成占位字符串：无 TS 语义依据。 |
| 建议与分级 | **选 A，自决·待复核（本卡最弱一格，需人确认）**。证据：TS `droppedEnqueues` 该格顺序 `[S(T),U,S(G),E(G)]`（U 在 S(T) 后）；TS record 语义里 state 由落盘事件推导（`agent_status`→running，终结事件→completed/aborted/failed），无终结事件落盘即 running。 |
| 原则 → 断言 | 不得把未落盘的终结登记成已落盘 → 该类 result 必须缺席、state=running；负控改 state/result 必须 FAIL。若人否决，改登记只需重开冻结事件，不动装置。 |

## D4 · S(T) registry 追加失败：U 在 E(T) 之前

| 卡项 | 内容 |
|---|---|
| 卡在哪 | S(T) append 失败发 U，E(T) append 成功发 E(T)：两帧顺序由 TS 入队顺序决定。 |
| 谁受影响 | `registry-timeout-status-append-failed`（2 case：timeout-status-event-append-fails）。 |
| 能否撤回 | 登记可撤。 |
| 动不动尺子 / 意向 | 同 D1：以实测入队顺序为准。 |
| 选项与后果 | A frames=[U,E(T)]：与 TS `droppedEnqueues`（`[U,E(T),S(G),E(G)]`）一致，失败 append 不占 seq（TS 实测 events 首项 `n:error:turn_timeout`）。B frames=[E(T),U]：与实测顺序相反。 |
| 建议与分级 | **选 A，自决·待复核**。 |
| 原则 → 断言 | 帧序=入队序 → 交换两帧必须被负控杀掉；registry events=[n:E(T)]、state=failed、result=E(T)。 |

## D5 · G 侧写失败：归入超时收尾类，不新增差异面

| 卡项 | 内容 |
|---|---|
| 卡在哪 | G 侧失败（catch-status-fails、catch-*-append-fails 等）在 TS 里造成 G 帧/G 事件缺失；Rust 侧 G 本来就被抑制，两类失败叠加时表面无差别。 |
| 谁受影响 | `registry-timeout-final`（G 侧失败并入的 12 变体 / 24 case）与 `registry-timeout-status-failed`（timeout-status-and-catch-append-fail）。 |
| 能否撤回 | 登记可撤。 |
| 动不动尺子 / 意向 | 叠加故障不改变已批准的收尾口径。 |
| 选项与后果 | A **按「G 完全不存在」推导（即各归其 T 侧类）**：G 抑制使 G 侧故障不可观测。B 为 G 侧失败单独立类：会产生与 A 完全相同的期望，徒增类别。 |
| 建议与分级 | **选 A，自决·待复核**。注意 `catch-append-fails-status-fast/slow` 的 TS result 本就是 E(T)（E(G) append 失败），与 rust 期望值相同——登记的是值相等，不是无差异面（frames/durable/events 仍差）。 |
| 原则 → 断言 | 类推导只看 T 侧故障配置 → `expectation_class` 对 `failStatus='gateway_submit_failed'` 或 G 侧 `failAppend` 一律归 final/相应 T 侧类；类标错位负控必须杀掉。 |

## 需人定

1. **D3 的 `state=running` / `result=(absent)`（8 case）**：从「写失败如实报错」推出，但 D4-B 原文没有逐字覆盖「registry 停在 running、result 缺席」这一读法。若人倾向「state 也补成 failed」，需重开冻结事件改登记（值会变成 state=failed、result 仍 absent 或 E(T)，二选一也要人定）。
2. **D4 的帧序 [U,E(T)]（2 case）**：纯实测顺序推导，风险低，一并列出供确认。

其余 62 case 的期望直接由人批口径与实测唯一确定，不在「需人定」之列。
