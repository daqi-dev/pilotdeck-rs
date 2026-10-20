# Model stream Google（provider HTTP 公共层 + google）TS 对拍

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测：`src/model/streaming/streamModel.ts` 的 Google 分支（`streamGoogleProviderRequest`）及其 import 闭包（`providers/google/{request,client,schema,stream,response,modelId}.ts`、`thinking/registry.ts`、`request/sanitizeProviderBody.ts` 等，`sourceSha256` 逐文件记录）。期望值全部来自真实 TS 运行，Rust 侧不写死。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --stack-size=8192 --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-stream-google/generate.mts "$(mktemp -d)"
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test model_stream_google
```

- 生成器先跑共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`，再用 `sourceClosureSha256` 记 `streamModel.ts` 的 import 闭包；断言 `en-US` locale。输出写到调用方给的目录（建议 `mktemp -d`），不覆盖仓库候选；连跑两次逐字节一致。
- 独占 mock：`127.0.0.1:0` 的 HTTP 服务（`tests/common/model_mock.rs`，沿用 model-stream 的 mock SSE 做法），外加一个已关闭端口（连接拒绝）。`Math.random` 固定 0.5，重试退避可复算。
- 请求记录：method、path、头（去掉 `transportHeaders` 列出的 fetch/undici 传输字段：host、content-length、user-agent、accept*、sec-fetch-mode、connection 等；名字小写、排序）、体逐字节；保持连接的回复额外记 `clientClosed`。SDK 的 `x-goog-api-client` 是协议头，保留并逐字节比较。
- 时间线：`event`（canonical 事件，含 `raw`）、`progress`（`onRetryProgress`）、`thrown`（抛出的错误：`name`、`message`，ModelProviderError 带 `error`，ModelRequestError 带 `code`）。origin 替换为 `$MOCK`/`$CLOSED`。
- `raw` 里的 `sdkHttpResponse`（SDK 附带的响应头，含当前时间）属于传输元数据而非 provider 协议投影，在生成器里剔除；两侧都不记录。

## 覆盖（28 例 / 28 次请求）

文本与 usage；多函数调用（responseId 派生、id 清洗与后缀去重、数组 args → `{}`）；thinking part（thought + thoughtSignature）；`finishReason` 的 `STOP`/`MAX_TOKENS`/`SAFETY`/`MALFORMED_FUNCTION_CALL`/未知值；schema 清理边界（`$defs`/`$ref`、`anyOf` 字面量合并、`const`→`enum`、`type` 去 null、非法属性名、`required` 过滤、array 补 `items`、不支持的 keyword）；多模态 contents 与 tool result/reference/media_reference；`extraBody` 深合并（`generationConfig.temperature` 剔除）；`$MOCK/v1beta` 与 `$MOCK/api` 端点；强制 toolChoice；thinking 不支持（`ModelRequestError` → error 事件，0 请求）；HTTP 401 JSON 错误体；HTTP 429 重试一次；HTTP 500 HTML 三次耗尽；断流（`terminated` → `timeout` + `streamInterruption.phase=text`）；空流（`IncompleteStreamError` → error 事件）；取消（`AbortError`，服务端确认连接被关）；请求校验失败（抛 `ModelRequestError`）；连接拒绝（重试耗尽 → error 事件）。修复轮 1 另加三例（真实 TS 生成）：自定义 `statusText: "Custom Bad"` 的非 JSON 错误体（P1-1）；大写 `Application/JSON` 走非 JSON 分支（P1-2）；tool_result / tool_result_reference 的孤立代理字符（P1-3）。

另有单元测试覆盖跨事件文本累积：`runtime/model/http_tests.rs` 的 `text_deltas_accumulate_under_the_v8_string_limit` 把 V8 上限缩到 4 个 UTF-16 单元，第三个 `text_delta` 经 `append_js_string` 报 `RangeError: Invalid string length`。

修复轮 2：`citation-metadata` 录制真实 SDK 对引文的映射（`citationSources` → `citations`，metadata 的未知字段丢弃、来源项保留、null sources → `{}`）。`schemaCases` 是真实 TS 对 128/512/1200/2000 层 properties 和 union 的紧凑结果；生成器显式使用 `--stack-size=8192`，只用于稳定录制深层算法结果，不把 V8 随 JIT/调用栈变化的 RangeError 点当作产品深度阈值。Rust 的 `schema_two_mib_stack_probe` 在 **2 MiB** 线程栈上经 `build_request` 比较深度和叶值，并覆盖枚举提取及深合并；需分别用 debug / `--release` 显式运行该 ignored 探针。

本轮禁止 git 且沙箱不允许监听：已保留正式生成器的 `bindSourceTree` / import 闭包守卫；本轮新增 case 由真实 `streamModel` + SDK 内存 fetch 录制，复核已有 46 个源码哈希，两次增量产物逐字节一致。全量带守卫的 loopback 重生成及回放仍由协调者执行，不能把增量录制当作本轮干净树证明。证据与边界见 `_tmp/20261020-a64-pr8/RESULT.md`「修复轮 2」。

`tool-name-object` 钉住名字值不经 String 强转：TS 原样投影工具名字对象，工具结果按 `??` 回退；checked 转换只用于确需字符串的路径。
