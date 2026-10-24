# native complete_model TS 金样（#610 / #611）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器先用
`baseline_source_guard.mjs` 绑定整个 `src` Git blob，再记录
`src/model/streaming/streamModel.ts` 的相对 import 闭包 SHA-256。Rust replay
由 `tests/common/baseline.rs` 核查来源。

真实调用链：TS `complete(request, config)` → `validateModelRequest` →
`buildModelRequest` → 真实回环 HTTP mock → `parseRawJson` →
`parseModelResponse`。Rust 侧通过 `ModelHttpTransport` 的 `Peer` + body
frames 喂同一状态码、响应头和响应体字节，真正调用
`runtime::model::http::complete_model`，再按 `JSON.stringify` 比较 canonical
response 或抛出的 `ModelProviderError`；不是只测自写 parser。

覆盖 45 例，三协议各含正常与空 body / 非 JSON：

- anthropic：thinking+signature 与 `thinking.text` 回退、未知 block 忽略、text、
  tool_use、usage（含 cache 与 totalTokens）、`end_turn` / `tool_use` /
  `refusal` / `stop_sequence` / `max_tokens` / 未知 stop reason。
- openai：`reasoning_content` 字符串与对象形式、`reasoning_details` 多段、
  string / array content、重复与缺失 tool-call id 的生成与去重、
  `stop`/`tool_calls`/`content_filter_results`/未知 finish reason、usage
  （cache、nativeCost）。
- openai-responses：`output_text`、message/output_text 文本、reasoning
  summary、function_call（call_id/id/arguments repair）、`output_text` 去重、
  completed/incomplete/failed/queued 对应 stop/length/error/unknown、usage。
- `openai-tool-id-suffix-collision` / `responses-tool-id-suffix-collision`
  真正让生成 id 撞上已用的 incoming id，从而进入 `_2` 去重分支；
  `openai-tool-id-sanitize-run` 钉住 `safeToolCallIdPart` 的连续非法字符合并
  与首尾下划线裁剪。`openai-tool-id-whitespace` 证明仅含空白的 id 走生成；
  `openai-tool-id-padded` 证明非空 id 原样保留两侧空格；
  `openai-choice-index-2` 用 `choice.index`（而非数组下标）生成 id；
  `openai-arguments-missing` 走 `{}`，`openai-arguments-empty` 抛
  `invalid_tool_arguments`。
- `responses-output-text-mismatch`：剩余 message 文本与 `output_text` 不同，
  TS 不做去重，两个 text block 都保留。
  `responses-response-id-sanitized-collision`：id 清洗为空后回退
  `response`，且生成 id 撞上已用 `call_response_1`，真正进入 `_2` 分支。
- 负控金样：`responses-body-parsed-as-openai` 与
  `openai-body-parsed-as-responses` 证明协议分派不是 chat parser 的代称；
  `responses-normal` 若回退到 chat parser 会得到空 content 和 unknown。
- 非 2xx：401 为 `auth_error` 且 `/api` 的两个候选 endpoint **不** fallback；
  500 HTML 与 503 纯文本保留原始 body 字符串；400 / 404 / 405 在
  `/api`（候选 `[base/v1/chat/completions, base/chat/completions]`）会换到
  第二个 endpoint 成功，金样锁两次请求的 method/path/body 序列。
- 429 + `Retry-After`：TS `complete()` 对非 2xx **不重试**，只抛
  `ModelProviderError`（`retryable: true`）；响应头不进入 canonical error
  的 `retryAfterMs`，body 里的 `retryAfterMs` 保留。金样同时锁请求数 = 1。
- 非法 tool arguments：抛 `invalid_tool_arguments`，message 与 TS 逐字一致，
  `raw` 是单个 tool-call item。
- Rust replay 对每一例比较实际 wire 的 method、path、完整 body 与
  `requestCount`，不是只比 canonical response + 请求数；fallback 例按顺序
  回放每个 endpoint 的响应。

传输层重试（`complete()` 的 `fetch` 失败、小数上限、abort 分类、timeout）
在 `../model-complete-transport/`；真实回环 socket 的 complete 请求超时在
`../model-complete-timeout/`。两者都用受控/真实传输驱动真实 `complete()`，
不与本文件的 HTTP mock 混在一起。

## 重新生成

```sh
export PDRS_BASELINE=/absolute/PilotDeck
export LANG=en_US.UTF-8
out=$(mktemp -d /tmp/pdrs-model-complete.XXXXXX)
timeout 180 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-complete/generate.mts "$out"
cmp "$out/oracle.json" src/pilotdeck-gateway/tests/fixtures/model-complete/oracle.json
```

`sourceSha256` 依赖固定基线 checkout 与 Node v22；生成器断言 locale 为
`en-US`。`#[ignore]` 测试 `regenerates_model_complete_oracle_identically`
执行同样的重生成并逐字节比较。

## Replay

```sh
export CARGO_TARGET_DIR=$PWD/target
timeout 600 cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test model_complete
```

只读固定基线；不访问外网、`~/.pilotdeck` 或用户运行中的 PilotDeck。

## 负控

```sh
export CARGO_TARGET_DIR=$PWD/target
timeout 1800 python3 \
  src/pilotdeck-gateway/tests/fixtures/model-complete/negative-controls.py
```

七个基线负控：Responses 委派给 chat parser、OpenAI finish reason 改成
`tool_calls`、Responses failed 改成 unknown、Anthropic `refusal` 改成 unknown、
工具 id 去重后缀从 `_2` 改成 `_1`、complete 对 429 重试、complete 内层重试
边界改成 `<=`（小数 1.5 上限的请求数与 exhausted 错误形状会变）。另加七个
本轮变异：complete 请求超时改读 stream idle timeout（`model-complete-timeout`
的 300ms 例会在 10ms 抛错）、aborted 消息不再优先于 premature（transport
abort 例从 1 次变 3 次）、请求体多插一个字段（逐字节 body 比较失败）、
usage cache 字段丢弃、Responses `output_text` 去重条件反转、Anthropic
`stop_sequence` 视为 unknown、仅空白 tool-call id 改回旧的非 trim 判空
（`openai-tool-id-whitespace` 不再生成 id）。每个变异都先编译，只接受已
编译测试的断言失败。

恢复：变异前把原始字节与 SHA-256 写到 worktree 外的状态目录（默认
`$TMPDIR/pdrs-negctl-<worktree哈希>`），SIGTERM/SIGINT/SIGHUP 处理器同步恢复；
SIGKILL 无法执行处理器，下一次启动会因源文件仍等于记录的变异哈希而拒绝运行。

```sh
python3 …/negative-controls.py --check-state   # 有未恢复变异时退出 3
python3 …/negative-controls.py --recover       # 只恢复与记录变异哈希一致的源文件
python3 …/negative-controls.py --self-test-timeout  # 进程组回收实测
python3 …/negative-controls.py --self-test-hold     # 供外部信号测试持住真实变异
```

cargo 槽位在变异**之前**检查并有界等待；`ps` 被沙箱拒绝时回退到 `pgrep -x`
的 cargo 入口名，两个探针都失败则拒绝变异（绝不把失败当 0）。子进程以独立
进程组启动，超时先 SIGTERM 再 SIGKILL 整组并回收。
