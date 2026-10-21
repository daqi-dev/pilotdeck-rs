---
name: test-log-tree-identity
description: 跨 worktree 跑全量测试后，用「本分支新测试在、他分支新测试不在」核日志属于哪棵树；不凭通过数相等或命令里写了 cd 就认。管验证证据归属，不管测试本身对错。
metadata:
  type: feedback
---

**为什么**：2026-10-21 协调者在 wt-a64-pr7（#396）后台跑全量测试，命令写了 `cd …/wt-a64-pr7 && CARGO_TARGET_DIR=$PWD/src/target cargo test --manifest-path src/Cargo.toml`，日志却与 pr10b 那棵树的日志通过名单完全相同（都是 1638/0/42，含 pr10b 新测试 `regexp_stack_model…`、缺 #396 新测试）。差点把 pr10b 的结果当成 #396 的验收证据。原因未定位（推断与后台命令的 cwd 继承或 `$PWD` 展开有关）。

**怎么用**：
1. 后台或跨树测试一律用绝对路径：`W=/abs/wt; CARGO_TARGET_DIR=$W/src/target cargo test --manifest-path $W/src/Cargo.toml`，日志开头先打印 `git -C $W log -1`。
2. 判通过前 grep 两个判别测试：本分支新增的至少一条必须出现，另一条在跑分支的新增测试必须不出现；两份日志通过数恰好相等时先 diff 名单。
3. 写进 PR 的测试数字附上判别结果（own=N foreign=0）。
