# streamModel retry-word / RouterRuntime transient-word 真实 TS 金样

`generate.mts` 直接 import 固定 `cc7c4e6` 的 `streamModel.ts`、`parseModelConfig.ts`
和 `RouterRuntime.ts`，不复制实现、不手写期望。共享 `../_shared/baseline.mts`
核验 HEAD、tracked 工作区，并对三份源码做已提交字节与工作区字节双 SHA-256 校验。
Node v22，所有输出写指定目录；不修改基线、不写 `node_modules`。

```sh
PDRS_BASELINE=/absolute/PilotDeck node --import /absolute/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/model-stream-retry-words/generate.mts /tmp/retry-words-generated
```

## 探针走的是哪条路径

- `cases`：OpenAI 流式请求第一次的响应体 `ReadableStream` 直接 `controller.error(new Error(message))`，
  第二次返回正常 SSE。`streamModel` 在读取 body 时拿到裸 `Error`，走
  `isRetryableStreamError` 的 `error instanceof Error` 分支——也就是
  `http.rs::retryable_message` 对应的真实清单。记录 `calls`（2 = 重试）、
  `retried`、`retryProgress`、事件序列和未重试时抛出的错误。`retryWord` 是从
  `streamModel.ts` 的 `isRetryableStreamError` 函数体里抽出的 `msg.includes("…")`
  字面量归属，独立于运行结果；`aborted` 在源码清单里但被 `isAbortError` 提前短路，
  所以 `retryWord=true` / `retried=false`，两条都保留。
- `routerCases`：`RouterRuntime.execute` 注入的 `modelInvoker.stream` 直接抛
  `Error(message)`，`transientRetry.enabled=false`，记录 canonical error 的
  `code`/`retryable`。这对应 `fallback.rs::canonical_error` 移植的
  `RouterRuntime.ts::isNetworkTransient` + `classifyNetworkErrorCode`，与
  `streamModel` 的清单是**两层不同的 TS 清单**：这里 `dns`/`etimedout`/`abort`
  可重试，`timeout`/`aborted` 不可重试。
- 注意：同一词若让 `options.fetch` 直接 reject，TS 会先在 `sendProviderRequest` 里
  `normalizeModelError` 包成 `ModelProviderError`，重试由 canonical `retryable` 决定，
  可能与本夹具不同（例如 `network`/`epipe` 的 transport 失败不重试）。本夹具测的是
  stream 读 body 的裸 `Error` 分支，也就是 `http.rs::retryable_message` 对应的分支。
- `sourceLists.isRetryableRequestError`：`complete()` 的兄弟清单，与 stream 清单
  差一个 `aborted` / `etimedout`。这条裸消息分支在 `complete()` 里不可达
  （`sendProviderRequest` 先把所有 transport error 包成 `ModelProviderError`），
  所以只记录基线源码字面量，Rust 侧按源码金样做常量级对拍；不是运行时实测。

## 复算

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --lib real_ts_stream_retry_message_words
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --lib complete_retry_message_list_matches_the_ts_request_classifier_source
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --lib real_ts_router_transient_thrown_words_match_fallback
```

两次生成逐字节一致：`cmp gen-a/oracle.json gen-b/oracle.json`。
