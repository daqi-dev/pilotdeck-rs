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

修复轮 1 扩展：workspace 接纳与 SDK 写入顺序、cwd setter、pre_user / post_agent 快照、timeoutMs 与 operationDeadline。TS timeout 用注入到真实模块词法作用域的受控 setTimeout/clearTimeout；Rust 用 Tokio 暂停时钟显式 advance，无墙钟等待。快照 recorder 在此夹具是受控宿主 port，生产实现 `NativeSubmitSnapshots` 调用已移植的 ContentAddressedWorkspaceSnapshotRecorder（spawn_blocking），不是空实现。snapshot recorder、cwd 服务与 FailureGuard 配置均须由 SubmitHost 明确提供；snapshot/cwd 配置要求但能力缺失时返回 CAPABILITY_UNAVAILABLE。FailureGuard 通过 `failure_guard_config` 取 refresh 后的规范化配置，启用时每轮创建真实计数器。

真实 TS 的 timeout 有两个出口：无 registry 时超时 status/error 直接可见；有异步 registry 时先发起 append 再关闭 live queue，因此后完成的超时事件仅在 registry/replay 中可见。两者分别入金样；不把“落盘后才可见”误写成“timeout 必然在 live 流可见”。`close_live` 保留此 TS 顺序。

消费者契约：持有 TurnSubmission 时须持续 drain 到 End，或者取消时 drop；原生 LiveTurn 在 abort 后须自行排空终态和 SessionEnd，独立 wall-clock timeout 才是异常 provider 的逃生路径。前者由 pr5/pr6 适配器验收，后者由 pr6 验收。此夹具不保证尚未装配的生产 host/WS 行为。

negative.py 为每个变异核验特征断言文本；缺 PDRS_BASELINE 时先拒绝再取锁，不能留下锁。剩余 snapshot/cwd 能力拒绝是明确记录的 Rust 能力边界检查。FailureGuard 已改为真实 TS 对拍，不再列作能力拒绝。

A7.2-pr4b：增加 14 个 FailureGuard submit 场景，总计 70 个场景、206 个步骤快照。生成器加载真实 FailureGuard/classify、tool result contentToText 和 artifact store；工具文本均小于持久化阈值，不产生预览文件。覆盖未 trip、禁用、模型/工具/子代理 trip、写状态失败仍停止、abort 抛错、跨轮 reset、refresh 启停、trip 后快照、取消与 timeout 清理。每次状态落盘调用和 abort 原因均记录，最终重新打开 FileRunRegistry 比较事件与状态；trip 之后由受控 session 响应真实 abort 发出 aborted terminal/SessionEnd。TS 的 recordGatewayStatusMessage 吞持久化异常，所以写失败仍有停止事件。

纯移植及接线负控在 `../failure-guard/negative.py`；本目录 `negative.py` 保留现有 submit 负控并把 FailureGuard 能力拒绝负控换为不观察模型错误的真实金样负控。`turn-gateway/negative-fence.py` 的 submit admission 锚点和剩余 capability 筛选契约不变。

启用/配置 guard 的全部场景还保存每一步完整可见事件的原始 `JSON.stringify` 字符串（guardWire），Rust 在转入 serde 桥接前以协议 stringify 逐字节比较，包含对象属性顺序；状态记录调用也有独立原始 wire 字符串。另有只改变停止事件属性顺序的负控。Rust host port 在 refresh 真正执行后才切换测试配置，读取快照提前到 refresh 之前的负控必须失败。
