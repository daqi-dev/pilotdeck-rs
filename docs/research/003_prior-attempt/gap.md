# 两类发现 · 之前那次 PilotDeck Rust 迁移里，哪些可以照搬、教训是什么

> 2026-10-14。依据：同目录 `findings-package.md`（方案）、`findings-execution.md`（执行现场）；`docs/experiments/exp001-baseline-parity-run/`（本仓复现）。

## 一、它们做到的

| # | 命门 | 旧方案 / 执行 | 差距 | 修起来要什么 | 分级 |
|---|---|---|---|---|---|
| 1 | 尺子先被证伪 | **方案里有**（`execution.md:50, 53`），执行时撞上之后才补，**但最终做成了**：G03 v1 于 10-11 采纳，原版自比 52/53、旧 Rust sidecar 51–53/53（`checkpoint.md:481-486`） | 尺子绑在 `b243fa0` 与旧 runner 上，没在 `cc7c4e6` 复核过 | **回捞，不新写**：把 G03 v1 与验证脚本搬进本仓，在 `cc7c4e6` 上重跑全部对照 | **P0** |
| 2 | 有限、冻结的 MUST 清单 | **没有**：`CORE-01` 一个任务包下整个 loop + transport；`CORE.6` 停在 16 MUST / 10 open | 粒度要从「一个子系统」降到「一组场景」 | 上游 53 个场景本身就是天然的 MUST 单位 | **P0** |
| 3 | 每天进 main、资源有上限 | 有资源锁位（重型 1、runtime 1），**但没进集成仓**；磁盘无上限 | 合并节奏与磁盘上限 | 本仓已有 PR 流程与分支保护；补上限数字 | P1 |

## 二、它们没做的

| 找过什么 | 结论 | 是哪一种 |
|---|---|---|
| 正式的「已接受差异」清单 | 只有分散原则（`contracts/reuse.md:3` 等） | 没顾上做 → 这次做 |
| 请求体的归一化规则 | 只写到「tokenizer 与请求 fixtures」 | 没顾上做 → 这次做（exp001 已见 system prompt 里嵌路径） |
| 把 Rust 成果合进集成仓 | `integration_git_write_blocked: true` | **做不到**（当时的权限设计）→ 这次不设这道墙 |

## 可以直接拿来的

| 东西 | 在哪 | 怎么用 |
|---|---|---|
| 比较分层 + 分诊规则 + 比较器证伪要求 | `~/ws/PilotDeck-Rust/docs/rust-migration-package/execution.md:47-53` | 第一条 issue 的验收标准 |
| inventory（93 能力族、911 接口面） | `…/inventory/` | 按 `b243fa0..cc7c4e6` 的 2 个提交增量修正后用（阶段 A / B 用得上） |
| 「证明跑的真是 Rust」的验收 | `…/tasks.yaml:323-325` | 写进阶段 C 的验收：trace 里 sidecar 的身份必须是 Rust 二进制 |
| 旧 Rust agent crate | 执行现场 `build/CORE01-*/…/crates/pilotdeck-agent`（16 份，含 mutant 变体）；封存件见 `control/coordinator.json`（`latest_sealed_sidecar`）；按 G03 v1 已达 51–53/53 | **待人定**：照搬封存件为起点 / 只参考 / 不用 |
