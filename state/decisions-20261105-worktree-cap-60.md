# 2026-11-05 · worktree 上限 40 → 60（2026-11-05 23:0x 人令，AI 代批待人复核），真闸换成磁盘

## 卡

- **卡在哪**：人 2026-11-05 问「worktree 的限额还能放宽吗」，随后 **2026-11-05 23:0x 人令「放宽 worktree 上限（之前说按硬盘算上限的方案），别让名额卡住满载」**（记录见 #1157 与 #1565 正文；此前上限 40 是 2026-11-01 由 24 放宽，PR #1236，人同意；`scripts/lib/worktree-count.sh`，2026-11-05 起口径为「全部 linked worktree，排除主仓」）。本卡按该人令把**生效默认值改为 60**，**主从关系是磁盘阈值说了算**：60 只在磁盘充裕时兜底，真正的闸是下面那套 200/180/160 GiB 三档。人令是口头令，故标「AI 代批，待人复核」。实测：（2026-11-05 21:5x）
  - `git worktree list` 38 行（含主仓），即 37 棵 linked；
  - `_build/wt-*` 合计约 130 GiB，单棵 3–34 GB：`wt-1334` 34 GB、`wt-1336` 30 GB、`wt-1258` 17 GB —— 大头全是树内 `_build/cargo-target`（`du -sm _build/wt-1334/_build/cargo-target` = 33.9 GB）；
  - `_build`+`_tmp` = 165.9 GiB（`du -sk` 原值 173,996,420 KiB），上限 200 GiB —— **磁盘才是当前真瓶颈，不是树数**。

  40 棵本身没被顶满（37/40），但每开一棵就可能带进十几 GB 编译产物；只放宽数字而不加磁盘闸，等于把闸门从「数树」换成「撞 200 GiB 上限时停工」——这正是人令「按硬盘算上限」要避免的，所以磁盘档是主机制、60 只是兜底。
- **谁受影响**：协调者的开树闸门（`pdrs-ops/bin/new-tree`）、回收闸门（`pdrs-ops/bin/recycle` / `scripts/wt-status.sh` rc）、外层循环 `scripts/pdrs-loop.sh`、本机磁盘与并行席位；不改任何产品代码行为。
- **能否撤回**：可撤回。改回 40 只改 `PDRS_WORKTREE_CAP_MAX` 一处常量；磁盘三档可单独调阈值或移除，无数据迁移、无历史重写。
- **动不动尺子/意向**：都不动。不碰 `.42cog/intent.md`、不碰任何冻结文件与判分语义。

## 决定

### A. 仓内（本 PR 落地，合并进 main 即生效）

1. **上限常量仍只有一处**：`scripts/lib/worktree-count.sh`——`PDRS_WORKTREE_CAP_MAX=60`，`PDRS_WORKTREE_CAP_DEFAULT` 取同一个值，即**不设环境变量时生效值 = 60**（2026-11-05 23:0x 人令，AI 代批待人复核）；非数字 / 0 / >60 一律回落 60，笔误不会把闸门关掉；调低（自测用）仍允许并打一行提示。回退到 40 只改这一处常量。`scripts/wt-status.sh` 与 `pdrs-ops` 的 `new-tree.sh` 都 source 它，不另写数字。
2. **磁盘三档的阈值与测量也收敛到仓内一处**：新增 `scripts/lib/disk-budget.sh`（`PDRS_DISK_TOTAL_GIB=200` / `PDRS_DISK_HARD_GIB=180` / `PDRS_DISK_WARN_GIB=160`、`pdrs_disk_used_kib`、`pdrs_disk_level_of_kib`、`pdrs_disk_level`）。判断一律用 `du -sk` 的 **KiB 原值**，不做 GiB 四舍五入；判档只走 `pdrs_disk_level_of_kib`（`pdrs-loop.sh` 与 ops `new-tree.sh` 都调它，调用方不再自写比较式）。四处易踩的坑已实测并处理：
   - **命令行符号链接 `du -sk` 不跟随**（11-05 BSD 实测：指向 3 MiB 目录的链接报 `0 KiB`、rc=0）——本仓 runner 树确有 `_build` 指向外接卷的配置，直接量会让闸门静默失效。改为先 `cd + pwd -P` 取物理路径再量。
   - **断链**同样是 `0 KiB` + rc=0，按测量失败处理（rc=2 拒开）。
   - **仓根不存在**此前被算成 0 GiB 放行（与断链处理相反）；改为前置错误 rc=2。仓根在、但没有 `_build`/`_tmp`（远端宿主）仍按 0。
   - **`du` 部分失败**：`_build` 是 cargo target 树，编译并发删改文件时 du 会以非 0 退出。改为失败先重试一次，仍失败才 fail-closed；`pdrs_disk_level` 失败打印 `unknown` 而不是空串（空串会被 `[ "$lvl" = hard ]` 形式静默放行）。
