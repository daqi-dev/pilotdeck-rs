# U7 设置六方法「调用 → 通知 → 再读」金样（#1320）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU 78.2
（生成器断言 `process.version === 'v22.22.0'` 与 `Intl.Collator` 解析为 `en-US`，
须 `LANG=en_US.UTF-8`）。#1320 把 `resolve_settings` / `apply_flag_settings` /
`update_settings` / `reload_output_styles` / `reload_config` /
`reload_extensions` 六方法接进原生请求路由；本金样按完成判据录
「调用 → 通知 → 再读」：每行记 WS 响应帧、该响应帧**之前 / 之后**连接收到的
**全部**帧（含并发探针里的 response 帧，只有 `type` 不含 id；以响应帧在连接
缓冲中的位置切分）、随后一次 `resolve_settings` 再读
（证明写与读共享同一 store 代）与后置状态（会话 / 覆写 / 各场景树的
version 与 YAML）。#1397 的三条原样输出并入本批：
`styles-scalar-params`（标量 params 读 `projectKey` 得 undefined → 全量分支）、
`extensions-null-params` / `extensions-scalar-params`（`input?.projectKey`
在 nullish 与原始值上都短路到全量分支）。

生成器加载真实三层栈：真实 `GatewayWsConnection`（假文本传输；`bindServer`
镜像 `GatewayServer.broadcastNotification` —— 迭代连接逐个 `sendNotification`，
未完成 hello 的连接被其 `sendNotification` 自身丢弃，与生产同构）、真实
`InProcessGateway`、真实 `SessionRouter`、真实
`ProjectRuntimeRegistry.prototype`（真 Map 状态，`applyFlagSettingsForSdk` /
`reloadOutputStylesForSdk` 走真实路径）、真实 `GatewayRuntimeRefreshBundle`
（configStore delegate 始终指向当前场景树的真实 `createPilotConfigStoreSync`
store；registry delegate 的 `reload()` 以 `setImmediate` 宏任务 settle ——
`markAllDirty` + `broadcast` 因此落到响应帧之后，与生产 `onConfigChange`
的 `executor.spawn` 结算同位）、真实 `updatePilotLocalSettings` 写盘。
`updateSettings` 闭包体为 `createLocalGateway.ts:1384-1404` 逐字副本、
`toGatewayResolvedSettings` 为 `createLocalGateway.ts:1580-1610` 逐字副本
（`redactConfig` 真实投影：`/(apiKey|authorization|cookie|token|secret|
password|credential)/i` 命中键名即 `<redacted>`，故 `maxContextTokens`
在出站快照中被抹除，原始写入由树的 YAML 后置格证明）。

行协议：setup 词表（`tree:<name>` / `ensure-session` / `begin-turn` /
`styles-session` / `styles-config` / `write-yaml` / `break-store-yaml` /
`deny-org` / `allow-org`）→ 断言树态 → 清空事件与连接缓冲 → 调用 →
**drain**（一次 `await setImmediate`：registry 的 setImmediate 在 harness 的
之前入队，故其结算链必已落地，之后无悬挂）→ 按响应帧切分 before / after →
再读。行内再读不 bump 代（只读）。

`oracle.json` 全由生成器输出（`flag:'wx'`，输出根目录须为新目录），不手写期望：
`cases` 21 行（resolve 3 / apply 3 / update 3 / styles 3 / reload_config 4 /
extensions 5，含成功与忙、无效参数、组织策略拒绝、schema 错误、runPolicy 同步
分支等负路径）+ `probes`（`concurrency`：两个在途 `reload_config` 共享 store 的
单次在途 reload，代数只 bump 一次、结算链只跑一次；`reconnect`：reload 在途时
同 tick 发出的 `resolve_settings` 读到旧代 v1，线上序为
[mid 响应, reload 响应, notification]，换新连接读 v2，续连 reload 到 v3 证明
未换 store；`secondConnection`：广播到达每个连接）+ `matrix` 6 行（生产接线形态：
仅 `interactionCoordinator` / `resolveSettings` / `reloadConfig` /
`reloadExtensions` 四委托的 `InProcessGateway` —— 三条拒绝方法
`apply_flag_settings` / `update_settings` / `reload_output_styles` 答
`CAPABILITY_UNAVAILABLE "<method> is unavailable."`；`resolve_settings`
与两个 reload 在 matrix 中成功）+ `divergenceCases` 2 行
（`U7-EXTENSIONS-RELOAD-STRING-COERCION-001` 登记差异：`changedPaths:[1,true,null]`
与 `projectKey:5` 两格 TS 原值透传、Rust `String()` 强转；录 TS 真相为证据，
Rust 重放在登记有效期间跳过该节，不改冻结账本）。事件流词汇：
`store-event:` / `log:` / `sdk-config-change:` / `registry-reload` /
`mark-*-dirty:` / `invalidate:` / `broadcast:config_changed:` /
`close-session:` / `dispose:` / `evict:` / `resolve:`。

确定性：行内数据均为内容函数；时间戳归一为 `<ts>`（ISO 全串）、输出根前缀归一为
`<root>`，所有录帧（response / notification / 再读）统一过归一层。本机 macOS
两个新目录重生成逐字节一致；`_shared/regeneration-masks.tsv` 无本 fixture 行
（无需任何掩码）。`contentHash` 为纯内容哈希（与 scratch 根无关，已实测两个根
不同树比对一致）。

复现（从仓库根，输出目录必须为新目录）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/native-settings/generate.mts /tmp/1320-golden-1
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/native-settings/generate.mts /tmp/1320-golden-2
cmp /tmp/1320-golden-1/oracle.json /tmp/1320-golden-2/oracle.json
cmp /tmp/1320-golden-1/oracle.json \
  src/pilotdeck-gateway/tests/fixtures/native-settings/oracle.json
```

Rust 侧：`src/pilotdeck-gateway/src/native_process/settings_tests.rs` 的
`#[ignore]` 守卫 `native_settings_fixture_regenerates_identically_from_ts`
（`PDRS_BASELINE` 下实跑一次，逐字节比对）；同文件的 S4 重放逐行执行同一
setup 词表，经 `Requests::settings_request` 驱动——那是 **settings 网关层**
（#1305 registry 查表选中 settings 家族之后、AfterAdmission handler 的那一层），
**不是** `Requests::request` 的完整路由层——重建 WS 帧，比对
response / before / after / 再读 / post 五格；隔离准入门与 registry 查表由
同文件的路由级用例 `settings_methods_route_through_the_native_request_gate`
覆盖。含外来 / 非字符串 `projectKey` 的金样行（`styles-closes-matching` 的
`"/proj-A"`、`extensions-project` 的 `"/p"`、登记差异
`extensions-project-key-number` 的 `5`）因此只对网关层成立：完整路由会先被
隔离准入门拒掉，答案与金样不同。负控（`negative/`，S4 落盘）
由 `neg-for-diff` 装置按点名测试判杀。
