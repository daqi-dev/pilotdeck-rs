# 2026-11-05 · worktree 上限 40 → 60，真闸换成磁盘

## 卡

- **卡在哪**：人问「worktree 的限额还能放宽吗」并同意往宽做。当前上限 40（`scripts/lib/worktree-count.sh` 的 `PDRS_WORKTREE_CAP_MAX`，2026-11-05 起口径为「全部 linked worktree，排除主仓」）。实测：（2026-11-05 21:5x）
  - `git worktree list` 38 行（含主仓），即 37 棵 linked；
  - `_build/wt-*` 合计约 130 GiB，单棵 3–34 GB：`wt-1334` 34 GB、`wt-1336` 30 GB、`wt-1258` 17 GB —— 大头全是树内 `_build/cargo-target`（`du -sm _build/wt-1334/_build/cargo-target` = 33.9 GB）；
  - `_build`+`_tmp` = 165.9 GiB（`du -sk` 原值 173,996,420 KiB），上限 200 GiB —— **磁盘才是当前真瓶颈，不是树数**。

  40 棵本身没被顶满（37/40），但每开一棵就可能带进十几 GB 编译产物；只放宽数字而不加磁盘闸，等于把闸门从「数树」换成「撞 200 GiB 上限时停工」。
- **谁受影响**：协调者的开树闸门（`pdrs-ops/bin/new-tree`）、回收闸门（`scripts/wt-status.sh` rc）、本机磁盘与并行席位；不改任何产品代码行为。
- **能否撤回**：可撤回。改回 40 只改 `PDRS_WORKTREE_CAP_MAX` 一处常量；磁盘闸（180 GiB 硬闸 / 160 GiB WARN）可单独调阈值或移除，无数据迁移、无历史重写。
- **动不动尺子/意向**：都不动。不碰 `.42cog/intent.md`、不碰任何冻结文件与判分语义。

## 决定（已执行）

1. 上限常量仍只有一处：`scripts/lib/worktree-count.sh` 的 `PDRS_WORKTREE_CAP_MAX`，值 40 → **60**。`scripts/wt-status.sh` 与 `pdrs-ops` 的 `new-tree.sh` 都 source 它，不另写数字。
2. `new-tree.sh` 开树前加磁盘闸：`du -sk _build _tmp` 求和**用 KiB 原值**判断（不做 GiB 四舍五入）——
   - `> 180 GiB`：直接拒开，打印「先回收」；
   - `160–180 GiB`：打 WARN 但放行；
   - `≤ 160 GiB`：放行。
3. 回收树（`pdrs-ops/bin/recycle`）在 `git worktree remove` 前删该树的 `_build/cargo-target` 与 `target/`，并打印释放量。`_build/cargo-target` 是每棵树的主要磁盘来源（见上）。

## 选项与后果

| 选项 | 后果 |
|---|---|
| A. 只把 40 改 60，不加磁盘闸 | 树数不再卡，但一棵新树可能带进 10–30 GB；`_build+_tmp` 会更快撞 200 GiB，撞上即全仓停止新增写入（CLAUDE.md §五） |
| B. **40 → 60 + 磁盘闸 180/160 + 回收删 target**（采纳） | 放宽并行席位的同时把闸门落在真瓶颈上；回收即释放十几 GB，磁盘回到可用区间 |
| C. 不动上限，只加磁盘闸 | 37/40 已接近顶，并行度上不去；人已明确同意放宽 |
| D. 按磁盘折算成树数（比如每 3 GiB 算一棵） | 需要新的测量口径与换算表，且磁盘已有独立阈值，重复建模 |

## 建议

采纳 B。**上限不是目标**：READY 有互相独立的产品单才并行，60 只是天花板。磁盘闸取 180 GiB 是给 200 GiB 上限留 20 GiB 缓冲（一次 cargo 构建/对拍的瞬时增量），160 GiB WARN 是提前提醒回收。

**残留风险**：
- `new-tree.sh` 用绝对路径 source 主仓的 `scripts/lib/worktree-count.sh`；主仓检出切到不含该文件的旧提交时开树会 exit 2（fail-closed），不会静默退回旧口径。
- 磁盘闸的 `du` 有缓存/延迟，实际值可能略低于瞬时真值；180 vs 200 的 20 GiB 缓冲覆盖这个误差。
- 回收删 `target/` 只删可重建的编译产物；`git status` 干净检查与 `_tmp` 归档在此之前完成，不碰未提交改动与证据。

## 原则（一次拍板 → 一条原则）

上限数字跟着真瓶颈走：树数不再是瓶颈时放宽树数，同时把闸门挪到磁盘；每棵树的构建产物必须可回收，回收时必须真删。

## 登记者

AI 代批，待人复核。
依据：人已明确同意放宽 worktree 上限（2026-11-05 询问后答复「往宽做」）；改动可撤回、不动尺子与意向；上限常量收敛到一处，磁盘阈值以实测（165.9 GiB 现状、单棵 target 17–34 GB）为依据。
关联：`state/decisions-20261105-worktree-cap.md`（计数口径，PR #1548）、PR #1547（`scripts/lib/worktree-count.sh` 实现）。
