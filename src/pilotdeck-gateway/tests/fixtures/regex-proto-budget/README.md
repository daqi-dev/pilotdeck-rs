# regex-proto-budget

#1265 的负控夹具：证明 pilotdeck-regex-proto 的匹配预算真的在约束执行
（预算是 #685 报告验收清单的硬性行：耗尽必须是显式错误，不得降级为
无匹配/false）。

- 变异对象：`src/pilotdeck-regex-proto/src/engine.rs`（先 WIP 提交，
  `mutating_sources` 要求源码等于 HEAD 才允许变异）。
- 被绑定的测试：`src/pilotdeck-regex-proto/tests/budget.rs`（仅
  `--features regex-proto` 下编译运行）。
- 3 项变异绑定完整：
  1. `interrupt-decrement-removed` —— 中断回调不扣费 → 三态测试失败；
  2. `exhaustion-downgraded-to-no-match` —— 耗尽降级 NoMatch → 三态测试失败；
  3. `budget-unarmed-between-requests` —— 中断标志跨请求不清 → 新预算测试失败。

运行（调用方需设 `CARGO_TARGET_DIR`，PATH 含 cargo 槽位门的 bin）：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/regex-proto-budget/negative_controls.py OUT
```

每个变异先过 `cargo test --no-run` 编译关，再跑绑定测试；全部变异跑完才
输出 summary.json；任一非 KILLED 以非零退出。被杀断言全部带 `#1265 …`
专属消息。证据目录 OUT 必须是新建空目录。
