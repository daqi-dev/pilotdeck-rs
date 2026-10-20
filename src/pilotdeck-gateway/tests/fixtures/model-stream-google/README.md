# Model stream Google（provider HTTP 公共层 + google）TS 对拍

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测：`src/model/streaming/streamModel.ts` 的 Google 分支（`streamGoogleProviderRequest`）及其 import 闭包（`providers/google/{request,client,schema,stream,response,modelId}.ts`、`thinking/registry.ts`、`request/sanitizeProviderBody.ts` 等，`sourceSha256` 逐文件记录）。期望值全部来自真实 TS 运行，Rust 侧不写死。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-stream-google/generate.mts "$(mktemp -d)"
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test model_stream_google
```

- 生成器先跑共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`，再用 `sourceClosureSha256` 记 `streamModel.ts` 的 import 闭包；断言 `en-US` locale。输出写到调用方给的目录（建议 `mktemp -d`），不覆盖仓库候选；连跑两次逐字节一致。
- 独占 mock：`127.0.0.1:0` 的 HTTP 服务（`tests/common/model_mock.rs`，沿用 model-stream 的 mock SSE 做法），外加一个已关闭端口（连接拒绝）。`Math.random` 固定 0.5，重试退避可复算。
- 请求记录：method、path、头（去掉 `transportHeaders` 列出的 fetch/undici 传输字段：host、content-length、user-agent、accept*、sec-fetch-mode、connection 等；名字小写、排序）、体逐字节；保持连接的回复额外记 `clientClosed`。SDK 的 `x-goog-api-client` 是协议头，保留并逐字节比较。
- 时间线：`event`（canonical 事件，含 `raw`）、`progress`（`onRetryProgress`）、`thrown`（抛出的错误：`name`、`message`，ModelProviderError 带 `error`，ModelRequestError 带 `code`）。origin 替换为 `$MOCK`/`$CLOSED`。
- `raw` 里的 `sdkHttpResponse`（SDK 附带的响应头，含当前时间）属于传输元数据而非 provider 协议投影，在生成器里剔除；两侧都不记录。

## 覆盖（26 例 / 26 次请求）

文本与 usage；多函数调用（responseId 派生、id 清洗与后缀去重、数组 args → `{}`）；thinking part（thought + thoughtSignature）；`finishReason` 的 `STOP`/`MAX_TOKENS`/`SAFETY`/`MALFORMED_FUNCTION_CALL`/未知值；schema 清理边界（`$defs`/`$ref`、`anyOf` 字面量合并、`const`→`enum`、`type` 去 null、非法属性名、`required` 过滤、array 补 `items`、不支持的 keyword）；多模态 contents 与 tool result/reference/media_reference；`extraBody` 深合并（`generationConfig.temperature` 剔除）；`$MOCK/v1beta` 与 `$MOCK/api` 端点；强制 toolChoice；thinking 不支持（`ModelRequestError` → error 事件，0 请求）；HTTP 401 JSON 错误体；HTTP 429 重试一次；HTTP 500 HTML 三次耗尽；断流（`terminated` → `timeout` + `streamInterruption.phase=text`）；空流（`IncompleteStreamError` → error 事件）；取消（`AbortError`，服务端确认连接被关）；请求校验失败（抛 `ModelRequestError`）；连接拒绝（重试耗尽 → error 事件）。修复轮 1 另加三例（真实 TS 生成）：自定义 `statusText: "Custom Bad"` 的非 JSON 错误体（P1-1）；大写 `Application/JSON` 走非 JSON 分支（P1-2）；tool_result / tool_result_reference 的孤立代理字符（P1-3）。

另有单元测试覆盖跨事件文本累积：`runtime/model/http_tests.rs` 的 `text_deltas_accumulate_under_the_v8_string_limit` 把 V8 上限缩到 4 个 UTF-16 单元，第三个 `text_delta` 经 `append_js_string` 报 `RangeError: Invalid string length`。
