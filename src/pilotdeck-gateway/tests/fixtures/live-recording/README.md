# Live Recording 所有权矩阵

固定 cc7c4e6：真实 AgentSession / TurnRunner / recorder / SessionRuntime / JsonlPersistence / AgentHandle / AgentFactoryProvider；仅 AgentLoop 为 stub。生成器哈希递归运行依赖，输出 exclusive create。每格用 subscriber Promise gate 挂住 start/accept/complete，真实 factory dispose 和 return 清理均有界。

Rust 12 格在生产 LiveSessionAdapter + blocking TranscriptStore.append 上验证。cancel 对应丢掉 next 借用后重新 next；JS 则取消等待者后等待同一个 Promise。drop 对应 TS 排队 return 的资源不变量，不是析构 API 等价。dispose 在 Rust 组合 LiveSession.dispose 的取消/拒绝入口与 ProjectRuntimeRegistry.dispose 的实际资源回收；不声称已测完整 factory 自动接线。Rust 消费 active/writes/event/replacementStarted；其他金样字段保留 TS 原始观察，Rust 自身独立断言 attempt、pending、租约和 dispose。

金样 complete 的 afterWriteActive=1，而 Rust 终态交付即释放活动槽，属既有行为，见 `specs/gateway-parity/diffs/A7.2-LIVE-TERMINAL-SLOT-001.md`（未批准）。失败注入三例仅验证新 transition 接线不会把 runner 的失败回执当成功；不是失败事件转发或完整失败门禁验收。

重生成：设置 PDRS_BASELINE，运行 generate.mts <新目录>；ignored Rust `live_recording_regenerates_identically` 必须字节一致。负控：调用方先设 CARGO_TARGET_DIR，运行 `python3 .../live-recording/negative.py <新证据目录>`；使用 mutating_sources、run_owned、wait_for_slot，源码须对应 mutating_sources 校验的不可变 Git 基线（默认 HEAD）。所有 I/O gate 显式释放；Node 5s、Rust next/资源回收15s、blocking gate10s 超时只判测试失败，绝不当作产品解锁条件。
