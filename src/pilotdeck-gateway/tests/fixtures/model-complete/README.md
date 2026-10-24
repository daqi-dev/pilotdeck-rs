# native complete_model TS 金样（#610 / #611）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器先用
`baseline_source_guard.mjs` 绑定整个 `src` Git blob，再记录
`src/model/streaming/streamModel.ts` 的相对 import 闭包 SHA-256。Rust replay
由 `tests/common/baseline.rs` 核查来源。

真实调用链：TS `complete(request, config)` → `validateModelRequest` →
`buildModelRequest` → 真实回环 HTTP mock → `parseRawJson` →
`parseModelResponse`。Rust 侧通过 `ModelHttpTransport` 的 `Peer` + body
frames 喂同一状态码、响应头和响应体字节，真正调用
`runtime::model::http::complete_model`，再按 `JSON.stringify` 比较 canonical
response 或抛出的 `ModelProviderError`；不是只测自写 parser。

覆盖 25 例，三协议各含正常与空 body / 非 JSON：

- anthropic：thinking+signature、text、tool_use、usage（含 cache 与
  totalTokens）、`end_turn`/`tool_use`/`refusal`、未知 stop reason。
- openai：`reasoning_content` 字符串与对象形式、`reasoning_details` 多段、
  string / array content、重复与缺失 tool-call id 的生成与去重、jsonrepair、
  `stop`/`tool_calls`/`content_filter_results`/未知 finish reason、usage
  （cache、nativeCost）。
- openai-responses：`output_text`、message/output_text 文本、reasoning
  summary、function_call（call_id/id/arguments repair）、`output_text` 去重、
  completed/incomplete/failed/queued 对应 stop/length/error/unknown、usage。
- 负控金样：`responses-body-parsed-as-openai` 与
  `openai-body-parsed-as-responses` 证明协议分派不是 chat parser 的代称；
  `responses-normal` 若回退到 chat parser 会得到空 content 和 unknown。
- 429 + `Retry-After`：TS `complete()` 对非 2xx **不重试**，只抛
  `ModelProviderError`（`retryable: true`）；响应头不进入 canonical error
  的 `retryAfterMs`，body 里的 `retryAfterMs` 保留。金样同时锁请求数 = 1。
- 非法 tool arguments：抛 `invalid_tool_arguments`，message 与 TS 逐字一致，
  `raw` 是单个 tool-call item。

## 重新生成

```sh
export PDRS_BASELINE=/absolute/PilotDeck
export LANG=en_US.UTF-8
out=$(mktemp -d /tmp/pdrs-model-complete.XXXXXX)
timeout 180 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-complete/generate.mts "$out"
cmp "$out/oracle.json" src/pilotdeck-gateway/tests/fixtures/model-complete/oracle.json
```

`sourceSha256` 依赖固定基线 checkout 与 Node v22；生成器断言 locale 为
`en-US`。`#[ignore]` 测试 `regenerates_model_complete_oracle_identically`
执行同样的重生成并逐字节比较。

## Replay

```sh
export CARGO_TARGET_DIR=$PWD/target
timeout 600 cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test model_complete
```

只读固定基线；不访问外网、`~/.pilotdeck` 或用户运行中的 PilotDeck。

## 负控

```sh
export CARGO_TARGET_DIR=$PWD/target
timeout 900 python3 \
  src/pilotdeck-gateway/tests/fixtures/model-complete/negative-controls.py
```

四个变异：Responses 委派给 chat parser、OpenAI finish reason 改成
`tool_calls`、Responses failed 改成 unknown、complete 对 429 重试。每个变异
都先编译，只接受已编译测试的断言失败；无论成败都按 SHA-256 恢复源文件。
