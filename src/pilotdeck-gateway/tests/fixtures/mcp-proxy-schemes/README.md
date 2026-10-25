# #676 代理协议金样

`generate.mts` 直接 import 固定 cc7c4e6 的 `src/cli/proxy.ts` 与 `src/mcp/client/McpClient.ts`，使用 Node v22.22.0、undici 8.3.0。`sourceSha256` 由现有 source guard 绑定真实 src 依赖闭包。22 场景包含 HTTP 基线、HTTPS 双层 TLS（streamable/SSE）、SOCKS5/socks、认证、拒绝、非法配置及有效 config 补装。

`net.mjs` 的代理和目标仅监听 127.0.0.1；目标 URL 使用 `mcp.test`，代理只允许该测试域名/本地地址且只转发到本次目标端口的 127.0.0.1，不解析域名。首次非法配置场景的目标直接使用 127.0.0.1，证明 Rust 没有悄悄绕过代理。TLS 私钥仅供这个本地测试服务器，不是生产凭据。

每个场景隔离进程及代理环境变量。金样记录实际目标方法、CONNECT/SOCKS wire、greeting、认证和代理 TCP 接受收据；连接复用与重试次数去重，字段和目标方法保持精确比较。`tls.json` 的 CA 同时验证代理与目标：正常用例显式注入根证书，untrusted 用例保持默认根，不关闭证书检查。

复采（输出目录必须不存在；临时数据指定在本树）：

```sh
PDRS_BASELINE=/abs/PilotDeck TMPDIR="$PWD/_tmp/20261025-issue-676/tmp" \
node --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/mcp-proxy-schemes/generate.mts \
  _tmp/20261025-issue-676/new-ts-run
```

回放：`cargo test --offline -p pilotdeck-gateway --test mcp_proxy_schemes -- --nocapture`（在 src）。可用 `PDRS_SCHEME_EVIDENCE` 指定原始日志父目录，`PDRS_SCHEME_FILTER` 选场景；测试不自动删除 wire 证据。

TS 对非法 reload 保留旧 dispatcher，初次非法安装仍可能直连。Rust 对结构非法及 reload 安装失败明确拒绝连接，startup userinfo-only 失败保留直连；边界见 `A6.5-MCP-PROXY-FALLBACK-001-R1`。streamable 的公开错误沿用 `MCP handshake failed: fetch failed`；共享 connector 单测验证具体配置错误 cause，并证明结构非法时 NO_PROXY 本地目标也没有收到连接。SSE/底层错误文本和 SOCKS 分阶段计时不声称完全逐字对齐（SOCKS 使用共享 10 秒 whole-connect deadline，TS 另有 5 秒阶段计时）。

原始实测：本树 `_tmp/20261025-issue-676/ts-wire/`、`ts-repeat/`（两次 oracle 逐字节一致）、`ts-edges.log`、`ts-socks-edges.log`；负控与恢复哈希见该目录 `negative-controls.json`。原有 HTTP/NO_PROXY 测试与旧金样不重录。

## userinfo 启动失败补充（#676 fix-r1）

`generate-userinfo.mts` 调用同一固定 TS 基线的真实安装入口和 `McpClient`，仅连接 127.0.0.1。
`userinfo-oracle.json` 含 61 例：四协议的 user/password 非法转义、截断/非法 UTF-8、
空 username、合法编码，以及 env/config/startup/reload 来源组合。每例独立进程，12 s 截止；
客户端握手/调用上限 2 s。TS 的 startup `%ZZ`/`%FF` 安装失败后直连；reload 则保留旧代理。
生成器保存每例 Node 原始输出、MCP 目标请求收据、代理连接数及 dispatcher 状态。

```sh
PDRS_BASELINE=/absolute/path/to/PilotDeck LANG=en_US.UTF-8 \
node --import /absolute/path/to/PilotDeck/node_modules/tsx/dist/loader.mjs \
src/pilotdeck-gateway/tests/fixtures/mcp-proxy-schemes/generate-userinfo.mts /new/evidence/directory
```

两次生成 `oracle.json` 必须逐字节一致。Rust 的
`runtime::model::http::proxy_tests::proxy_userinfo_install_paths_replay_ts_matrix`
逐例消费该金样，通过生产解析入口和真实 connector 验证连接/拒绝；结构非法与 reload 的
已授权差异显式列出，边界见 `specs/gateway-parity/diffs/A6.5-MCP-PROXY-FALLBACK-001-R1.md`。
本轮复采原始证据及负控见 `_tmp/20261025-issue-676/fix-r1/RESULT.md`，旧 35-case 金样保持不变。
