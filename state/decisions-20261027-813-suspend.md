# 决策卡 · #813 负控判杀共享件挂起（2026-10-27 15:1x）

- **卡在哪**：#813（负控判杀收窄到被点名测试的 panic 块 + 共享件 negative_panic / negative_selection / negative_source）已走 fix-r1 … fix-r8。「marker 绑定到目标断言」这一处：r5 GLM P1-2 → fix-r7；r6 两家同撞 P0（看门狗前缀被判 KILLED）→ fix-r8（看门狗固定前缀判 INVALID、marker 改为目标断言完整专属消息）；r7 宿主：其余 7 套负控全部按预期，唯独 failure-guard 的 `tool-threshold-minus-one` 判 INVALID——变异让 `real_ts_failure_guard_and_classify` 失败在 `accumulate-success-does-not-reset-and-trip-latches action 4` 这一格，不在它声明的专属 marker 上（宿主原始日志 `_tmp/20261026-issue-813/remote-host/neg-fg/`）。
- **为什么挂起**：fix-r8 任务书写明「marker 绑定这一处第 2 次修，再不过挂起写决策卡」；按 CLAUDE.md「同一失败 3 次挂起该项」执行。
- **谁受影响**：#818、#819、#835 等它合入（共享件）；不在 #741 / EdgeClaw 关键路径上。
- **能否撤回**：能。分支 `claude/issue-813-negctl-scope`（249e32ac2）与树保留。
- **动不动尺子**：动负控判杀口径（测试基础设施，非冻结装置）。

## 选项与后果
| 选项 | 做法 | 后果 |
|---|---|---|
| A（建议） | 把 `tool-threshold-minus-one` 的期望 marker 改为它**实际最先触发**的那一格（`accumulate-success-does-not-reset-and-trip-latches action 4`），经评审核实该格确实反映阈值 -1 的变异；不放宽判杀；随后两家增量评审 | 约 30 分钟；其余 7 套已按新判杀全部通过，只差这一条期望 |
| B | 换实现方（Codex）整体复核 failure-guard 全部变异的期望 marker | 更稳但更慢（1–2 小时 + 一轮评审） |
| C | 暂停 #813，先合 #818/#819/#835 各自独立修 | 共享件推迟，三单要各自重复一部分工作 |

**状态**：AI 未代批。READY 改为 `human`，等人在 #813 拍板。

## 人定（2026-10-27T09:42 Z，#813 评论原文）
> 按照建议选A

已执行：见 #813 「已记录」回复与 state/board.md 同时刻段。

## 追记 · r8 兄弟变异（2026-10-27 18:4x，协调者，AI 代批待复核）
fix-r9 后 r8 宿主：`model-threshold-minus-one` 同类 INVALID（声明 action 3、实际 action 2）。按人定 A 的同一原则延伸到兄弟变异 → DeepSeek fix-r10，要求本地跑完 failure-guard 全部变异、逐条列「声明格 / 实际格」后交付。可复用原则：**按原则修一处时，同表里同构的兄弟条目当轮一起实跑核对。**
