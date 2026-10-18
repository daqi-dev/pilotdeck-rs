# Token accounting 运行时对拍

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器通过 `../_shared/baseline.mts` 检查整个 tracked checkout 干净，生成前后对选定源码复算 `sourceSha256` 并与固定 commit 的 blob 对照。仅调用真实 TS 的 TokenAccountingRuntime、TokenBudgetManager 和 budget dispatcher，不复制 TS 实现。

**证据状态**：协调者已在非沙箱环境验证原 `oracle.json`：默认 HTTP 生成器 41 组 / 323 步与仓内字节一致（SHA-256 `b6c218a84066f8ee3f65901e1ed55b77689fc5fab50c4fc64394c80747318e40`），且第一轮整包测试 421 过 0 败。第二轮新增的 `http-oracle.json` 仍是候选：当前沙箱禁止监听，其来源是真实 TS + Node 原生 fetch 的 scripted dispatcher；新增默认 HTTP 双录和 socket 回放须由协调者执行。两批证据分别记账。

默认生成器创建只监听 `127.0.0.1:0` 的 HTTP 服务。真实 TS 与 Rust 回放分别向同协议 mock 发送请求；官方 Responses 的逻辑目标保留为 `https://api.openai.com/v1/responses/input_tokens`，测试 transport 仅把它重路由到独占本地服务。其他地址不得连接外网。记录包含 method、path、业务 headers、完整请求体、逻辑 URL、TS 快照及错误。HTTP Host/Content-Length 等 transport 自动字段不属于请求投影断言。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/token-accounting/generate.mts [existing-output-parent]
```

输出父目录默认系统临时目录。每次 `mkdtemp` 建独占子目录，stdout 最后一行包含 `output`、`transport` 和精确条数；生成器不覆盖仓库候选、不删除固定路径。仅在无法监听时，可显式追加 `--capture-transport` 生成非 HTTP 候选；日志明确标 `injected (HTTP not verified)`。

精确人口：41 组 / 323 步（各组长度也固定断言）、42 个 snapshot、20 个各 operation 自有必填字段/值域用例。覆盖本地拆分、工具/多媒体、浅校验允许的深层异常、校准匹配和限幅、provider 字段优先与有限负数/小数、HTTP 状态失败、官方 URL 判定、headers、缓存命中/淘汰/禁用、thinking、schema normalization、cache markers、UTF-16、schema prototype。local-composition/local-shallow-boundary 只从已有金样复用输入，每个期望都重新调用真实 TS。

Rust 回放通过 protocol 的 JsValue 保留对象次序及孤立 surrogate；比较视图对孤立 surrogate 使用 `$utf16` 测试载体，不把它替换成 U+FFFD。每次回放始终检查 commit/hash 格式；设置 `PDRS_BASELINE` 时逐个检查固定 blob 与实际源码。临时文件与 HTTP 服务均每次运行独占，无固定路径清理。

```sh
cargo test --manifest-path src/Cargo.toml --locked --offline \
  -p pilotdeck-gateway --test token_accounting
cargo test --manifest-path src/Cargo.toml --locked --offline \
  -p pilotdeck-gateway --test token_accounting regenerate_twice_identically -- --ignored --nocapture
```

第二条只允许默认 HTTP 模式，精确比对两次生成和仓库候选。当前沙箱可运行的独立部分为第一条追加 `-- --skip real_ts_provider_requests_over_loopback_http`；这是有明确排除项的局部验证。

## 第二轮：fetch 重定向与 JSON 错误

`generate-http.mts` 另录 143 例 / 320 次实际请求，不改原 41 组金样。默认创建两个独占 `127.0.0.1:0` 服务，第二个端口提供不同 origin；无真实 provider 请求。`http-oracle.json` 保存每个响应脚本、逐跳 method/path/origin/headers/body 和真实 TokenAccountingRuntime 最终快照，包括失败后的本地回退及 `estimatorError`。完整人口和唯一名称在回放中断言，生成器另核请求次数和成功重定向确实返回 provider count。

覆盖 301/302/303/307/308、相对/根/绝对/query/空 Location、跨域及返回原域、循环、20/21 跳边界、缺失/非法/重复 Location、凭据和禁用端口；逐跳核 POST→GET、body 与 body headers 清除，以及 Authorization/Cookie/Proxy-Authorization 清除和 x-api-key 保留。用户 Host 由实际目标 URL 替换。另有未结束重定向 body 不得阻塞跟随的 Rust 回归。

错误文案钉当前实测 Node **v22.22.0 / V8 12.4.254.21-node.33**（记录在候选中）：HTML、纯文本、空/截断、对象/数组语法、数字、escape、非法 UTF-8、BOM、UTF-16 位置和错误上下文截断。消息中的孤立 surrogate 经 ModuleError/预算快照保持。JSON 合法值仍复用 protocol codec；诊断扫描仅在失败时运行，不改变其他模块的解析错误。重定向策略参考 [Fetch HTTP-redirect fetch](https://fetch.spec.whatwg.org/#http-redirect-fetch)，细节以本机 Node 实录为准。

```sh
# 默认真实 HTTP；stdout 给出独占输出目录，不覆盖仓内候选。
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/token-accounting/generate-http.mts

# 允许监听的环境：两次真实 HTTP 录制与候选逐字节核对。
cargo test --manifest-path src/Cargo.toml --locked --offline \
  -p pilotdeck-gateway --test token_count_http \
  regenerate_http_twice_identically -- --ignored --nocapture

# Rust 默认 hyper 实际走两个 HTTP mock，比较全部快照和请求轨迹。
cargo test --manifest-path src/Cargo.toml --locked --offline \
  -p pilotdeck-gateway --test token_count_http

# 沙箱可执行：底层交换注入，生产重定向/解码/JSON/预算逻辑全部保留。
cargo test --manifest-path src/Cargo.toml --locked --offline \
  -p pilotdeck-gateway --lib runtime::model::count_http::tests
```

显式 `generate-http.mts [existing-output-parent] --capture-transport` 只用于禁止监听的环境：注入在 Node fetch 下方的 dispatcher 边界，重定向与 JSON 解析仍由真实 Node 处理；日志标 `scripted dispatcher (HTTP not verified)`。此模式双录一致不能代替上述 HTTP 命令，ignored 再生测试禁止该模式。测试、生成器均保留独占临时目录，无固定路径删除。

运行时默认 transport 使用已有 hyper HTTP；TLS transport 由后续 A6.4-pr5 注入，不新增依赖。tokenizer、JSON 构造/解析在线程池内执行，最多四个 CPU 批次；取消会撤销排队批次和网络 future，已开始的纯 CPU 批次在持有 permit 时完成。深层 JSON 转换使用迭代算法，并沿用 gateway 的深层转换上限（2000）；V8 实际栈阈值随调用栈变化，不声称逐深度等同。
