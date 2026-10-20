# Model stream OpenAI（chat completions）TS 对拍

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测：`src/model/streaming/streamModel.ts`（非 Google 路径）、`src/model/providers/openai/{request,stream}.ts`、`response/normalize{FinishReason,Usage}.ts` 及其共享 import 闭包（`sourceSha256` 逐文件记录）。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-stream-openai/generate.mts "$(mktemp -d)"
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test model_stream_openai
```

- 生成器先跑共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`，再用 `sourceClosureSha256` 对三个入口（streamModel、openai/request、openai/stream）取 import 闭包；断言 `en-US` locale。输出写到调用方给的目录（建议 `mktemp -d`），连跑两次逐字节一致。
- 独占 mock：`127.0.0.1:0` 的 HTTP 服务，外加一个已关闭的端口（连接拒绝）。`Math.random` 固定 0.5，重试退避可复算。TLS 传输层已由 `tests/fixtures/model-stream/` 覆盖，本夹具不做 TLS。
- 请求记录：method、path、头（去掉 `transportHeaders` 列出的 fetch/undici 传输字段；名字小写、排序）、体逐字节；保持连接的回复额外记 `clientClosed`。时间线：`event`（canonical 事件，含 `raw`）、`progress`、`thrown`。origin 替换为 `$MOCK`/`$CLOSED`。

## 覆盖（23 例 / 28 次请求）

正常文本（注释行、`id:`/`retry:` 行、emoji、usage 在末块）；UTF-8 多字节分片中间拆开 + BOM + CRLF + 非法字节；reasoning_content 递进与 `reasoning` 字段；多工具调用分片交错（空 name 块、增量拼接）；工具 id 缺失/重复（确定性 id 生成与 `_2` 去重）；finish_reason 全部取值（stop/length/tool_calls/function_call/content_filter/content_filter_results/未知/空串）；usage-only 末块；流内 error 可重试（server_error）/code 回退（timeout_error + 空 message）/不可重试抛出；HTTP 429 + Retry-After 0.02、401 不重试、500 HTML 三次耗尽；连接中途断开；无完成标记续写；不可修工具 JSON + length（`max_output_reached`）；可修工具 JSON（尾逗号 `{"q":"x",}` → `wasRepaired:true`）；取消（服务端确认 clientClosed）；空闲超时（clientClosed）；无版本段 URL 的 `/v1/chat/completions` 404 → `/chat/completions`；请求侧 tool pairing（缺 id 生成、重复 id `_2`、紧跟结果配对、缺失补 `[result truncated]`、孤立 tool 消息丢弃、reference 展开）；请求校验失败；连接拒绝重试耗尽。

已知缺口：Google-compatible OpenAI 主机的 `cleanSchemaForGoogle` 归 pr8。工具参数解析已复用 pr5 的 `jsonrepair` 模块（尾逗号例见上）；V8 字符串上限见 `openai.rs` 单元测试与 `probe3.out.txt`。
