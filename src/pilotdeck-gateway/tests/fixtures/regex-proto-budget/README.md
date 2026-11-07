# regex-proto-budget

#1265 的负控夹具：证明 pilotdeck-regex-proto 的匹配预算与入口防线真的在
约束执行（预算是 #685 报告验收清单的硬性行：耗尽必须是显式错误，不得降级
为无匹配/false）。

- 变异对象：`src/pilotdeck-regex-proto/src/engine.rs`（先 WIP 提交，
  `mutating_sources` 要求源码等于 HEAD 才允许变异）。
- 被绑定的测试：`src/pilotdeck-regex-proto/tests/budget.rs` 与
  `tests/lifecycle.rs`（仅 `--features regex-proto` 下编译运行）。
- 5 项变异绑定完整：
  1. `interrupt-decrement-removed` —— 中断回调不扣费 → 三态测试失败；
  2. `exhaustion-downgraded-to-no-match` —— 耗尽降级 NoMatch → 三态测试失败；
  3. `budget-unarmed-between-requests` —— 中断标志跨请求不清 → 新预算测试失败；
  4. `zero-budget-entry-removed` —— 入口零预算拒绝被删，只剩轮询侧中断，
     亚粒度主体跑到裁决 → 零预算测试在平凡主体断言处失败；
  5. `cross-engine-check-removed` —— 跨引擎 exec 不再拒绝 → lifecycle
     跨引擎测试失败。
- doctest 不变量：engine.rs 的 `compile_fail,E0277` doctest 只在本特性门下
  运行（CI 无网装不了 rquickjs），夹具在第一个变异前自己跑一遍
  `--doc`，红即拒跑（退出 2，日志留在 OUT/doctests.log）。

运行（调用方需设 `CARGO_TARGET_DIR`，PATH 含 cargo 槽位门的 bin）：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/regex-proto-budget/negative_controls.py OUT
```

每个变异先过 `cargo test --no-run` 编译关，再跑绑定测试；全部变异跑完才
输出 summary.json；任一非 KILLED 以非零退出。被杀断言全部带 `#1265 …`
专属消息。证据目录 OUT 必须不存在（脚本自建，已存在即「未启动」拒绝）。

退出码四分表：

- `0` —— 至少一行且全部 KILLED；
- `1` —— 有 SURVIVED / INVALID 行（或零行）；
- `2` —— 未启动的拒绝：未知/重复变异名（共享 `select`）、名册与
  `EXPECTED_NAMES` 静态清单不符、`CARGO_TARGET_DIR` 缺失或不可用、
  OUT 已存在、doctest 基线红（此条发生在认领 OUT 之后，日志留在
  OUT/doctests.log）；
- 其他 —— 脚本自身异常。
