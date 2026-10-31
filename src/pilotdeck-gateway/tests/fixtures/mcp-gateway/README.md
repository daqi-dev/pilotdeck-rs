# U6 MCP control gateway adapter fixtures (#1215)

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41c69396ae85`。
被测源码（SHA-256 在 `oracle.json.sourceSha256`）：

- `src/gateway/server/GatewayWsConnection.ts` — 四方法的 WS dispatch
  （387–398；reconnect/toggle 包 `.then(() => ({ok:true}))`，未知方法同步
  throw `Unknown gateway method …`）
- `src/gateway/client/InProcessGateway.ts` — 2284–2317：字段校验、
  `hasActiveTurn` SESSION_BUSY 门、registry 回调、成功后 `router.close`
- `src/gateway/client/GatewayInteractionCoordinator.ts` — `closeSession`
  的 reason 字符串
- `src/gateway/SessionRouter.ts` / `src/agent/scope/AgentHandle.ts` —
  close/evict 顺序与 dispose 失败的 `AggregateError`（消息
  `Failed to dispose agent handle.`）
- `src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts` /
  `src/cli/ProjectRuntimeRegistry.ts` — `*ForSdk` 与 ask 规则固定

`generate.mts` 组装真实三层栈：真实 `SessionRouter`（真实 beginTurn/close、
真实 AgentHandle 会话）、真实 `GatewaySessionPermissionRuleSetRegistry`、
真实 `InProcessGateway`（只在原型链上包 interactionCoordinator 记录
`close-session:<key>:<reason>`）、`ProjectRuntimeRegistry` 原型（只 stub
resolve / pluginRuntime，同 sdk-mcp-p2 的做法），以及假文本传输上的真实
`GatewayWsConnection` 交换真实帧。每个 case = 一个请求帧，记录 WS 响应、
事件流（resolve/refresh/snapshot/create/dispose/evict/close-session）与
post 状态（活跃 turn、存活会话、MCP 表、ask 规则）。

37 行矩阵（键 `s`/` w `/`b`/`d`/`o`/`f`/`t`/`tb`/`rb`/`v`/`vb`）：四个方法
各自的成功、变更、`INVALID_*` 校验、SESSION_BUSY（含 `override-busy-valid`
的 mode-先于-busy 次序）、close 失败（`f`）、关闭后重入、`tb`/`rb`
（toggle/reconnect 后的 busy 门）、`turn-during-callback`（S3：回调内
beginTurn 后门仍放行——TS 的门只在回调前查一次）。另录 `assemblyCases`：
不带四 options 的网关答 `CAPABILITY_UNAVAILABLE`、未知方法
`mcp_server_pause` 抛 `Unknown gateway method mcp_server_pause.`——这两类属
接线层，组件刻意不接（`dispatch.rs` 未动），故只记录不重放。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 \
  node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/mcp-gateway/generate.mts <scratch>
```

必须用 Node v22.22.0（`~/.fnm/node-versions/v22.22.0/installation/bin/node`；
PATH 上的 node 是 v26）。Rust 默认测试
`runtime::mcp::gateway_tests::mcp_gateway_replays_the_real_ts_oracle`
重放全部 37 行（重放测试聚齐全部行差异后一次性 panic，所以一次运行能报出
所有不一致）；`#[ignore]` 测试
`runtime::mcp::gateway_tests::mcp_gateway_fixture_regenerates_identically_from_ts`
重生成并逐字节比较。

负控：`negative_controls.py` 只变异产品源码
（`runtime/mcp/gateway.rs`），用 `negative_suite.run_cases` 逐项汇总
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
脚本要求提交后的源码等于 `HEAD`。六行变异与绑定：
`busy-gate-removed`（四个 busy 行的响应）、`close-after-success-removed`
（诊断性 set 与全部 override 行的事件 + close-fault 行的响应——registry
自身的成功路径已关闭会话，组件的 close 只在这些行可观察）、
`control-validation-relaxed`（toggle/reconnect/override 的缺字段行；set
的校验被 registry 层等价复检遮蔽，不可观察，故不绑定）、
`ok-true-shape-removed`（两个 void 控制的 `{ok:true}`）、
`override-mode-after-busy`（mode 检查必须先于 busy 门）、
`unknown-method-dispatched`（dispatch 对非四方法必须返回 None）。
