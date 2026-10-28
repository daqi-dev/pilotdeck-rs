# #978 后台持久化测试的进展等待

`tests/background_persist.rs` 保持三条持久化回归的原范围：dispose 前读取 spill、已完成快照恢复、主动取消后的快照恢复。原有 2 s / 1 s 完成估计改为 `runtime.wait` 的 done 通知；100 ms 启动估计删除，直接使用 `start` 已确认的 Running/PID。新增的完成等待和写盘等待各最多 60 s，超时是失败，不重试整个测试。

spill 的 done 不代表异步写链已落盘。测试另外观察实际文件长度达到 2,000,000 字节，再独立断言准确长度，并保持原先的切片内容、长度和 truncated 断言；10 ms 仅是读取文件进展的轮询间隔，时间流逝不能使测试成功。

生命周期矩阵复用 `../background-create-gate/oracle.json` 中 release/stop/dispose 三格及 never-reader/cancelled-reader，Rust 覆盖在 `background_tasks.rs::creation_gate_lifecycle`。本单未新增 TS 金样，也不声称 task.wait 会排空后代进程输出。测试设施的 60 s 上限不属于 TS 业务行为。

## 负控

须先提交本单文件，源文件和执行器均由 HEAD 守卫验证。调用方必须提供自己的 `CARGO_TARGET_DIR`；不接受替代 baseline 或私建仓库。

```sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-978/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 -I src/pilotdeck-gateway/tests/fixtures/background-persist-progress/negative_controls.py --check
python3 -I src/pilotdeck-gateway/tests/fixtures/background-persist-progress/negative_controls.py --compile-only _tmp/20261026-issue-978/host-negative-compile
python3 -I src/pilotdeck-gateway/tests/fixtures/background-persist-progress/negative_controls.py _tmp/20261026-issue-978/host-negative
```

12 个变异分别覆盖完成通知缺失、两个等待结果错误、落盘停滞、文件超长、两个生产者失败、启动 PID 丢失、输出计数损坏、磁盘读取遗漏、完成状态损坏和恢复 PID 丢失。每项先编译，再以准确测试名和 `#978` 消息判杀，再恢复并跑正控；锚点过期、编译失败、错误 panic、超时或恢复失败均为 INVALID。始终汇总所有选中项，任一非 KILLED 即非零退出。`--compile-only` 只证明编译，不能代替判杀。

本 worker 的源码未提交，实际变异被 HEAD 守卫拒绝；变异的编译和判杀由协调者提交后完成。原始证据与验收缺口见本树 `_tmp/20261026-issue-978/RESULT.md`。
