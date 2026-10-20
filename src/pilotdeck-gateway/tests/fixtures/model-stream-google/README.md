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

## 覆盖（29 例 / 29 次请求）

文本与 usage；多函数调用（responseId 派生、id 清洗与后缀去重、数组 args → `{}`）；thinking part（thought + thoughtSignature）；`finishReason` 的 `STOP`/`MAX_TOKENS`/`SAFETY`/`MALFORMED_FUNCTION_CALL`/未知值；schema 清理边界（`$defs`/`$ref`、`anyOf` 字面量合并、`const`→`enum`、`type` 去 null、非法属性名、`required` 过滤、array 补 `items`、不支持的 keyword）；多模态 contents 与 tool result/reference/media_reference；`extraBody` 深合并（`generationConfig.temperature` 剔除）；`$MOCK/v1beta` 与 `$MOCK/api` 端点；强制 toolChoice；thinking 不支持（`ModelRequestError` → error 事件，0 请求）；HTTP 401 JSON 错误体；HTTP 429 重试一次；HTTP 500 HTML 三次耗尽；断流（`terminated` → `timeout` + `streamInterruption.phase=text`）；空流（`IncompleteStreamError` → error 事件）；取消（`AbortError`，服务端确认连接被关）；请求校验失败（抛 `ModelRequestError`）；连接拒绝（重试耗尽 → error 事件）。修复轮 1 另加三例（真实 TS 生成）：自定义 `statusText: "Custom Bad"` 的非 JSON 错误体（P1-1）；大写 `Application/JSON` 走非 JSON 分支（P1-2）；tool_result / tool_result_reference 的孤立代理字符（P1-3）。

另有单元测试覆盖跨事件文本累积：`runtime/model/http_tests.rs` 的 `text_deltas_accumulate_under_the_v8_string_limit` 把 V8 上限缩到 4 个 UTF-16 单元，第三个 `text_delta` 经 `append_js_string` 报 `RangeError: Invalid string length`。

修复轮 2：`citation-metadata` 录制真实 SDK 对引文的映射（`citationSources` → `citations`，metadata 的未知字段丢弃、来源项保留、null sources → `{}`）。`schemaCases` 是真实 TS 对 128/512/1200/2000 层 properties 和 union 的紧凑结果；生成器显式使用 `--stack-size=8192`，只用于稳定录制深层算法结果，不把 V8 随 JIT/调用栈变化的 RangeError 点当作产品深度阈值。Rust 的 `schema_two_mib_stack_probe` 在 **2 MiB** 线程栈上经 `build_request` 比较深度和叶值，并覆盖枚举提取及深合并；需分别用 debug / `--release` 显式运行该 ignored 探针。

本轮禁止 git 且沙箱不允许监听：已保留正式生成器的 `bindSourceTree` / import 闭包守卫；本轮新增 case 由真实 `streamModel` + SDK 内存 fetch 录制，复核已有 46 个源码哈希，两次增量产物逐字节一致。全量带守卫的 loopback 重生成及回放仍由协调者执行，不能把增量录制当作本轮干净树证明。证据与边界见 `_tmp/20261020-a64-pr8/RESULT.md`「修复轮 2」。

`tool-name-object` 钉住名字值不经 String 强转：TS 原样投影工具名字对象，工具结果按 `??` 回退；checked 转换只用于确需字符串的路径。

修复轮 3：`prototype-schema-and-extra-body` 经真实 `streamModel`/SDK 同时覆盖工具 schema、output schema 和 extraBody 的 `__proto__`。另有 17 条 `prototypeSchemaCases` 与 8 条 `prototypeMergeCases`，验证对象赋值与 spread 的差别、null/标量原型、继承属性的 required/enum 合并、重复 union 内的原型函数，以及未合并的 const/default/嵌套 payload 保留 own `__proto__`。SDK 的 private deepMerge 从固定安装包原文截取并在内存执行；完整请求另覆盖 SDK 在 deepMerge 前的 `patchHttpOptions` 浅赋值。

### 深层夹具的适用范围

`schemaCases` 的 128/512/1200/2000 层期望仅验证扩栈（`--stack-size=8192`）下真实 TS cleaner 的算法输出，**不证明默认 Node 栈下端到端成功，也不是默认栈与 Rust 的深度边界对拍**。Rust 的 2 MiB 探针止于 `build_request` 与结果遍历，不涵盖指纹/HTTP 请求序列化。

2026-10-21 实测：Node v22.22.0 / V8 12.4.254.21-node.33，本机默认 `--stack-size=864` KiB。沿用本夹具基础请求，把工具 schema 从 `{const:"leaf"}` 向外包装 depth 次 `{properties:{a:...}}`，每个深度启动独立进程，运行真实 `streamModel` → Google SDK → 内存 fetch（末端替换传输，未替换 cleaner 或 SDK）：

| 条件 | 最后成功 | 首次 RangeError | 邻界复核 |
|---|---:|---:|---|
| 冷进程、默认栈 | 1229 | 1230 | 两侧各 3 次一致 |
| 先完成 20 次 32 层请求，再测目标深度 | 1291 | 1292 | 两侧各 3 次一致 |

2000 层在默认栈下先产生 `request_started`，随后 error 事件，`error.raw.name=RangeError`、message=`Maximum call stack size exceeded`，实际 fetch=0；Google 分支把原异常包装进 error 事件，并非直接从异步迭代器 throw。相同请求加 `--stack-size=8192` 后成功，fetch=1。边界受版本、输入形状、调用栈和预热影响，以上仅是记录条件下的实测；Rust 不增加模拟 V8 爆栈的层数限制。原始输出、二分/复跑探针与负控见 `_tmp/20261020-a64-pr8/RESULT.md`「修复轮 3」。

本轮仍禁止任何 git：README 首部的正式生成命令会调用 `bindSourceTree`，未执行，保留给协调者在允许 git/loopback 的环境连续运行两次。实际完成的是复核 46 项既有源码哈希后的真实 TS 增量录制两次逐字节一致，不能代替完整干净树溯源和 loopback 验收。
