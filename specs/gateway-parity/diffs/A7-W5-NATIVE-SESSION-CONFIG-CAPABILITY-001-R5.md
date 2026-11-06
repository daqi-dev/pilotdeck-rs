---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R5
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

跟进实现：-R4 把三个选择器方法收窄为「校验通过后如实回 `CAPABILITY_UNAVAILABLE`」，真正接线（写入有读方的存储、下一回合生效）转独立跟进单。本条 #1864 落地该接线：新增 per-gateway 的 `SessionOverrides` 存储（TS `sessionOverrides` / `sdkThinkingOverrides` / `sdkSessionConfigs.outputStyle` 三张 registry map 的 native 对应物），三个方法校验通过后写入并回 `applied: true`，下一次会话组装读回生效；`output_styles_list` 回显 `selected`。门序、报文、TypeError 形状与 R4 完全一致，不放宽任何判分。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0。-R4 已引：`createLocalGateway.ts:1371-1382` / `:1403-1409` 的回调把选择写进 per-gateway `registry`，下一次 `createSession` / `query` 读回。本次补齐读侧事实：`SessionAgentConfigBundle.compose` 的 `permissionMode` 优先级为 `organizationPermissions?.defaultMode ?? sdk?.managedPermissions?.defaultMode ?? (sdk?.permissionMode === "dontAsk" ? "default" : sessionOverride?.permissionMode ?? options.permissionMode)`；thinking 写入以 presence 为准（`ProjectRuntimeRegistry.ts:906-912` 仅在 `undefined` 时 delete，`structuredClone` 连 `null` 一起存）；`appendSystemPrompt` 合并为 `[existing, styleAppend].filter(v => v?.trim()).join("\n\n") || undefined`；`InProcessGateway.ts:1443` 每次 run 重读 `this.permissionModes.get(key)`（`inputMode ?? livePermissionMode ?? default`），permission mode 因此无需 close；`setSessionThinking` / `setOutputStyle` 回调存表后 `router.close`（后者 reason `sdk_output_style_changed`）；`outputStylesListForSdk` 从 `sdkSessionConfigs` 回显 `selected`。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/session_overrides.rs`（新增）：`SessionOverrides`，`Mutex<Inner>` 三张 `HashMap`（键为 `JsString` 本身，哈希按 UTF-16 units，孤立代理不会折叠共享槽位；presence 即覆盖），poisoning 用 `unwrap_or_else(|e| e.into_inner())`。`assembly` 创建唯一实例，同时交给 `Requests`（三个方法的写入方）与 prepare 闭包（读方）。

`src/pilotdeck-gateway/src/native_process/methods/config.rs`：

- `set_permission_mode`：门序不变 → 写存储 → `{applied: true}`，不 close。
- `set_session_thinking`：门序不变 → `validate_thinking` 后 thinking 缺失/Undefined 清除、否则写入（null 亦是值）→ `owner.close`（`runtime_error` 映射）→ `{applied: true}`。
- `set_output_style`：门序不变 → name 非空校验后存（null 清除）→ `owner.close` → `{applied: true}`，name 非空时附 `selected`。
- `output_styles_list`：sessionKey 存在且存储有值时回 `selected`。
- `session_config_unavailable` 辅助删除（无调用方）。

`src/pilotdeck-gateway/src/native_process/assembly.rs`：prepare 闭包读回三选（`apply_session_overrides` 纯函数 + `output_style_content` 异步查 PluginRuntime 视角，同 `plugin_styles` 的解析路径、不 refresh、样式缺失不报错不覆盖），替换原先写死的 `permissionMode: "default"`；host 闭包经 `TurnHostFactory::with_permission_mode_source` 给 `LiveSessionAdapter` 装上 run 时重读源。

`src/pilotdeck-gateway/src/runtime/turn/live.rs` / `host_factory.rs`：`LiveSessionAdapter` 新增 `permission_mode_source`，`submit()` 在装 payload 前按源覆盖 `permissionMode`（`managedPermissions.defaultMode` 非空仍优先）；`permission_mode()`（#1457 `seed_read_state` 的准入模式来源）同一优先级：managed → 源 → 组装的 `permissionMode` → `"default"`。

`src/pilotdeck-gateway/src/native_process/tools.rs`（fix-r1）：`SessionTools` 同款 `permission_mode_source`，`turn()` 在发射 `permissionContext` 前把有效模式（managed → 源 → 现值）写回 `Permissions` 状态，`Permissions::decide` 每次 clone 现值，host 侧准入与 sidecar 同回合同一模式。三处优先级谓词收拢为 `runtime/permission.rs` 的 `managed_default_mode` / `effective_permission_mode`。

能力清单 7 项不变（-R4 早已登记，本条不触碰 `describe_server`）。

## 范围

三个选择器方法的写入生效与 `output_styles_list` 的 `selected` 回显。`session_model_set` / `session_model_clear` / `seed_read_state` 未动。per-request `input.mode`（`inputMode`）与 `user_dialog_changed` 广播等 R4 未涉及的面继续不在范围。`tests/fixtures/native-config/negative_controls.py` 的 stub-revert 锚点已随本 diff 重锚到更名后的用例；#1864 读写路径各补函数内变异（permission/thinking/style 三写点，compose 的 permission/thinking 读点与 style 合并，`output_styles_list` 回显读点，live submit 源）。遗留：assembly 接线级（store 实例共享）端到端装置转 P2 跟进，本轮靠函数内变异 + `session_overrides.rs` 键单测覆盖。

## 机器判据

`cargo test -p pilotdeck-gateway --lib native_process::methods::config`：`set_permission_mode_applies_and_validates`（有效 mode 存储并 `applied: true`，门表原样）、`set_session_thinking_applies_and_validates`（mode 对象与显式 null 都写入，null 是值不是删除）、`set_output_style_validates_names_and_key`（门表原样 + 成功路径 `applied: true` 与 store 读回 `probe:compact`，锚点 `#1864 set_output_style applies the probe style` + null 清除）、`output_styles_list_reports_project_styles`（有选择回 `selected`，无 sessionKey 或无选择则缺席）、`thinking_and_style_close_the_session_permission_mode_does_not`（thinking/style close、permission mode 不 close）、`apply_session_overrides_composes_the_selections`（纯函数：managed 优先级、null thinking、appendSystemPrompt 合并与双空 → undefined）、`selector_writes_reach_the_compose_step`（方法写入经存储进入组装步骤）。`cargo test -p pilotdeck-gateway --lib native_process::session_overrides`：`lone_surrogate_keys_do_not_fold_onto_replacement_chars`（`JsString` 键下 0xD800 与 0xFFFD 不互覆）。`cargo test -p pilotdeck-gateway --lib permission_mode_source`：同一 live 会话第二回合读到新存的 mode，`managedPermissions.defaultMode` 非空时源不覆盖，`permission_mode()` 同优先级（seed 准入一致）。`cargo test -p pilotdeck-gateway --lib session_tools_admission_mode`：`set_permission_mode` 写入后下一回合 `SessionTools` 发射的 `permissionContext.mode` 与 `Permissions` 准入上下文都是新值，managed 非空时压过它。

## 批准记录

未批准，非已接受差异。2026-11-07 #1864 实现轮 r1 由 AI 代登记：-R4 明示的「真正接线转独立跟进单」落地，待人复核。
