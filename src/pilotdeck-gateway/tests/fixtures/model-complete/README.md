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

覆盖 57 例，三协议各含正常与空 body / 非 JSON：

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
- 2xx 形状回退（12 例）：`shouldUseEndpointResponse` 在非 stream 且多候选时
  clone + JSON-parse 每个 2xx body，再由 `isExpectedProviderResponseShape`
  按协议判形状。OpenAI 的 `{}`、HTML、全候选错形状会换 endpoint（最后候选
  返回原 body）；合法首候选立即停止。Anthropic 分别接受 `content` 数组与
  `type:"message"`；Responses 分别接受 `object:"response"`、`output` 数组与
  空字符串 `output_text`（truthiness 会误拒空字符串）。Anthropic 的 200 `{}`
  从 `/api/v1/messages` 换到 `/api/messages` 后成功（2 次请求）；Anthropic
  全候选错形状时返回末项 `{"last":true}`（canonical 为空 content、unknown）。
  Responses 的 200 `{}` 从 `/api/v1/responses` 换到 `/api/responses` 后成功
  （2 次请求）。这些 body 都很短；body 读取跨过请求 deadline 的真实时序由
  `../model-complete-timeout/` 的 flushHeaders 回环例覆盖。
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
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
timeout 600 cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test model_complete
```

只读固定基线；不访问外网、`~/.pilotdeck` 或用户运行中的 PilotDeck。

## 负控

```sh
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
timeout 1800 python3 \
  src/pilotdeck-gateway/tests/fixtures/model-complete/negative-controls.py
