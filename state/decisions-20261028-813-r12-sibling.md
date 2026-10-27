# 决策卡 · #813 r12 GLM sibling P1 按「已另案」降档（2026-10-28 01:0x，协调者）

- **卡在哪**：#813 r12 GLM sibling P1：8 个负控 runner 仍用裸 `marker in raw` 判杀（尺子类，可能假 PASS）。
- **事实**：其中 4 个已在 #818（A 批）/ #819（B 批）登记（session-owner、session-abort-contract、edgeclaw-dangling-links、plugin-loader）；其余 4 个（metadata-process-cwd、commands-catalog、snapshot-projection、runtime-spawn-panic/helpers_negative）为 10-26 后新合入装置，补开 **#930（C 批）**。三批均前置于 #813 合入（共享件 negative_panic 在本 PR）。
- **裁定**：自决 · 待复核（AI 代批，待人复核）。按评审自身建议「若已另案延期，登记书面豁免依据后可降档」，该 P1 不挡 #813 合入；#813 关单前须 #818 #819 #930 已派出。
- **不降档的**：Codex r12 P1（turn-submit marker 仍为场景前缀，在 #813 自身 diff 范围内）→ 本 PR 内修（fix-r13）。
- **可复用原则**：**共享件 PR 不负责迁移全部兄弟调用方，但必须把未迁移的兄弟逐个登记成批次单；新合入的兄弟装置要在共享件 PR 合入前补登。**
