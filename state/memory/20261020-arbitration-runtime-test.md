---
name: arbitration-runtime-test
description: 评审仲裁的结论取决于运行时 / 标准库 / 底层库行为时，必须写最小实测并贴输出；只读上层源码会误判。不管纯逻辑或纯文档类分歧。
metadata:
  type: feedback
---

**为什么**：2026-10-14，Node `readdir` 实际在 libuv `uv_fs_scandir` 里做 strcmp 排序，只读 TS 上层源码会误判。一家误报的 P1 被协调者仲裁成立，另一家又确认了改错的版本——评审与仲裁可以在同一个错误事实上达成一致。原文：`git show 4fd3ee5:CLAUDE.md` 第 116 行。

**怎么用**：仲裁前先问「这条结论依赖哪一层的实际行为？」；凡落到运行时、标准库、系统调用或第三方库，写一个最小可复现脚本（Node 与 Rust 各跑一次），把命令与原始输出贴进 PR 后再判级。
