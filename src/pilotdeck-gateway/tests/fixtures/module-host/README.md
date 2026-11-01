# module-host transport TS 金样

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0。生成器以 `startGatewayServer` 起两个真实 TS 服务器（`main` 带 `publicHostCapabilities` 解析器、`bare` 不带），`publicHostHttpTransport` 保持原样；provider 为按 principal.agentId 选场景的录制型 stub。每个 cell 用裸 socket 写入原始请求字节并原样捕获响应字节（250 ms 静默窗、EOF 或错误结束），Date 头掩码。运行前核 commit 与三个源文件的 SHA-256。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/module-host/generate.mts src/pilotdeck-gateway/tests/fixtures/module-host
cd src && cargo test -p pilotdeck-gateway --lib module_host
```

`oracle.json` 87 个 cell 记录 server、原始请求字节、响应字节（latin1 映射）与 closed/halfClose 标志。32 MiB 预算 cell 的请求体不落盘实料：oracle 只存 `__PDRS_PAD__` 标记与长度，重放方写入 `'a'.repeat(size)`。已测场景族：

- 鉴权（缺 / 错 / 小写 scheme / 双空格 / GET+错鉴权序）、方法（GET / PUT / HEAD / GET 带体）、路由（尾斜杠 / 近似路径 / 点段 / `%64` 百分号 / 大写 / 双斜杠 / 查询串）；
- 未配置解析器（501）、坏 JSON / 空 / 非对象 body、principal 缺失 / 空 / 空串 / 纯空白 / NBSP / 非字符串 / 缺字段；
- describe 过滤序（wire ∩ provider 序）、call 未知 / 缺 operation / 非字符串 / 未广告 / 空表；
- input 缺失 / null / 字符串 / 数字 / 数组 / false、scope 越权（camel / snake / 嵌套不越权）；
- 成功体（对象 / 字符串+text-plain / 字符串+json ct 转义 / 空串 / Uint8Array / rawBody / rawBody 无 ct / null / undefined / 数字 / 布尔）、wire 头白名单（6 项保留、非白名单丢弃）、坏 etag / 坏头名（ERR_INVALID_CHAR catch）；
- provider 失败（默认 502 / 自定义 status+code / 418 无 message）、499 透传（`unknown` reason）、600 非法 status；
- 流（ndjson 对象、ndjson 字节、sse 字符串、ndjson 数组事件销毁、sse 对象事件销毁、流+普通 ct 502、缺 body 502、空串 0 事件、字符串逐 UTF-16 单元、数组逐元素、对象销毁、非空 Uint8Array 销毁、空 Uint8Array、rawBody 忽略）；
- 32 MiB 预算（恰 / 超 1 字节）、Connection: close、HTTP/1.0（Node 1.1 状态行，已登记差异）、keep-alive 双请求复用、提前断连（Node 解析器层 400，已登记差异）。

Rust 重放在 `src/module_host/tests.rs`：同一请求字节经真实 TCP 监听器打到 `http_proxy::serve_connection_native_with_shutdown`，module-host 组件前置、#1209 路由器兜底 404；响应按 #1209 规则归一化（Date 掩码、头名小写、空 `content-length: 0` 删除、chunk 大小小写）逐 cell 与金样比对，closed 标志另比。`http10`（HTTP/1.0 状态行）与 `premature-close`（Node 解析器层 400）为字节 pin，不进归一化比对；`replay_cells_cover_exactly_the_oracle` 断言重放列表 + pin 列表 = oracle cell 全集，两边都不能单独漂移。

重生成守卫（`#[ignore]`）：`PDRS_BASELINE=… cargo test -p pilotdeck-gateway --lib module_host::tests::regenerates_identically -- --exact --ignored --show-output`。生成器输出目录递归创建，oracle.json 以 `flag:'wx'` 独占创建。除 Date 掩码外无时钟 / PID 依赖，未登记 regeneration mask。

负控：`negative_controls.py`（`_shared/mutant_switch.py` 编译开关执行器，17 个开关，每个绑定一个 replay cell 的单行专属断言）。新增 cell 时同步更新 `src/module_host/tests.rs` 的 `replay_tests!` 表与 `negative_controls.py` 的开关绑定。
