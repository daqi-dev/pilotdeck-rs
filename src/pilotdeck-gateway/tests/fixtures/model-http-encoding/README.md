# #867 共享模型 HTTP 编码金样

`generate.mts` 在 Node **v22.22.0** 上调用真实 `fetch` 和真实 loopback HTTP server，不复制 fetch 或产品函数体。固定 `cc7c4e6` 的模型、EdgeClaw 源闭包由 `baseline_source_guard.mjs` 只读核验；本夹具的运行时判据是 Node 内置 fetch 的字节行为。输出目录递归创建，`oracle.json` 用 `wx` 独占写入。

- `cases`：gzip、x-gzip、deflate（zlib/raw）、br、identity、大小写、反序编码栈、多 gzip member、未知整串透传、坏流、空流、头/正文/尾截断、HEAD、204、205；同时记录保留的 wire headers。
- `headers`：Latin-1、宽字符、孤立代理项、trim、重复头、控制字符、CR/LF、非法/宽字符/代理项头名、整 record 的 ByteString 转换顺序。
- `lifecycle`：三种 codec 在服务端发送第一段并停住时得到明文；尾段随后释放或前驱永不完成时取消。
- `endings`：完整压缩成员到达仍不能代替 HTTP EOF，分别释放终止块、取消、断链。`interrupted` 则在成员中途断链。此处明确使用 chunked + keep-alive；Node 对 `Connection: close` 的额外容忍不充当正常 HTTP EOF 判据。
- `progress`：wire 每 400 ms 送一个 gzip 头字节，bodyTimeout=1000 ms，1600 ms 后送完；正常得到明文，但独立 900 ms reader watchdog 仍到期。Rust 用 paused clock 和受控 body frame 覆盖这两格；空 data frame 只传递 raw 活动，不冒充明文。

Node 小型 loopback 有 10 秒 watchdog；末尾同步的大体积子进程另受 120 秒 timeout 约束（详见下节）。所有响应与 agent/server 在 finally 关闭。Rust loopback 等待有 10 秒上限，所有 server task 均 join。`encoding_fixture_regenerates_identically` 是 ignored 字节重生成守卫。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-867/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
scripts/cargo-slots.sh
cd src
cargo test -p pilotdeck-gateway --lib encoding_tests
cargo test -p pilotdeck-gateway --lib encoding_fixture_regenerates_identically -- --ignored
```

`negative.py --check-anchors` 只读核锚点。正式变异须产品源码已经提交且与 HEAD 一致；`negative_process.mutating_sources` 持树锁、核对 HEAD 并还原，cargo 由 `run_owned` 管理，调用前 `wait_for_slot`。脚本必须继承调用方的 `CARGO_TARGET_DIR`。每项做正控 → 变异精确测试 → 还原正控，逐项记录 KILLED / SURVIVED / INVALID，最后统一非零判失败；编译失败、无关 panic、超时、锚点过期均不是 KILLED。点名测试每个本地失败点均有 `#867` 消息，杀死判据绑定命名测试的 panic 块。

全套 51 个变异需多次重编，交协调者宿主执行；本树未提交时应拒绝，不能创建副本仓库或绕过守卫。证据目录由调用者指定并独占创建：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/model-http-encoding/negative.py _tmp/20261026-issue-867/negative-host
```

## fix-r1 两条安全边界

- `layers` 用真实 Node fetch 测 5/6 层 gzip、br、identity、custom，以及 HEAD。Node v22.22.0 内置 undici 6.23.0 上限为 5；第 6 层在返回 response 前拒绝，外层 `TypeError: fetch failed`，cause 为 `too many content-encodings in response: 6, maximum allowed is 5`（无 cause.code）。总数检查先于 unknown token 判断，HEAD 仍为空正文。Rust 在构造解码器前检查，与传输层既有接口一样保留外层消息和 cause.code。
- `sizes` 是 gzip/br 各 16384 与 16385 字节的重复 ASCII payload；Rust 注入已有 `LimitGuard=16384`，压缩输入每 7 字节分帧，解码输出跨 8192 字节缓冲。`sizeProbe` 来自独立 `size-probe.mjs` 子进程，对真正的 `MAX_STRING_LENGTH=536870888` 与 +1 各跑 gzip/br fetch；前者成功，后者为普通 `Error` / `ERR_STRING_TOO_LONG`，不是 `RangeError` 或 `terminated`。子进程仅记录长度、边缘字节和错误，无大明文落盘；生成器限制该子进程为 120 秒，每个 fetch 为 60 秒。
- `decoded_size_boundary_rejects_before_text_allocation` 观察真实文本转换入口的 test-only 字节计数，确认超限帧在文本累计分配之前被拒，并验证 `response_text` 的错误名/消息。固定解码缓冲和 codec 内存仍会使用；这里不声称解码零分配。`decoded_size_boundary_shared_text_reader` 补 EdgeClaw 所用共享 reader 的同一组 exact/+1 边界。既有 drain 到 EOF、错误优先级与流式 SSE 行为不变。
- 现有 ignored 重生成守卫覆盖新增字段及真实大边界探针；父生成器仍使用 `recursive:true` / `flag:'wx'`。

新增 6 个变异（fix-r1 时全套 34 个）：`encoding-layer-cap`、`encoding-layer-exact`、`encoding-layer-unknown`、`decoded-size-cap`、`decoded-size-late-allocation`、`decoded-size-shared-cap`。后三个分别去掉模型大小上限、让超限帧提前进入文本转换、去掉共享 reader 上限。每个变异在精确测试前先 `cargo test --no-run`，失败列 INVALID 后继续下一项，最后统一汇总。已有源码未提交时仍拒跑，不绕过 HEAD 守卫；协调者提交后可只跑本轮 6 项：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/model-http-encoding/negative.py \
  _tmp/20261026-issue-867/fix-r1/negative-host \
  encoding-layer-cap encoding-layer-exact encoding-layer-unknown \
  decoded-size-cap decoded-size-late-allocation decoded-size-shared-cap
```

