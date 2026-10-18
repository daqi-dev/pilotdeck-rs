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
- 错误文本：identity 拒绝统一为 `Sidecar host event does not match the active turn.`；非 `emit` operation（含空串、null、数字、空白、孤立代理项、`Emit`）为 `Unsupported sidecar event operation.`；op 字段缺失/错型沿用字段助手文本；响应经 `SidecarTurnProtocol.moduleResponseFor` 投影为 `HOST_MODULE_FAILED`。
- emitter 抛错：`sink-throws-error`（`Error("sink failed")`）、`sink-throws-error-code`（同一 Error 带 `code: "SINK_CODE"`）、`sink-throws-non-error`（抛字符串）三个场景走真实 `moduleResponse` 的异常投影：无 code 或非 Error 时响应 code 为 `HOST_MODULE_FAILED`、message 为 `String(error)`；字符串 `code` 原样投影。Rust 侧对应的 sink 用 `ModuleError::from_js_fields` 重建同样的 message/fields。
- drain：`ordering` 连续 3 次成功 emit 后一次 drain 按发出顺序返回 3 条，随后 drain 为空；`failed-emit-does-not-buffer` 证明 identity 拒绝的事件不进入缓冲区；抛错 sink 的 `drained`/`drainAfter` 均为空。每一步的 `drained` 与场景末尾的 `drainAfter` 都由真实缓冲区产生。
- 非对象 payload：数组、字符串、数字仍会到达 dispatcher，统一报 `operation` 字段错误。`payload: null` 在 client 的 `dispatchModuleCall` 读取 `.operation` 时（`agentLoopSidecarClient.ts:1086`）就抛 TypeError，属于 transport 前置校验而非本 module 行为，因此不纳入金样；Rust 侧由字段助手直接覆盖同等输入。
- `{__pdrsUndefined:true}` 是唯一标记，仅用于让 JSON 表达 JS 的 `undefined`；Rust 回放把该标记还原为 `JsValue::Undefined`，因此“键缺省”和“键存在但值为 undefined”仍是两个不同输入。

Rust 回放直接构造 `ModuleRouter` 中同一类型的 `event::Handler`，注入假 `EventSink`，逐值比较完整响应与 drain 结果；抛错场景的 sink 用 `ModuleError::from_js_fields` 重建异常，验证 TS 的 `code`/message 投影与 Rust 一致。另本地覆盖 router 路由、sink 抛普通 `HostError` 时的原样拒绝、未安装 sink 时 `emit` 显式失败（`Host did not provide an event sink.`，无 TS 对应物，属宿主接线保护，不伪造金样）。client 循环如何调用 drain、Gateway 事件协调器如何把事件发布到 sink 属于后续接线，不在本 PR。
