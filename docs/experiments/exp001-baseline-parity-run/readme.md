# exp001 · 基线能不能在隔离环境里无头跑、接 mock 模型、比出差异

> 2026-10-14。基线 `resources/PilotDeck` @ `cc7c4e6`；Node v22.22.0、pnpm 10.32.1、Python 3.14.3（上游记录用 3.12）。
> 原始输出在 `_tmp/20261014-exp001/`（不入库）。

## 假设

调研 002 打算自己搭对拍装置。真正的前提是：基线能在隔离的 `PILOT_HOME` 下无头跑起来、模型换成确定性 mock、同一输入跑两遍能比出噪声。

## 做法

第 7 步一开始就发现，**上游自带了一套对拍装置** `tools/agent-loop-parity/`（调研 002 漏读）。它对比 agent 循环的两条链路：native（进程内）与 sidecar（独立进程，经 Module Protocol / stdio）。于是 exp001 改成**原样跑上游装置**：

1. `pnpm install --frozen-lockfile && pnpm build`（全程 `PILOT_HOME`、`TMPDIR` 指向实验目录）
2. 装置自检：`python3 -m unittest test_trace.py`（含阴性对照）
3. 全量对拍：`run.py --comparison same-version --surface gateway --scenario all`，跑**两遍**（run1、run4）
4. 挑两个场景各重跑两遍，看同一侧自己跑两次是否稳定
5. 用装置自己的投影函数（`trace.py` 的 `project_semantic_trace`）复核稳定差异的成因

## 结论

| 问题 | 结果 |
|---|---|
| 能不能无头跑 | **能**。构建通过；53 个场景全部执行，**BLOCKED = 0**，oracle failure = 0；一遍约 4 分钟 |
| 装置自己靠不靠得住 | 自检 **54/54 OK**（上游文档记 42，测试已增加），含阴性对照 |
| 隔离 | 运行期间真实 `~/.pilotdeck` 里只有 3 个 `memory/.../MEMORY.md` 变了；持有该目录的是用户本机自己在跑的 `ui/server`（10-12 启动），我们的进程退出后它仍在改写。**强证据：不是我们写的** |
| 同一侧跑两次稳不稳 | `pure_text` 三次全 PASS；`single_tool` 每侧两次的事件顺序**逐条相同** |
| native vs sidecar（都是 TS） | **31 个场景稳定不一致**，另有 **7 个时有时无**（run1 与 run4 各自多出几个，像是负载相关） |
| 稳定差异的成因 | **26/31 只差两类事件的位置**：`agent.status`（`turn_continued`）与 `context.budget`；去掉这两类后语义事件**完全一致**。其余 **5 个**是更深的顺序差：`plan_mode_*` 与 `one_shot_subagent_*` 里 `tool.finish` 与下一个 `model.request` 先后不同；`sidecar_budget_limit` 的 `durable.status` 不同 |
| 能不能把 sidecar 换成别的二进制 | **能，零改动**：`PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND` / `_PATH`（`src/cli/AgentLoopDeploymentProfile.ts:62, 72`） |

> ⚠️ 上游 2026-09-18 的结果文档记录的是 53/53 通过，但那是另一个分支（`codex/integrate-sdk-0901`）。**在我们钉住的 `cc7c4e6` 上，两条 TS 链路之间就已经有 31 处稳定差异。**

## 改变了哪个取舍

1. **出现了第三条边界 C：agent 循环 sidecar。** 协议小（Module Protocol v2 schema 330 行、14 个 `$defs`），宿主无关（模型、工具、权限、上下文都经 host module 调用），**对拍装置现成、带阴性对照、零改动就能接 Rust 二进制**。它切的正是 exp002 说边界 A 切不动的那一块（轮次与 agent 循环）。⇒ 取舍卡第 1 项「先 A 后 B」**值得重新拍板**：可能是「先 C，再 A，再 B」。
2. **对拍基准要选清楚**：Rust sidecar 应该对 **TS sidecar**（同一条传输链路），不对 native；否则那 31 处 native/sidecar 差异会全算到 Rust 头上。
3. **「已接受差异」清单第一天就不是空的**：基线两条 TS 链路之间的 26 + 5 处差异，要先登记成已知，Rust 才有干净的起点。
4. **命门 3（跑两次相同）只是部分成立**：每侧大体稳定，但有 7 个场景时有时无。要么每个场景跑多遍取「稳定结论」，要么先找出这 7 个的噪声来源。

## 结论的边界

- 只跑了 **agent 循环这一层**的对拍（gateway 表面，mock 模型与工具）；没有覆盖 HTTP 路由、`ui/server`、落盘格式的完整对比。
- 「7 个时有时无」只有两次全量样本，**不够下定论**；稳定差异的成因分析只覆盖 31 个里的 26 个，另外 5 个只定位到首个分叉点。
- Python 3.14 与上游的 3.12 不同；装置自检通过，但没有逐条排除版本影响。
- exp003（真 `ui/server` 连 18789 代理）**还没跑**；注意用户本机的 PilotDeck 正占着 18789。

## 补充（同日）· 否定对照：原版自己跟自己比

调研 003 发现旧迁移的第一条教训是「先验证验收标准原版自己能过」。本方案让 Rust sidecar 对 **TS sidecar**，所以该做的否定对照是 **TS sidecar 自比**（两次运行）。用 run1 与 run4 的 trace，按装置自己的 `compare_trace_details` 比：

| 自比（run1 vs run4） | 一致 |
|---|---|
| TS sidecar 对 TS sidecar | **35 / 53**（18 个不一致） |
| native 对 native | **46 / 53**（7 个不一致） |

⚠️ **方法先错了一次**：第一次比出 52/53 不一致，连 `pure_text` 都不过——查下去是 system prompt 里嵌了工作目录路径（`…/parity-run1/.runtime/…` 与 `…/parity-run4/…`），两次运行输出目录不同所致；同一次运行里 native 与 sidecar 共用路径，所以对拍时看不出来。把路径对齐后才是上表。**这就是「比较器先被证伪」的必要性——这次被证伪的是主会话自己写的比较。**

**结论**：上游比较器的严格模式（事件逐位置比）**连原版自比都过不了**，不能直接拿来判 Rust。
⇒ **第一件事是做尺子**：比较规则必须先满足 ① TS sidecar 自比（多次运行）一致 ② 注入的已知差异判为不一致，然后才开始写 Rust。旧方案的分层规则（`~/ws/PilotDeck-Rust/docs/rust-migration-package/execution.md:47-53`：端口通道值严格比、公共事件流按各自局部顺序比）是现成的起点。

边界：只有两次全量样本；run1 与 run4 的机器负载不同（run1 期间还有其他构建在跑）。
