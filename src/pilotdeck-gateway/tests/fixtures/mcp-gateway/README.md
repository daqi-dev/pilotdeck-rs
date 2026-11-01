# U6 MCP control gateway adapter fixtures (#1215)

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41c69396ae85`。
被测源码（SHA-256 在 `oracle.json.sourceSha256`）：

- `src/gateway/server/GatewayWsConnection.ts` — 四方法的 WS dispatch
  （387–398；reconnect/toggle 包 `.then(() => ({ok:true}))`，未知方法同步
  throw `Unknown gateway method …`）
- `src/gateway/client/InProcessGateway.ts` — 2284–2319：字段校验、
  `hasActiveTurn` SESSION_BUSY 门、registry 回调、成功后 `router.close`
- `src/gateway/client/GatewayInteractionCoordinator.ts` — `closeSession`
  的 reason 字符串（fixture 对 coordinator 是替换而非包裹：真实
  `closeSession` 的副作用不执行，coordinator 层不在本组件对拍范围）
- `src/gateway/SessionRouter.ts` / `src/agent/scope/AgentHandle.ts` —
  close/evict 顺序与 dispose 失败的 `AggregateError`（消息
  `Failed to dispose agent handle.`）
- `src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts` /
  `src/cli/ProjectRuntimeRegistry.ts` — `*ForSdk` 与 ask 规则固定

`generate.mts` 组装真实三层栈：真实 `SessionRouter`（真实 beginTurn/close、
真实 AgentHandle 会话）、真实 `GatewaySessionPermissionRuleSetRegistry`、
真实 `InProcessGateway`（interactionCoordinator 在原型链上替换 closeSession
以记录 `close-session:<key>:<reason>`——只钉 reason 字符串，真实副作用
不执行）、`ProjectRuntimeRegistry` 原型（只 stub resolve / pluginRuntime，
同 sdk-mcp-p2 的做法），以及假文本传输上的真实 `GatewayWsConnection`
交换真实帧。每个 case = 一个请求帧，记录 WS 响应、事件流
（resolve/refresh/snapshot/create/dispose/evict/close-session）与 post 状态
（活跃 turn、存活会话、MCP 表、ask 规则）。省略 `params` 的帧在 oracle 里
省略键（`undefined`），与显式 `params: null` 区分记录（fix-r1 实测两种帧
应答不同）。

44 行矩阵（fix-r1）扩至 53 行（fix-r2，键 `s`/` w `/`b`/`d`/`o`/`f`/`t`/
`tb`/`rb`/`v`/`vb`）：四个方法
各自的成功、变更、`INVALID_*` 校验、SESSION_BUSY（含 `override-busy-valid`
的 mode-先于-busy 次序）、close 失败（`f`）、关闭后重入、`tb`/`rb`
（toggle/reconnect 后的 busy 门）、`turn-during-callback`（S3：回调内
beginTurn 后门仍放行——TS 的门只在回调前查一次）、`sessionKey` 缺失/空白/
非字符串行（fix-r1：`set-missing-session-key`、`toggle-missing-session-key`、
`toggle-blank-session-key`、`reconnect-missing-session-key`、
`override-missing-session-key`、`override-nonstring-session-key`；
fix-r2 补齐三控制面共享校验的余格：`toggle-nonstring-session-key`、
`reconnect-blank-session-key`、`reconnect-nonstring-session-key`、
`reconnect-blank-server-name`、`toggle-nonstring-server-name`、
`reconnect-nonstring-server-name`、`override-blank-session-key`、
`override-blank-server-name`、`override-nonstring-server-name`）、
`set-params-omitted`（帧省 `params` 键，与 `params: null` 区分）。
另录 `assemblyCases`：
不带四 options 的网关答 `CAPABILITY_UNAVAILABLE`、未知方法
`mcp_server_pause` 抛 `Unknown gateway method mcp_server_pause.`——这两类属
接线层，组件刻意不接（`dispatch.rs` 未动），故只记录不重放。
close 与 owner shutdown 竞争的行为不在金样里（fixture 不建模 shutdown），
由 Rust 单测
`mcp_gateway_close_during_owner_shutdown_awaits_the_keys_own_eviction` 钉住
（#1245 对齐后：门控 dispose 使驱逐 mid-flight，close 透传该 key 自己的
per-key 文案；TS 依据为真实路由探针
`_tmp/20261026-issue-1245/probe/close-during-shutdown.out`，登记为
`specs/gateway-parity/diffs/A7.2-SESSION-CLOSE-DURING-SHUTDOWN-001-R1.md`）。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 \
  node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/mcp-gateway/generate.mts <scratch>
```

