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
退出后删除。端口与源替换成 `<mock>`/`<redirect>`/`<stall>`/`<closed>`，两次独立运行逐字节一致
（2026-10-23 修复轮 1 实测两次均为
sha256 `92e1759a3b60501d8a20cc0d82ce2f59c0ce2398e3812c9da07ceebc2a77272b`）。

证书材料：受信场景复用 `../model-stream/tls.json` 的一次性测试 CA/叶子（`127.0.0.1` SAN，
非凭据）；证书错误场景用本目录 `untrusted-tls.json` 的一次性自签叶子（无 CA 签、不被任何
信任库接受，非凭据）。Rust 回放通过 `McpClient::with_tls_roots` 只信任该夹具 CA；生产
`McpClient::new()` 仍用 `webpki-roots`（Mozilla 捆绑根），夹具 CA 不会进入生产信任源。

## 用例（12）

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
- `https-to-http-308`：同上但状态 308；308 与 307 一样保留方法与 body，跨源只删
  `authorization`；5 个请求。
- `https-sse-redirect`：`https://<redirect>/sse` 307 → `https` mock。EventSource 的 GET 跟随
  重定向；SDK 用原始 URL 解析 endpoint，因此后续 POST 也经 307 到 mock；5 个请求。
- `https-handshake-stall`：只 accept TCP、不回 TLS ClientHello。`handshakeTimeoutMs` 故意设
  30 s，失败必须来自 undici `connectTimeout`（10 s，覆盖 TCP + 握手）→ `fetch failed`、
  `mcp_handshake_failed`；0 个请求。
- `https-refused`：先绑定再关闭的回环端口，TLS 连接拒绝 → `fetch failed`、
  `mcp_handshake_failed`；0 个请求。
- `https-sse-certificate`：SSE GET 打到自签叶子（`untrusted-tls.json`）→
  `MCP handshake failed: SSE error: TypeError: fetch failed: self-signed certificate`；0 个请求。
- `https-sse-handshake-stall`：SSE GET 的 TCP 连上但 TLS 握手不响应；`handshakeTimeoutMs` 设
  30 s 让 undici 的 10 s `connectTimeout` 先失败 →
  `… fetch failed: Connect Timeout Error (attempted address: <stall>, timeout: 10000ms)`；0 个请求。
- `https-sse-refused`：SSE GET 到已关闭的显式端口 → `… fetch failed: connect ECONNREFUSED <closed>`；
  0 个请求。
- `https-sse-default-port`：`https://127.0.0.1/sse`（无显式端口）→
  `… fetch failed: connect ECONNREFUSED 127.0.0.1:443`，钉住 https 的默认端口。生成器先断言本机
  443 未监听，回放同样要求 443 空闲；0 个请求。

## 回放与负控

回放：`tests/mcp_https.rs` 的 `replays_ts_mcp_https_over_local_tls`（逐步骤按
`JSON.stringify` 比较，再逐条比较服务端请求日志；12 场景 / 42 步 / 30 个请求），
`production_roots_do_not_trust_the_fixture_ca`（生产根拒绝夹具 CA），以及 `#[ignore]` 的
`mcp_https_fixtures_regenerate_identically_from_ts`（设 `PDRS_BASELINE` 后重跑真实 TS 一次并与
入库逐字节比较；两次独立运行逐字节一致的记录见 RESULT「修复轮 1」）。回放需要 PATH 上有
`node`（>= 18）。

负控（修复轮 1 实测，详见 RESULT）：把 `target_address` 的默认端口改回固定 80 时
`https-sse-default-port` 转红；把 SSE 的 connect-timeout / 自签证书 / 拒连 cause 文本分别去掉时
`https-sse-handshake-stall`、`https-sse-certificate`、`https-sse-refused` 分别在 start 转红；
把 IPv6 主机去掉方括号时单元测试
`failure_addresses_use_the_scheme_default_and_bracket_ipv6` 转红。

## 已知未对齐（未批准，见 RESULT 修复轮 1）

- rustls 把所有无法验证的链都报成 `UnknownIssuer`；Node/OpenSSL 区分自签叶子
  （`self-signed certificate`）与「私有 CA 签发但不被信任」（`unable to verify the first
  certificate`）。本层拿不到叶子证书，映射只能取前者；后者的 cause 文本仍与 TS 不同。属
  `A6.5-MCP-TLS-TRUST-001` 候选（私有 CA 信任源残余）的一部分。
- 主机名不匹配（`NotValidForName*`）的 cause 文本未对齐：Node 的
  `Hostname/IP does not match certificate's altnames: Host: …. is not cert's CN: …` 需要证书
  CN，rustls 不回传；本轮未录该场景金样，SSE 仍只报裸 `fetch failed`。
- 非 `http`/`https` 协议 Rust 立即报 `mcp_unsupported_transport`，TS 要到 `fetch` 才失败；
  这是既有、已写进模块注释的故意差异，账本尚未登记（见 RESULT 六字段候选）。
