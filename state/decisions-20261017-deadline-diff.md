# 决策卡 · #19 deadline 场景残差的差异登记（待人批，不停机）

日期：2026-10-17 ｜ 档：**暂不执行**（冻结尺子判分语义变更须人批）｜ 状态：缺口登记，#19 保持 OPEN

## 卡在哪
- `deadline` 场景 5 组对拍 4/5：r3 时 TS 宿主对账多写一条 durable `gateway_submit_failed`（"terminal outcome is result_unknown and host reconciliation found no final result."）。
- **两侧 sidecar wire 已逐帧一致**（PR #161 原件：`agent.execute.result_unknown / DEADLINE_EXCEEDED / payload:{}`，内层 aborted completion sequence 位置一致）；extra durable 由**同一份 TS 宿主**的竞态产生——证据支持**原版宿主缺陷/时序问题**（与 exp001「时有时无」记录相符），非 Rust 差异。

## 谁受影响
- 阶段 C 分数：51/53 → 本场景修满即 52/53；53/53 还差 #35（cancel_during_tool，原版不稳定）同类的处理。
- 宿主修复属 A7（Rust 宿主接管）；在本之前该竞态持续存在。

## 能否撤回
可撤回（accepted-diffs 条目可按冻结程序再变更）。

## 动不动尺子
动：需在 `specs/parity/accepted-diffs.md` 追加 deadline 场景「宿主对账竞态 extra durable」差异条目（或按原版不稳定口径调整该场景判分/分母——两种方案待人选）。

## 选项与后果
- **A. 登记为已接受差异**（推荐）：条目按既有「原版缺陷」类别写，含 r3 原件与两侧 wire 一致证据；53 场景全保留，deadline 场景对拍时 extra durable 不计差异 → 阶段 C 可到 53/53 口径（另 #35 单独处理）。
- B. 调整分母（53→52，剔除 deadline）：损失覆盖面，且与 #35 先例口径不一致。
- C. 等 A7 换 Rust 宿主后自然消除：阶段 C 停在 52/53 数月量级，与「冲 53/53」目标冲突。

## 建议
选 A。批准后：登记条目（两家评审）→ 复算 deadline 场景 5 对 → 关 #19。

> 补注（2026-10-17 入库时）：批准本卡只覆盖残差登记。关 #19 仍须满足 `docs/plan/005_phase-a-session-domain.md:128`（A7.7）的全部条件：A7 Rust 收尾实现、两条 deadline 各 5 对按已批准口径通过、完整 C/A 回归无新差异及受控交错证据；如要缩小关单范围，需另行报批。

## 原则（一次拍板 → 一条原则；随所选方案生效）
**宿主侧竞态对称缺陷原则**：当两侧被测组件（sidecar）wire 逐帧一致、差异帧由两侧共用的宿主代码对称产生时，该差异登记为原版缺陷，不阻塞被测组件的对拍计分。
