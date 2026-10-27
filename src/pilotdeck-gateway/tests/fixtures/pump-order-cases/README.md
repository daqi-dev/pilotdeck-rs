# #855 / #741i / #741k pump 顺序用例与真实 TS 前缀

`generate.mts` 用 Node v22.22.0 执行 cc7c4e6 的真实 session、TurnRunner 和
stdio sidecar，只给 start write 注入 gate。`oracle.json` 覆盖拒绝、持有后取消、
持有后开放三格；每次生成还输出完整事件和 wire 原始记录。Rust 精简 peer 的
成功投影为 `turn_started → input_accepted → user_prompt_submitted → model_request_started → turn_completed`，
逐事件读取和断言，不跳过未知事件；重生成守卫为 `pump_order_regenerates_identically`。

TS 在 gate 持有时不连接 sidecar，取消后也要等前驱释放；释放后仍交付
`turn_started → input_accepted → user_prompt_submitted`，但无终态。
Rust 的取消终态仍按 **未批准** 差异 `specs/gateway-parity/diffs/A7.2-TURN-ABORT-001.md`
单独断言。金样投影只比较双方共有事件，不宣称取消终态对齐。Delivery 发送链和
泵计数仍是 Rust 结构保证；TS 金样不证明 Rust 内部排程细节。

`negative_controls.py <out-dir> [name ...]` 在提交后的干净产品树上施加变异，
默认跑全部并汇总；`--sample` 只跑 `model-prefix-lost`；`--compile-only` 对选定的
每种变异先做独立 `cargo test --no-run`，恢复后再处理下一项。调用方必须传入
`CARGO_TARGET_DIR`，各输出目录须不存在。长负控由协调者在宿主运行。

- `gate-order`：把 #832 持久化门禁（`execute_wait`）、client select 与 `handoff()`
  整体搬进 `with_pump` 包装的 future——泵先于门禁武装。三个门禁场景**各自**转红：
  `gates_pump_reject_never_starts_the_pump` 与 `gates_pump_cancel_never_starts_the_pump`
  的 with_pump 进入计数（`pump_starts_for_test`，进入即武装 500 ms 计时器）非零；
  `gates_pump_open_pins_the_start_order` 同样在门禁持有窗口计数非零，且
  「门禁开放后单次 poll 不得 tick」探针 `drain_calls` 1 != 0。
- `wake-recheck`：删除 live.rs 唤醒后 `stopped` 复查。由
  `host_pump_final_orders_behind_the_reserved_slot` 的模块 deadline 探针转红
  （`probe_batch` 未按期发布，`expect("#855: the probe batch must publish …")`）。
- `final-overtakes`：`event()` 的 final 分支 `reserve()` 后清空 `previous`，让
  final 跳过预留槽前驱 K 直接发送。由同一测试新增的「消费 b 后、驱动泵前再轮询
  final_call」探针转红（`"#855: the final must stay parked …"`）。
- `model-prefix-lost`：在产品发送路径把 `model_request_started` 改成合法
  `model_event`；开放场景必须在 `#741i [open]: model_request_started must precede turn_completed`
  断言失败，证明修复没有吞掉或跳过模型前缀。

- `submitted-prefix-lost`：在宿主记录交付路径把 `user_prompt_submitted` 改成合法
  `model_event`；取消和开放场景分别在 `#741k [cancel/open]: user_prompt_submitted must follow input_accepted`
  断言失败，证明新宿主前缀既不丢失，也不能被其他事件替代。

运行复用 `negative_suite.py`：每个绑定用完整测试名和 `--exact` 独立执行
positive → mutant → restored；`gate-order` 的三个场景分别判杀。只有点名测试
确实失败且含指定语义断言才是 KILLED，未变红为 SURVIVED，编译错误、错误 panic、
超时及恢复失败为 INVALID。初始化和清理消息不在有效判杀列表中。所有选定项
跑完写 `summary.json`；任何非 KILLED 都令进程非零退出。五种源码锚点各唯一，
任何源码恢复失败或非干净基线均拒绝继续变异。
