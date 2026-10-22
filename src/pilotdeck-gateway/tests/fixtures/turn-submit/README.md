# turn-submit 真实 TS 金样

`generate.mts` 在固定 PilotDeck 基线上执行 `InProcessGateway.submitTurn`、真实 `SessionRouter` / `AgentHandle` / completion fence、真实 `TurnRunner.run` 的输入接纳段及失败段、projector / coordinator / queue / `FileRunRegistry`。不复制产品方法体。宿主策略、transcript writer 与模型循环边界使用显式可控 port；写入与 generator 的事件顺序由真实 TurnRunner 决定。成功轮次在真实输入接纳后由受控模型边界发文本和终态，SessionEnd 由受控 session 生命周期发出。

每一步保存全部可见业务事件、durable 调用、registry 调用、宿主调用、admission 与返回结果；最终重新读取 registry 的事件和状态。`native` 保存真实 session/runner 边界的 AgentEvent，供 Rust 的 LiveSession port 回放；Error 的非枚举 message 显式保存，不丢成 `{}`。Rust 必须逐步与 TS 的输出匹配，不能由 native 自己推导期望。TS 未注入依赖即使被产品 catch 吞掉，生成也必须失败。

生成命令（Node v22；`PDRS_BASELINE` 指向固定 `cc7c4e6` 的干净 checkout）：

```sh
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/turn-submit/generate.mts /tmp/turn-submit-generated
```

生成器调用共享 `assertBaseline` / `baselineSourceHashes`，检查 commit、tracked 状态与逐文件已提交源码哈希。两次输出必须逐字节一致。`real_ts_turn_submit_regenerates_identically` 是默认忽略的真实重生成守卫。

```sh
PDRS_BASELINE=/absolute/PilotDeck \
  cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  runtime::turn::submit::tests::real_ts_turn_submit_regenerates_identically -- --ignored --exact
```

`negative.py <证据目录>` 独占临时修改 submit 产品实现，逐条编译并运行对应真实 TS 金样场景，要求断言失败，再逐字节恢复。编译失败、超时、SIGKILL 不计负控成功；原始 SHA、逐条原始输出、替换内容与汇总写证据目录。可选的后续参数是精确负控名称，便于有界复核。不要与另一构建或实现写入同时运行。

```sh
PDRS_BASELINE=/absolute/PilotDeck \
  python3 src/pilotdeck-gateway/tests/fixtures/turn-submit/negative.py \
  _tmp/20261023-a72-pr4-submit/negative
```

WS 最终包装归 pr5；本夹具比较业务流，不重新分配 registry、WS 或 Module 的 seq。普通业务终态及 WS 最终包装的区分复用 `turn-publish` 已有真实 WS 金样与测试。

修复轮 1 扩展：workspace 接纳与 SDK 写入顺序、cwd setter、pre_user / post_agent 快照、timeoutMs 与 operationDeadline。TS timeout 用注入到真实模块词法作用域的受控 setTimeout/clearTimeout；Rust 用 Tokio 暂停时钟显式 advance，无墙钟等待。快照 recorder 在此夹具是受控宿主 port，生产实现 `NativeSubmitSnapshots` 调用已移植的 ContentAddressedWorkspaceSnapshotRecorder（spawn_blocking），不是空实现。snapshot recorder、cwd 服务与 FailureGuard 配置均须由 SubmitHost 明确提供；配置要求但能力缺失时返回 CAPABILITY_UNAVAILABLE。FailureGuard 运行时尚未移植，启用时拒绝进入 native submit，不能以此宣称 guard 行为已移植。

真实 TS 的 timeout 有两个出口：无 registry 时超时 status/error 直接可见；有异步 registry 时先发起 append 再关闭 live queue，因此后完成的超时事件仅在 registry/replay 中可见。两者分别入金样；不把“落盘后才可见”误写成“timeout 必然在 live 流可见”。`close_live` 保留此 TS 顺序。

消费者契约：持有 TurnSubmission 时须持续 drain 到 End，或者取消时 drop；原生 LiveTurn 在 abort 后须自行排空终态和 SessionEnd，独立 wall-clock timeout 才是异常 provider 的逃生路径。前者由 pr5/pr6 适配器验收，后者由 pr6 验收。此夹具不保证尚未装配的生产 host/WS 行为。

negative.py 为每个变异核验特征断言文本；缺 PDRS_BASELINE 时先拒绝再取锁，不能留下锁。所有新增能力拒绝的断言是明确记录的 Rust 能力边界检查，不伪装成与 TS 相同的 guard 实现。
