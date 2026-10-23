# Host event.emit TS fixtures

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`oracle.json` 保存三份源码的 SHA-256、轮身份、46 个场景与 49 次 `event` 调用的真实 TS 响应，以及每次成功调用交给 sink、再经真实 `createAgentEventBuffer().drain()` 取回的事件。生成器直接 import 基线 `createSidecarDefaultModuleDispatcher` 和 `src/agent/protocol/events.ts`，注入真实事件缓冲区（或按场景声明抛错的 event port）作为假 event port；不修改、不复制基线实现。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/host-event/generate.mts [outdir]
cd src
cargo test --locked --offline -p pilotdeck-gateway host::tests::event
```

生成器断言基线 HEAD 等于固定 commit，且 `git status --porcelain --untracked-files=no` 为空（不检查未跟踪文件）；每个 `sourceSha256` 取 `git show <commit>:<path>` 的 blob 哈希并与工作树内容核对，随后写成临时文件再 rename，连续跑两次字节一致。省略 `outdir` 时就地写入夹具目录；Rust 的 `#[ignore]` 测试会把生成器重跑到每次运行独有的临时目录再逐字节比较。

覆盖范围：

- identity 通过：只有 `sessionId` 的会话级事件；`turnId` 匹配；空字符串 `type`；孤立代理项 `type`；额外字段、嵌套对象/数组、Unicode 与孤立代理项原样到达 sink。
- identity 拒绝：`sessionId` 缺省/空串/null/数字/不匹配/孤立代理项；`turnId` 不匹配/null/数字/数组/孤立代理项，以及 **键存在但值为 `undefined`**（真实 TS 的 `"turnId" in event` 语义，缺键才放行）；`type` 缺省/`undefined`/null/数字；`event` 缺省/`undefined`/null/数组/字符串/数字/布尔。
- 错误文本：identity 拒绝统一为 `Sidecar host event does not match the active turn.`；非空字符串但不是 `emit` 的 operation（未知名、空白、孤立代理项、`Emit`）为 `Unsupported sidecar event operation.`；operation 缺失/`undefined`/空串/null/数字走 `stringField`，为字段助手文本 `Sidecar payload field operation must be a non-empty string.`（金样 `operation-empty`/`operation-null`/`operation-number`/`operation-undefined`）；响应经 `SidecarTurnProtocol.moduleResponseFor` 投影为 `HOST_MODULE_FAILED`。
- emitter 抛错：`sink-throws-error`（`Error("sink failed")`）、`sink-throws-error-code`（同一 Error 带 `code: "SINK_CODE"`）、`sink-throws-non-error`（抛字符串）三个场景走真实 `moduleResponse` 的异常投影：无 code 或非 Error 时响应 code 为 `HOST_MODULE_FAILED`、message 为 `String(error)`；字符串 `code` 原样投影。Rust 侧对应的 sink 用 `ModuleError::from_js_fields` 重建同样的 message/fields。
- drain：`ordering` 连续 3 次成功 emit 后一次 drain 按发出顺序返回 3 条，随后 drain 为空；`failed-emit-does-not-buffer` 证明 identity 拒绝的事件不进入缓冲区；抛错 sink 的 `drained`/`drainAfter` 均为空。每一步的 `drained` 与场景末尾的 `drainAfter` 都由真实缓冲区产生。
- 非对象 payload：直接调 dispatcher 时数组、字符串、数字统一报 `operation` 字段错误（金样）。经 wire 时 null/数组/字符串/数字 payload 都到不了 module：TS 协议校验 `agent/modules/protocol.ts:288-292`（`isRecord(payload)`，否则 `MISSING_MODULE_CALL_FIELD`「Module call field payload is required.」）与 Rust `pilotdeck-protocol/src/module.rs:250-251` 同文同码在帧解析时拒绝。因此 `payload: null` 不纳入金样；Rust module 边界对它给字段助手文本只服务直接调用，无 wire 可达的 TS 对应物。
- `{__pdrsUndefined:true}` 是唯一标记，仅用于让 JSON 表达 JS 的 `undefined`；Rust 回放把该标记还原为 `JsValue::Undefined`，因此“键缺省”和“键存在但值为 undefined”仍是两个不同输入。

Rust 回放直接构造 `ModuleRouter` 中同一类型的 `event::Handler`，注入假 `EventSink`，逐值比较完整响应与 drain 结果；抛错场景的 sink 用 `ModuleError::from_js_fields` 重建异常，验证 TS 的 `code`/message 投影与 Rust 一致。另本地覆盖 router 路由、sink 抛普通 `HostError` 时的原样拒绝、未安装 sink（未安装 event 模块）时所有 `emit` 调用在解析 payload 之前即返回 TS 未注册模块错误 `Unsupported sidecar host module: event`（对齐 agentLoopSidecarClient.ts:1155-1158 的 handler 查找顺序，#583 起与金样同源，不伪造）。client 循环如何调用 drain、Gateway 事件协调器如何把事件发布到 sink 属于后续接线，不在本 PR。顺序契约（#289）：宿主驱动按到达顺序构造每个 module_call 回调，`event::Handler::call` 在构造时同步校验并 `emit`，sink 因此按 sidecar 发送序逐条收到事件（`host::tests::event::concurrent_emits_reach_the_sink_in_arrival_order`，16 路并发 stdio，负控：把 emit 挪回回调任务内即乱序）。
