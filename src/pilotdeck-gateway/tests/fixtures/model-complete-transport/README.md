# complete() 传输重试 TS 金样（#610 / #611）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。与
`../model-complete/` 相同，生成器先用 `baseline_source_guard.mjs` 绑定整个
`src` Git blob，再记录 `src/model/streaming/streamModel.ts` 的相对 import
闭包 SHA-256；Rust replay 由 `tests/common/baseline.rs` 核查来源。

这里控制的是 `complete()` 的 `fetch` 传输，不是响应体：真实 TS `complete()`
（重试环、`sendProviderRequest` 的 error 包装、非 2xx 分支、body 读取）原样
运行，只让 `fetch` 在前若干次发送时抛错，或返回一个合法 `Response`。因此
金样钉住的是 TS 基线的重试边界与最终错误形状，而不是自写 mock 的期望。

覆盖 7 例：

- `transport-max-retries-0` / `-1` / `-1.5` / `-2`：同一 `fetch failed` 下
  `fetch` 调用数分别为 1 / 2 / 2 / 3。TS 的循环是
  `for (attempt = 0; attempt <= maxRetries; attempt++)`；`1.5` 在 attempt 0、1
  失败后退出循环，最终抛 `Error("complete() exhausted all retry attempts
  without a result.")`，而不是 `ModelProviderError`。
- `transport-non-retryable`：`boom` 分类为 `network_fetch_failed` 且
  `retryable: false`，只发送 1 次并抛 `ModelProviderError`。
- `transport-abort-before-send`：预中止 signal 在循环顶 `throwIfAborted` 抛
  `AbortError`，`fetch` 调用 0 次。
- `transport-fail-once-then-success`：第一次失败、第二次返回真实 `Response`，
  最终 canonical response 与 TS 逐字一致，`fetch` 调用 2 次。

body 读取失败在 TS 的 `try` 之外（`response.text()` 不在发送 catch 中），
所以本金样不把 body 异常混进 send 重试；`complete_model` 的 body 读取也在
重试环之外。

## 重新生成

```sh
export PDRS_BASELINE=/absolute/PilotDeck
export LANG=en_US.UTF-8
out=$(mktemp -d /tmp/pdrs-model-complete-transport.XXXXXX)
timeout 180 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-complete-transport/generate.mts "$out"
cmp "$out/oracle.json" src/pilotdeck-gateway/tests/fixtures/model-complete-transport/oracle.json
```

`#[ignore]` 测试 `regenerates_model_complete_transport_oracle_identically`
执行同样的重生成并逐字节比较。

## Replay

```sh
export CARGO_TARGET_DIR=$PWD/target
timeout 600 cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test model_complete native_complete_transport_retry_matches_real_ts -- --exact
```

Rust 侧用实现 `ModelHttpTransport` 的脚本化 transport 前 N 次返回
`TransportFailure`，之后回放同一响应帧；真正调用
`runtime::model::http::complete_model`，比较 `fetch` 调用数、canonical
response 或抛出的错误。只读固定基线；不访问外网。
