# MCP HTTP 走代理真实 TS 夹具（#551 / A6.5-MCP-HTTP-LIFETIME-001）

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，SDK
`@modelcontextprotocol/sdk@1.29.0`，Node `v22.22.0`（生成器断言版本）。出处：

- `src/cli/proxy.ts:20-31` — `getProxyUrl()` 的优先级
  `PILOTDECK_PROXY > https_proxy > HTTPS_PROXY > http_proxy > HTTP_PROXY`；空字符串按 falsy 跳过。
- `src/cli/proxy.ts:52-90` — `installGlobalProxy()`：显式 URL（配置来源）在环境变量缺失时兜底，
  环境变量来源优先；重复安装直接返回。
- `src/cli/proxy.ts:122-147` — `EnvHttpProxyAgent({httpProxy: proxyUrl, httpsProxy: proxyUrl,
  noProxy, ...createLongTimeoutOptions()})` + `setGlobalDispatcher`，即 http 与 https 目标都用同一个代理。
- `src/cli/proxy.ts:163-167` — `buildNoProxy()`：
  `no_proxy || NO_PROXY`、额外配置值、`127.0.0.1`、`localhost` 以逗号拼接；回环永远绕过代理。
- `src/cli/pilotdeck.ts:28` — CLI 启动即 `await installGlobalProxy()`；`pilotdeck.ts:48-51` 在
  `server` 命令里用 `snapshot.config.proxy.url` 补装（环境变量已装过时被忽略）。
- `src/mcp/client/McpClient.ts:163-205` — `streamable_http` / `sse` 的 transport 都不读代理，只把
  URL 交给全局 `fetch` / `EventSource`，因此代理行为完全来自上面的全局 dispatcher。
- `node_modules/undici/lib/dispatcher/env-http-proxy-agent.js:82-140` — `NO_PROXY` 按 `/[, \s]/`
  拆分、可选 `*.`/`.` 前缀、精确或 `.<entry>` 子域匹配、`host:port` 端口钉死、`*` 全绕过。
- `node_modules/undici/lib/dispatcher/proxy-agent.js`（默认 `proxyTunnel: true`）— **http 与 https
  目标都发 CONNECT**；CONNECT 行是 `host:port`（URL 未写端口时补 80/443），`host` 头是 URL 的
  `host`（仅显式非默认端口才带端口），并带 `proxy-connection: keep-alive`；URL userinfo 生成
  `proxy-authorization: Basic base64(decodeURIComponent(user):decodeURIComponent(password))`。
  非 200 响应抛 `Proxy response (<status>) !== 200 when HTTP Tunneling`。
- `src/model/streaming/streamModel.ts:56,61,63` — `UNDICI_TRANSPORT_TIMEOUT_MS = 600_000`、
  `connections = 1000`、`keepAliveTimeout = 120_000`，作为 `EnvHttpProxyAgent` 的
  `headersTimeout` / `bodyTimeout` / `connections` / `keepAliveTimeout`。本夹具只验证连接/隧道/
  NO_PROXY/认证/错误文本；这些 undici 池与超时参数不属于连接器层，MCP 仍用自身的 10 s 连接与
  调用超时（见 RESULT 范围外）。

生成（仓库根目录）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/mcp-proxy/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/mcp-proxy
```

生成器接共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`（显式绝对 Git、
HEAD==`cc7c4e6`、已跟踪文件干净）与 `sourceClosureSha256`（TS 依赖闭包，`oracle.json` 的
`sourceSha256` 记录全部条目），并断言 en-US locale、SDK 1.29.0 与 Node v22.22.0。每次生成写入
`oracle.json`，两次独立运行逐字节一致（2026-10-23 修复轮 1 实测两次均为 sha256
`cb221a1bfe94cd176437eb18a7b450e1c50c70cbffbb7c4c0f49206607fe13b7`，128,350 B）。
`proxy.url` / `proxy.noProxy` 由 `src/pilot/config/loadPilotConfig.ts:856-911`
（`parseProxyConfig`）读取，启动时 `pilotdeck.ts:48-49` 只传 URL，配置变更时
`pilotdeck.ts:150` 的 `reinstallGlobalProxy(url, noProxy)` 才把 `noProxy` 一并传入。

## 为什么目标地址是 `0.0.0.0`

