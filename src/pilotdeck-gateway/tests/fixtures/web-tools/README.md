# web_fetch / web_search TS 对拍

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测：`src/tool/builtin/webFetch.ts`、`webSearch.ts` 及其 import 闭包（`additionalWebSearch.ts`、`web/*`、`network/fetch.ts` 等，`sourceSha256` 逐文件记录）。期望值全部来自真实 TS 运行。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/web-tools/generate.mts "$(mktemp -d)"
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test web_tools
```

- 输入在 `cases.mjs`；生成器跑共享溯源守卫，断言 `en-US`，`Math.random` 固定 0.5（networkFetch 退避抖动），输出以 `wx` 写到调用方给的空目录；连跑两次逐字节一致。
- 独占 mock：`127.0.0.1:0` 的 HTTP 与 HTTPS（复用 `../model-stream/tls.json` 的测试 CA），外加已关闭端口。origin 记为 `$TLS`、`$PLAIN`、`$UPGRADE`（同一 TLS 端口的 `http://`，验证 HTTP→HTTPS 升级）、`$CLOSED`。每个回复带 `content-length` 与 `connection: close`，原因短语按 Node 实发值记进 `replies[].reason`，Rust mock 照发。
- 请求记录：method、path、头（去掉 `transportHeaders`；`user-agent` 仅在等于 fetch 默认值 `node`/`undici` 时去掉）、体。
- `html-turndown-missing`：`no-turndown.mjs` 加载钩子让 `?no-turndown` 模块图里的 `turndown` 按「包未安装」解析失败（Node 22 `ERR_MODULE_NOT_FOUND` 原文，父路径固定为 `/pilotdeck/src/tool/builtin/web/urlFetcher.ts`）。这是 Rust 侧未注入 HTML 转换器时的行为；真实 turndown 转换由 A6.5-pr9b 补。
- 大正文（`hashOnly`）只记 `content[].text` 的 UTF-16 码元 SHA-256 与长度。
- `cacheOps`：`WebFetchUrlCache` 在逐步固定的 `Date.now` 下的 TTL 边界、LRU 顺序、覆盖、50 MiB 驱逐与超大跳过，每步记私有 map 的键序与字节总数。
- `preapprovedEntries`：基线导出的整张 `PREAPPROVED_ENTRIES`（136 条）原样入 oracle，回放逐条比对 host/pathPrefix/allowSubdomains 与顺序。
- 响应体取消不在 oracle 内（基线在收到响应头后就摘掉了父 signal 监听，见 `_tmp/20261020-a65-pr9/probes/`）；Rust 侧用仅测试可见的 `holdAfterHeadersMs` 让 mock 先回头再停住建，验证父取消能罩住 body 读取，负控为同一回复在不取消时继续挂起。

## 覆盖

fetch 45 例（114 次请求含 search）：raw 文本/JSON/二进制/无类型/空/非法 UTF-8/截断（含代理对切分）/10 MB 边界与超限；同主机重定向、跨主机与协议变化不跟随、相对路径、缺 Location、空 Location、303、恰好 10 跳成功、第 11 跳超限；出口代理拦截；404/410/401 二进制/418 长预览/自定义原因短语/429 Retry-After 重试耗尽/503 后恢复；缓存命中与键为原始输入；LLM 模式：无模型、经 context 注入、预批域名（缓存种子）、空白输出、error 事件、Error/规范错误/字符串抛出、中途取消。search 39 例：glm（含 env key/endpoint、code/error 字段、500 重试、坏 JSON、null JSON）、tavily、serper、brave、baidu、bocha、exa、serpapi（引擎、gl、错误、密钥脱敏）、custom（POST/GET、bearer/query/body 密钥、none、端点缺失/非法、code 错误）、env 选 provider、连接拒绝。另有 URL 校验 21、升级 9、重定向判定 10、预批 14、二级提示 3、输入校验 15、可用性 6、缓存操作 17。

## 已知偏离（Rust 不发送的 undici 隐式头）

undici 自动加的 `accept-language`、`sec-fetch-mode`、`accept-encoding` 及默认 `user-agent` 不属于工具投影，两侧都不比较。
