# #1233 dispose 冲刷与重启恢复金样

基线为 PilotDeck `cc7c4e6`，Node v22.22.0 实测三遍（`persist-restart-run1/2/3.out`）。
九个 case 覆盖：终态任务重启恢复（含输出）、崩溃 running 任务以 unknown 恢复、
spill 写失败后重启、dispose 冲刷拒绝（`flush` 返回 `#1233 flush refused`）、
dispose 冲刷同步 throw、重启长度不匹配（`has 7 bytes; expected 6`）、重启输出缺失、
settle 正常、settle 写失败（completionEvents 空、diagnostics `#1233 settle write refused`）。
Rust 侧对应 `tests/background_persist.rs`，`background_persist_restart_regenerates_identically`
守卫比较整个 oracle 字节。settle 写失败仍通知、hasDurableOutput 只认实际落盘字节、
start 二次持久化均为**已登记未批准**差异，见
`specs/gateway-parity/diffs/A6.5-BACKGROUND-PERSIST-001-R2.md`。

## 负控

须先提交本单文件，源文件和执行器均由 HEAD 守卫验证。调用方必须提供自己的
`CARGO_TARGET_DIR`；不接受替代 baseline 或私建仓库。

```sh
export CARGO_TARGET_DIR=<协调者专用构建目录>
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 -I src/pilotdeck-gateway/tests/fixtures/background-persist-restart/negative_controls.py --check
python3 -I src/pilotdeck-gateway/tests/fixtures/background-persist-restart/negative_controls.py --compile-only <新证据目录>
python3 -I src/pilotdeck-gateway/tests/fixtures/background-persist-restart/negative_controls.py <另一新证据目录>
```

10 个变异分别覆盖：冲刷错误被吞掉、失败后仍进 Disposed、拒绝未 memoize、resources
dispose 隐藏冲刷拒绝、损坏输出变软恢复、不可恢复任务被跳过、恢复退回信任 persisted
标志、append 不再持久化、终态任务一律改写 unknown、以及 output.rs 中
`has_durable_output` 直接返回 true。每项先编译，再以准确测试名和 `#1233` 消息判杀，
再恢复并跑正控；锚点过期、编译失败、错误 panic、超时或恢复失败均为 INVALID。始终汇总
所有选中项，任一非 KILLED 即非零退出。`--compile-only` 只证明编译，不能代替判杀。

本 worker 的源码未提交，实际变异被 HEAD 守卫拒绝；变异的编译和判杀由协调者提交后
完成。原始证据与验收缺口见本树 `_tmp/20261026-issue-1233/RESULT.md`。
