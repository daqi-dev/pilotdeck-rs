---
name: gate-head-rebase
description: 开 PR 前先 rebase 到最新 main 再跑最终 run-review-roles；main 保护要求分支最新，pr-gate 结果块须对应当前 head。不管评审轮次本身。
type: feedback
---

# 最终四角色审计前先 rebase

**为什么**：2026-10-17 #192 两家评审收敛、CI 全绿后，`gh pr merge --rebase` 被拒（main 保护要求分支跟上 base）。rebase 改了 head，而 `check-pr-gate.py` 比对结果块 `head=` 与 PR head，只能在新 head 上把四角色整套重跑一遍（约 5 分钟、4 个 claude -p），白花一轮。

**怎么用**（2026-10-25 起，人定方案 A）：结果块带 `patch=` 补丁指纹，纯 rebase 且补丁逐字不变时 pr-gate 自动沿用旧块，不必重跑、也不用手写 diff 证明。前提是该块审的是整个 PR 的内容：叠包的最终评审用 `--base <父包 head>`，范围就是本包全部改动；只审增量的块不会沿用。rebase 后改过代码，或 main 改了改动行附近 3 行内的上下文，指纹会变，必须重跑。见 `state/decisions-20261025-gate-carry.md`。
