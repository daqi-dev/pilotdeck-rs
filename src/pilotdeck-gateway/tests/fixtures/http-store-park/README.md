# #1395 零容量 park 的可达性守卫

生产行为保持原样。新增两条默认测试使用真实 HTTP store 入口，钉住 hyper
每次提供至少 8192 字节读容量、不会进入零容量 park，以及请求体确实停在
`Request + inner Pending + 无连接 deadline` 后的关停/客户端取消可结束。

另有一条 ignored 诊断测试 `issue_1395_synthetic_zero_room_shutdown_window`，
直接驱动私有 `TimeoutStream`，用真实 duplex 字节构造「ahead 满」和
「ahead + peer EOF」两种状态。按目标 Arc 过滤的一次性钩子暂停在 break 后、
store_waker 前，另一任务调用 begin_shutdown；握手最多 5 秒，任务全部 join。
它记录当前合成调用的丢 wake，不把该旧行为设成产品必须保持的契约。

生产不可达依据：hyper 1.7.0 的 h1 `poll_read_from_io` 调用底层前
`reserve(next)`，Adaptive 下限 8192；实际 begin_shutdown 和 connection poll
同处一个任务的 select；服务端收尾另有 request_timeout + 1 秒的总期限。
以后更换 IO 驱动、依赖或关闭时序时，必须重评这些前提。

```sh
export CARGO_TARGET_DIR=/absolute/path/to/this-tree/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
export CARGO_PROFILE_TEST_DEBUG=line-tables-only
scripts/cargo-slots.sh
cd src
cargo test -p pilotdeck-gateway --lib issue_1395_ -- --include-ignored --nocapture
```

在源码已提交且工作树符合 HEAD 后，回树根执行：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/http-store-park/negative_controls.py NEW_OUTPUT_DIR
```

负控包含 A/B/C（钩子命中、动作执行、shutdown 状态）、目标过滤、真实零容量
输入、真实 Pending 观察、两层超时失效和 park deadline 不 poll。逐项先正控、
应用变异、no-run 编译、绑定断言判杀、恢复正控，全部项执行完才汇总。
锚点失配/编译错误/HEAD 不一致均为 INVALID、非零退出，绝不算 KILLED。
新文件未提交时由 HEAD 守卫拒跑是预期行为，不能通过副本仓库或更换 baseline
绕过；提交后由协调者运行并检查每项 `compiled=true` 和 `KILLED`。

本夹具没有 TS 金样。Rust 私有 poll/waker 格在 TS 中无对应概念；调查用的
Node v22.22.0 / ICU 78.2 实际 TS 服务探针及原始输出随任务证据交付。
