---
name: merge-email-exposure
description: GitHub 服务端生成的提交（merge commit、rebase 后的提交者）用账号主邮箱而非本地 noreply；公开仓库每次合入后都要核 %ae %ce。不管仓外 clone 的空邮箱（见 clone-author-email）。
metadata:
  type: feedback
---

**为什么**：2026-10-13，`--merge` 生成的 merge commit 作者是账号主邮箱（原 `4172dc2`）；`--rebase` 虽保留作者，**提交者**也换成主邮箱（`c720725`、`cfd7841`）。仓库是公开的。根治靠账号设置（人做）：GitHub Settings → Emails 勾「Keep my email addresses private」；merge queue 按 rebase 合入，历史线性，但**不**解决邮箱问题。原文：`git show 4fd3ee5:CLAUDE.md` 第 57–60 行。

**怎么用**：以 `gh pr view <N> --json state` 为 `MERGED` 后，`git fetch` 再 `git log origin/main --format='%h %ae %ce' -3`，看到非 noreply 邮箱即登记到 board 并告知人；不假定账号设置已勾选。