3. **`scripts/pdrs-loop.sh` 接同一实现**：删掉自带的 `DISK_CAP_KIB=167772160` 与内联 `du`，改调 `pdrs_disk_used_kib`；保留「在 160 GiB 就停派新活」的保守行为（比开树硬闸早一档），并把陈旧的「超 100GiB 上限」文案改正。测量失败同样停机（fail-closed）。
4. `CLAUDE.md` §五「资源上限」两条与 `.42cog/real.md` 如实写明：**worktree 上限 40 → 60（2026-11-05 23:0x 人令「放宽、按硬盘算」，AI 代批，待人复核；见 #1157）**，并写死主从关系——**磁盘阈值说了算，60 只在磁盘充裕时兜底**；磁盘档位指向 lib，不重复写数字。

### B. ops 侧（**已提交在 ops 本地分支 `chore/worktree-cap-60-1105`（commit `74cb144`），尚未合并进 ops 运行树——合并后才生效**）

5. `pdrs-ops/scripts/loop/new-tree.sh` 开树前加磁盘闸：`pdrs_disk_used_kib` 测 `_build`+`_tmp`，KiB 原值比较——
   - `> 180 GiB`：直接拒开，打印「先回收」并列出 `wt-status` 可回收项；`du` 失败 rc=2 拒开；
   - `> 160 GiB`：打 WARN 但放行；
   - `≤ 160 GiB`：放行。
   （阈值与测量 source 上面的 `scripts/lib/disk-budget.sh`，`PDRS_DISK_LIB` 仅供自测指向同内容副本。）
6. `pdrs-ops/scripts/loop/recycle-tree.sh` 在 `git worktree remove` 前删该树的 `_build/cargo-target` 与 `target/`，并打印释放量；顺带加树名单段校验（脚本现在会 `rm -rf`，挡 `../` 越界）。
7. `pdrs-ops/scripts/remote-worker/README.md` §7.6 写明：每棵树的 `CARGO_TARGET_DIR` 必须在树内 `_build/cargo-target`（或 `target/`），否则回收删不掉。

> 状态说明：ops 是仓外本地 git（无远端），按本次任务约定只提交到新分支、不合并、不推送。**在 ops 侧合并之前，`/Users/da/ws/pdrs-ops` 主检出里的 `new-tree.sh` 仍是旧行为（185 GiB 硬闸、无 WARN、GiB 四舍五入）**，仓内文档描述的是合并后的目标行为。要核这侧实现，读 worktree `/Users/da/ws/pdrs-ops-cap60` 的分支 `chore/worktree-cap-60-1105`（提交含：名额/磁盘闸/回收删 target、改用主仓 `disk-budget.sh`、判档改调 `pdrs_disk_level_of_kib`；ops main 每次前进都要 rebase，故这里不写死 SHA），不要读 `/Users/da/ws/pdrs-ops`（那里有别的会话未提交改动，本单未触碰）。

## 选项与后果

| 选项 | 后果 |
|---|---|
| A. 只把 40 改 60，不加磁盘闸 | 树数不再卡，但一棵新树可能带进 10–30 GB；`_build+_tmp` 会更快撞 200 GiB，撞上即全仓停止新增写入（CLAUDE.md §五） |
| B. **40 → 60 + 磁盘三档 180/160 + 回收删 target**（采纳，人令指定「按硬盘算上限」） | 放宽并行席位的同时把闸门落在真瓶颈上；回收即释放十几 GB，磁盘回到可用区间。**磁盘阈值说了算，60 只在磁盘充裕时兜底** |
| C. 不动上限，只加磁盘闸 | 37/40 已接近顶，并行度上不去；与 2026-11-05 23:0x 人令「别让名额卡住满载」相反 |
| D. 按磁盘折算成树数（比如每 3 GiB 算一棵） | 需要新的测量口径与换算表，且磁盘已有独立阈值，重复建模 |

## 建议

采纳 B。**上限不是目标**：READY 有互相独立的产品单才并行，60 只是天花板。磁盘闸取 180 GiB 是给 200 GiB 上限留 20 GiB 缓冲，160 GiB WARN 是提前提醒回收。

