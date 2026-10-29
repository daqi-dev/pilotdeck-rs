# #1022 fix-r2：dispose 错误与重复调用

固定 cc7c4e6、Node v22.22.0，直接运行真实 AgentHandle。四格：正常完成、同步 throw、异步 reject、首次 Promise 不等待再重复 dispose。`onDispose` 使用显式门闩，5 秒墙钟 watchdog 只识别装置挂起；输出目录递归创建，oracle 独占创建。

`oracle.json` 记录结果和源闭包哈希；Rust `disposal_retry_regenerates_identically` ignored 守卫复算。Rust 的取消/panic/writer budget 不存在 TS 对应机制，差异见 `specs/gateway-parity/diffs/A7.2-SESSION-DRAIN-FILE-STATE-001-R2.md` 与 `A7.2-SESSION-WRITER-FENCE-001-R3.md`。负控在相邻 `session-factory/negative_controls.py`，`r2-disposal-fixture-regeneration` 与 `r2-disposal-probe-error` 分别验证金样和真实 TS 错误探针不能静默变更。
