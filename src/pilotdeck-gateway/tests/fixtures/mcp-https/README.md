# MCP HTTPS 传输真实 TS 夹具（#361）

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，SDK
`@modelcontextprotocol/sdk@1.29.0`，Node `v22.22.0`（生成器断言版本）。出处：

- `src/mcp/client/McpClient.ts:163-205` — `buildTransport()` 对 `streamable_http` 用
  `new URL(this.spec.url)` 构造 `StreamableHTTPClientTransport`，对 `sse` 用
  `new URL(this.spec.url)` 构造 `SSEClientTransport`；两者都不检查协议，`https:` 直接交给
  Node 的 fetch / EventSource。
- `src/network/fetch.ts:1,194-202` — streamable_http 的 POST/GET 走基线自有 `undici.fetch`。
- `node_modules/@modelcontextprotocol/sdk/dist/esm/client/streamableHttp.js:89,306,443` —
  `fetch(this._url, ...)`，URL 原样使用。
- `node_modules/@modelcontextprotocol/sdk/dist/esm/client/sse.js:70` —
  `new EventSource(this._url.href, ...)`，全局 `fetch`。
- `node_modules/undici/lib/core/connect.js:69,126` — `connectTimeout` 默认 `10e3` ms，
  一个计时器覆盖 TCP 建连与 TLS 握手。
- `node_modules/undici/lib/web/fetch/request.js:908` — `redirect` 默认 `follow`；
  `.../fetch/index.js:1200` 起按 303/307 等状态决定是否改方法、丢体。

生成（仓库根目录）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/mcp-https/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/mcp-https
```

生成器接共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`（显式绝对 Git、
HEAD==`cc7c4e6`、已跟踪文件干净）与 `sourceClosureSha256`（本夹具依赖 6 个 TS 文件），并断言
en-US locale、SDK 版本与 Node 版本。legacy SSE 用全局 `fetch`，其信任库只在进程启动时读取
`NODE_EXTRA_CA_CERTS`，因此生成器先以该变量指向临时 PEM 复执行一次自己；临时目录在子进程
退出后删除。两次运行逐字节一致（无时间戳；端口与源替换成 `<mock>`/`<redirect>`/`<stall>`/
`<closed>`），2026-10-23 实测 sha256
`42e0822179e71041939cb5d3d455be2af1b0fbbc3c5446079cc6c4acb4dc2dc9`。

证书材料复用 `../model-stream/tls.json` 的一次性测试 CA/叶子（`127.0.0.1` SAN，非凭据）。
Rust 回放通过 `McpClient::with_tls_roots` 只信任该夹具 CA；生产 `McpClient::new()` 仍用
`webpki-roots`（Mozilla 捆绑根），夹具 CA 不会进入生产信任源。

## 用例（6）

回放服务器是 `../mcp-client/mock_server.mjs` 的 `--http` + `MOCK_TLS` 模式（同一份脚本 TS / Rust
两侧共用），重定向、TLS 握手卡住与拒绝端口由回放测试自带的回环服务器复现。

- `https-streamable`：`https://<mock>/mcp` 上 `initialize` / `tools/list` / `tools/call` /
  `close`，`X-Api-Key` 头与 `mcp-session-id` 回传；服务端收到 5 个请求。
- `https-sse`：`https://<mock>/sse` 的 legacy SSE：GET 事件流给出 endpoint，POST 202 + SSE
  回帖，`initialize` / `tools/list` / `tools/call` / `close`；5 个请求。
- `http-to-https-redirect`：`http://<redirect>/mcp` 307 → 同一主机另一端口的 `https` mock。307 保留
  POST 方法与 body；跨源重定向丢掉 `Authorization`，自定义 `x-api-key` 保留；5 个请求。
- `https-to-http-downgrade`：`https://<redirect>/mcp` 307 → 同一主机另一端口的 `http` mock，同样保留
  方法与 body、丢 `Authorization`；5 个请求。
- `https-handshake-stall`：只 accept TCP、不回 TLS ClientHello。`handshakeTimeoutMs` 故意设
  30 s，失败必须来自 undici `connectTimeout`（10 s，覆盖 TCP + 握手）→ `fetch failed`、
  `mcp_handshake_failed`；0 个请求。
- `https-refused`：先绑定再关闭的回环端口，TLS 连接拒绝 → `fetch failed`、
  `mcp_handshake_failed`；0 个请求。

## 回放与负控

回放：`tests/mcp_https.rs` 的 `replays_ts_mcp_https_over_local_tls`（逐步骤按
`JSON.stringify` 比较，再逐条比较服务端请求日志；6 场景 / 22 步 / 20 个请求），
`production_roots_do_not_trust_the_fixture_ca`（生产根拒绝夹具 CA），以及 `#[ignore]` 的
`mcp_https_fixtures_regenerate_identically_from_ts`（设 `PDRS_BASELINE` 后重跑真实 TS 两次并
逐字节比较）。回放需要 PATH 上有 `node`（>= 18）。

负控：把 `parse_endpoint` 改回只收 `http:` 时 `replays_ts_mcp_https_over_local_tls` 在
`https-streamable: step 0 (start)` 转红；把 MCP 调用处的 `ConnectDeadline` 去掉后
`https-handshake-stall` 在测试自带的 12 s 上限转红（`connect deadline did not fire`）。
