# complete() 真实回环超时 TS 金样（#610）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器先用
`baseline_source_guard.mjs` 绑定整个 `src` Git blob，再记录
`src/model/streaming/streamModel.ts` 的相对 import 闭包 SHA-256；Rust replay
由 `tests/common/baseline.rs` 核查来源。

与 `../model-complete/` 的 mock 表和 `../model-complete-transport/` 的受控
`fetch` 不同，这里起一个真实的 `node:http` 回环 server，让真实 TS
`complete()` 发真实 HTTP/1.1 请求：server 接受连接后或永不响应，或先回响应
头再把 body 拖过 provider 超时，或延迟响应头，或宣告更长的 content-length
后截断连接。Rust 侧不 mock transport，而是用真实的 `HyperModelTransport`
对该 server 重放，断言抛出的错误形状、耗时上下界、请求数与 wire request。

## 关键生命周期（r4b 真实 TS 实测修正）

`writeHead()` 只是把响应头放进 Node 缓冲；`flushHeaders()` 才真正写到
socket。早期探针在 delay 前只 `writeHead()`，测到的其实是“响应头到达”受请求
定时器约束，而不是 body 读取。r4b 探针在 delay 前真实 `flushHeaders()`，
`provider.timeoutMs=100` 时 body 延迟约 120ms 仍成功（耗时 304–322ms 区间
的不同场景），证明：

- `sendProviderRequest` 的定时器确实在整个 `sendWithEndpointFallback` 期间
  武装；但 `networkFetch` 在响应头到达后返回，并在 `finally` 里取消自己的
  父 abort 转发。之后 timer 即使 abort 外层 controller，也不会再拆掉已经
  resolve 的响应 body；
- 因而 2xx 形状回退里的 `response.clone().json()`/body 读取可以跨过请求
  deadline（也能跨过父 abort），直到 body 自己结束或读失败（undici body
  idle timeout 仍生效）；
- 但读取结束后若要 fetch 下一个候选，controller 已被 abort：第二次
  `fetch` 在发请求前就 reject。`sendProviderRequest` 的 catch 会先看**调用方
  signal**：父 signal 已 abort 就抛父 AbortError（即使 timeout 更早触发也会
  被覆盖）；父 signal 未 abort 才把 timeout 包成 `ModelProviderError`；
- 最后一个候选无论形状对错、body 是否读失败，都返回原 `Response`，随后
  `response.text()` 才可能抛 `TypeError: terminated`。

`delayHeadersMs` 是反向对照：在 `writeHead()` 之前 sleep，响应头尚未到达，
请求定时器仍然生效。

## 覆盖 17 例

1. `loopback-provider-timeout-100`：`provider.timeoutMs=100`，server 不响应；
   TS 抛 `Model request timed out after 100ms.`（`code: timeout`、
   `retryable: true`、raw `{code: network_timeout, name: NetworkFetchError}`）。
   `requestCount=1`（`retry.requestMaxRetries=0`）。
2. `loopback-stream-timeout-ignored`：`provider.timeoutMs=300` 且
   `options.streamTimeoutMs=10`；若 complete 误用 stream idle 解析器会在 10ms
   失败，金样要求 message 为 300ms、耗时 ≥150ms。
3. `loopback-default-fallback-not-stream-timeout`：不设 `provider.timeoutMs`，
   `streamTimeoutMs=10`，外部 signal 在 150ms abort；实际抛 `AbortError`，
   说明默认仍是 600000ms 请求定时器，而不是 10ms stream idle。
4. `loopback-body-outlives-request-timeout`：单 endpoint，`timeoutMs=50`，
   server 立即回响应头、200ms 后才结束 body；`complete()` 成功且耗时
   ≥150ms。单 endpoint 不做形状检查，证明请求定时器在 `send` 返回时已清除。
5. `loopback-multi-first-shape-valid-slow-body`：双 endpoint，首个候选
   `timeoutMs=100` 但 body 延迟 120ms；形状探针成功并直接返回首个候选，
   `requestCount=1`，耗时 ≥110ms。
6. `loopback-multi-last-shape-wrong-slow-body`：首个错误形状立即返回，最后
   候选错误形状且慢 120ms；body 读完、返回最后原 body（`{"last":true}`），
   `requestCount=2`。
