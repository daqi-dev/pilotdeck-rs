# Token accounting 运行时对拍

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器通过 `../_shared/baseline.mts` 检查整个 tracked checkout 干净，生成前后对选定源码复算 `sourceSha256` 并与固定 commit 的 blob 对照。仅调用真实 TS 的 TokenAccountingRuntime、TokenBudgetManager 和 budget dispatcher，不复制 TS 实现。

**第三轮证据状态**：两份候选均重新调用真实 TS 双录，原 41 组 / 323 步与原 HTTP 143 例的数据保持不变。本轮增加异常 provider body、thinking efforts、headers 和初始 URL 用例；第三轮录制时沙箱禁止监听，该批新增部分当时仍需协调者执行下列默认 HTTP 双录与 socket 回放。capture 模式的来源与覆盖界限不变，不能代替 HTTP 验收。

默认生成器创建只监听 `127.0.0.1:0` 的 HTTP 服务。真实 TS 与 Rust 回放分别向同协议 mock 发送请求；官方 Responses 的逻辑目标保留为 `https://api.openai.com/v1/responses/input_tokens`，测试 transport 仅把它重路由到独占本地服务。其他地址不得连接外网。记录包含 method、path、业务 headers、完整请求体、逻辑 URL、TS 快照及错误。HTTP Host/Content-Length 等 transport 自动字段不属于请求投影断言。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/token-accounting/generate.mts [existing-output-parent]
```

输出父目录默认系统临时目录。每次 `mkdtemp` 建独占子目录，stdout 最后一行包含 `output`、`transport` 和精确条数；生成器不覆盖仓库候选、不删除固定路径。仅在无法监听时，可显式追加 `--capture-transport` 生成非 HTTP 候选；日志明确标 `injected (HTTP not verified)`。

精确人口：53 组 / 361 步（各组长度也固定断言）、42 个 snapshot、20 个各 operation 自有必填字段/值域用例、3 个直接 provider-count 错误。覆盖本地拆分、工具/多媒体、浅校验允许的深层异常、校准匹配和限幅、provider 字段优先与有限负数/小数、HTTP 状态失败、官方 URL 判定、headers、缓存命中/淘汰/禁用、thinking、schema normalization、cache markers、UTF-16、schema prototype、provider/model 原型链查找。local-composition/local-shallow-boundary 只从已有金样复用输入，每个期望都重新调用真实 TS。

Rust 回放通过 protocol 的 JsValue 保留对象次序及孤立 surrogate；比较视图对孤立 surrogate 使用 `$utf16` 测试载体，不把它替换成 U+FFFD。每次回放始终检查 commit/hash 格式；设置 `PDRS_BASELINE` 时逐个检查固定 blob 与实际源码。临时文件与 HTTP 服务均每次运行独占，无固定路径清理。

```sh
cargo test --manifest-path src/Cargo.toml --locked --offline \
  -p pilotdeck-gateway --test token_accounting
cargo test --manifest-path src/Cargo.toml --locked --offline \
  -p pilotdeck-gateway --test token_accounting regenerate_twice_identically -- --ignored --nocapture
```

第二条只允许默认 HTTP 模式，精确比对两次生成和仓库候选。当前沙箱可运行的独立部分为第一条追加 `-- --skip real_ts_provider_requests_over_loopback_http`；这是有明确排除项的局部验证。

## 第二轮：fetch 重定向与 JSON 错误

`generate-http.mts` 单独维护 HTTP 语料，第三轮由 143 例 / 320 次请求扩为 149 例 / 324 次请求，第四轮再增 3 例 model 原型链请求至 152 例 / 327 次请求。默认创建两个独占 `127.0.0.1:0` 服务，第二个端口提供不同 origin；无真实 provider 请求。`http-oracle.json` 保存每个响应脚本、逐跳 method/path/origin/headers/body 和真实 TokenAccountingRuntime 最终快照，包括失败后的本地回退及 `estimatorError`。完整人口和唯一名称在回放中断言，生成器另核请求次数和成功重定向确实返回 provider count。

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

运行时默认 transport 使用已有 hyper HTTP；TLS transport 由后续 A6.4-pr5 注入，不新增依赖。tokenizer、JSON 构造/解析在线程池内执行，最多四个 CPU 批次；取消会撤销排队批次和网络 future，已开始的纯 CPU 批次在持有 permit 时完成。深层 JSON 转换使用迭代算法；普通转换沿用 gateway 的 2000 上限，cache key 的有序 clone 单独使用实测保守下界 1800（真实 TS `sortValue` 在 schema 深度 1890 成功、1895 失败，5/5 次），V8 实际栈阈值随调用栈变化，中间带不作逐深度等同声明。

## 第三轮：方法接收者与 transport 前置失败

`oracle.json.providerErrors` 记录 OpenAI Responses 的 tool_result content 为 null、缺失、5 时，真实 TS `TokenAccountingRuntime.countWithProvider` 的异常及零次 transport 调用。公共预算入口先做本地估算，直接用它会在抵达 provider projection 前失败，无法钉住 `flatten_result`；因此生成器直接调用同一真实 TS 实例的 private 方法，Rust 对位测试也直接进入 `provider_count`，没有复制 TS 行为或注入估算结果。该测试在守卫修复前实际失败，在修复后应通过。

```sh
cargo test --manifest-path src/Cargo.toml --locked --offline \
  -p pilotdeck-gateway --lib real_ts_responses_tool_result_content_errors
