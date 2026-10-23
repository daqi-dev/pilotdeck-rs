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

## 覆盖（24 个场景）

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
`8f6576a518d9db2c917cef97b9b7c74976fc6b91be9379b0ef1b74b24c702284`。

常规回放复用 `tests/common/baseline.rs` 的 `oracle()`；未设 `PDRS_BASELINE` 时仅检查固定
commit 与哈希结构，不调用 Git。Rust 回放见 `tests/host_model_port.rs`（HTTP 场景在测试内
起 `tests/common/model_mock.rs` 的 loopback mock，需要本机回环监听权限）。
