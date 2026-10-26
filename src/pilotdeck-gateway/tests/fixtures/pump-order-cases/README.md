# #855 pump 顺序用例的负控

无 TS 金样：两条不变量是 Rust 侧结构保证（`live.rs` 的 Delivery 发送链与
`next()` 的 preflight 门禁），不是对拍行为，因此没有 `generate.mts`/`oracle.json`。

`negative_controls.py <out-dir> <name>` 在提交后的干净产品树上逐个施加变异：

- `gate-order`：把 #832 持久化门禁（`execute_wait`）、client select 与 `handoff()`
  整体搬进 `with_pump` 包装的 future——泵先于门禁武装。三个门禁场景**各自**转红：
  `gates_pump_reject_never_starts_the_pump` 与 `gates_pump_cancel_never_starts_the_pump`
  的 with_pump 进入计数（`pump_starts_for_test`，进入即武装 500 ms 计时器）非零；
  `gates_pump_open_pins_the_start_order` 同样在门禁持有窗口计数非零，且
  「门禁开放后单次 poll 不得 tick」探针 `drain_calls` 1 != 0。
- `wake-recheck`：删除 live.rs `:909` 唤醒后 `stopped` 复查。由
  `host_pump_final_orders_behind_the_reserved_slot` 的模块 deadline 探针转红
  （`probe_batch` 未按期发布，`expect("#855: the probe batch must publish …")`）。
- `final-overtakes`：`event()` 的 final 分支 `reserve()` 后清空 `previous`，让
  final 跳过预留槽前驱 K 直接发送。由同一测试新增的「消费 b 后、驱动泵前再轮询
  final_call」探针转红（`"#855: the final must stay parked …"`）。

每个变异体先跑 positive（绿，且点名测试数恰为判杀数）→ mutant（必须
`test result: FAILED`，且每个被点名测试自己的 panic 块里含其 #855 专属标识；
编译失败、门禁/清理超时等其他 panic 一律判未杀）→ restored（绿）。锚点字符串在
`live.rs` 中各恰出现一次，过期即拒绝。