```

七个基线负控：Responses 委派给 chat parser、OpenAI finish reason 改成
`tool_calls`、Responses failed 改成 unknown、Anthropic `refusal` 改成 unknown、
工具 id 去重后缀从 `_2` 改成 `_1`、complete 对 429 重试、complete 内层重试
边界改成 `<=`（小数 1.5 上限的请求数与 exhausted 错误形状会变）。另加八个
r5 变异：complete 请求超时改读 stream idle timeout（`model-complete-timeout`
的 300ms 例会在 10ms 抛错）、aborted 消息不再优先于 premature（transport
abort 例从 1 次变 3 次）、请求体多插一个字段（逐字节 body 比较失败）、
usage cache 字段丢弃、Responses `output_text` 去重条件反转、Anthropic
`stop_sequence` 视为 unknown、仅空白 tool-call id 改回旧的非 trim 判空
（`openai-tool-id-whitespace` 不再生成 id）、请求超时改成
`duration(timeout_ms * 10.0)`（100ms 例约 1000ms，超过 600ms 上界）。再加两个
r6 形状绕过负控：在 `http.rs` 的形状调用处分别把 Anthropic / Responses 的
判断按协议短路为真，使首候选 200 `{}` 被误接受；只有目标新例
`anthropic-200-empty-endpoint-fallback` /
`responses-200-empty-endpoint-fallback` 的 `request count`（或 canonical）
断言失败才算抓住，输出必须含目标用例名。再加两个 r7 lib 负控（只走 `--lib`
指定 full-path 测试，其余仍走 `model_complete` 集成 target）：主循环 cap
比较由 `>` 放宽为 `> limit + 1`，目标跨帧测试的 limit+1 拒绝/不保留前缀断言
必须失败；把 `string_limit()` 的唯一共享默认尾部改成 `usize::MAX`（等价 r5
off-tree M9 控件），由钉死 `0x1fffffe8` 字面量的默认值断言抓住，不再用同一
常量与自身比较。r8（#641 fix-r1）把 `read_shape_probe` 与 `response_text`
的逐行重复状态机抽成共享 `CappedText`（解码、UTF-16 计数、超限丢弃并排空到
EOF、EOF 残片计入），r7 的 cap 负控随之改锚到 `CappedText::push` 并更名
`capped-text-cap-allows-limit-plus-one`：原两行锚文本在 #641 把同款循环复制进
`response_text` 后于 http.rs 出现两次，脚本自身的锚点检查报
`mutation anchor count 2` 并跳过该控件，负控当时已失效。另加两个 r8 lib 负
控：`CappedText::finish` 去掉 EOF 残片 U+FFFD 的计数（目标
`response_text_counts_eof_replacement_toward_the_cap` 的 limit+1 断言必须失
败）；`response_text` 循环在跨限帧上提前 `break`（目标
`complete_response_text_overlong_body_reads_to_eof_then_throws_plain_error`
的 70ms EOF 下限必须失败）。r9（#647）把 `CappedText` 的计数从 UTF-16 码元
改为收到的 UTF-8 字节（node v22.22 实测见
`_tmp/20261025-issue-647-probe/RESULT.md`）：cap 负控锚点随 `push` 改写；
「EOF 残片 U+FFFD 计数」控件按新语义删除（EOF 不再新增计数），换成
「计回 UTF-16 码元」控件 `capped-text-cap-counts-utf16-units`（目标
`response_text_cap_counts_received_bytes_not_utf16_units` 的多字节超限断言
必须失败）；`response-text-returns-before-eof` 的判据从 70ms 墙钟下限改为
目标测试断言的 EOF 标志（脚本化的 body 被 poll 到末尾），不再依赖帧间距。
恢复、进程组回收与 cargo 槽位逻辑未改。共 21
条，每个变异都先编译，只接受已编译测试的断言失败。#1111 把 complete 解析
家族（含 `retry_decide_request` / `is_retryable_request_error` / `complete_model`
与各 parser）搬进 `http/complete.rs` 后，锚在那些字节的 12 条控件与
`--self-test-hold` 锚点改指新文件：表行各带一个 `HTTP` / `COMPLETE` 路径字段，
锚文本与判据不变；锚在共享传输（shape probe / `CappedText` / `response_text` /
超时 / 请求体）的 9 条仍指 `http.rs`。

恢复：变异前把原始字节与 SHA-256 写到 worktree 外的状态目录（默认
`$TMPDIR/pdrs-negctl-<worktree哈希>`），SIGTERM/SIGINT/SIGHUP 处理器同步恢复；
SIGKILL 无法执行处理器，下一次启动会因源文件仍等于记录的变异哈希而拒绝运行。
入口另外统一走 `negative_process.mutating_sources`：先取树内共享
`.negative-source.lock` 并逐字节确认整个 gateway 产品树等于 HEAD，才允许写第一个
变异；因此其它负控组 SIGKILL 留下的残留也会让本脚本拒绝启动，而不是把残留当原件。
`--recover` 是唯一例外：它按自己的 sidecar 哈希恢复已记录的变异，无法要求源文件等于
HEAD；除此之外 `--self-test-hold` 与正常负控都在同一把锁和同一基线校验内。

处理器同时记录当前 `run_bounded` 的进程组：收到上述信号时先对该组做**有界**
回收（SIGTERM，宽限 `NEGCTL_SIGNAL_GRACE`（默认 5s）后 SIGKILL），确认组已结束，
再恢复源码并清状态；直接子进程用 `waitpid(WNOHANG)` 回收，避免僵尸占住进程组。
处理器有重入保护，第二个信号不会再次进入有界等待。正常结束、`TimeoutExpired`
与其它异常路径同样在 `finally` 中清掉进程组跟踪；超时路径会向整组升级 SIGKILL。
SIGKILL 本身仍不声称有处理器。

```sh
python3 …/negative-controls.py --check-state   # 有未恢复变异时退出 3
python3 …/negative-controls.py --recover       # 只恢复与记录变异哈希一致的源文件
python3 …/negative-controls.py --self-test-timeout  # 进程组回收实测
python3 …/negative-controls.py --self-test-hold     # 供外部信号测试持住真实变异
```

cargo 槽位在变异**之前**检查并有界等待；`ps` 被沙箱拒绝时回退到 `pgrep -x`
的 cargo 入口名，两个探针都失败则拒绝变异（绝不把失败当 0）。子进程以独立
进程组启动，超时或收到信号都先 SIGTERM 再 SIGKILL 整组并回收。
