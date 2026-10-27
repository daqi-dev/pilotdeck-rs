# #886 MCP HTTP Content-Encoding

Node **v22.22.0**、基线 **cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85**、真实
`@modelcontextprotocol/sdk@1.29.0`。生成器从基线 node_modules 加载 SDK Client、Protocol、
StreamableHTTPClientTransport 和 SSEClientTransport。使用 Protocol.connect 建立 transport，
不做与本单无关的 initialize 握手；Streamable fetch 注入真实基线 `networkFetch`，参数与
McpClient.buildTransport 同形（测试缩短 timeout）。legacy 使用 Node 全局 fetch。
仅连接本机 HTTP server，基线只读，无外发。

## 矩阵与结果

7 个入口 × 11 个编码/载荷格，原始输出保留 name/code/message，不统一错误形状：

| 入口 | plain / gzip / br / deflate / mixed / five / unknown / mixed-unknown | six / unknown-six | corrupt gzip 载荷 |
|---|---|---|---|
| Streamable POST JSON | tools/list 结果 | NetworkFetchError / fetch failed | TypeError / terminated |
| Streamable POST SSE | tools/list 结果 | NetworkFetchError / fetch failed | 请求超时 |
| Streamable GET SSE | tools/list 结果 | 请求超时（GET 错误经 onerror） | 请求超时 |
| legacy GET SSE endpoint | start 成功 | SSE error 含层数与最大值原因 | **现存差异，见下** |
| Streamable POST HTTP 400 | 解码后的 HTTP 错误消息 | NetworkFetchError / fetch failed | 错误消息中的 null |
| legacy POST HTTP 400 | 解码后的 HTTP 错误消息 | TypeError / fetch failed | 错误消息中的 null |
| 307 → Streamable POST JSON | tools/list 结果 | 终点 NetworkFetchError / fetch failed | TypeError / terminated |

`unknown` 的 Content-Encoding 是 custom；`mixed-unknown` 是 gzip, custom，体为合法明文，
验证任一未知 token 导致整链不解码。unknown-six 验证计数先于 token 识别。
redirect 的中间响应始终带六层 Content-Encoding，必须跳过；终点仍执行自己的解码/上限。
Rust 直接跑 HttpLink/Wire 与真实 TCP socket，无注入替代解码器。

**70 个核心编码格 + 6 个附加载荷损坏格对齐，共 76 格。** 第 77 格 legacy-start/corrupt
保留原始 TS 金样并由 `mcp_legacy_corrupt_gap_is_observed` 单独重放，不计对齐：
TS 立即拒绝 `SSE error: TypeError: terminated: incorrect header check`；Rust 既有
legacy_stream 在读体失败后留下未 settle 的 start，进入既有重连循环。
共享 #867 decoder 只暴露 BodyFailure::Terminated，未保留具体 codec cause。
本单不修改 decoder 语义和 legacy 重连生命周期；此差异**未获接受、待协调者跟进**，
不能据此声称整个损坏压缩体范围已完成。测试用真实失败 body 返回作为确定性观测点，
不靠固定睡眠猜测 pending；外层 10 s watchdog 保证测试终止。

GET SSE 测试先注册请求，收到 server POST 屏障后发送 initialized，再自动 GET，
避免响应早于 pending 注册；每条请求 500 ms，其他测试 I/O 有 10 s watchdog。
服务端任务拥有所有连接，退出时关闭，不留下后台 server。

## 重生成

设 PDRS_BASELINE 为固定基线路径，Node 必须是 v22.22.0：

```sh
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/mcp-http-encoding/generate.mts <新目录>
```

目录 recursive 创建，oracle.json 用 wx 独占创建，已存在会报错；不能覆盖旧金样。
`mcp_encoding_regenerates_identically` 是实际运行生成器并逐字节比较的 ignored 守卫。

## 负控

`negative.py --audit` 只读检查所有锚点；正式执行要求所有被改源码等于 HEAD。
脚本使用 negative_process 的 mutating_sources / run_owned / wait_for_slot，复用
negative_suite 逐项 KILLED/SURVIVED/INVALID 汇总。每个变异先 no-run 编译、恢复，
再做正控→变异→恢复正控；编译失败不是 KILLED。每条测试和前置失败均带 #886 专属标签。

必须由调用方设置 CARGO_TARGET_DIR；禁止复制仓库或换 Git 基线绕过守卫。
开发树未提交时只交付脚本，协调者提交后运行：

```sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-886/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
python3 src/pilotdeck-gateway/tests/fixtures/mcp-http-encoding/negative.py \
  _tmp/20261026-issue-886/negative-host
```

可加 `--compile-only` 单独编译预检，也可在输出路径后给 1–2 个变异名做有界样本；
其余长运行交协调者宿主。任一非 KILLED 非零退出，锚点过期直接报错退出。
