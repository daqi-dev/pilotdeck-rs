# Host model module port 对拍（A6.9-pr1）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`record.mts` 组装真实 TS 生产链
（`AgentSessionRuntimeBundle.ts:188-208` 的 `createRouterModelInvokerPort` →
`createDurableModelInvokerPort` → `createSidecarModuleComposition().model.bindTurn`），
按 `sidecarDefaultModuleDispatcher.ts:69-158` 的方式逐轮驱动：`prepare`、迭代器逐条
`next()` 拉取、显式 `return()`（`close_stream`）、五个 metadata 查询、
`materializeRequest`。recorder 是真实 `AgentSessionEventRecorder` +
`JsonlTranscriptWriter`（固定时钟 `2026-10-18T00:00:00.000Z`、确定性 `id-N` entryId，
transcript 逐字节可比）。HTTP 场景把 providers 指向本进程独占的 loopback mock，经真实
`streamModel` 回放，同 `router-execute` 金样。`generate.mts` 通过共享 `bindSourceTree`
与 `sourceClosureSha256` 固定 651 项相对 import 闭包，并断言 en-US locale。生成目录由
调用者独占提供；生成器用 `wx` 拒绝覆盖，没有删除动作。

## 覆盖（41 个场景）

文本流、工具调用 delta、`modelOverride` 显式路由（含 subagent 位）、provider error 事件
后的 fallback、抛出网络错误的 fallback 与终态、`managedModelPolicy` 拒绝（execute 内
`SDK_MANAGED_MODEL_DENIED`）、流中途 `close_stream`（scripted 与 HTTP 各一，HTTP 侧断言
`clientClosed`）、零拉取即 close（未启动的 generator 不触 recorder、不建 provider 请求）、
单轮两次 stream（recorder 开第二个 step）、跨轮 sticky 由 `bindTurn` 一次性消费
（`invalidateSticky` 探针）、`agent.maxContextTokens` 覆盖 `getModelMaxContextTokens`
（`getModelTokenLimits` 不受覆盖，按真实 TS）、HTTP 文本流 / 工具调用 / 取消 / 429→fallback。

修复轮 1 追加七个 TS yield 边界与 `context.abortSignal` 场景：内容后 error 事件全量拉尽
（`execute_failed` 与 provider 健康失败只发生在消费完已交付事件的拉取之后）、只拉一条即
`return()`（read-ahead 停在拉取边界，无失败发布）、预中止（首拉记录请求后 done，0 次
provider 调用，`RouterRuntime.ts:685`）、拉取后中止（缓冲的 read-ahead 事件仍交付，下一条
provider 读取裸抛 AbortError）、HTTP 等待响应体时中止（等待被打断，`clientClosed`）、HTTP
流中途中止（全部缓冲事件先交付再裸抛 AbortError）、pre-content 终态错误（drain 的 pending
事件是 TS yield——`RouterRuntime.ts:873-877`，`execute_failed`、重放 buffered 事件与富化
error 都留在消费它们的拉取之后；TS 在该路径会把非 error 的 buffered 事件 yield 两次，
`RouterRuntime.ts:993-998`，Rust 按 TS 同样保留）。abort error 的比较面只有 `message`
（`code: 20` 数字不上模块线，`errorOf` 只保留字符串 code）。

修复轮 3 追加 pre-content 终态错误的中止变体（只拉首条后取消轮次再 `return()`）：取消仅拆除
provider 连接（`streamModel.ts` 的 `cancelReader`），不会推进挂起中的 generator，故
`execute_failed` / `observeRequest` 不发布（`RouterRuntime.ts:962` 位于 `:873-877` 的逐条
yield 之后），`return()` 丢弃其余。

修复轮 4 追加六个 `modelOverride` 真值判定场景（`routerModelInvokerAdapter.ts:28`）：
判定是 JavaScript 真值而非形状检查——缺 `model`（只剩 `provider`）、缺 `provider`（只剩
`model`）、非字符串 `provider`（数字 42 原样进 decision / request / invoker 调用）、非对象
真值（`'x'`，读 `.provider` / `.model` 得 `undefined`，键序列化时省略）、`null`（假值，照常
走 `router.decide`）、空对象 `{}`（真值，两键均 `undefined`）。半路由（缺 provider 或缺
model）跳过 capabilities 钳制（`getCapabilities` 抛错被捕获，`maxOutputTokens` 保持 100）。

修复轮 5 追加十个场景，两层各半：进程内（`routerModelInvokerAdapter`）与宿主模块线
（真实 `createSidecarDefaultModuleDispatcher` 驱动 `Handler::call` 的 payload/call，捕获
wire 上的 `context.modelOverride` 与 prepared 结果）。线侧五个覆盖
`sidecarDefaultModuleDispatcher.ts:473` 的 modelOverride 净化（对象且 provider/model 均
字符串才拷两字段，否则丢弃）：保留（extra 字段被净化）、缺 model 丢弃、非对象真值丢弃、
非字符串 provider 丢弃、数组丢弃。进程内五个覆盖：`{provider:{toString:null}}` 在 execute
入口抛 `TypeError: Cannot convert object to primitive value`（0 次 provider 调用，请求仍
先落盘，`RouterRuntime.ts:581-585`）；普通对象 provider 照常走（`[object Object]`）；
对象 / 数组 provider 与 `request.cachePlan` 的 `===` 比较按引用身份
（`RouterRequestMaterializer.ts:22-25`）——独立同形对象清 cachePlan 与
cacheBreakpoints，同值字符串保留。极深数组路由在 TS 侧更早失败（prepare 的
`snapshotCanonicalModelRequest` → `structuredClone` 抛 `RangeError`），无法经 oracle 回放，
由 Rust 单测 `unconvertible_route_rejects_first_stream_pull`（depth 2100）覆盖。

每个场景记录：`trace`（router 事件、prepared（含 `opaque` 决策）、每次拉取的事件、done、
closed、error、metadata、materialized、invalidate 探针）、`calls`（invoker 收到的请求）、
HTTP 场景的 `requests`（含 `clientClosed`）、`transcript`（完整 JSONL 文本）。全部与
Rust `tests/host_model_port.rs` 逐字节比较（JSON 序列化字符串，byte 级；HTTP origin 替换
`$MOCK`）。

error 的比较面是 `{message, code?}`——模块线上保留的字段（TS 端口抛出的 `Error` 的
`name` 不上模块线，Rust `ModuleError` 同样只带 message/code/canonical/retryable）。

## 正式复现

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
export PDRS_GIT=/usr/bin/git
# 每次指定一个新的 /tmp 独占目录；不向基线或 node_modules 写入。
LANG=en_US.UTF-8 TMPDIR=/tmp node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/host-model-port/generate.mts /tmp/<owned-run-1>
LANG=en_US.UTF-8 TMPDIR=/tmp node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/host-model-port/generate.mts /tmp/<owned-run-2>
cmp /tmp/<owned-run-1>/oracle.json /tmp/<owned-run-2>/oracle.json
cmp /tmp/<owned-run-1>/oracle.json src/pilotdeck-gateway/tests/fixtures/host-model-port/oracle.json
```

本次入库的两次运行哈希（SHA256）：
`46233d1758e5a6d526ab8de0bd27785ef790526c578a65c3330de4284e06677d`。

常规回放复用 `tests/common/baseline.rs` 的 `oracle()`；未设 `PDRS_BASELINE` 时仅检查固定
commit 与哈希结构，不调用 Git。Rust 回放见 `tests/host_model_port.rs`（HTTP 场景在测试内
起 `tests/common/model_mock.rs` 的 loopback mock，需要本机回环监听权限）。
