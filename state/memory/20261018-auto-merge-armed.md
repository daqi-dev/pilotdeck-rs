---
name: 20261018-auto-merge-armed
description: 入队失败也可能已开 auto-merge；PR 后续再推修复前先 --disable-auto，否则队列会合入旧 HEAD
metadata:
  type: feedback
---
2026-10-18：#257 第一次 `gh pr merge` 因检查未齐「失败」，但 auto-merge 已经开着。之后 rebase 推了 dae2557，检查一转绿，队列就把它合了。16:01 推的闸门 P2 修复 e6aa919 没进 main，只好另开 PR 补。

**Why:** merge queue 是异步的。`gh pr merge` 在必需检查没齐时会开 auto-merge，不会报错退出；只要 auto-merge 开着，任何一次检查变绿都会触发入队。

**How to apply:** 往已经尝试过入队的 PR 推新提交之前，先跑 `gh pr merge <N> --disable-auto`。闸门收敛后再重新入队。合入以后，核对 `git log origin/main` 里有没有最后那个修复提交，不能只看 state=MERGED。

**第二次（2026-10-20，#351）**：上一个会话在 10-19 15:20 给 #351 开了 auto-merge，交接时没写进 board。本会话 10:44 rebase 后推送，CI 转绿，队列 10:57 就合入了 b478338（gate5 P1 还没修）。之后 5 个修复提交（5614d0d…63526b5）都推到了已合并的分支上，13:58 才发现。
**加固（防第三次）**：
- 每次往分支推送前，都先跑 `gh pr list --state all --head <分支> --json number,state,autoMergeRequest`。PR 已是 MERGED 就别再推，改开新 PR；autoMergeRequest 不为空就先 `--disable-auto`。
- 交接时在 board 列出所有开着 auto-merge 的 PR。
- 合入后用 `git merge-base --is-ancestor <最后修复提交> origin/main` 确认修复确实在 main 上。
