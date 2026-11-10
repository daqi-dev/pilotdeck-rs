---
id: U9-ASYNC-HOOK-BRIDGE-001
---

## 类别

不可避免：本 slice 没有 `sdk_session_config` wire 方法与 SDK session 注册表（`src/pilotdeck-gateway/src/capabilities.rs` 中 `sdk_session_config` 为 NOT_ADVERTISED），无法按 TS 的 per-session `sdkHooks`/`includeHookEvents` 门控装配注册桥；桥的安装条件与 `include_hook_events` 取值因此固定，行为面在 TS 对应条件的两类取值下分别等价（见范围）。冲突传播属 bug 修复（TS throw 形状，Rust 原实现为静默吞掉）。

## 基线依据

固定 cc7c4e6：`src/cli/SessionInteractionBundle.ts:146-167`（`gatewayAsyncHookRegistration && options.sdkHooks` 双门控 → HookRuntime ctor `onAsyncHookRegistered`，`includeHookEvents: options.includeHookEvents === true`）；`src/cli/ProjectSessionFactory.ts:231-243`（闭包包 `sessionKey: context.sessionKey`）；`src/gateway/client/InProcessGateway.ts:699-731`（registerAsyncHook：activeTurnRunId 圈定 + blank id 静默返回 + 同 session 同 run 幂等 + 冲突 throw `ASYNC_HOOK_INVOCATION_CONFLICT` :724-729）；`HookRuntime.ts:170`（run 不捕获，throw 上抛为 dispatch 错误）。实跑 Node v22.22.0：`/home/clawbox-08/ws/pdrs-runner` `_tmp/20261026-issue-1438/ts-probe/output.txt`（12/12 断言，dist InProcessGateway + SessionRouter）与静态 grep `expireAsyncHooksForTurn` 零调用（本单 RESULT 附原始输出）。不是只读码推断。

## Rust 行为

`native_process/assembly/steer.rs` `build_async_hook_registry` 构建注册表后把 `GatewayAsyncHookSink` 装入 InteractionHooks 资源（`set_async_hook_sink`，`runtime/project/interaction_hooks.rs` State 新字段）；`bind_with_options` 在调用方未显式传 `on_async_registered` 且 sink 已装时，构造 per-turn handler（捕获 `binding.session_id` + sink），生产 `bind_turn`（默认 options）恒获得该桥。`GatewayAsyncHookSink::register` 补 `session_key`、固定 `include_hook_events: false`，透传其余字段到 `AsyncHookRegistry::register`（scoping/幂等/冲突语义在该处实现，对齐 InProcessGateway.ts:699-731）。`runtime/hooks/runtime.rs` 的 `AsyncHookRegistrationHandler::handle` 改为返回 `Result<(), HostError>`，调用点 `?` 上抛——冲突以 `module_error.code = ASYNC_HOOK_INVOCATION_CONFLICT` 的 HostError 失败所在 hook dispatch，对齐 TS throw。

## 范围

（1）桥的安装条件：InteractionSteer 与 InteractionHooks 均已 stage 时无条件安装（等价于 TS `gatewayAsyncHookRegistration` 恒真 + `sdkHooks` 恒真——本 slice 的 hook 配置均来自会话/宿主装配，与 TS `sdkHooks` 为真的 SDK 会话同形；无 hooks 资源或无 steer 资源时不装，on_async_registered 保持 None → 本地 pending 回退，即无桥的既有退化）。（2）`include_hook_events` 固定 false：TS 读 `sdkSessionConfig?.includeHookEvents`，本 slice 无该配置面，undefined → false；唯一可见差异是 hook 生命周期事件不通过 registry event sink 发出（#1305 事件族已另有覆盖）。（3）冲突传播通道：注册失败使产生该 async 输出的 dispatch 失败（等价 TS HookRuntime.ts:170 不捕获）；正常注册路径无返回值差异。不涉及过期侧：`expire_for_turn` 保持生产零调用（TS 侧同名方法亦零调用，实测定案，见 RESULT「异常/裁决」）。

## 机器判据

`native_process/assembly/steer.rs` tests：`production_hook_dispatch_registers_async_output_in_the_gateway_registry`（真实 dispatch → registry 注册 → 方法层 submit delivered + 本地 pending 空）、`production_async_hook_conflict_fails_the_dispatch`（r2 同 id dispatch → Err code=ASYNC_HOOK_INVOCATION_CONFLICT）、`hook_async_result_delivers_registered_context_through_the_method_layer`（经 `request("hook_async_result")` 的 delivered 正控）、`production_async_hook_pending_survives_turn_close_until_submit`（close 后 pending 存活至 submit 拒付过期）。steer family 行为测试：`native_process/methods/tests.rs` `cancel_steer_round_trip_empties_the_mailbox_and_reports_too_late_after_drain`（cancel 后 drain 空 + 已 drain 项再 cancel → cancelled:false reason=too_late）、`cancel_steer_after_a_same_text_turn_restart_reports_the_stale_shapes`（方法层 runId 不匹配 → turn_mismatch；runtime 层 admission 持锁 cancel → TurnClosing）；`runtime/turn/gateway_tests.rs` `abort_turn_request_is_intercepted_before_the_methods_registry`（拦截正控 `ok:true` + 同 mock 对未拦截方法仍报错的对照）。负控 `tests/fixtures/issue-1438-registry-wiring/negative_controls.py` 五行（sink 不装 / handler 不装 / 冲突吞回 / too_late 拒绝移除 / abort_turn 拦截移除）各绑上述 receipt 断言，变异体已逐个 apply → `cargo test --no-run` → kill 复验 → restore 编译验证（本单五/五全灭于各自绑定 oracle）并由协调者在宿主跑。复算命令：`cargo test -p pilotdeck-gateway --lib assembly::steer`（无 -q）。

## 批准记录

未批准，非已接受差异。2026-11-11 任务书 #1438（aafcbe978831）要求本接线并指定注入点；`include_hook_events=false` 与无条件装桥的取值属实现必要选择，如实记录待人复核，不增加评分豁免。
