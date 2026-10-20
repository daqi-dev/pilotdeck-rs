---
name: rebase-conflict-merge
description: rebase 冲突时纯状态文件可取 main 侧，代码与测试必须逐块真合并；机械取一侧会静默丢测试。不管 merge queue 自身的 rebase。
metadata:
  type: feedback
---

**为什么**：2026-10-17 #142 r2，rebase 冲突时机械取 main 侧，丢了 8 个 pairing 配对测试，被 Claude 评审判 P1 拦下，r3 才恢复（`state/board.md:204`）。原文：`git show 4fd3ee5:CLAUDE.md` 第 22 行。

**怎么用**：board/decisions/memory 冲突可取 main 侧；`src/`、测试、装置文件逐块核对两侧语义再合并，合并后数一遍测试个数与 rebase 前对得上；做不到就挂起该项转人工合并，不 `--ours/--theirs` 整文件覆盖。
