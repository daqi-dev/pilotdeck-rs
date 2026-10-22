# dispose / lease 释放窗口的 abort 契约

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`generate.mts` 经 tsx 运行，使用已安装的 esbuild 在内存中转译完整、未改写的
AgentHandle、createAgentSession、ProjectRuntimeRegistry、ProjectSessionFactory、
GatewaySessionResourceLeaseBundle 与 SessionMcpRuntimeRegistry；无关 import 访问即报错。
清理顺序不是手写的：handle 由真实 `createAgentSessionWithStorage` 构造，其
`onDispose` 就是生产闭包 `disposeAgentSessionLifecycle`（先 `configuredDisposer`
再 `runtimeResources.dispose()`，createAgentSession.ts:223-241）；`configuredDisposer`
来自真实 `ProjectSessionFactory.composeDisposer`（ProjectSessionFactory.ts:306-324，
其中 `prepared.resources.release()` 即 lease bundle），bundle 由真实
`GatewaySessionResourceLeaseBundle` 以 `add("project runtime lease", …)` 组成
（ProjectSessionRuntimeBundle.ts:217），release 走真实 ProjectRuntimeRegistry 的
lease 获取、retire、release 与 retired runtime 的 `resourcesBundle.dispose`。
只绕过无关 provider 构造（TurnRunner、AgentLoop 等以空 stub 注入），fake session
只提供 turn/abort 面——生产网关组合本就不调用 `AgentSession.dispose`（那是
`asAgentHandle` 裸会话路径的并行行为，AgentHandle.ts:411-433），session 自有
资源由 `runtimeResources.dispose()` 这一步释放。

idle 和 active 两种情况均用 Promise 闸门停在 lease release 内、lease release
返回而 session 资源 dispose 尚未返回两个位置；每个窗口重复同一 abort 两次，再测
handle 完全 disposed 后的 abort。金样包括每步状态、completion、调用次数与完整调用
顺序。没有 sleep、定时器、网络、UUID 或时间归一化。
生成器前后核基线 HEAD、tracked 状态和五个源文件的提交版/工作版 SHA-256。
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

已提交的 oracle.json 与生成器输出的逐字节一致性由
`runtime::session::handle::abort_contract_tests::real_ts_abort_contract_regenerates_identically`
守卫（`#[ignore]`，需 PDRS_BASELINE，比照 session-owner 的同名测试）。

Rust 测试使用真实 ProjectRuntimeRegistry/RuntimeLease，通过 retired runtime 的最后
一个 lease 触发带闸门的 RuntimeResources.dispose；Semaphore 通知进入精确窗口，
全场景 3 秒有界，不依赖线程调度或 sleep。每个检查点实际 poll handle 的共享清理
future，验证 session 资源释放未完成时 completion 仍 pending；active 情况额外核
disposer 自身的 abort。普通回放命令（从 src/，先运行任务指定 cargo-slots.sh）：

```sh
cargo test --offline -p pilotdeck-gateway --lib \
  runtime::session::handle::abort_contract_tests:: -- --nocapture
```

`negative.py <evidence-directory>` 需要 CARGO_HOME、PDRS_BASELINE。它在独占 /tmp
源码副本和独立 CARGO_TARGET_DIR 内先跑原实现，再编译四项产品变异：Draining 时
不转发 abort、lease release 前提前标 Disposed、Disposed 后仍转发 abort、把清理
顺序改回 session dispose 先于 lease release 的旧顺序；外加把 oracle 的
sourceSha256（AgentHandle.ts）置 `0`*64 的溯源变异，要求 `test_baseline::check`
以 `oracle hash for … does not match` 失败。每项必须成功编译、目标测试实际运行
并在金样 trace、测试自带的 3 秒有界（reverted-cleanup-order：旧顺序下 lease 窗
口永不打开，被回放自身的有界等待抓住，非外部超时）或溯源断言失败；编译错误、
外部超时或无测试执行不能算抓住。原始输出、具体替换表达式与退出码保存在证据
目录；结束恢复并清理副本，不修改真实检出树、基线、冻结判据或 state。
