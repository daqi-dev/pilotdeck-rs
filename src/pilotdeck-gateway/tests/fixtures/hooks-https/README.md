# HTTP hook HTTPS fixture

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。期望值全部来自真实 TS
`HttpHookExecutor`（全局 `fetch` 通过基线 `undici` 访问本地 TLS 服务），无实现源码复制。

生成（仓库根目录，Node v22.22.0，基线 lockfile 的 tsx）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/hooks-https/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/hooks-https
```

生成器先接 #280 共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`（显式绝对 Git、
HEAD==`cc7c4e6`、已跟踪文件干净、逐 blob 对照）；`oracle.json.sourceSha256` 记录本夹具依赖的 3 个 TS
文件。端口/源在 `oracle.json` 里替换成 `<tls>`/`<http>`/`<closed>`，两次生成逐字节一致
（2026-10-23：`417560a7744d5111d5bd938d9c06be8b74dc5d27a319b3c170fc93f300b494da`）。

## 用例（4）

- `https-direct`：`https://127.0.0.1:<port>/hook`。信任夹具 CA 的 `undici.Agent({ connect: { ca } })`
  全局 dispatcher，`POST` + `content-type: application/json` 到达 TLS 服务；响应体回显方法/体/内容类型。
- `http-to-https-redirect`：HTTP 302 → 同一 TLS 源的 `/hook`。`fetch` 的 302 规则把 POST 改为 GET、
  丢体，TLS 服务收到 `GET`、空体、无 `content-type`。
- `https-untrusted-certificate`：同一 TLS 服务，用默认 dispatcher（夹具 CA 不在 Node 捆绑信任库），
  握手失败 → `fetch failed`、`non_blocking_error`。
- `https-connection-refused`：先绑定再关闭的回环端口，TLS 连接拒绝 → `fetch failed`。

自签证书信任方式比照 `tests/fixtures/model-stream`：夹具 CA/叶子是 `../model-stream/tls.json` 的
一次性测试材料（`127.0.0.1` SAN，非凭据）。TS 生成器用 undici `connect.ca` 注入；Rust 回放用
`HttpHookExecutor::with_roots`（与 `HyperModelTransport::with_roots` 同一测试路径）。生产 `new()`
使用 `webpki-roots`，与 model 传输一致；`NODE_EXTRA_CA_CERTS` 不被读取（见 RESULT 残余差异）。

回放：`tests/hooks_https.rs`。金样再生成测试 `hooks_https_fixtures_regenerate_identically_from_ts`
（`#[ignore]`）重跑真实 TS 并逐字节比对。负控：把 `HttpHookExecutor` 改回 plain HTTP client 后
`http_hooks_over_tls_match_real_ts` 必须转红（见 RESULT）。
