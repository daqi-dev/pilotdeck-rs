# Router decision TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测源码：`src/router/RouterRuntime.ts` 的决策部分、`router/scenario/*`、`router/tokenSaver/classifyAndRoute.ts`、`router/policy/*`、`router/session/SessionRouterStore.ts`。`oracle.json` 的每个 decision、sticky 结果、mutation、judge prompt、事件和 materialized request 都由真实 TS 产生；Rust 侧不写死期望值。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/router-decision/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/router-decision
```

生成器先跑共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`（固定 HEAD、干净 tracked 树、逐 blob 对照 Git 对象），再对 `src/router/RouterRuntime.ts` 与 `src/pilot/config/loadPilotConfig.ts` 的 import 闭包逐个文件记 `sourceSha256`。`locale` 断言 `en-US`（`Intl.Collator` 默认 locale），与 config/snapshot 夹具一致。

配置不是手写对象：每个 case 将 YAML 写入独占临时 home，调用真实 `loadPilotConfig({configPath, env, version:1})`，把 `snapshot.config.router` 传给 `createRouterRuntime`，同一份 JSON 存进 `oracle.json` 供 Rust 用 `RouterConfig::from_snapshot` 读取。临时目录用 `mkdtemp` 创建，`finally` 只删本进程创建的那一棵。

## 覆盖（27 个 case）

- `disabled-router`：`router.enabled=false`，决策纯透传（含 subagent 形状）。
- `scenario-resolution`：default、`isMainAgent=false`、`<ccr-subagent-model>` 标签、显式 `metadata.explicitProvider/Model`；覆盖 `missingAgentTool` 造成的 `subagentTagStripped`。
- `unicode-case-fold`：ES 无 `u` 的 `i` 不把非 ASCII 折到 ASCII——工具名 `taſk`（U+017F）不算 agent tool，`<ccr-ſubagent-model>` 不算标签；ASCII `task` / `<ccr-subagent-model>` 为对照，materialize 一步验证长 s 标签不被剥掉。
- `unicode-tier-tag`：judge 连续输出 `<tier>K</tier>`（U+212A KELVIN SIGN）不能匹配 tier 正则，3 次后回退 `defaultTier=complex`；随后 ASCII `<tier>SIMPLE</tier>` 正常命中 `simple`。`k` 是真实 tier，旧 Rust 的 Unicode `(?i)` 会误判为 `k`。
- `sticky`：第一次 judge 后，第二轮带历史命中 sticky 不再调 judge；`invalidateSticky` 后重新 judge。
- `cache-aware`：`observeUsage` + `previousProvider/Model`，judge 目标更贵时 `cacheAwareSwitch=kept_sticky`，保留原模型与 previous tier。
- `orchestration`：`autoOrchestrate.triggerTiers` 首次 `continued:false`，sticky 第二轮 `continued:true`。
- `media-reroute`：选中模型不支持 image 时落到首个兼容 fallback，`resolvedFrom=fallback` 且 `mediaCapabilityRerouted`。
- `judge-model-error`：非重试 provider 错误（`auth_error`）立即回退默认 tier，`token_saver_failed` 事件含脱敏 message。
- `judge-timeout`：`judgeTimeoutMs:500` 下超时不重试，事件 `reason=timeout`、`errorCode=judge_timeout`。
- `subagent-skip`：`subagent.policy=skip` 时子代理不调 judge。
- `validate-policy`：host `isModelAllowed` 拒绝、managed deny/allow 拒绝与放行。
- `materialize`：cache plan provider/model 匹配时保留并取 `messages`，不匹配时 plan/breakpoints 清空；`maxOutputTokens` 按 `getCapabilities` clamp。
- `custom-router`：自定义 router 选中模型，`resolvedFrom=custom`。
- `sticky-expiry`：`advanceClock` 步推进注入时钟；正好 1h 仍命中（严格 `<`），命中刷新 TTL，再过期后重判。
- `judge-caller-abort`：judge 超时后记录调用方 `AbortSignal.aborted`，必须是 `false`（P1-1 回归）。
- `judge-retryable-provider`：可重试 provider 错误打满 3 次；message `无效token=sk-abc` 的真实 TS 脱敏结果为 `无效token=<redacted>`。
- `judge-request-error`：`ModelRequestError` 不重试（1 次后回退）。
- `judge-parse-error`：空文本 3 次，`parse_error` fallback `attempts=3` 且无 `errorCode/errorMessage`。
- `judge-retry-success`：空文本第 1 次、第 2 次拿到 tier 后成功（1s 重试间隔）。
- `subagent-judge-sticky`：subagent `policy=judge` 使用独立 `:sub` sticky 槽，与主 agent 互不污染。
- `invalidate-orchestrating`：编排中 `invalidateSticky` 保留 provider/model/tier，下一轮 `continued:true`。
- `cache-aware-switched`：目标模型 prefill 更便宜且已有缓存命中时 `cacheAwareSwitch=switched`。
- `custom-failed`：自定义 router 抛错发 `pilotdeck_router_custom_failed` 并回退 scenario。
- `disabled-invalidate`：`enabled=false` 时 `invalidateSticky` 返回 `{orchestrating:false}`。
- `empty-description`：`description:""` 与 `previousTier:""` 按 JS 真值不追加到 judge prompt。
- `judge-empty-error-fields`：错误 `code/message` 为空串时事件省略对应键。

每个 step 记录结果、累计 judge 调用数与该 step 新发事件；case 级记录全部 judge prompt，Rust 回放逐字比较生成的 prompt。

## Rust 回放

`tests/router_decision.rs`：

- 对每个 case 构造 `RouterConfig::from_snapshot` + 注入的假 `RouterModelInvocation`（capabilities/multimodal/judge script），同一个 runtime 跑完整 step 序列；decision、mutation、invalidate、validate 错误码/文案、materialize 结果、事件与 judge prompt 全部比对。
- 负控 1：把 oracle 第一条 expected decision 的 `model` 改掉，`run_case` 必须报错。
- 负控 2：sticky case 每个 step 新建 runtime（共享 judge invoker），必须复现失败——证明 sticky 走的是共享 session store，而不是碰巧同模型。
- `expected_decision_mutation_is_detected` / `sticky_replay_requires_shared_runtime_state` 是上述两条负控的入口。

## 不在本夹具

- provider HTTP、SSE、真实 judge 调用（A6.4-pr5）；fallback/execute/health/stats 的运行时行为（A6.4-pr9）。
- 代码围栏/词边界的纯解析边界在 `runtime/model/router/tests.rs` 单测覆盖；node 探针原值见 `_tmp/20261019-a64-pr4/probes.json`。
