---
type: arch
number: "001"
date: 2026-10-14
title: 阶段 C：agent 循环 Rust sidecar——边界、对拍尺子与旧代码复用
tags: [phase-c, sidecar, parity, module-protocol, reuse]
related: [research/002_parity-boundary, research/003_prior-attempt, plan/001_phase-c-agent-loop-sidecar]
---

# 阶段 C：agent 循环 Rust sidecar

## 背景动机

收敛方向（`.42cog/intent.md`）要求 Rust 后端与基线 `OpenBMB/PilotDeck@cc7c4e6` 行为一致、前端不改一行。调研 002 比了两条边界（A：18789 gateway；B：3001 浏览器 API），第 7 步探针又找到第三条：

- **C：agent 循环 sidecar**。基线自带「agent 循环作为独立进程、经 Module Protocol v2 / stdio 与宿主通信」的正式链路（`resources/PilotDeck/src/cli/AgentLoopDeploymentProfile.ts`），以及配套的对拍装置 `tools/agent-loop-parity/`（53 个场景）。
- exp002 证明 A 边界上真正的核心（轮次、权限、交互）在迁走之前**切不动**；C 切的正是这块核心。
- 调研 003 发现用户之前那次迁移**已经走到这一步**：做成了 G03 v1 比较器，Rust sidecar 在旧基线 `b243fa0` 上量得 51–53/53，暂停在 `CORE.6` 的 10 个 open 项。`b243fa0` → `cc7c4e6` 只差 2 个提交，不碰 agent 循环。

人已拍板：**先 C，再 A，再 B**；第一条 issue 回捞并复核尺子；旧 sidecar 源码找得到就照搬（已找到）。

## 技术选型

### 1. 切在哪

| 候选 | 协议 | 对拍装置 | 切下来的是 | 选否 |
|---|---|---|---|---|
| **C sidecar** | Module Protocol v2（schema 330 行、14 个 `$defs`） | **现成**，53 场景，带自检与阴性对照 | agent 循环（核心） | **选** |
| A gateway | 帧协议，约 100 个 method，载荷类型 1666 行 | 要自己搭 | 只有约 35 个只读 method 能先切 | 阶段二 |
| B ui/server | 约 280 条路由 + 25 种无类型 WS 消息 | 要自己搭，且多依赖真模型 | — | 阶段三 |

### 2. 怎么接进去：只靠环境变量，TS 零改动（Rust 入口要适配一处）

`PILOTDECK_AGENT_LOOP_TRANSPORT=stdio`，`PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND=<Rust 二进制>`，`PILOTDECK_AGENT_LOOP_SIDECAR_PATH=<占位参数>`（`AgentLoopDeploymentProfile.ts:62, 72`）。宿主仍是 TS gateway，模型、工具、权限、上下文都由宿主经 module call 提供；Rust sidecar 只含循环逻辑。

⚠️ 评审更正：profile 总是给子进程传一个路径参数（`AgentLoopDeploymentProfile.ts:73`），旧 Rust 入口收到任何参数都退出（旧 `pilotdeck-agent-loop-sidecar.rs:22`）；旧执行靠 `formal-bind.mjs` 拦截 spawn 绕过。所以「零改动」只对 TS 成立，**Rust 入口要接受并忽略这个参数**（或沿用拦截，待人定）。

### 3. 拿什么比：Rust sidecar 对 **TS sidecar**，不对 native（要新做「sidecar 对 sidecar」配对）

exp001 实测：基线自身 native 与 TS sidecar 之间就有 31 处稳定差异（与旧执行 `b243fa0` 上的记录一致）。对 native 比会把这些全算到 Rust 头上。对 TS sidecar 比，两边走同一条传输链路，差异才归因得清。⚠️ 上游 `run.py` 与旧驱动都只会比「native 对 X」，所以要各跑一遍、各取 sidecar 一侧的 trace 再比（见计划 S1）。

### 4. 尺子：回捞 G03 v1，不新写，也不直接用上游严格模式

| 候选 | 原版自比 | 判 Rust 的历史 | 选否 |
|---|---|---|---|
| 上游 `run.py` 严格模式（逐语义事件比） | TS sidecar 自比 **35/53**（exp001） | — | 不能直接判 Rust |
| **G03 v1 通道拆分**（旧执行 `build/CORE01-g03-rule-decision-independent-a10/verify_a9.py`，481 行） | 52/53（旧基线） | 51–53/53 | **选**，但要在 `cc7c4e6` 上复核 |
| 从零写 | — | — | 不选：旧执行做成它花了约一天半 |

G03 v1 的规则：按（参与方 × 生产者通道）拆流，每条流用原版自己的 `_diff_semantic_records` 比；跨流先后只经 E1–E12 规则与原版 `_partial_order_differences`。

### 5. 代码起点：照搬旧 sidecar 源码

旧执行现场 `build/source-repository` @ `0eba113`（基于 `b243fa0` 的 224 个提交）。照搬范围只限阶段 C 需要的：

| crate | 产品代码 / 测试（约） | 照搬 |
|---|---|---|
| `pilotdeck-agent` | 1.65 万 / 1.80 万行，484 个测试 | ✔ |
| `pilotdeck-transport` | 0.85 万 / 0.68 万行，158 个测试 | ✔ |
| `pilotdeck-protocol` | 0.21 万 / 0.10 万行，26 个测试 | ✔（agent 依赖它） |
| `pilotdeck-tokenizer` + `vendor/tiktoken-rs`（8.4 MB） | 约 240 行 | ✔ |
| `plugin/workspace/terminal-protocol`、`ui-probe`、`desktop-probe` | — | ✘ 不属于阶段 C |

**体量风险**：产品代码约 2.7 万行，是 TS 侧 sidecar 职责（`agent/loop` + `agent/modules` 约 1.4 万，加子 agent 约 1.8 万）的 1.5–2 倍。怀疑有一部分是旧执行追对拍时加的防御代码。⇒ 计划里有一步**对应检查**：每个 Rust 模块对应到 TS 源头，对应不上的列为「可能膨胀」，**尺子复核之后**再交给后续 issue 精简。

## 架构设计

```
TS gateway（cc7c4e6，不改）
  └─ PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND ──stdio / Module Protocol v2──▶ Rust sidecar（src/，本仓）
                                                                         ├─ pilotdeck-transport（stdio 帧、握手、binding）
                                                                         ├─ pilotdeck-agent（循环、轮次、子 agent、预算）
                                                                         ├─ pilotdeck-protocol（共享类型）
                                                                         └─ pilotdeck-tokenizer（预算估算）
对拍：resources/PilotDeck/tools/agent-loop-parity/（上游，跑 TS 宿主与 mock）
      + scripts/parity/（本仓：回捞的 G03 v1 比较器与绑定驱动）
      + specs/parity/（本仓：冻结的验收口径、归一化规则、已接受差异清单）
```

## 相关文件

- `.42cog/intent.md` · `docs/research/002_parity-boundary/decision.md` · `docs/research/003_prior-attempt/decision.md`
- `docs/experiments/exp001-baseline-parity-run/` · `docs/experiments/exp002-connection-state/`
- 旧执行：`~/ws/PilotDeck-rust-program-20261009/build/source-repository`（@ `0eba113`）、`build/CORE01-g03-rule-decision-independent-a10/verify_a9.py`、`build/CORE01-astra-deadline-closure-a1/harness/`

## 相关变更

无（第一份架构决策）。
