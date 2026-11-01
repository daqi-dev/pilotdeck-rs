---
type: research
number: "010"
date: 2026-10-31
title: "#1140 包三：deadline native 接线不新增聚合出口（#892 运行时观测口径）决策卡"
tags: [phase-a, decision, session, aggregation, #892, #1140]
status: AI 代批，待人复核；#892 本体仍挂起（decisions-20261028-892-suspend.md 方向 A）
related: [research/009_a19-host-expectations/decision, state/decisions-20261028-892-suspend.md, specs/gateway-parity/frozen/20261031-1140.md]
---

# 010 · 决策卡（#1140 包三的聚合出口不扩张）

**建议：#1140 包三（`--native-capture` deadline 判分接线）不新增任何聚合出口；全部新状态计算复用共享 `common.aggregate_status`，以运行时观测（test_safety 断言）证明批次状态确由共享汇总产生，不做新的静态 AST 证明；`check_cleanup.py` 的既有 non-point 理由文字本包不动，留待后续事件。** 依据：`state/decisions-20261028-892-suspend.md`（#892 修 3 轮仍可假 PASS → 挂起，方向 A：改运行时校验，不再做静态 AST 证明）、issue #1140 协调者包三补充（#892 二选一）、#1177 登记的 A19-HOST 期望。

以下按 `CLAUDE.md` 三档填写。「自决·待复核」表示本包采用该口径（AI 代批），不表示 #892 已解挂或人已批准。

## D1 · 本包聚合出口盘点：零新增

| 卡项 | 内容 |
|---|---|
| 卡在哪 | #1140 让 native 格产生真实 PASS/FAIL 后，批次状态、`executionStatus` 与退出码必须仍由唯一聚合器决定，否则 #892 挂起期间又长出新出口。 |
| 谁受影响 | `scripts/gateway-parity/session_domain.py`（`deadline_report` / `load_native_capture` / `score_native_cases` / `main`）、`controls/session_domain.py`、`test_safety.py`。 |
| 能否撤回 | 能；登记可重开冻结事件。 |
| 动不动尺子 / 意向 | 判分接线只加「每 case 观测 vs 登记期望」的比较，不碰聚合语义。 |
| 选项与后果 | A **新增函数一律不调用 `aggregate_status`，全部以 verdict 列表表达式构造；native 格与批次状态各在已登记聚合点（`main`）或已登记 non-point（`deadline_report`）内调用共享 `aggregate_status`**：`check_cleanup.py --mode aggregation-audit` 候选树实测 PASS（unclassified=[], failures=[]）。B 在 loader/scorer 内直接 return 状态字符串：产生未登记出口，审计失败。C 扩 `AGGREGATION_POINTS` 登记新出口：违背「零新增」。 |
| 建议与分级 | **选 A，自决·待复核**。`score_native_cases` 输出 `verdicts.append({...})` 表达式语句（非赋值），不触发审计标记；`deadline_report` 里 native 格 `cell['status'], _ = common.aggregate_status(...)`、批次 `status = common.aggregate_status(*statuses)[0]`。 |
| 原则 → 断言 | 比较器可以新增，聚合器唯一 → 任何新函数调用 `aggregate_status` 或返回状态码必须被 aggregation-audit 拒绝（候选树实测拒绝路径见 RESULT.md）。 |

## D2 · 运行时证明：#892 方向 A 的本包落地

| 卡项 | 内容 |
|---|---|
| 卡在哪 | 挂起决议要求「以真实入口在受控环境执行，观测退出码确由共享汇总产生」；静态 AST 证明已被三次假 PASS 否决。 |
| 谁受影响 | `test_safety.py` 的 #1140 块。 |
| 能否撤回 | 能。 |
| 动不动尺子 / 意向 | 观测已发布报告，不读内部实现。 |
| 选项与后果 | A **test_safety 以真实 `entry.main()`（仅 patch 租约）跑 deadline：native PASS + 其余 NOT_RUN → exit 3 / `status=NOT_RUN`，并断言 `report['status'] == common.aggregate_status(*[cell['status'] for cell in report['cells']])[0]`；单面变异 → exit 1 / FAIL**：退出码与状态是运行时从报告观测的，不是 AST 推断。B 在 controls 里也放此断言：controls 属库代码，会被 aggregation-audit 的 `calls-aggregate` 标记拒绝（实测发生过并已移除）。C 只写文档不测：无证明。 |
| 建议与分级 | **选 A，自决·待复核**。该断言在脏候选树与提交后树均运行（`unittest.main` 不经 run_entry）。 |
| 原则 → 断言 | 运行时观测优先于静态推断 → 删除 `main` 聚合调用或另算批次状态的变异必须使该断言 FAIL（由既有 #164 负控族覆盖 `main` 出口）。 |

## D3 · `check_cleanup.py` non-point 理由文字：本包不动

| 卡项 | 内容 |
|---|---|
| 卡在哪 | `deadline_report` 等既有 non-point 的登记理由写「four-cell NOT_RUN structure only; main maps NOT_RUN to exit 3」；#1140 后该函数还计算 native 每 case 状态并调用共享聚合，理由文字过时但登记本身仍正确。 |
| 谁受影响 | `check_cleanup.py`（冻结清单文件，#892 挂起域）。 |
| 能否撤回 | 改理由文字需另一冻结事件。 |
| 动不动尺子 / 意向 | 审计只核成员与调用形状，不核理由文字；文字过时不产生假 PASS。 |
| 选项与后果 | A **本包不改 `check_cleanup.py`，理由在此记录，留给后续专门事件刷新**：避免在 #892 挂起期间动它的宿主文件。B 顺手改一行理由：把 #892 挂起域的冻结文件卷入本包，评审面扩大且无行为收益。 |
| 建议与分级 | **选 A，自决·待复核**。 |
| 原则 → 断言 | 挂起问题不搭车 → `check_cleanup.py` 本包零 diff（RESULT.md 自查项核对 `git diff --stat`）。 |

## 需人定

1. **D3 的理由文字何时刷新**：是否接受留待 #892 解挂或专门事件统一更新，还是要求本包后立即补一个小登记事件改写理由。本包行为不受影响。