`buildNoProxy` 无条件把 `127.0.0.1` 与 `localhost` 加进 `NO_PROXY`，因此本机回环目标永远绕过
代理，无法验证“成功走代理”。macOS 上 `127.0.0.2` 不能监听（`EADDRNOTAVAIL`）。夹具改用
`0.0.0.0:<port>` 作为 MCP 目标：本机可连、不在默认 `NO_PROXY` 内；`NO_PROXY=0.0.0.0:<port>`
时又能验证绕过。HTTPS 夹具的叶子证书 SAN 为 `IP:0.0.0.0`（见 `proxy-tls.json`），仅用于本地
冒烟，CA/私钥都不受任何系统信任，不是凭据（`.gitleaks.toml` 有精确值放行条目）。

`connect_proxy.mjs` 是本目录的脚本化 HTTP CONNECT 代理：`PROXY_AUTH=user:pass` 时要求匹配的
Basic 头，`PROXY_STATUS` 可返回 403/407，`PROXY_LOG` 逐行记录
`{method,target,host,proxyConnection,proxyAuthorization}`（认证值写成 `Basic <scrub>`）。它同时
接受 http 与 https 目标的 CONNECT，并原样双向转发隧道字节。

## 用例（25）

每个场景由父进程起 mock 与代理，再用**独立 Node 子进程**调用真实 `installGlobalProxy()`
（或配置来源的 `installGlobalProxy(config.url)` / `reinstallGlobalProxy(config.url,
config.noProxy)`）；子进程环境只含该场景的代理变量，避免全局 dispatcher 的进程级状态互相污染。

| # | id | 传输 | 来源 | 结果 |
|---|----|------|------|------|
| 1 | `proxy-http-streamable` | streamable_http | `HTTP_PROXY` | 成功；1 CONNECT |
| 2 | `proxy-https-streamable` | streamable_http | `HTTPS_PROXY` | HTTPS 成功；1 CONNECT |
| 3 | `proxy-http-sse` | sse | `https_proxy`（小写） | 成功；1 CONNECT |
| 4 | `proxy-https-sse` | sse | `HTTPS_PROXY` | HTTPS + SSE 成功；1 CONNECT |
| 5 | `proxy-auth-https-streamable` | streamable_http | `PILOTDECK_PROXY` | Basic 认证成功；1 CONNECT |
| 6 | `proxy-precedence-https-over-http` | streamable_http | `HTTPS_PROXY` 活、`HTTP_PROXY` 关闭 | 成功，钉住优先级 |
| 7 | `proxy-precedence-pilotdeck-over-https` | streamable_http | `PILOTDECK_PROXY` 活、`HTTPS_PROXY` 关闭 | 成功，钉住最高优先级 |
| 8 | `proxy-empty-env-falls-through` | streamable_http | 前三个变量为空、`http_proxy` 活 | 成功，钉住空值 falsy |
| 9 | `no-proxy-http` | streamable_http | `NO_PROXY=<mock>` | 直连成功；0 CONNECT |
| 10 | `no-proxy-https` | streamable_http | `no_proxy=<mock>`、`NO_PROXY=127.0.0.9` | HTTPS 直连成功；0 CONNECT |
| 11 | `no-proxy-miss` | streamable_http | `NO_PROXY=127.0.0.9` | 仍走代理成功；1 CONNECT |
| 12 | `no-proxy-empty-lowercase-uses-uppercase` | streamable_http | `no_proxy=""`、`NO_PROXY=<mock>` | 直连成功；0 CONNECT |
| 13 | `proxy-config-url-streamable` | streamable_http | 仅 `proxy.url` | 成功；1 CONNECT |
| 14 | `proxy-config-startup-ignores-no-proxy` | streamable_http | 仅 `proxy.url` + 配置 `noProxy=<mock>` | 启动路径忽略配置 noProxy，仍走代理；1 CONNECT |
| 15 | `proxy-config-env-wins` | streamable_http | env 活 + `proxy.url` 关闭 | env 胜；1 CONNECT |
| 16 | `proxy-config-after-empty-env` | streamable_http | 五个 env 为空 + `proxy.url` | 配置兜底成功；1 CONNECT |
| 17 | `proxy-config-no-proxy` | streamable_http | `reinstall(url, noProxy=<mock>)` | 直连成功；0 CONNECT |
| 18 | `proxy-config-no-proxy-miss` | streamable_http | `reinstall(url, noProxy=127.0.0.9)` | 仍走代理；1 CONNECT |
| 19 | `proxy-config-env-no-proxy` | streamable_http | `proxy.url` + `no_proxy=<mock>` | 直连成功；0 CONNECT |
| 20 | `proxy-config-env-url-ignores-config` | streamable_http | env 活 + `proxy.url` 关闭 + 配置 noProxy | env 胜，配置 noProxy 不参与；1 CONNECT |
| 21 | `proxy-auth-failure-streamable` | streamable_http | 错误密码 | 407 → `MCP handshake failed: fetch failed` |
| 22 | `proxy-auth-failure-sse` | sse | 错误密码 | 407 → `… fetch failed: Error: Request was cancelled.: Proxy response (407) !== 200 when HTTP Tunneling` |
| 23 | `proxy-rejected-sse` | sse | 代理返回 403 | 同形文本，状态 403 |
| 24 | `proxy-unreachable-streamable` | streamable_http | 关闭端口 | `MCP handshake failed: fetch failed` |
| 25 | `proxy-unreachable-sse` | sse | 关闭端口 | `… fetch failed: connect ECONNREFUSED <proxyAuthority>`，地址是代理而非目标 |

