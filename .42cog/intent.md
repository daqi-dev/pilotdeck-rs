# PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust · 意向书

> 意向即行动。这一份文件决定这个系统会长成什么样。
> **只有一处必须你亲自过目：收敛方向。其余是草稿，看不顺眼再动。**
> **它现在一定是粗的。跑完 `-research` 会回来改——四步是循环，不是直线。**

## 收敛方向（唯一）

一个用 Rust 写的 PilotDeck **agent 循环 sidecar**（阶段 C）：TS 那一侧一行不改，只靠环境变量 `PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND` 指向它，在上游自带的对拍装置 `tools/agent-loop-parity/` 里，与 **TS sidecar** 的语义事件归一化后一致；基线 `OpenBMB/PilotDeck@cc7c4e6`（standalone_alpha1，2026-10-13）。阶段 A 再替换 18789 上的 gateway，阶段 B 再替换 `ui/server`，届时回来改这一句。

怎么算数：**对拍**。关联度 ＝ Rust sidecar 与 TS sidecar 一致的场景数 / （53 − 原版自己就不稳定的场景数）；「一致」与「原版不稳定」的判法以冻结的 `specs/parity/README.md` 为准（k = 5 遍全一致；原版不稳定的场景逐个由人批准排除，且排除后仍须 ≥ 45 个）。基线自身 native 与 TS sidecar 之间的 31 处稳定差异先登记进「已接受差异」清单，不算到 Rust 头上。每个 PR 要么让这个数变大，要么说明它为什么不变。对照物钉死在 `cc7c4e6`，不是「最新的上游」。

> 依据：`docs/research/002_parity-boundary/decision.md`（拍板：先 C 再 A 再 B · 驱动用 TS · 默认修 bug · 归一化后相同）· `docs/experiments/exp001-baseline-parity-run/` · `docs/experiments/exp002-connection-state/`。

## 真难题

一个 18 万行 vibe coding 的 agent 平台，**它的真实行为没有成文规格**——行为散落在 31 个模块的实现、9.8 万行测试和前端的隐含假设里。重写最难是**先把「什么叫一致」变成机器能判的东西**。尤其是在哪个颗粒度上保持一致，架构不合理怎么办，原始代码有错怎么办，测试或者探针本身有问题怎么办？

## 不做什么

| 不做 | 为什么不做 |
|---|---|
| 迁移期间不加新功能、不"顺手"改行为——**修 bug 除外**（2026-10-14 人定「默认修」） | 重写失控的第一原因。想到改进就记一条 issue，不当场做（demo-2/04 §六）。**什么算 bug（2026-10-14 人定）**：基线 ① 崩溃、挂起、丢数据 ② 与它自己的类型定义、文档或测试断言相矛盾 ③ 有安全问题。三条都不沾的是「行为偏好」，记 issue、不改。修了一律记进「已接受差异」清单（第 9 步建立），写明属于哪一条 |
| 阶段 C、A 都不改 `ui/`（含 `ui/server`）和 `apps/desktop`；阶段 C 连 TS 的 `src/` 也不改 | 阶段 C 只换 agent 循环 sidecar 这一个进程；阶段 A 才换 gateway，阶段 B 才动 `ui/server` |
| 不追上游最新提交 | 参照物在动，对拍就没有意义。升级基线 = 单独一条 issue，整批升 |
| 不照搬 TS 的模块内部结构（Bundle / 注入层层套）；**也不在移植一个模块的同时重构它** | 一致的是**协议和行为**，不是类图：模块边界与数据流照原样映射，内部按 Rust 习惯写。要重构放在移植前或移植后（`docs/research/001` 第二节第 1 条） |
| 不自己发明协议、字段、错误格式 | 以基线 TS 实测输出为准；有疑问跑一次基线看，不猜 |
| 不对外提供网络服务、不发版 | 个人学习项目。仓库公开（2026-10-13 人定），源码按 AGPL-3.0 开放，满足上游许可证；对外跑服务或发二进制会带来新的义务和维护承诺，先不做 |

## 真相源与权重

冲突时信谁，从高到低：

1. **基线 cc7c4e6 的 TS 后端实际跑出来的行为**（实测输出）
2. 基线的测试（`tests/`）——测试与实测冲突时，以实测为准并记录
3. 基线的源码（`resources/PilotDeck/src/`，结论必须带文件行号）
4. 基线的文档（`docs/`、README）——可能过时，只作线索

## 作品区

- 目录：`src/`（Cargo workspace；`pilotdeck-cli` 是入口，`pilotdeck-core` 是能力，后续按模块再拆 crate）
- 系统实际产出什么：阶段 C 是一个 agent 循环 sidecar 二进制，TS gateway 经环境变量拉起它；阶段 A 起是 `pilotdeck` 二进制，`pilotdeck server` 起在 18789 后，原版 `ui/server` 连过去就能用
- **怎么算合格（验证闭环）**：CI 三条（fmt / clippy / test）全绿 ＋ 对拍一致的场景数不降 ＋ 人工用原版 UI 实测一遍（TS gateway 配上 Rust sidecar）

---

*`aias-meta-init` 起草于 2026-10-13。收敛方向：2026-10-13 人过目第一版；2026-10-14 按调研 002 的拍板改为阶段 A 版本，人已过目（含 bug 定义）；同日按 exp001 改为阶段 C 版本，人已过目。*
