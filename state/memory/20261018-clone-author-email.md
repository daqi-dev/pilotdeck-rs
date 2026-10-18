---
name: clone-author-email
description: 仓外临时 clone 提交前必须设 user.email/name（本机无全局配置，只有主仓 .git/config 有 noreply 邮箱）；否则作者邮箱为空且进 main 后不可改。
metadata:
  type: feedback
---

**为什么**：2026-10-18 守护会话因 worktree 满 8 改在仓外 `mktemp` clone 里提交 #256、#259，6 个提交作者邮箱为空（`郑达奇 <>`），已经过 merge queue 进 main（14c19a1…e333c4b）；main 禁强推，不能改写。本机 `git config --global user.email` 为空，只有主仓 `.git/config` 设了 `225912+zhengdaqi@users.noreply.github.com`；worktree 共享它，**仓外 clone 不共享**。

**怎么用**：优先用 worktree（共享主仓配置）；必须在仓外 clone 时，clone 后立刻
`git -C <clone> config user.email "$(git -C <主仓> config user.email)"; git -C <clone> config user.name "$(git -C <主仓> config user.name)"`，
提交前 `git log -1 --format='%an <%ae>'` 核一眼，邮箱为空不推。
