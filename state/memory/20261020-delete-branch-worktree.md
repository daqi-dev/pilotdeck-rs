---
name: delete-branch-worktree
description: gh pr merge --delete-branch 删本地分支时会连带删掉签出它的 worktree 及其中未入库的 _tmp/ 证据；合并一律不加该参数。不管远端分支清理授权（见 CLAUDE.md 合并闸门）。
metadata:
  type: feedback
---

**为什么**：2026-10-14 夜，`gh pr merge --delete-branch` 删除本地分支时连带删掉了签出它的 worktree，未入库的 `_tmp/`（实验证据、交接报告）一起丢失。原文：`git show 4fd3ee5:CLAUDE.md` 第 75–77 行。

**怎么用**：合并只用 `gh pr merge <N>`（进 merge queue），不加 `--delete-branch`，也不靠仓库「自动删分支」设置。清理 worktree 前先核持有人、未提交/未跟踪内容与保留要求，保存并校验证据；要被文档链接的实验证据放主仓 `_tmp/<日期>-<名>/`，不放在会被清理的 worktree 里。
