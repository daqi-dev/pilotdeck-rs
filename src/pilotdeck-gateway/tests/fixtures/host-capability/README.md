# Capability / permission TS 金样

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器直接 import 原始 `createSidecarDefaultModuleDispatcher`，注入假工具、权限、上下文、plan/todo 和观察者端口。基线仅只读使用，不复制实现；运行前核 commit、dispatcher 无修改和源码 SHA-256。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/host-capability/generate.mts src/pilotdeck-gateway/tests/fixtures/host-capability
cd src
cargo test --locked --offline -p pilotdeck-gateway --test host_capability
```

`oracle.json` 的 120 条场景保存输入、假端口配置、真实 dispatcher 响应以及端口调用顺序。构造 dispatcher 的 manifest 枚举从 trace 中排除；其余 trace 同时核对 refresh/list、context、execute、observer、permission catalog 与 decision。响应为 handler 的返回值或错误消息；关联 ID 和标准错误投影继续由已有 `host-modules` 金样验证。

覆盖 list 刷新后的目录、空目录、descriptor 额外字段过滤/省略/null/schema/三个谓词异常和缺省/runtime capabilities 空数组；execute / batch 自有字段的缺省/null/空串/错型/空白/孤立 UTF-16；arguments 只对 nullish 补 `{}`；空 batch、短/长结果、单次多结果仅取首项；异步端口/观察者错误；plan/todo 原 call 转交；permission 未知工具、首个同名工具、完整可信定义、输入缺省/null 和 context/ID 的错误优先级。

Rust 回放通过真正的 `ModuleRouter` 注入假 trait，逐值比较全部响应与 trace。另有 current-thread 挂起 observer 测试，证明等待观察者时其他回调仍能前进；opaque runtime context 测试证明通知句柄及共享状态的对象身份跨 capability / permission 保留。真实 checkpoint/工具运行服务留给 A6.5-pr2；manifest 合成留给 A6.9。

端口约束：`ToolExecution` 返回对象结果列表，`ToolDefinition` 的 runtime capabilities 为可选字符串数组；不模拟违反 TS 端口类型的任意 primitive 结果或不可迭代 capabilities。descriptor 谓词用 `Result` 表示业务失败，纯计算且不得阻塞；所有可能做 I/O 的端口返回 `HostFuture`，实现者必须及时返回 future。
