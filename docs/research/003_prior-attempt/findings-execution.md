# 旧迁移的执行现场（`~/ws/PilotDeck-rust-program-20261009/`）—— 核查结果

> 2026-10-14。自有积累。由子 agent 只读产出（未进 `build/`、未跑任何脚本），主会话抽查 3 条（✔）。
> 本仓公开：**人当时的原话与花费明细不录入**，只写结论。行号相对该目录。

## 一、时间线与进度

- 2026-10-09 开始；**10-12 05:09Z 按上级指令显式暂停** ✔（`control/checkpoint.md:625-627`；`coordinator.json` 中 `execution_paused: true` ✔）。
- 43 个任务：**done 6 · in_progress 2 · todo 35** ✔（`state.json`）。完成的是 BOOT-01~04、ARCH-01/02；进行中的是 `CORE-01`（AgentLoop sidecar）与 `UI-00`。
- 集成仓 `~/ws/PilotDeck-Rust` 至今**只有初始化提交**（`integration_git_write_blocked: true`）；没有可安装运行的产品，只有封存的 sidecar 与 UI 探针。

## 二、卡点

1. **验收标准原版自己就过不了** ✔：`checkpoint.md:472` 记录对照组「原版 native vs 原版 TS sidecar，Rust 根本没起」严格比较 **31/53 失败，全是位置挪动**（`agent.status/turn_continued`、`model.request`、`model.response`、`context.budget` …）。之后比较器从 v2.0 改到 v2.4.1，大量轮次花在调契约上。
   → **与本仓 exp001 独立复现的结果一致**（2026-10-14，基线 `cc7c4e6`：31 个稳定差异，同样是位置挪动）。
   ⚠ **更正（评审 B 提出，主会话核实 `checkpoint.md:478-486`）**：尺子**后来做成了**。10-11 15:42Z「G03 v1 通道拆分比较器」被正式采纳——按（参与方 × 通道）拆流，每条流用原版自己的 `_diff_semantic_records` 比，跨流先后只经 E1–E12 规则与原版 `_partial_order_differences`。按 v1，**封存的 Rust sidecar `8a0eced4` 四轮确认为 53/53、52/53、53/53、51/53**；非 PASS 的全是 `deadline` 类，独立审查判为宿主侧竞态（一条多出来的 host 写的 `durable.status`），与 Rust 无关。「v2.4.1 仍只算诊断」说的是另一个 observer 工具，不是尺子的结局。
2. **父任务收不了口**（尺子做成之后的真正卡点）：`CORE-01` 下的 `CORE.6` 停在「16 MUST / 6 preserved / 10 open」（`checkpoint.md:627`），剩余范围证明不了有限；`CORE-01` 3 天里留下 168 个尝试目录。
3. **会话生命周期反复出故障**：额度上限、未写交接就退出、运行 5400 秒被强杀（exit 143）、MCP 配置错误导致启动即失败等，至少 4 次（`checkpoint.md:386, 443, 495, 490`）。
4. **状态记录退化**：`state.json` 的 notes 变成去空格的压缩黑话，人已基本读不懂。
5. **磁盘 127 GB**（推断，未测量）：约 266 个按尝试划分的 build 目录、245 个 worker 工作树（各是完整的 PilotDeck 副本）、337 个 evidence 目录。

## 三、组织方式

并发上限 4 个 agent（重型构建 1、真实 GUI / runtime 1）；只允许一个协调者写队列；claim 296 个（163 个缺 status 字段）、lease 127 个；任务标 done 必须有**独立审查者的回执 + 附件哈希逐项核对**，作者自测不算。

## 四、人的评价

没有正式的验收结论文件。（过程中人的反馈记在 `user-updates.json`，公开仓不录入。）

## 子 agent 的判断

**三条最重要的教训**
1. **先验证验收标准原版自己能过，再开工。**（旧执行是撞上之后才补，花了约一天半做成 G03 v1）
2. **每个任务先有一份有限、冻结的 MUST 清单**，否则父任务永远关不掉。
3. **成果尽早合入集成仓，并给磁盘与会话设上限。**

**可回捞的成果**：G03 v1 比较器与它的验证脚本（`build/CORE01-*/harness/formal-round.py`、`formal-bind.mjs`、`build/PROGRAM-claude-continuation-20261011-a10/record-g03-decision.py`）；封存的 Rust sidecar（`control/coordinator.json` 的 `sealed_candidate_a7`、`latest_sealed_sidecar`，带源码 commit 与 sha256，版本串 `implementation=rust wire=2.0`）。

**最值得照搬的机制**：lease / claim + 独立回执——资源锁位固定，done 以独立审查者回执与哈希核对为准，挡住了「虚报完成」。
