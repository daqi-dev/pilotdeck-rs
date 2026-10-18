---
name: 20261018-auto-merge-armed
description: 入队失败也可能已开 auto-merge；PR 后续再推修复前先 --disable-auto，否则队列会合入旧 HEAD
metadata:
  type: feedback
---
2026-10-18：#257 第一次 `gh pr merge` 因检查未齐「失败」，但 auto-merge 已经开着。之后 rebase 推了 dae2557，检查一转绿，队列就把它合了。16:01 推的闸门 P2 修复 e6aa919 没进 main，只好另开 PR 补。

**Why:** merge queue 是异步的。`gh pr merge` 在必需检查没齐时会开 auto-merge，不会报错退出；只要 auto-merge 开着，任何一次检查变绿都会触发入队。

**How to apply:** 往已经尝试过入队的 PR 推新提交之前，先跑 `gh pr merge <N> --disable-auto`。闸门收敛后再重新入队。合入以后，核对 `git log origin/main` 里有没有最后那个修复提交，不能只看 state=MERGED。
