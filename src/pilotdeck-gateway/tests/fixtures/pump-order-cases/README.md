# #855 pump 顺序用例的负控

无 TS 金样：两条不变量是 Rust 侧结构保证（`live.rs` 的 Delivery 发送链与
`next()` 的 preflight 门禁），不是对拍行为，因此没有 `generate.mts`/`oracle.json`。

`negative_controls.py <out-dir> <name>` 在提交后的干净产品树上逐个施加变异：

- `gate-order`：把 #832 持久化门禁（`execute_wait`）、client select 与 `handoff()`
  整体搬进 `with_pump` 包装的 future——泵先于门禁武装。由
  `gates_pump_starts_only_after_the_gate_commits` 的「门禁开放后单次 poll 不得 tick」
  探针转红（`drain_calls` 1 != 0）。
- `wake-recheck`：删除 live.rs `:885` 唤醒后 `stopped` 复查。由
  `host_pump_final_orders_behind_the_reserved_slot` 的模块 deadline 探针转红
  （`probe_batch` 未按期发布，`rx.try_recv()` 得到 `Empty`）。

每个变异体先跑 positive（绿）→ mutant（必须 `test result: FAILED` 且 `panicked at`，
编译失败不算）→ restored（绿）。锚点字符串在 `live.rs` 中各恰出现一次，过期即拒绝。