必须用 Node v22.22.0。Rust 默认测试
`runtime::mcp::gateway_tests::mcp_gateway_replays_the_real_ts_oracle`
重放全部 53 行（重放测试聚齐全部行差异后一次性 panic，所以一次运行能报出
所有不一致）；`#[ignore]` 测试
`runtime::mcp::gateway_tests::mcp_gateway_fixture_regenerates_identically_from_ts`
（另有 `tests/generator_provenance.rs` 的批量登记
`mcp_gateway_regenerates_identically`）重生成并逐字节比较。

负控：`negative_controls.py` 只变异产品源码
（`runtime/mcp/gateway.rs`、`runtime/mcp/sdk.rs` 与
`runtime/session/handle.rs`），用 `negative_suite.run_cases` 逐项汇总
KILLED/SURVIVED/INVALID；每个 marker 是重放测试里一个确切的断言消息，
`cases_for` 把每行 mutation 的每个 marker 各变成一个 Case。锚点过期在
`--check-anchors` 预检直接退出 2。因新增/修改文件在提交前不等于 HEAD，
`mutating_sources` 会按设计拒绝；脚本在提交后的宿主运行：

```sh
CARGO_TARGET_DIR=/path/to/target python3 \
  src/pilotdeck-gateway/tests/fixtures/mcp-gateway/negative_controls.py \
  _tmp/<date>-issue-1215/negative-controls
```

`--check-anchors` 预检锚点；`--compile-only` 只证明每个变异体能编译。
脚本要求提交后的源码等于 `HEAD`。九行变异与绑定：
`busy-gate-removed`（四个 busy 行的响应）、`close-after-success-removed`
（诊断性 set 与全部 override 行的事件 + close-fault 行的响应——registry
自身的成功路径已关闭会话，组件的 close 只在这些行可观察）、
`control-validation-relaxed`（toggle/reconnect/override 的缺/空白/非字符串
serverName 与缺/空白/非字符串 sessionKey 行，含 fix-r2 新 9 行——除被遮蔽
三格外全部绑定；set 的 sessionKey 校验与 override 的非字符串
sessionKey/serverName TypeError 都被 Rust `SdkMcpConfig` 镜像层等价复检遮蔽
——TS `ProjectRuntimeRegistry.setMcpServersForSdk` 本身不复检，这是 Rust
mirror 的额外校验——故 `set-missing-session-key`、
`override-nonstring-session-key`、`override-nonstring-server-name` 只作正控
不绑定）、`ok-true-shape-removed`（两个 void 控制的 `{ok:true}`）、
`override-mode-after-busy`（mode 检查必须先于 busy 门）、
`unknown-method-dispatched`（dispatch 对非四方法必须返回 None）、
`dispose-aggregation-bypassed`（#1011 `LiveSessionHandle` 的 dispose 聚合
改为单失败透传——fake 的底层 `fixture dispose failure` 会漏进两个
close-fault 行的信封，证明金样的终稿文案来自真实聚合而非 fake 回显，
fix-r1 P2-3）、
`undefined-params-conflated`（`require_input` 去掉 Undefined 臂（只保留
Null 臂）后 Undefined 落入透传臂，`set-params-omitted` 行应答漂移，
fix-r1 P2-2）、
`close-message-hardcoded`（gateway.rs close 的错误映射改为硬编码字面量
——#1245 对齐后所有 close 窗口都答 per-key dispose 文案，旧的 per-key
字面量会存活，故变异硬编码旧的整个 shutdown 聚合
`Failed to shut down all agent sessions.`，杀点在 shutdown 竞争单测的
per-key 断言，fix-r2 P1 / #1245）。
