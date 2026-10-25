# 决策 · 2026-10-26 · #19 deadline 多写：TS 宿主竞态，登记原版缺陷 + A7 Rust 宿主修

## 人定 · 选 A（2026-10-26 00:57，#723 评论）
- **卡在哪**：#19「deadline 场景 Rust 多写一条 gateway_submit_failed」。Codex 64 轮实测 + 受控交错：根因在两侧共用的 TS 宿主 `InProcessGateway.ts` 外层 catch 与 timeout fence 竞态；Rust 只因收尾快约 6 ms 更常踩中；受控交错下 TS sidecar 也多写。现有协议无「fence 已完成」通知，sidecar 单侧无可靠修法。此前人在待批清单把 #3 判「应修代码」依据的是「TS 0/10、Rust 多写 → Rust 产品问题」的频率推断，新证据推翻该前提。
- **人的原话（逐字，#723 评论）**：选择 A（建议）：认定为 TS 宿主原版缺陷，在 specs/parity/accepted-diffs.md 追加一条「宿主超时竞态多写 gateway_submit_failed」
- **决定**：`specs/parity/accepted-diffs.md` 末尾追加 #6（类：原版缺陷，已批，更正 #1–#3）；修复归 A7 的 Rust 宿主（timeout 已接管后外层 catch 不再重复登记，配确定性交错用例），另开单；#19 等 A7.7 关单条件满足后关。
- **协调者更正（重要）**：#723 **issue 正文里协调者写的选项 A 说明**（不是人的评论；人的评论全文即上一条，已逐字完整收录）有一句「阶段 C 对拍不再因此判 FAIL」，**这句不准确**——冻结计分 `scripts/parity/score.py:87 approved_unstable` 只对「已批 + 原版不稳定」行从分母剔除场景；「原版缺陷」行对计分无机器效果。故本条登记后，deadline 场景仍会偶发 FAIL（约 1/10），阶段 C 维持现状，直到 A7 Rust 宿主替换 TS 宿主。若要计分上不再 FAIL，只能按「原版不稳定」剔除 deadline / deadline_during_tool（分母 53→51，失去这两个场景的覆盖），需人另行决定，协调者不建议。
- **动不动尺子**：只追加一行，表头字节与 FROZEN 登记不变（check_frozen 通过）；不改判分语义。
- **能否撤回**：能；该行可按「只增不删，划线写原因」作废。
- **原则**：两侧被测组件 wire 规范化后一致、差异由共用宿主对称产生且受控交错两侧都复现时，登记为原版缺陷，修复归替换该宿主的阶段；计分剔除另作决定。