## fix-r2 编码令牌与重定向边界

- `tokenBoundaries`：五个 gzip 加尾逗号、六个空令牌、多行 3+3 均超限；多行 3+2 正常解码；空值正常；`gzip,,gzip` 与空白 token 保留**完整原始 wire bytes**，不做部分解码；204/304 先清空正文再绕过层数检查。Rust 用真实重复响应头回放并比较 hex，不把压缩字节经过有损 UTF-8 转换。空 token 的已有产品逻辑未改。
- Node v22.22.0 内置 undici 6.23.0 的 `internal/deps/undici/undici` 第 11376 行以 `split(',')` 保留空 token；11382–11405 从后往前解析，遇未知或空 token 将 `decoders.length` 清零；11373–11374 对非空 Location + follow + 301/302/303/307/308 跳过整个解码段。可用 `process.binding('natives')['internal/deps/undici/undici']` 检查实际运行二进制内嵌源，而非猜测依赖版本。
- `redirects`：五种状态分别测 follow/manual；默认 follow 得到 target，manual 六层失败。额外覆盖无 Location、空 Location、200 + Location、终点六层，以及正文永不结束/在目标请求到达后断开的前驱。Node 用 `flushHeaders` 与目标请求作确定性钩子；Rust 使用既有受控 chunked server 与显式 release，所有任务有界 join。
- Rust 模型入口使用默认 follow：`model/http.rs`、`model/count_http.rs`、`memory/edgeclaw/llm/transport.rs` 通过共享 `fetch_redirect::follow`。共享 follow 在 Request extensions 中传递 `FollowRedirects` 标记，`HyperModelTransport` 只对带标记请求豁免。网页抓取 `tools/builtin/web/network.rs:433` 直接调用同一 transport，对应 TS `tool/builtin/web/urlFetcher.ts:101` 的 manual 模式；未标记请求保留检查与解码行为，Rust 同时回放五种状态的 manual 金样。空 Location 仍须在传输层拒绝，避免后续重试/重定向报同一外层错误掩盖错误豁免。
- TS `streamModel.ts:741–747` 未指定 redirect，经 `network/fetch.ts:194–202` 调 npm undici；本机该包实际为 8.3.0。EdgeClaw `llm-extraction.ts:2347–2352` 直接调用 global fetch，也未指定 redirect。本轮另用真实 `networkFetch` 复测上述 25 格；HTTP 行为相同，网络错误由其包装为 `NetworkFetchError`。金样版本仍严格为 Node v22.22.0 的 global fetch。

新增 17 个负控：十一个 `redirect-*` 分别去掉 follow 标记、忽略 manual 模式、恢复旧检查、逐个移除五种状态、放宽状态、放宽无 Location/空 Location；六个 `token-*` 分别过滤空令牌、只读首行、误用部分解码、拒绝空值、移除 204/304 短路。变异绑定完整命名测试及 `#867` 专属失败消息，body 解码阶段的专属 `expect` 也计入预期失败。每项先编译再精确运行，全部汇总；任何未提交产品 diff 会被 HEAD 守卫拒绝，新增变异编译与执行须在协调者提交后进行。

```sh
python3 src/pilotdeck-gateway/tests/fixtures/model-http-encoding/negative.py \
  _tmp/20261026-issue-867/fix-r2/negative-host \
  redirect-follow-marker redirect-manual-mode redirect-decode-intermediate \
  redirect-status-301 redirect-status-302 \
  redirect-status-303 redirect-status-307 redirect-status-308 redirect-any-status \
  redirect-no-location redirect-empty-location token-count-empty token-multiline \
  token-empty-chain token-empty-value token-null-204 token-null-304
```
