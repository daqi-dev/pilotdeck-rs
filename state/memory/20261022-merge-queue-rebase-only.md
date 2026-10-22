---
name: merge-queue-rebase-only
description: 合并队列用 REBASE 方式，PR 分支里不能有合并提交；与 main 冲突时用 rebase 解决，rebase 冲突中 ours/theirs 是反的
metadata:
  type: feedback
---

**规则**：PR 分支和 main 冲突时，用 `git rebase origin/main` 解决，不要 `git merge origin/main`。合并队列的方式是 REBASE（`gh api repos/daqi-dev/pilotdeck-rs/rules/branches/main` 里 `merge_queue.merge_method=REBASE`），分支里有合并提交时，入队后会被 merge-queue bot 静默踢出，没有任何提示。

**Why:** 2026-10-22 #497 用 merge 解冲突后，20:50 与 21:14 两次入队、两次在几分钟内被踢出，检查全绿也没给原因，查了规则才发现是这个。改成 rebase 时又踩了第二个坑：rebase 冲突里 `<<<<<<<` 一侧是 **upstream（main）**，`>>>>>>>` 一侧才是**本分支提交**，和 merge 正好相反；两侧的切分边界也和 merge 时不同，照搬 merge 那次的脚本拼接，截掉了一个函数的结尾，编译报 unclosed delimiter。

**How to apply:** 解冲突先 `git log --merges origin/main..HEAD` 确认分支里没有合并提交。rebase 冲突时，别套 merge 的拼接逻辑：要么从之前验证过的合并结果里直接取该文件（先比对两侧的函数集合，确认没漏），要么逐块手工合并；之后必须跑 fmt、clippy 和全量 lib 再推送，推送用 `--force-with-lease`。关联 [[rebase-conflict-merge]]。
