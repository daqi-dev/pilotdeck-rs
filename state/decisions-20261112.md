# 决策卡 2026-11-12（coord2/e7）

## #2106 冷暖 target 判定（补卡，AI 代批待人复核）

> 补记 2026-11-07 #2106 收货轮 r2 的两条判断题（当时按「AI 代批」执行，决策卡漏写；#589 收口轮转交后仍未落，本卡补齐）。事后补记，行为已按此落地（PR #2330，MERGED 2026-11-07T21:36:27Z）。

### 卡在哪

`PDRS_NEGATIVE_BASELINE`（冷启动基线）场景下 `_ensure_workspace_scanner` 写死 HEAD，非 HEAD 基线在冷 target 上必被拒。修法是让 scanner 按「身份」而非「时间上是否 HEAD」判定可用，收货轮 r1 评审出两条判断题须拍板：

1. **「暖 target 不受影响」的限定口径**（MM boundary#1，原评 P1）：暖 target 的豁免按什么范围放行。
2. **暂存（staged）要求的限定**（MM boundary#2，P2a）：候选改动必须暂存的区域边界。

### 谁受影响

所有以非 HEAD 基线起跑的冷 target 负控/对拍车道（`PDRS_NEGATIVE_BASELINE` 消费方）；以及 scanner 在暖 target 上跳过重扫的宿主链。

### 能否撤回

能。两条都只改判定边界（5 文件清单 / 区域列表），回滚即恢复 r1 前行为。

### 动不动尺子 / 意向

不动冻结尺子。属负控装置的可用条件收窄（变严不放松）。

### 选项与后果（当时实际选定）

1. 「暖 target 不受影响」无条件断言（原实现，评审判与实现相反）：会放行身份不符的暖 target——**否**。选定：**按 scanner 身份 5 文件限定**——触碰 5 文件之外（如 `_negkit/README.md`）候选 ENTER-OK，触碰 `src/Cargo.toml`（身份文件）候选 REJECTED。后果：身份漂移立即拒、非身份漂移可复用暖 target（省重扫）。实测已贴 `_tmp/20261108-harvest-2106-r1/remote-verify/warm-identity-probe/`（cb8）。
2. 暂存要求无条件（r1 形态）：区域外候选也强制暂存——**否**。选定：**限定到被扫描区域（`src`/`.cargo`/`rust-toolchain*`）**，区域外候选不暂存也通过。后果：文档类改动不被迫进暂存区，误拒减少；被扫描区域仍严格。

### 建议

维持两条 r2 落地口径（PR #2330 已 MERGED）；后续同族判定沿用「身份限定 + 区域边界」先例。

### 一条可复用原则

**可用性豁免按身份（identity）判定，不按时间（是否 HEAD）判定**；豁免清单与强制暂存清单都必须显式列名，不允许无条件断言。可机检：warm-identity-probe 探针用例化（已并入 #2305 清单）。

## #1985 残余四张并批次车道（路线 A，AI 判定备案待人复核）

P2-跟进 #1550 已关后残余 #2053(P2)/#2056(P1)/#2057(P2)/#2730(P2) 均改 `transcript_adapter.rs`/金样/factory，与在飞 #2043+#2044（cb8，同模块）串行：等该两单收货落地后**并一张批次车道**派（Refs 五单，P1 优先），不逐张派。依据 memory「same-module-serial」「sibling-fix-shared-file」。可撤回（批次未起跑前可改逐张）。

## #3010 更正件编号顺延（-R2 → -R4，AI 判定备案待人复核）

#3010 开单拟名 `A7.8-ALWAYS-ON-BACKGROUND-001-R2`，但其后 `-R2`（#2408 cron 半口径订正）与 `-R3`（#2487 构造铺开偏差登记）先行合入；按账本只追加、同名 ID 不得复用规则顺延为 **-R4**。登记于 #3010 评论（6806446767）。

## #2692 r13 停轮：撤普查器收口（长尾卡判据 4，AI 代批待人复核）

- 触发：fix-r13 双谱系独立撞同一 P0——新增全仓共享断言普查器 `test_shared_assertion_pins.py` 按表名白名单 `('mutations','mutants')` 取表，约 55 张 `MUTATIONS`/`CASES`/`CONTROLS`/`ROWS` 等命名的表被判 `no-table` 静默放行、不进 `ROW_PIN_GAPS`/`SHAPE_GAPS`（活实例：`sms-stringify-depth` 大写表 `MUTATIONS` 的 `cap-removed`/`cap-off-by-one` 共用 `#1929` marker，双表同删静默过）——尺子假通过（协调者实测：普查 7 例 OK 而该文件不在任何登记）。评审：DS boundary P0（`_tmp/20261112-2692-fixr13-review-ds/`）、MM boundary P0 + sibling P1 同根因（`_tmp/20261112-2692-fixr13-review-mm/`），另 MM boundary P1×2（普查只扫单 crate / Case 分支 fail-open）同属普查器。**rN=13 ≥ 3 出 P0 → 长尾卡判据 4 命中：不再开修复轮。**
- 裁决：**撤件收口，不修普查器**。r13 的普查器部分（`test_shared_assertion_pins.py` + README 全仓声明段 + selfcheck 相关接线）整体撤出；ROW_PINS 主体与红腿（turn-submit/native-config/session 两家，评审对其零 P0/P1）保留合入。普查器结构性重建（AST 形状枚举任意表名、仓根遍历、Case 分支 fail-closed、全仓表名盘点落登记）转新 P2-跟进单，30 ROW_PIN_GAPS + 54 SHAPE_GAPS 债表以 r13 提交 a00925dee 为快照正本由跟进单承接。理由：判据 4 禁开 r14 修复轮；撤件非修复（删引 P0 组件而非让它工作），与 #1953「停轮转残差登记收口」同形。
- 谁受影响：PR #2960、#2133 家族、#1357 收口。
- 能否撤回：可（撤件轮前可改回停手挂单待人）。
- 不动尺子/冻结语义；不触红线。
- 原则：**新增审计/普查类机检若带「全仓」声明，其盲区必须 fail-closed（不可读形状落登记表）而非按白名单静默放行——登记不进去的文件等于声明为安全。**
