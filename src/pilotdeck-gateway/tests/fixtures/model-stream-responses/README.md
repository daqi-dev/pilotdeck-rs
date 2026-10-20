# Model stream（openai-responses）TS 对拍

固定基线 `cc7c4e65`；被测是 `streamModel.ts` 走 `openai-responses` 协议的路径（`providers/openai-responses/{request,stream}.ts`、`normalizeOpenAIUsage`、`resolveThinkingPlan` 等，`sourceSha256` 记 import 闭包）。生成器与回放装置沿用 `../model-stream/`（同一 mock、同一请求记录口径，回放共用 `tests/common/model_mock.rs`），只去掉 TLS 场景。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-stream-responses/generate.mts "$(mktemp -d)"
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test model_stream_responses
```

## 覆盖（19 例 / 21 次请求）

文本流（注释行、`response.in_progress`、空 delta、未知事件、`[DONE]`）；两个函数调用交错分片 + `arguments.done` / `output_item.done` 去重 + 重复 call_id 与缺 call_id 的合成 id（含响应 id 清洗）；三种 reasoning delta 与 chat 风格 usage（cached / cache_write / cost）；`response.incomplete` → `length`；`response.failed` 与 `error` 事件（不可重试，抛 ModelProviderError；空 code/message 回退默认文案）；HTTP 400 错误体；429 + `Retry-After: 0.015` 重试；断流（`TypeError: terminated`）；取消（服务端确认断连）；工具参数 jsonrepair 修复与修不好（`invalid_tool_arguments` 可重试 → interruption 错误事件）；`response.completed` 冲刷未完成调用；无完成标记的续写；请求体分支（指定函数 tool_choice、空 instructions、speed<0.5、metadata `String()` 化、outputSchema `strict:false` + schema 规范化、thinking disabled → `effort:"none"`、qwen-cloud bodyPatch）；不支持的 effort（ModelRequestError）；`/v1/responses` 404 → `/responses` 回退。

已知留白（DeepSeek rv P2-2）：回放的 `merge()` 只能覆盖、不能删除基础请求的键，所以「字段真缺省」的分支（无 `tools` / `metadata` / `systemPrompt` / `maxOutputTokens`、`tool_choice` 为 `none`/`required`）没有对拍用例。这些分支只由读码和单测保证，补全需要给夹具加整请求覆盖能力，已登记跟进。
