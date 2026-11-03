---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

原生隔离宿主的会话配置能力收窄：7 个方法（output_styles_list、set_output_style、get_context_window、set_context_window、get_max_turns、set_max_turns、reset_settings_overrides）接线后恒返回 `CAPABILITY_UNAVAILABLE`，且 `describe_server` 不广告这 7 个方法。不放宽判分。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0。`src/gateway/client/InProcessGateway.ts` 在 `_options.session.*` 接口存在时广告并处理上述方法；native 进程的 `createLocalGateway` 不传入 session configuration callbacks。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/config.rs`：7 个方法注册到 native 方法族，实现均返回 `CAPABILITY_UNAVAILABLE`（「Session configuration not available in native mode.」）。`requests.rs` 的 `describe_server` 不广告这 7 个方法（`is_session_config_enabled()` 返回 `false`）。

实现理由：native 进程没有 session configuration runtime；这是已知隔离差异，先例见 `A8-NATIVE-SNAPSHOT-CAPABILITY-001.md`。

## 批准记录

2026-11-04，AI 代批，待人复核。依据：#1310 实现单与 TS 基线行为一致（native 模式下 session config 不可用）；先例 A8-NATIVE-SNAPSHOT-CAPABILITY-001 对同类差异的处理方式。