**残留风险**：
- **磁盘闸是「开树时刻」的快照，不覆盖构建期增长**：`du` → 判 → `git worktree add` 之间无锁，建树后编译出的 target（单棵实测 17–34 GB）不在闸的视野里。165.9 GiB 时放行、新树编译出 33.9 GB 就贴到 200 GiB 线上。缓解是 `pdrs-ops/scripts/loop/disk-guard.sh`（>160 GiB 时每 10 min 清**空闲**树），正在编译的树的增量仍不设防——所以 **160 GiB 起就必须真回收，不能等到 180**。
- **180 GiB 缓冲（20 GiB）小于单棵树的 target 峰值（33.9 GB）**：硬闸挡住的是「再开一棵」，挡不住「已开的树把磁盘写满」。真正兜底是 200 GiB 上限 + 回收纪律，不是 180 这个数。
- `new-tree.sh` 用绝对路径 source 主仓的 `scripts/lib/{worktree-count,disk-budget}.sh`；主仓检出切到不含这两个文件的旧提交时开树会 exit 2（fail-closed），不会静默退回旧口径。
- `du` 有缓存/延迟，实际值可能略低于瞬时真值；KiB 原值比较至少消掉了四舍五入这一层误差。
- 回收删 `target/` 只删可重建的编译产物；`git status` 干净检查与 `_tmp` 归档在此之前完成，不碰未提交改动与证据。
- ops 侧改动未合并（见上「状态说明」），合并前磁盘闸不生效。
- **闸的测量面 = 主仓 `_build` + `_tmp`**：`~/.codex/worktrees/…`、`/tmp` 下的树同样吃磁盘（它们已计入名额口径），但它们的树内 `_build/cargo-target` 不在磁盘闸视野里。阈值按「少算的数」标定，真占用可能高于闸看到的数。
- **树内嵌套符号链接不计入**：`cd + pwd -P` 只解决「被量的 `_build` 本身是链接」；`du` 遍历时不跟随树内遇到的链接，`_build/wt-*/_build` 指向外接卷的配置那份体积仍不可见。
- 已补：`du` / `cd` 走 `timeout`（10 s / 180 s，本机有 GNU coreutils；无 `timeout` 的环境退化为不限时）；`pdrs-loop.sh` 从 worktree 启动时改量**主仓**根（`git worktree list` 第一行），避免量到自己那棵没有 `_build`/`_tmp` 的树而恒判 ok。
- **冻结装置 `scripts/ui-server-parity/calibrate.py` 的 80 GiB 闸仍有同类反模式**：`p.exists()` 会滤掉断链、`du -sk` 不跟随 symlink、失败时取 `max(totals)` 的部分和。阈值属已登记的例外，但**测量**要在冻结程序里改，本单不动。
- **ops 侧还有两处自带测量**：`disk-guard.sh`（内联 `du | awk` + 字面量 160）与 `disk-trim-idle.sh`；「唯一实现」目前只对仓内成立，收敛这侧要另开一笔。
- **开树的名额/磁盘闸与 `git worktree add` 之间无锁**：两个并发 `new-tree` 可读到同一快照（59 棵 / 179 GiB）后都放行。上限放宽到 60 反而让并发开树更常见；收口需一把全局 flock。

## 原则（一次拍板 → 一条原则）

上限数字跟着真瓶颈走：树数不再是瓶颈时放宽树数，同时把闸门挪到磁盘；阈值与测量各只有一处实现，闸门 fail-closed、回收必须真删。

## 登记者

AI 代批，待人复核。
依据：2026-11-05 23:0x 人令「放宽 worktree 上限（之前说按硬盘算上限的方案），别让名额卡住满载」（口头令，记录见 #1157 与 #1565 正文；两条独立记录一致）。按人令落地为「**磁盘阈值为主、60 为兜底默认**」：真正的闸是 200/180/160 GiB 三档，60 这个数字只在磁盘充裕时生效。人令没有签字动作，故不写「已批准」，只写「AI 代批，待人复核」。改动可撤回、不动尺子与意向；上限常量与磁盘阈值各收敛到一处，磁盘档位以实测（165.9 GiB 现状、单棵 target 17–34 GB）为依据。
关联：`state/decisions-20261105-worktree-cap.md`（计数口径，PR #1548）、`state/decisions-20261105-parallel-speedup.md`（其「worktree ≤ 40」条目已被本卡取代）、PR #1547（`scripts/lib/worktree-count.sh` 实现，本卡改动叠在其分支上）。
