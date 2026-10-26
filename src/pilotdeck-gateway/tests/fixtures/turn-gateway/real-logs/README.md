# #813 fix-r1 真实宿主日志

这四份是宿主在 `claude/issue-813-negctl-scope`（HEAD c6aba3fe8）上跑
`turn-gateway/negative.py` 的原始日志，用于回归 `negative_panic.py` 的击杀判定：

- `emit-bypasses-serial-host.log` / `emit-deferred-after-return.log`：被点名测试
  `concurrent_external_emits_are_serialized_on_the_serial_host` 自身 `FAILED`，
  断言 panic 在它 `std::thread::spawn` 出来的 `<unnamed>` 线程里；被点名线程块
  只有 `caller thread: Any { .. }`。修复前判 INVALID。
- `expiry-runs-on-clock-thread.log` / `expiry-callback-dropped.log`：被点名测试
  `serial_timer_cancels_queued_callbacks_and_replacements` 自身 `FAILED`，panic
  在被点名线程里；修复前因为锚点还沿用 replay 测试的 `serial timer:` 而判
  INVALID。

`test_negative_panic.py` 用 `turn-gateway/negative.py` 里登记的 marker 重放这四份
日志，要求全部判 KILLED。日志保持宿主原字节，未做清洗。
