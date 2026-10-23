# Host model module TS fixtures

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`oracle.json` 保存三份源码的 SHA-256、43 个顺序场景、134 次 model 调用的真实 TS 响应，以及每次调用实际进入假 model port 的参数；其中 124 次经过 `SidecarTurnProtocol.moduleResponseFor`，10 次直接调用 dispatcher；另有 9 组 `concurrency` 金样。dispatcher 场景的预期由真实 `createSidecarDefaultModuleDispatcher` 执行产生；`stickyUnstarted` 直接调用真实 composition 的 `bindTurn` execution。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/host-model/generate.mts src/pilotdeck-gateway/tests/fixtures/host-model
cd src
cargo test --locked --offline -p pilotdeck-gateway host::tests::model
```

生成器直接 import 基线的 `createSidecarDefaultModuleDispatcher` 与 `createSidecarModuleComposition`；后者负责 `sidecarHostModulePorts.ts:265-314` 的 sticky `bindTurn` 包装。client 的 `SidecarTurnProtocol` 未导出，因此只在内存追加 export，经基线 tsx 的 esbuild 转为 ESM；相对依赖重定位至原基线文件，不修改、不复制基线实现。协议调用 `moduleResponseFor` 两次，确认首次真实执行、第二次命中回放缓存且响应一致。`execution_context_values` 标记 `dispatcherOnly`：非字符串/空值 ID 无法通过协议的执行身份检查，因此直接实测真实 dispatcher 的适配边界，`response` 记录其返回 payload，不声称这些输入能通过 transport。

假 port 由 `script` 数据驱动：`prepare` 返回 prepared invocation 或带 `code`/`canonical`/`retryable`/`retryAfterMs` 的错误；`stream` 按步产生事件、在创建/下一步/`return()` 处抛错；metadata 只安装脚本中出现的 lookup，并按固定顺序记录调用。`{undefined:true}` 与 `{nonfinite:"NaN"}` 仅用于让 JSON 表达 JS 的 `undefined`/非有限数。

覆盖范围：

- `prepare` 的合法 canonical request、准备字段、覆盖已有 preparation、错误投影；`materialize_prepared_request` 的 fallback、未知 preparation、非法 candidate。
- fallback 的 prepared request 为字符串、数组、null、数字；字符串含 emoji 和孤立代理项，按 UTF-16 码元展开，数组只带下标键、不带不可枚举的 `length`。展开后再次执行 canonical request 校验。
- execution context 原值透传 `runId`/`operationId`，`idempotencyKey` 按 JS 真值保留或省略；覆盖数字、null、布尔、数组、对象、空串，核对 prepare/stream/stream_next 的实际 port 入参。
- `stream` 的完整 drain、request 覆盖、流内错误、错误后 preparation 仍保留；`stream_next` 的多步、done 清理、next 错误清理、创建错误不清理、缺少 request 的 canonical 错误、未知 preparation。
- `close_stream` 的提前关闭、未知流、`return()` 报错后两表已清；`dispose` 对多个打开流的 `return()` 清理。
- `get_metadata` 的完整/部分/空/缺失 metadata、安全整数边界、小数/零/负数/非有限/字符串/null、tokenLimits 字段（可选字段显式 undefined 与缺省一致）、非法 protocol、lookup 抛错、额外字段原样回传。
- prepare 后的 metadata 分发（修复轮 6）：dispatcher 把 raw prepared 路由原样交给每个已定义 lookup（`sidecarDefaultModuleDispatcher.ts:88,445-450`）再校验返回值（`:451-470`），不做预转换、不因路由键转换抛错而跳过。raw 路由 `{toString:null}` / 数字原样进 lookup 参数日志；非法返回值（0）在五个 lookup 全部调用后才拒绝（`Host model metadata returned an invalid maxContextTokens.`）；合法返回值照常成功、`metadata.provider` 是 raw 对象；prepare 失败后 preparation 已存（`:84`），fallback materialize 再因非字符串路由被 canonical request 校验拒绝（`:443`）。
- sticky 路由：prepare context 注入 `previousTier`/`previousProvider`/`previousModel`，首个流完成、出错或建流失败后清 tier，`orchestrating` 保留，空 sticky 注入空 metadata，空串不覆盖远端 metadata，sticky 返回 null 不注入。
- 并发金样：dispose 同时启动所有关闭及单流关闭失败隔离；同流已受理 next 的 EOF；next/close 与未启动 sticky 流的关闭归属；建流期间 next→close、双 next→close（含关闭报错）；跨流 next/close 互不阻塞。Rust 侧 `next`/`return` 串行化，比较已受理响应、事件顺序与关闭归属，不比较 JS 调度时刻。建流等待由真实 async generator 的首次 next 内 gate 表达，`return()` 排在已受理 next 后；金样没有按 Rust 实现改写预期。
- operation 字段边界与 unsupported 文本；所有 model 操作自带的 `preparationId`、`request`、`provider`、`model` 必填字段都由金样携带（#265 第 2 条）。Rust port trait 明确要求协作式异步、不得同步阻塞（#265 第 3 条）。

Rust 默认测试回放整个金样，并额外断言 dispose 后同一 preparation 的 `stream_next` 报未知 preparation。真实 provider、路由 materialize 与 token 预算不在本 PR 范围。
