---
name: gate-head-rebase
description: 开 PR 前先 rebase 到最新 main 再跑最终 run-review-roles；main 保护要求分支最新，pr-gate 结果块须对应当前 head。不管评审轮次本身。
type: feedback
---

# 最终四角色审计前先 rebase

**为什么**：2026-10-17 #192 两家评审收敛、CI 全绿后，`gh pr merge --rebase` 被拒（main 保护要求分支跟上 base）。rebase 改了 head，而 `check-pr-gate.py` 比对结果块 `head=` 与 PR head，只能在新 head 上把四角色整套重跑一遍（约 5 分钟、4 个 claude -p），白花一轮。

**怎么用**：评审收敛后、跑最终 `run-review-roles.py` 之前，先 `git fetch && git rebase origin/main` 并推送；rebase 后用 `git diff --stat <旧head> HEAD -- src scripts` 证明代码未变，写进 PR 描述，评审结论沿用。开 PR 后若 main 又前进，再 rebase 就必须重跑四角色。