7. `loopback-multi-last-shape-valid-slow-body`：同上但最后候选形状正确，成功
   解析慢 body，`requestCount=2`。
8. `loopback-multi-first-shape-wrong-slow-body`：首个候选错误形状且慢 120ms；
   body 读完后才发现外层 signal 已 abort，不再发第二个请求，抛 100ms
   timeout，`requestCount=1`。
9. `loopback-single-endpoint-slow-non-json`：单 endpoint，非 JSON body 延迟
   120ms 超过 `timeoutMs=100`；无形状检查，保留 raw
   `"<html>oops</html>"`，`requestCount=1`。
10. `loopback-delay-headers-beyond-timeout`：双 endpoint，首个候选延迟 200ms
    才 `writeHead()`，`timeoutMs=100`；响应头阶段就超时，抛 100ms timeout，
    `requestCount=1`。这是 flushHeaders 区分的对照。
11. `loopback-parent-abort-after-headers-shape-valid`：父 signal 在 50ms
    abort，但响应头已到、首个候选形状正确且 body 慢 120ms；形状检查成功并
    返回首个候选，`requestCount=1`。
12. `loopback-parent-abort-after-headers-shape-wrong`：同上，但首个候选错误
    形状；body 读完后下一次 fetch 看到已 abort 的 signal，抛 `AbortError`
    而不发第二个请求，`requestCount=1`。
13. `loopback-parent-abort-wins-over-timeout-shape-wrong`：父 abort 在 50ms、
    timer 在 100ms 都触发，错误形状 body 到 120ms 才结束；下一 fetch 失败时
    `sendProviderRequest` 的 catch 看到 `signal.aborted`，抛父 `AbortError`
    而非 timeout，`requestCount=1`。
14. `loopback-later-parent-abort-masks-timeout`：反过来 timer 100ms 先触发、
    父 abort 150ms 才发生，body 到 200ms；catch 仍用父 abort 覆盖 timeout，
    抛 `AbortError`，`requestCount=1`。这两例证明不能按“谁先触发”选错误，
    真正判据是下一 fetch 失败时父 signal 是否已 abort。
15. `loopback-body-read-error-then-second`：首个候选 content-length 比实际
    多 10 字节，写完前缀 20ms 后销毁连接；`clone().json()` 读失败，回退到
    第二个候选并成功，`requestCount=2`。
16. `loopback-last-body-read-error`：最后候选同样截断；返回最后原
    `Response`，`complete()` 的 `response.text()` 抛 `TypeError: terminated`，
    `requestCount=2`。
17. `loopback-multi-first-shape-wrong-within-timeout`：首个候选错误形状但只慢
    50ms、`timeoutMs=1000`；在 deadline 内正常回退并成功，`requestCount=2`。

## 计时判据

计时是真实墙钟。生成器按 TS 请求的 delay/timer 预先写：

- `elapsedAtLeastMs`：该例必须等到的下界（body/header delay 减 10ms 的
  `Date.now()` 粒度，或 timeout 的 90%）；
- `elapsedAtMostMs = expectedWaitMs + 500`：500ms 是给 CI/回环调度的固定
  余量，不是从 Rust 实测结果反推。`setTimeout` 不会早触发，所以下界可靠；
  500ms 足以吸收小负载抖动，同时仍能拒绝 `timeout_ms * 10`：100ms 的 600ms
  上界会拒绝约 1000ms，300ms 的 800ms 上界会拒绝约 3000ms。

实际跑出的 `elapsedMs` 只写进同目录的 `observations.json` 供人查看，不进入
逐字节比较的 `oracle.json`；否则重生成测试永远无法一致。`oracle.json` 只含
预写的上下界，`observations.json` 是当次真实 TS 证据。

Rust replay 同时断言 `requestCount`、完整 wire request（method/path/body）和
页面级响应/异常；不再使用旧的共享 `<10s` 上界。计时判据的负控
`complete-timeout-duration-scaled-tenfold` 把产品里的 `duration(timeout_ms)`
改成 `duration(timeout_ms * 10.0)`，由本文件的第一个 100ms 超时例拒绝。

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
