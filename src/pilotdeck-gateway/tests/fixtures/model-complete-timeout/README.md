# complete() 真实回环超时 TS 金样（#610）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器先用
`baseline_source_guard.mjs` 绑定整个 `src` Git blob，再记录
`src/model/streaming/streamModel.ts` 的相对 import 闭包 SHA-256；Rust replay
由 `tests/common/baseline.rs` 核查来源。

与 `../model-complete/` 的 mock 表和 `../model-complete-transport/` 的受控
`fetch` 不同，这里起一个真实的 `node:http` 回环 server，让真实 TS
`complete()` 发真实 HTTP/1.1 请求：server 接受连接后或永不响应，或先回头
再把 body 拖过 provider 超时。Rust 侧不 mock transport，而是用真实的
`HyperModelTransport` 对该 server 重放，断言抛出的错误形状、实际耗时下界与
请求数。

覆盖 4 例：

- `loopback-provider-timeout-100`：`provider.timeoutMs=100`，server 不响应；
  TS 抛 `Model request timed out after 100ms.`（`code: timeout`、
  `retryable: true`、raw `{code: network_timeout, name: NetworkFetchError}`）。
  `requestCount=1`（`retry.requestMaxRetries=0`）。
- `loopback-stream-timeout-ignored`：`provider.timeoutMs=300` 且
  `options.streamTimeoutMs=10`；若 complete 误用 stream idle 解析器会在 10ms
  失败，金样要求 message 为 300ms、耗时 ≥150ms。
- `loopback-default-fallback-not-stream-timeout`：不设 `provider.timeoutMs`，
  `streamTimeoutMs=10`，外部 signal 在 150ms abort；实际抛 `AbortError`，
  说明默认仍是 600000ms 请求定时器，而不是 10ms stream idle。
- `loopback-body-outlives-request-timeout`：`provider.timeoutMs=50`，server
  立即回响应头、200ms 后才结束 body；`complete()` 成功且耗时 ≥150ms，证明
  请求定时器在 `send` 返回时已清除，不是整包总超时。

计时是真实墙钟，因此金样只记录 `elapsedAtLeastMs` 下界，不记录实际耗时；
Rust 测试另加 `<10s` 上界。生成器断言 locale 为 `en-US`；每一例都记录
`requestCount` 与 wire request，Rust 用真实 loopback 的接受连接数对比。

## 重新生成

```sh
export PDRS_BASELINE=/absolute/PilotDeck
export LANG=en_US.UTF-8
out=$(mktemp -d /tmp/pdrs-model-complete-timeout.XXXXXX)
timeout 180 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-complete-timeout/generate.mts "$out"
cmp "$out/oracle.json" src/pilotdeck-gateway/tests/fixtures/model-complete-timeout/oracle.json
```

`#[ignore]` 测试 `regenerates_model_complete_timeout_oracle_identically`
执行同样的重生成并逐字节比较。

## Replay

```sh
export CARGO_TARGET_DIR=$PWD/target
timeout 600 cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test model_complete \
  native_complete_timeout_uses_provider_timeout_and_ignores_stream_timeout -- --exact
```

只绑定 `127.0.0.1` 的临时端口；不访问外网、`~/.pilotdeck` 或用户运行中的
PilotDeck。`HyperModelTransport` 走项目已有的 no-proxy 回环规则，不依赖环境
代理设置。
