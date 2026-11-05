**决策卡：#1515 冻结重登记（新增 A8.6 retirement TS 标定切片）· AI 代批，待人复核**

> **复核入口**：PR #1515 <https://github.com/daqi-dev/pilotdeck-rs/pull/1515>（源单 #1355）。复核即核下面「逐条核验表」与「签字范围与限制」；有异议按「决策分级」改判，不默认生效。
> **状态**：AI 代批，待人复核（2026-11-06）。

## 卡在哪

PR #1515（源单 #1355，E1 `--suite retirement` TS 定标切片）含冻结范围变更：

```
M specs/gateway-parity/FROZEN.md
A specs/gateway-parity/diffs/A8.6-RETIREMENT-001.md
A specs/gateway-parity/frozen/20261105-1355-r{2,3,4,4b,4c,4d,4e,4f}.md   （8 个事件文件）
```

`CLAUDE.md`「冻结尺子的变更」要求：冻结文件不擅改，须独立 issue、理由、两家评审、真实数据复算、重新登记。协调者逐条核过后判定满足，按「决策分级」的「自决 · 待复核」签发；本卡是该签发的书面记录。

## 谁受影响

- **#1355**：E1 `--suite retirement` TS 定标切片本体。
- **#1357**（A8.6 收口）：退役报告引用本切片口径。
- **#1354 / #1356**：冷切换与回滚演练——本切片留 `BLOCKED: pending #1354` 显式占位，不静默跳过。
- **跟进单**：#1477（Rust 侧 retirement 逐方法 outcome 对照）、#1478（HOST_CENSUS 换路径盲区 / 网络采样时机）、#1524（解释器过滤过窄）、#1588（报告与对照路径观测键硬下标）。
- **用户可感知**：无。本切片只跑 TS 参考侧定标，不判 Rust，不改任何既有切片（calibrate / deadline）的判分结果，也不改生产行为。

## 能否撤回

- **合入前可整段撤回**：8 个事件文件与 `diffs/A8.6-RETIREMENT-001.md` 均不在 `origin/main`（`git ls-tree origin/main specs/gateway-parity/frozen/` 无任何 1355 条目，`diffs/` 亦无该文件）；关闭 PR 即回到登记前。
- **合入后不能原地撤**：按「差异账本只追加」与 frozen README「已有文件相对 merge-base 原字节冻结」，撤销须新增事件文件并重新 `--write`，不能删/改既有事件与哈希行。故本签只对「新增切片」有效，不预授权任何后续改动。

## 动不动尺子 / 意向

- **动尺子**：新增 `--suite retirement` 判分面（101 方法覆盖、入口清单、无 TS 宿主审计、六类阳性对照、k=5 账本）。已按冻结程序执行：独立 issue #1355、理由条目 `diffs/A8.6-RETIREMENT-001.md`、两家非作者评审、真实 TS 数据复算、`--write` 重新登记 + 8 个事件文件。
- **不改**：`calibrate` / `deadline` 既有判分、阈值、分母、归一化，以及 `accepted-diffs.md` 与 `FROZEN.md` 既有条目语义。本 PR 对 `FROZEN.md` 只动哈希清单：6 行 = 4 条改（`check_frozen.py` / `common.py` / `session_domain.py` / `test_safety.py`）+ 2 条新增（`fixtures/retirement-methods.json` / `retirement_driver.mts`）。
- **不动意向**：`.42cog/intent.md` 不变；授权来源是 #1301 B「E1 只授权 `--suite retirement`，走冻结程序」（`state/decisions-20261102-a-stage-scope.md`「#1301 → B：E1 只授权 `--suite retirement`」一节）。

## 逐条核验表

