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

负控（#996 起为编译期开关 `schemata.py`，旧的文本替换 `negative.py` 已删）需要
调用方设置 CARGO_TARGET_DIR 与 PDRS_BASELINE（`abort-tampered-source-hash` 只有
真实基线检出才能击杀，缺 PDRS_BASELINE 时预检即拒跑）。它预编译一次 feature 库测试
二进制，逐开关独立进程以 `PDRS_MUTANT` 选择，跑完即弃，全程不改工作区源码；GUARDED
清单记录相关源文件哈希，跑前跑后核对不变。每个开关先跑正控、再在开关下跑唯一绑定
测试、恢复后重跑正控。`--check` 为只读锚点预检。用法（从 src/）：

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" \
PDRS_BASELINE=/absolute/path/to/PilotDeck python3 -I \
  src/pilotdeck-gateway/tests/fixtures/session-abort-contract/schemata.py \
  <新证据目录> [switch ...]
```

五项开关保留原判据：Draining 时不转发 abort、lease release 前提前标 Disposed、
Disposed 后仍转发 abort、session dispose 先于 lease release，以及 oracle 的
AgentHandle.ts sourceSha256 置零（开关名 abort-tampered-source-hash）。行为失败
必须来自绑定完整测试名的专属 panic 消息；编译错误、无关 panic、外部超时或无测试
执行均记 INVALID。所有所选开关跑完才汇总 KILLED / SURVIVED / INVALID；任一非
KILLED 即非零退出。证据目录独占创建，最终 summary.json 在源码哈希核验后原子发布。
