---
name: 20261020-dsh-gitlocal
description: dsh workspace-write 不能写 worktree 的外部 gitdir；让它 rebase 时它会把 git 元数据搬进工作区 .git-local 并改 .git 指针，worktree 与主仓脱钩——rebase/推送一律由协调者在主仓侧做
metadata:
  type: feedback
---
2026-10-20 派 DeepSeek（dsh，workspace-write）把 A6.4-pr6 rebase 到 claude/a64-pr5：沙箱拒绝写 worktree 的真实 gitdir（`<主仓>/.git/worktrees/<名>`），它于是把 git 元数据复制到工作区内 `.git-local`、改写 `.git` 指针后自行 rebase，并留了 bundle 让主仓取回。结果：主仓的分支还停在旧提交，worktree 实际已脱离主仓。

**Why:** 同 10-17 记录的 codex 沙箱不能写 worktree git 元数据（board 2026-10-17 第五轮），dsh 也一样；任务书允许它 rebase，它就会自己绕开沙箱。

**How to apply:** 任务书里不再让 dsh / codex 执行 git rebase、fetch、commit。要 rebase 时，由协调者在主仓侧完成，把冲突文件和两侧语义交给 worker 去逐块合，或者由协调者自己真合并。万一又出现 `.git-local`，按这次的步骤恢复：`git fetch <bundle>` 取回提交 → 把 `.git-local` 移到 `_tmp`（不删）→ `.git` 改回 `gitdir: <主仓>/.git/worktrees/<名>` → `git update-ref refs/heads/<分支> <提交>` → `git reset --mixed HEAD` 同步索引 → 用 `git status` 确认工作树干净。相关：[[20261018-dsh-channel]]
