# Live session owner 的真实 TS 夹具

基线为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`generate.mts` 执行完整、未改写的 SessionRouter、AgentFactoryProvider、AgentRegistry、
AgentHandle 和 ScopedServiceRegistry。只注入 fake AgentSession 的 snapshot、abort、
dispose、compact，以及可控制的 factory/setup；不模拟 Router 的接纳、发布或停止判断。
另执行真实 InProcessGateway.abortTurn / GatewayTurnCompletionFence 与
ProjectRuntimeRegistry 的 lease/retire/dispose 方法。后者使用 prototype 上的原方法，
仅注入资源 bundle 和 background shutdown，未运行无关的 provider 构造。

完整源文件经基线已安装的 esbuild 在内存中转为 CJS；Router 的 agent barrel 指向真实
scope 模块，未用到的其它 import 通过访问即报错的 Proxy 隔离。所有异步交错使用
Promise 闸门和有界 microtask 检查，无 sleep、网络、模型请求或依赖安装。
时钟受控，idle timer 在生成器中关闭；无 UUID、路径或时间的结果归一化。
17 个场景的返回值、错误消息、handle 状态、发布/排空顺序和资源调用均来自真实执行。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
export TMPDIR=/tmp TSX_DISABLE_CACHE=1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-owner/generate.mts /tmp/owner-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-owner/generate.mts /tmp/owner-second
cmp /tmp/owner-first/oracle.json /tmp/owner-second/oracle.json
```

生成器运行前后均调用共享 `../_shared/baseline.mts`，核固定 HEAD、tracked 状态和
8 个执行源文件的提交版/工作版 SHA-256。Rust 回放统一先走
`crate::test_baseline::oracle`；设置 `PDRS_BASELINE` 时重新核对真实源码，不另写溯源判断。

```sh
cd src
cargo test --offline -p pilotdeck-gateway --lib runtime::session::
```

四个拆分判据分别对应 `dirty_identity_and_publication_matches_ts`、
`slow_factory_is_per_session_matches_ts`、`maintenance_blocks_turn_and_idle_matches_ts`、
`shutdown_during_creation_matches_ts`。其它回放验证 close、idle 严格边界、setup/factory
失败保留旧 writer、工具过滤合并、维护取消、shutdown 并行排空、project pause/resume、
带 runId 的 live abort、abort 抛错不等 completion，以及 fence 尚在但 Router 预约已移除的
精确 owner。项目 lease 回放之外，还验证真实 RuntimeLease 保留到 handle 消费者退出和
session dispose 完毕，retired project runtime 与新 runtime 使用不同 Arc 身份。

多线程回归用两个 OS 线程和屏障将新 owner 替换强制塞入 pr1 判断和真实 live sink
选目标之间，交替覆盖有/无 fence 的前缀；100 轮必须零命中新 writer。
另一回归在 close 等待资源排空时结束旧预约、以相同 runId 建立新预约，再触发旧
completion/close cleanup，验证只释放精确身份。另验取消等待者仍会排空、周期 idle
sweep 会停止，以及发布时旧 handle 的 admission cutoff 无需执行资源回调。

`negative.py <evidence-directory>` 使用独占、结束即清理的 `/tmp` 工作区副本，原样复用
全部 Cargo manifests、依赖和共享溯源守卫；不会修改真实产品文件、基线、node_modules、
冻结件或 state。全量源码副本仅用于编译变异，specs 副本仅满足已有 include_str。
先跑原实现，再注入八项变异：旧 handle 冒充当前 owner、所有 key 共用 factory 队列、
忽略 maintenance、忽略停止后的发布保护、live abort 不复核目标、旧 close 删除新预约、
旧 complete 释放新 owner、篡改 sourceSha256。每项必须成功编译并在对应测试的指定断言
处失败；编译失败、外部超时或非预期失败不计通过。全局等待的负控由测试内有界进度
断言拒绝；abort 负控要求实际观察到 100/100 次命中新 writer。
原始输出及完整替换表达式记录在指定目录。`CARGO_HOME` 和 `PDRS_BASELINE` 必填；
目标目录固定当前树 `src/target`，每次编译前运行任务指定的 cargo-slots 命令。

本包提供真实生命周期组件和可注入的 LiveSession/SessionFactory 接口，不提供默认
空 session。完整 submit/持久接纳、seed/compact 业务执行、MCP/native host adapter
和默认代理切换仍分别属于 A7.2、A7.5/A7.6、后续 provider/domain 装配。
RefreshBundle 通过现有 RefreshRouter trait 直接调用此 owner，未改 project/refresh 实现。
