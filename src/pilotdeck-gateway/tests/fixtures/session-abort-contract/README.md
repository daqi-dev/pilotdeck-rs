# dispose / lease 释放窗口的 abort 契约

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`generate.mts` 经 tsx 运行，使用已安装的 esbuild 在内存中转译完整、未改写的
AgentHandle 和 ProjectRuntimeRegistry；无关 import 访问即报错。
真实执行 handle 的 submit/dispose/abort，以及 registry 的 lease 获取、retire、release
与资源 dispose 方法；绕过无关 provider 构造，只注入 fake session 和 fake runtime
资源。通过 AgentHandle 公开的 onDispose 回调将 session.dispose 和 lease release
依次串接，对应 Rust handle 的两步清理，不宣称构造了完整生产 AgentSession。

idle 和 active 两种情况均用 Promise 闸门停在 session.dispose 内、session.dispose
返回而 lease release 尚未返回两个位置；每个窗口重复同一 abort 两次，再测 handle
完全 disposed 后的 abort。金样包括每步状态、completion、调用次数与完整调用顺序。
没有 sleep、定时器、网络、UUID 或时间归一化。
生成器前后核基线 HEAD、tracked 状态和两个源文件的提交版/工作版 SHA-256。
Rust 回放先调用 `crate::test_baseline::oracle`，设置 PDRS_BASELINE 时核真实源码。

从仓库根生成两次（不写基线或 node_modules）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
export TMPDIR=/tmp TSX_DISABLE_CACHE=1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-abort-contract/generate.mts /tmp/abort-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-abort-contract/generate.mts /tmp/abort-second
cmp /tmp/abort-first/oracle.json /tmp/abort-second/oracle.json
```

Rust 测试使用真实 ProjectRuntimeRegistry/RuntimeLease，通过 retired runtime 的最后
一个 lease 触发带闸门的 RuntimeResources.dispose；Semaphore 通知进入精确窗口，
全场景 3 秒有界，不依赖线程调度或 sleep。每个检查点实际 poll handle 的共享清理
future，验证 lease 未释放时 completion 仍 pending；active 情况额外核 disposer
自身的 abort。普通回放命令（从 src/，先运行任务指定 cargo-slots.sh）：

```sh
cargo test --offline -p pilotdeck-gateway --lib \
  runtime::session::handle::abort_contract_tests:: -- --nocapture
```

`negative.py <evidence-directory>` 需要 CARGO_HOME、PDRS_BASELINE。它在独占 /tmp
源码副本和独立 CARGO_TARGET_DIR 内先跑原实现，再编译三项产品变异：Draining 时
不转发 abort、session.dispose 返回后提前标 Disposed、Disposed 后仍转发 abort。
每项必须成功编译、目标测试实际运行并在金样 trace 断言失败；编译错误、外部超时
或无测试执行不能算抓住。原始输出、具体替换表达式与退出码保存在证据目录；结束
恢复并清理副本，不修改真实检出树、基线、冻结判据或 state。
