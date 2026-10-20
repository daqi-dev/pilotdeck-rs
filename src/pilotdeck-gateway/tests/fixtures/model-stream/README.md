# Model stream（provider HTTP 公共层 + anthropic）TS 对拍

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测：`src/model/streaming/streamModel.ts`（非 Google 路径）及其 import 闭包（`normalizeModelError.ts`、`providers/anthropic/*`、`network/fetch.ts`、`RetryPolicy.ts` 等，`sourceSha256` 逐文件记录）。期望值全部来自真实 TS 运行，Rust 侧不写死。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-stream/generate.mts "$(mktemp -d)"
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test model_stream
```

- 生成器先跑共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`，再用 `sourceClosureSha256` 记 `streamModel.ts` 的 import 闭包；断言 `en-US` locale。输出写到调用方给的目录（建议 `mktemp -d`），不覆盖仓库候选；连跑两次逐字节一致。
- 独占 mock：`127.0.0.1:0` 的 HTTP 服务与 HTTPS 服务（`tls.json` 里的测试 CA/叶子，首次由 openssl 在独占临时目录生成后复用；证书内容不进 `oracle.json`），外加一个已关闭的端口（连接拒绝）。`Math.random` 固定 0.5，重试退避可复算。
- 请求记录：method、path、头（去掉 `transportHeaders` 列出的 fetch/undici 传输字段：host、content-length、user-agent、accept*、sec-fetch-mode、connection 等；名字小写、排序）、体逐字节；保持连接的回复额外记 `clientClosed`（客户端是否关闭连接）。
- 时间线：`event`（canonical 事件，含 `raw`）、`progress`（`onRetryProgress`）、`thrown`（抛出的错误：`name`、`message`，ModelProviderError 带 `error`，ModelRequestError 带 `code`）。origin 替换为 `$MOCK`/`$TLS`/`$CLOSED`。

## 覆盖（25 例 / 30 次请求）

正常流（thinking + signature、文本、工具调用 JSON 分片、usage、`[DONE]`、注释行与 `id:`/`retry:` 行）；UTF-8 多字节在分片中间拆开 + BOM + CRLF 事件 + 非法字节；`event: error` 可重试（重来）/带 `try again in 5ms` 的续写/不可重试抛出；HTTP 429 + `Retry-After: 0.02`、529、500 HTML 三次耗尽、401 不重试；连接中途断开（`TypeError: terminated`）；无完成标记的续写与工具调用中断（`streamInterruption`）；取消（`AbortError`，服务端确认连接被关）；空闲超时；非法 SSE JSON（V8 `SyntaxError` 原文）；连接拒绝重试耗尽；无版本段 URL 的 `/v1/messages` 404 → `/messages` 回退；请求校验失败；TLS 受信握手成功；TLS 证书不受信（TS 投影为 `code=timeout`、`message=fetch failed`、可重试）。

修复轮 1 追加 5 例：工具参数经 jsonrepair 修复（`wasRepaired: true`）；截断参数 + `max_tokens` → `max_output_reached`；修不好的参数 + 仅 `message_stop` → `invalid_tool_arguments` + `streamInterruption`；错误体半截停滞（undici `bodyTimeout` 150ms → `TypeError: terminated`，用例级 `bodyTimeoutMs`）；HTTP 204 → `Missing response body.`。

#360-1 追加 4 例（共 31 例 / 38 次请求）：provider 流按 undici 跟随重定向（共用 `crate::fetch_redirect::follow`）。生成器另起一个回环源 `$OTHER`，该源收到的请求记录带 `server: "$OTHER"`；回复头里的 `$MOCK` / `$OTHER` 在发送时替换成真实源。
- 302：POST 改 GET，丢弃请求体和 content-type，保留 x-api-key 等头。
- 307：POST 原样重发到相对路径 `../v1/again`。
- 308 跨源：删掉 authorization 和 cookie，x-api-key 保留（undici 只删 authorization / proxy-authorization / cookie / host）。
- 跳到封禁端口 6000：`fetch failed`，规范化为 timeout。