| # | 程序要求 | 实测依据 | 结论 |
|---|---|---|---|
| 1 | **独立 issue** | `gh issue view 1355`：OPEN，标题「test(parity) E1 --suite retirement：101 coverage / 入口清单 / 无 TS 宿主审计 / 冷切换回滚 / 阳性对照（冻结装置新增判分，等 #1301）」；授权链 #1301 B（`state/decisions-20261102-a-stage-scope.md`「#1301 → B」一节）只授权本 suite、走冻结程序。 | 满足 |
| 2 | **理由** | `specs/gateway-parity/diffs/A8.6-RETIREMENT-001.md` 六节（类别 / 基线依据 / Rust 行为 / 范围 / 已知边界 / 机器判据）写明：新增 `--suite retirement` 判分面；101 方法四源一致（`frames.ts` / `coverage-manifest.json` / `method-inventory.md` / fixture）；阈值只看 TS 参考侧预注册（k=5、规则类、FAIL 口径）；明确「本轮不判 Rust」。 | 满足 |
| 3 | **两家非作者评审** | 作者 = GLM。末轮 head=`132805fab`，两家四角色全 PASS：<br>· anthropic/opus：`p0=0 p1=0`，P2=5（sibling 2 / boundary 2 / simplify 1）、P3=8（sibling 1 / simplify 7）；<br>· MiniMax：`p0=0 p1=1`，P2=11（sibling 4 / concurrency 2 / boundary 4 / simplify 1）、P3=14。MiniMax 该 P1 指「`20261105-1355-r4/r4b/r4c.md` 被原地改写，会撞 merge-base 逐字节比对」——实测 `git ls-tree origin/main specs/gateway-parity/frozen/` 无任何 1355 事件（三文件均为本 PR 新增），`scripts/gateway-parity/check_frozen.py:449-454` 只遍历 base 中的既有条目，新增事件不在比对集内 → 不适用，按实测降级并留痕。中间轮 P0/P1 均已修复并逐轮增量复核（r2→r4f 共 8 个事件文件对应 8 次重登记）。 | 满足（无遗留 P0/P1） |
| 4 | **真实数据复算** | 真实 TS 侧 k=5 轮：`_tmp/20261105-issue-1355/retirement-run/`（初轮）与 `retirement-run-r2/`（修复后复算）的 `report.json`，每轮 executed 101/101，六类阳性对照全 PASS；实施报告 `_tmp/20261105-issue-1355/report.md`。判分逻辑按真实轮数据定标，对照注入只证明能抓错（memory「ruler-real-data」）。 | 满足 |
| 5 | **重新登记** | `FROZEN.md` 哈希块由 `python3 -I scripts/gateway-parity/check_frozen.py --write` 生成（非手改哈希）：6 行变化见上节。8 个事件文件各带 `Change:` 行连接旧→新（r2 6 条、r3 3 条、r4 / r4b / r4c / r4d / r4e / r4f 各 2 条），`Independent-Reviews` 已回填两家路径。**机器证据**：PR #1515 head=`132805fab` 的 `pr-gate` 检查 **SUCCESS**（`gh api .../commits/132805fab/check-runs` 实测）；`check_frozen.py --base origin/main` **判自洽（PASS）**（协调者复核）。运行记录另见 `_tmp/20261105-issue-1355-receive/RESULT.md` §四。 | 满足 |

P2/P3 处置：两家 P2/P3 均不触及既有判分语义；影响 Rust 侧判定的闭环已开跟进单 #1477 / #1478 / #1524 / #1588，不随本签视为完成。

## 签字范围与限制

1. 本签**只覆盖「新增 A8.6 retirement TS 标定切片」**：该切片不判 Rust、不改任何既有切片的判分结果、不触碰 `accepted-diffs.md` 与 `FROZEN.md` 的既有语义。
2. 若后续要让它参与 Rust 判定，须**重新走两家评审并另行授权**。
3. 本签**不构成合并许可**：#1515 当时 `rust` / `check` 检查仍为 FAILURE，绿了才入队；合并仍按常规闸门（两家非作者评审、pr-gate、CI）走。

## 选项与后果

| 选项 | 后果 |
|---|---|
| A（采纳）签字放行 | #1515 走常规合并闸门；A8.6 收口（#1357）拿到 TS 定标侧。风险被「不判 Rust + 不构成合并许可」两条限制框住。 |
| B 暂不签 | #1355 停在评审后，冻结重登记不生效；#1357 无法引用该切片，A8.6 链挂起。 |
| C 要求更窄的授权（例如只签事件文件、不签 `diffs/` 条目） | 冻结登记与理由条目会分离，`--base` 链检查失去「每条变化有理由」的对应，不推荐。 |

## 建议

**A**。五条程序要求逐条有实测依据；末轮两家 P0/P1 清零，唯一 P1 经实测不适用并留痕降级；P2/P3 已登记跟进单。本签的限制条款（只覆盖 TS 定标切片、Rust 判定另行授权、不构成合并许可）把风险面压到最小。

## 一次拍板 → 一条原则 → 一条断言

- **原则**：冻结装置扩容按「切片」授权，不按 PR 授权；只有「不判 Rust、不改既有判分」的 TS 定标切片可以 AI 代批 + 待人复核，一旦参与 Rust 判定即须重新走两家评审并另行授权。
- **可机检断言**：① `FROZEN.md` 哈希行必须等于 `check_frozen.py --write` 的输出，且 `check_frozen.py --base origin/main` PASS；② retirement 的 Rust 判定代码若进入仓库，必须伴随新的冻结事件文件与授权指针，否则 `--base` 应报未登记变化。

## 签字记录

- 2026-11-06 协调者签发「AI 代批，待人复核」。依据：本表五行实测 + 末轮结果块（head=`132805fab`，anthropic `p0=0 p1=0`、MiniMax `p0=0 p1=1` 经实测降级）。
- 原始材料：`_tmp/20261105-issue-1355-r4f-final/{anthropic,minimax}/summary.md`；`_tmp/20261105-issue-1355-r4f-final/*/{sibling,concurrency,boundary,simplify}.md`；`_tmp/20261105-issue-1355/`（真实轮与复算）；`_tmp/20261105-issue-1355-receive/RESULT.md` §四。
