# #1553 legacy SSE 首个 endpoint 之前的 body 读失败

Node **v22.22.0**、基线 **cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85**、真实
`@modelcontextprotocol/sdk@1.29.0` 的 `SSEClientTransport`（legacy SSE transport，
Node 全局 fetch + undici + eventsource）。原始 TCP socket、仅本机回环、基线只读。
本夹具是探针（process.mts），不生成 oracle：Rust 侧 `mcp_legacy_start_failure_matrix`
自带 scripted server，逐行对位这里实测的 TS 行为。

## 实测矩阵（2026-11-05，本机 cb8）

| 行 | 场景 | TS start() | TS 后续 |
|---|---|---|---|
| corrupt-before-endpoint | endpoint 之前收到损坏 gzip（`invalid compressed body`，Content-Encoding: gzip） | **立即拒绝 `SSE error: TypeError: terminated: incorrect header check`** | onerror 同文 |
| clean-eof-empty-before-endpoint | 空体、clean EOF（CL: 0） | 拒绝 `SSE error: undefined` | onerror 同文 |
| clean-eof-data-before-endpoint | 合法 gzip 的 `data: hello` 后 clean EOF | 拒绝 `SSE error: undefined` | 先触发 JSON SyntaxError（SDK 把 message 行当 JSON 解析）再 SSE error |
| wire-interrupt-before-endpoint | CL 声明完整 gzip 成员但只发 18 字节后 destroy | 拒绝 `SSE error: TypeError: terminated: Response body length does not match content-length header` | onerror 同文 |
| abort-before-endpoint | 挂起的 body（发 10 字节后不再发） | pending-3000ms | **close() 之后仍 pending-1000ms** |
| endpoint-then-cut | endpoint 事件后 CL: 50 只送 29 字节即 FIN | **成功** | onerror 报 content-length 原因；**4s 窗口内重连（connections: 2）** |
| corrupt-after-endpoint-staged | 合法 endpoint 成员 500ms 后追加损坏成员 | **成功** | **损坏永远不上浮**：无 onerror、无重连（connections: 1），读挂起 |

原始 JSONL 输出见运行记录（每行含 name/code/message 原文，不统一形状）。

## Rust 对位（src/pilotdeck-gateway/src/runtime/mcp/http_encoding_tests.rs）

- **corrupt-before-endpoint 对齐**：`SSE error: TypeError: terminated: incorrect header check`
  逐字对上（也是 mcp-http-encoding oracle legacy-start/corrupt 金样，本单 #1553 的核心修复）。
  BodyFailure::Decode 携带 codec cause（zlib/brotli 文案映射见 model/http/encoding.rs
  `decode_failure`），legacy_stream 在首个 endpoint 之前读体失败时 settle start。
  该格重放金样同一 23 字节 wire，`Content-Length` 由 body 长度生成。
- **endpoint-then-cut 对齐两点**：start 成功 + 4s 内重连计数 2。
- **abort-before-endpoint 的 wire 与屏障**：与上表 TS 行同形（gzip 成员只发前 10 字节、
  `Content-Length` 取整成员长度、socket 持住不关），成员在运行时生成而非钉死字节。
  「客户端已进入读体」不靠固定睡眠判定：`read_events` 入口在 `#[cfg(test)]` 下自增
  `body_reads_started`，测试在 2 s 有界循环里等它变化，等不到即失败（abort 落在响应头之前时
  该计数不变，而两条路径的 settle 值相同，只有这个观测能区分）；随后才断言 start 仍 pending 并 `close()`。

## 登记差异（未接受，协调者跟进；不据此声称更大范围完成）

1. **clean EOF（endpoint 之前）**：TS 拒绝 start `SSE error: undefined`；Rust 不 settle、
   继续既有重连循环。`undefined` 来自 eventsource v3 在 `failConnection` 无 error 对象时
   的 onerror 路径，与本单「保留 codec cause」不同族。
2. **wire 中断的 cause**：TS cause 为 `Response body length does not match content-length
   header`（undici 的 wire 失败文案）；Rust settle `SSE error: TypeError: terminated`
   不带 cause（wire 失败族不经过 codec，cause 保留范围限定在解压失败）。
3. **abort 后 close()**：TS start 永远 pending（close() 也不 settle）；Rust close() 以
   `MCP error -32000: Connection closed` settle。Rust 行为是既有 close 生命周期，非本单引入。
4. **endpoint 之后staged 损坏**：TS 的损坏永远不上浮（无错、无重连、读挂起）；Rust 读体
   失败进入既有重连。Rust 行为同样为既有生命周期。
5. **endpoint-then-cut 的 onerror 文案**：TS onerror 报 content-length 原因（同差异 2 族）。

对位范围说明：上表 `wire-interrupt-before-endpoint` 与 `corrupt-after-endpoint-staged` 两行
**没有** Rust 对位行，差异 2 / 4 里写的 Rust 行为来自读码与既有生命周期，没有测试见证；
`mcp_legacy_start_failure_matrix` 覆盖的是 corrupt / abort-stall / clean EOF / endpoint-then-cut 四态。

## 运行

设 PDRS_BASELINE 为固定基线路径，Node 必须是 v22.22.0：

```sh
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/mcp-http-corrupt-body/process.mts <新目录>
```

目录 recursive 创建，matrix.jsonl 用 wx 独占创建，已存在会报错；不能覆盖旧输出。
每行 10s watchdog 保证进程终止；所有 socket 由行持有并在行结束时销毁，不留下后台 server。

## 负控

`negative_controls.py --audit` 只读检查所有锚点；正式执行要求所有被改源码等于 HEAD。
脚本使用 negative_process 的 mutating_sources / run_owned / wait_for_slot，复用
negative_suite 逐项 KILLED/SURVIVED/INVALID 汇总。每个变异先 no-run 编译、恢复，
再做正控→变异→恢复正控；编译失败不是 KILLED。5 个变异：

| 变异 | 绑定测试 | 判杀锚点 |
|---|---|---|
| decode-cause-removed（gzip 头映射删） | model codec_rejections_keep_the_zlib_cause | `#1553-5 gzip garbage` |
| decode-cause-wrong-literal（cause 文案改错） | 同上 | `#1553-5` 同上 |
| wire-class-reversed（wire 失败也映射成 Decode） | 同上 | `#1553-13 wire failure` |
| legacy-settle-removed（settle 分支删） | mcp_legacy_start_failure_matrix | `loopback watchdog`（start 永不 settle） |
| response-text-decode-arm（response_text 的 Decode 臂错映射） | model response_text_keeps_terminated_on_codec_rejection | `#1553-16` |

必须由调用方设置 CARGO_TARGET_DIR；禁止复制仓库或换 Git 基线绕过守卫。
开发树未提交时只交付脚本，协调者提交后运行：

```sh
export CARGO_TARGET_DIR=<调用方提供>
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 src/pilotdeck-gateway/tests/fixtures/mcp-http-corrupt-body/negative_controls.py \
  _tmp/20261105-issue-1553/negative-host-committed
```

可加 `--compile-only` 单独编译预检，也可在输出路径后给 1–2 个变异名做有界样本。
任一非 KILLED 非零退出，锚点过期直接报错退出。

