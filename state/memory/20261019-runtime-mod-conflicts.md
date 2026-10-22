---
name: runtime-mod-conflicts
description: 新增 runtime/ 子模块的 PR 都在 runtime/mod.rs 末尾相邻处加一行，彼此必冲突；闸门 head= 绑死 PR head，rebase 后要重跑闸门——这类 PR 串行合入，闸门前先 rebase 到最新 main。
metadata:
  type: feedback
---

10-19 同类冲突第三次（#307、#319、#320 都因 runtime/mod.rs 的相邻 `pub mod` 行冲突被队列踢出或无法入队）。

**为什么**：合并队列不会替冲突 PR 解决冲突，PR 留在 OPEN/DIRTY，enqueue 脚本却照样报 queued；rebase 改了 PR head，pr-gate 结果块的 head= 失效，必须重跑四角色闸门（约 30 分钟/个）。

**怎么用**：
- 派单时就核 diff 是否改 `runtime/mod.rs`（或 host/modules/mod.rs、storage/mod.rs 这类登记文件）；改的话，这批 PR 按合入顺序串行：上一个 MERGED 后，下一个才 rebase → 跑测试 → 跑闸门 → 入队。
- 闸门开跑前先 `git merge-tree --write-tree origin/main HEAD` 核一次无冲突，再跑。
- 入队后用 `gh pr view <N> --json mergeable` 复核，不只看 enqueue 日志；DIRTY 就立即 rebase。
- 冲突处理：两侧都只是追加 `pub mod`，按字母序全部保留（代码文件必须真合并，不取一侧）。

**同类：gitleaks 放行跟着分支走**：pre-push 钩子用的是分支工作区里的 `.gitleaks.toml`。在放行 PR（如 #316）合入前切出的分支，推送含相同公开哈希的夹具会被拦；先 rebase 到含放行的 main 再推（head 变了，闸门要重跑）。所以闸门开跑前就 rebase 到最新 main，一次解决冲突与放行两件事。