成功场景（1-20）各 5 个 mock 请求、4 个步骤；失败场景（21-25）各 0 个请求、3 个步骤。合计
95 步 / 100 个请求 / 18 条唯一 CONNECT。`proxy-unreachable-streamable` 的 fetch 层把原因吞成
`fetch failed`，只有 SSE 暴露完整 cause，故 407/403/拒连的详细文本只在 SSE 录制。

## 替换与回放

`oracle.json` 里端口/来源已替换为 `<mock>`、`<proxy>`、`<closedProxy>`、`<mockAuthority>`、
`<proxyAuthority>`、`<closedAuthority>`；CONNECT 的 target/host 都是 `<mock>`，因此回放不依赖
具体端口。`tests/mcp_proxy.rs` 用同一组占位符在 Rust 侧重填/清洗：

- 父测试启动 `mock_server.mjs --http`（`MOCK_HOST=0.0.0.0`，HTTPS 时加 `MOCK_TLS`）与
  `connect_proxy.mjs`，再以 `--exact replays_ts_mcp_http_proxy_scenarios` 重新执行**测试二进制**
  为每个场景跑子进程，把该场景的代理环境变量与配置来源（`PDRS_MCP_PROXY_CONFIG_*`）通过
  `Command::env` 注入；子进程只驱动 `McpClient`，结果写回临时文件。这样既避开 edition 2024
  `set_var` 的 `unsafe`（workspace `unsafe_code = "forbid"`），也保证“忽略代理变量”的负控是真正变红。
- 逐步骤比较 `JSON.stringify`，再逐条比较 mock 请求日志和代理 CONNECT 日志（去重后按文本排序）。

回放：`cargo test -p pilotdeck-gateway --test mcp_proxy`；需要 PATH 上有 `node`（>= 18）。
`#[ignore]` 的 `mcp_proxy_fixtures_regenerate_identically_from_ts` 设置 `PDRS_BASELINE` 后重跑真实
TS 并与入库逐字节比较。

## 回放与负控

负控（2026-10-23 修复轮 1 实测，输出见 `_tmp/20261023-issue-551-mcp-proxy/RESULT.md`
「修复轮 1」）：

- 配置 URL 优先于 env（反转启动优先级）→ `proxy-config-env-wins` 在 start 步骤转红（连到关闭的
  配置端口）。
- 丢弃配置 `noProxy` → `proxy-config-no-proxy` 在 CONNECT 日志比较处转红（多出一条隧道）。
- 忽略配置 URL（只读 env）→ `proxy-config-url-streamable` 在 CONNECT 日志比较处转红（0 条）。
- 空 env 不再跳过 → `proxy-empty-env-falls-through` 在 CONNECT 日志比较处转红（0 条）。
- https + SSE 强制直连 → `proxy-https-sse` 在 CONNECT 日志比较处转红（0 条）。
- 启动路径错误地把配置 `noProxy` 拼入 → `proxy-config-startup-ignores-no-proxy` 在 CONNECT
  日志比较处转红（0 条；基线的启动路径本应忽略配置 noProxy）。

## 已知未覆盖

- 代理 URL 只支持 `http:` scheme。TS 的 undici 8.3 `ProxyAgent` 支持 `https:`（TLS 连代理）与
  `socks5:`/`socks:`（`proxy-agent.js:143,166` 的 socks 分支）；Rust 对这两种 scheme 退回直连
  （`ProxyConfig::new` 返回 `None`，与 ftp/坏 URL 的 catch-and-direct 同形），差异与六字段候选见
  RESULT「修复轮 1」。
- `proxy.url` / `proxy.noProxy` 已进入 `McpClientOptions` / `native_mcp_runtime_factory_with_proxy`
  / `ProjectMcpRuntimeProviderOptions`；gateway 目前没有构造 `ProjectMcpRuntimeProvider` 的生产
  入口（model/hook 接线另单 #572），宿主接线点见 RESULT。
