# Gateway 用户对话 HTTP store 夹具（A7.4-pr2）

`oracle.json` 来自固定 TS 基线（`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`）的真实运行：
真实 `HttpGatewayUserDialogStore`（client / handler / `startGatewayUserDialogStoreHttpServer`）、
真实 `FileGatewayUserDialogStore`，以及真实 Node HTTP 栈（Fetch 与原始 socket 两种 framing）。
生成器用 `baseline_source_guard.mjs` 校验基线树与 Git blob：`baselineCount` 为基线 `src/` 下
全部 1000 个受信文件（逐个比对 Git blob），`sourceSha256` 覆盖 TS 相对 import 闭包 650 个文件
（含 `HttpGatewayUserDialogStore.ts`、`FileGatewayUserDialogStore.ts`、`GatewayUserDialogStore.ts`、
`src/gateway/index.ts` 等）。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/user-dialog-http-store/generate.mts /abs/output-parent
```

输出父目录须已存在；每次运行在其下 `mkdtemp` 并以 `flag:'wx'` 写 `oracle.json`；两次运行逐字节一致才更新正本。
提交时 `sha256(oracle.json) = 8b6a33d027b2e3131fce9f5f8c8a7d09c47ebac3c2fb154d3835b3d5ac27e5df`。
条数：clientRequests 30 · clientResponses 69 · handlerCases 111 · socketRequests 18 · flow 16 ·
constructors 29 · handlerConstruction 7。

## 各段

| 段 | 内容 |
|---|---|
| `clientRequests` | TS client 对真实 TS server 的 30 个请求：URL/header/body 字节，及 client 结果（含 `1e21`、`-0`、`2^53-1`、Unicode、空 key、endpoint 归一化 10 变体） |
| `clientResponses` | 直接替换 `fetch` 注入 69 种响应：非 2xx 文本/JSON/空体、版本与 result 校验、BOM、尾随垃圾、重复 key、已存 dialogs、lease/owner/answer 断言、abort、网络错误 |
| `handlerCases` | 直接调用 TS handler 的 111 个请求：方法、坏 JSON、超大 body、未知/转义 operation、缺字段、重复 header、store 抛错；记录 status/content-type/body 与 store 调用参数 |
| `socketRequests` | 原始 socket 打到真实 `http.createServer`：18 个 framing 用例（HEAD、重复 header、1 MB body、坏 UTF-8、超长 URL 等） |
| `flow` | 真实 client → 真实 server → 真实 FileStore 的 16 步顺序流程（claim/owner/answer/release/clear），固定时钟与 uuid |
| `constructors` | 29 个构造输入：endpoint 归一化、非法 URL/scheme、token、timeout |
| `handlerConstruction` | 7 个 handler 构造/请求分发输入：缺 store、缺 token、缺 method |

`flow` 固定 `now = 2026-01-01T00:00:00.000Z`，uuid 依次 `lease-1`、`lease-2`；Rust 回放通过
`DialogRuntime` 注入同样时钟与序列。

## 类型层跳过（不是遗漏）

- `timeoutMs: '1000'`（字符串）与 `token: undefined` 在 TS 类型层就不可达，未录制；Rust 类型同样不接受。
- handler 的 `missing-method`（无 `method` 字段）在 TS 走 `undefined !== 'POST'`，Rust 的
  `HttpStoreHttpRequest.method: String` 表达不了缺失；只录制 `missing-store` / `missing-token`。
- 网络错误名（`TypeError` / `AbortError`）在 Rust 侧统一为 `DialogError::Invalid` 的 message，
  逐字比较 message，不比较 JS 错误类名。

## 说明

- `handlerCases` 的重复 `authorization` 头经 Fetch `Headers` 合并为 `", "`；`socketRequests`
  的同名头按 Node 规则取第一个（第二个丢弃），服务端包装层单独处理，二者不矛盾。
- 原始 socket 用例必须自带 `Host` 头（生成器 `rawHttp` 会补 `127.0.0.1:<port>`），否则 Node
  在进入 handler 前就以 400 关闭连接。
- `huge-invalid-json` 的 1 MB body 在 Node 端会先写 400 再断连，读数可能伴随 ECONNRESET；
  生成器按记录的完整响应字节为准。
