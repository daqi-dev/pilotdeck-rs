---
id: D1-SDK-SIGNAL-RUNID-LITERAL-001-R1
original: D1-SDK-SIGNAL-RUNID-LITERAL-001
---

## 类别

同原条目 `D1-SDK-SIGNAL-RUNID-LITERAL-001`（已批准差异 · 动态字段）。本更正只同步 #1178 矩阵扩容后的分母事实，不改变差异类别、判分语义与豁免范围。

## 基线依据

同原条目：固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85、Node v22.22.0，`sdk-signals/source.json` 真实 TS CLI 观测。#1178（PR #2353）入库 4 个 abort-rst 格后矩阵为 100 格；新 4 格 stdout 均为空、不含 `recovery.runId`，字面比较口径不涉及新格。原「96 格矩阵 / 96 格分母」表述对应登记当时的矩阵状态，历史记录不变。

## Rust 行为

同原条目：`run--unknown-cleanup` / `resume--unknown-cleanup` 两格仍因每轮新 UUID 与冻结金样不符而 FAIL，失败面均为 stdout 单面。

## 范围

仍只覆盖 `run--unknown-cleanup` / `resume--unknown-cleanup` 两格的 `recovery.runId` 一个字段；其余格数由「94 格」更正为「98 格」。31 格 close-order、8 格 unhandled-rejection stderr、4 格 close reason 的既有登记引用不变。不动比较器、不动金样、不动产品代码。

## 机器判据

正本命令块（`src/pilotdeck-cli/tests/fixtures/sdk-signals/README.md`「Rust 回放与产品负控」节）现为必须恰好执行 **100** 个测试（0 个测试不构成验收）；预期结果为 **98 ok + 2 failed**，失败恰为 `run--unknown-cleanup` / `resume--unknown-cleanup` 且 `differing surfaces` 均仅 `["stdout"]`。原条目的字段级复算口径（36 字节 runId 替换后与金样逐字节比对）不变。身份与失败面判据（不只数红格）不变。

原条目对 `tests/common/signal_replay.rs` 的行号指针因本 PR 同文件改动（净 +25 行）整体后移，冻结原条目逐字节不可改，此处补正：`compare_surfaces` :1045 → :1070；`comparison.json` 归一化说明「UUID bytes stay literal」 :1199 → :1224；`acceptedDifferences` 清单 :1200-1202 → :1225-1227；剥精确 libtest driver 前缀 :1009-1018 → :1034-1043。机器判据不解析行号，此补正仅供人工按本条复核 runId 字面比较口径时定位。

## 批准记录

未单独报批：本更正为分母事实性修订（矩阵 96 → 100 的直接后果，防止按旧「其余 94 格 ok」字面判据产生审计假 FAIL），不改变原条目已批豁免的范围与判分语义；新分母由 README 命令块正本与本 PR 机器断言钉住并经双谱系评审（minimax + deepseek）。AI 代批，待人复核。