```

另增两种协议的 efforts 字符串命中/substring 命中、null、缺失、数字，共 10 组 / 20 步；HTTP 语料增加 header 值 5/true/false/null 的字符串转换，以及初始 URL 非法和含凭据的错误。初始 URL 两例在 fetch 构造阶段失败、请求数为零；重定向侧仍沿用已有语料中的 `fetch failed`。HTTP 生成器的 provider 覆盖项逐例记录并由 Rust 回放应用，非法 URL 的测试放行仅限两个固定字符串。

## 第四轮：provider/model 原型链查找

真实 TS 的 `this.modelConfig.providers[request.provider]` 与 `provider.models[request.model]` 是普通对象下标，键为 `__proto__`、`constructor`、`toString` 时会命中 `Object.prototype`，不是未命中。非 HTTP 语料新增 `prototype-lookup` 6 步：provider 三个键均使 `provider.models` 为 `undefined`，随后 `[request.model]` 抛 `Cannot read properties of undefined (reading 'm')`；model 三个键得到 truthy 的继承成员，请求未给 `maxOutputTokens` 时在 `model.capabilities.maxOutputTokens` 处抛 `Cannot read properties of undefined (reading 'maxOutputTokens')`。两种情况都回退本地估算，快照带 `estimatorError`，且零次 transport 调用。

HTTP 语料另加 `prototype-model-{__proto__,constructor,toString}` 3 例：请求带 `maxOutputTokens` 时 `??` 短路，真实 TS 会实际发出 count 请求，快照 `source=provider`。金样记录完整请求体与响应快照，Rust 回放逐字节比较。默认 HTTP 生成器的逐例 `request` 覆盖项也由 Rust 回放应用。

## 第五轮：原型键求值顺序与 cache key 深度

`prototype-lookup` 增 5 步：三个原型键 model 带 `maxOutputTokens: 10` 与 `speed: 'fast'` 时在 `model.capabilities.supportsSpeed` 抛 `(reading 'supportsSpeed')`；`tools: 'x'` 在 `maxOutputTokens`/`speed` 之前抛 `request.tools?.map is not a function`。`prototype-responses` 增 4 步，钉住 Responses builder 的求值顺序：messages → `max_output_tokens` → `tools` → `service_tier`，因此原型键 model 缺 `maxOutputTokens` 时先报 `maxOutputTokens`，而带 token limit 时 `tools` 先于 `speed`。三个 `unsupported-*` 组各增一步原型键 model，确认非 anthropic、非官方 Responses URL 在 builder 之前 `return undefined`，快照为 local 且无 `estimatorError`。

cache key 的深层失败边界由 `depth-probe.mts`/`depth-probe2.mts` 实测：默认 cacheSize 下 schema 深度 1890 成功、1895 抛 `RangeError: Maximum call stack size exceeded`（5/5 次），2500 同样抛错；cacheSize 0 时 1900 成功，说明抛错点在 `getCachedProviderCount` 的 `sortValue`，不在 `countWithProvider` 的 try 内。Rust 保持 `.await?` 传播，并把 cache key 有序 clone 的保守下界设为 1800；`cache_key_depth_matches_measured_v8_sort_boundary` 用 1700 成功、1900 失败做负控。
