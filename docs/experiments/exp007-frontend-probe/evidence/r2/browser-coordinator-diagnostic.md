# r2 协调者诊断 diff 归档（重建版）

4 份协调者 JSON 原件未改，哈希已追加到 manifest.json。临时诊断脚本未随 F1 入库；本次在主仓 _tmp、评审副本、worktree 与 /tmp 未找到原脚本。因此本 patch 是依据 readme、评审原文及 diagnosticDraftAfterSyntheticEnter 字段重建的可复现最小诊断，**不能证明协调者当时脚本逐字相同**。

patch 仅替换组合期 Enter 后严格草稿断言为记录真值，并将草稿复位为你好供原有后续严格守卫断言使用。这个复位是测试夹具动作，不改候选代码；历史说明「仅改 :55」未披露后续夹具处理，原脚本缺失时不能进一步断言。当时原件 raw 的 sent=0 已过；diag 记录你好\n与4/4机器检查PASS，只支持已记录的范围。

正式 #42 修正不会复位草稿：sent=0+前缀检查，并将 Enter 后完整草稿作为后续守卫保持断言的基值。当前正式脚本不依赖诊断副本。

在独立临时副本上用 patch --dry-run -p1 检查；不要把诊断 patch 应用到本 PR 正式脚本。正式复跑：node scripts/exp007/browser.mjs leptos，然后 dioxus；二者预期4/4 PASS_BROWSER_MACHINE_CHECKS；合成事件范围不代表 OS IME。
