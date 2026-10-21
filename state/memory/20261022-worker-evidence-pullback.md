---
name: worker-evidence-pullback
description: codex/dsh 的 workspace-write 沙箱写不了主仓 _tmp，证据会落在 <wt>/_tmp/<名>/；移回主仓一律用 _tmp/20261021-loop/pull-evidence.sh，不手写 mv/cat 覆盖。
metadata:
  type: feedback
---

**为什么**：2026-10-22 #433 修复轮 1、#434 修复轮 1–2、#421 修复轮 3 连续出现 worker 把 RESULT.md 写进树内 `_tmp`（主仓路径在沙箱外被拒）。03:35 协调者手写循环移回时，用树内只含「修复轮 2」的 RESULT.md 覆盖了主仓含「修复轮 1」的版本；靠 `fix1.err` 原始流与分项日志重建。违反铁律 7「绝不覆盖改过的文件」。

**怎么用**：
1. 任务书交付路径直接写 `<wt>/_tmp/<名>/RESULT.md`（工作区内），不要指主仓。
2. worker 结束后跑 `bash _tmp/20261021-loop/pull-evidence.sh <wt> <名>`：同名同内容删除；RESULT.md 只追加主仓没有的 `## ` 节；其它同名不同内容改名 `<base>.<wt>.<ext>` 保留；永不覆盖。
3. worker 改了 `state/board.md` 等范围外文件：先 `git diff > _tmp/<名>/<wt>-out-of-scope.diff` 存证再还原。
