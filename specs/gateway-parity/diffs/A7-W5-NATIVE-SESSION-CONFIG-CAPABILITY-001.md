---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

原生隔离宿主的会话配置能力收窄：7 个方法（session_model_set、session_model_clear、set_permission_mode、set_session_thinking、output_styles_list、set_output_style、seed_read_state）接线后恒返回 `CAPABILITY_UNAVAILABLE`，且 `describe_server` 不广告这 7 个方法。不放宽判分。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0。`src/gateway/client/InProcessGateway.ts` 在 `_options.session.*` 接口存在时广告并处理上述方法（IPG.ts:2191-2209,2321-2330,2364-2385,2423-2442）；native 进程的 `createLocalGateway` 不传入 session configuration callbacks。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/config.rs:12-20`：7 个方法（session_model_set、session_model_clear、set_permission_mode、set_session_thinking、output_styles_list、set_output_style、seed_read_state）注册到 native 方法族，各自验证入口参数（sessionKey、mode、name、path、mtime）后返回 `CAPABILITY_UNAVAILABLE`。`requests.rs` 的 `describe_server` 静态列出 capabilities，7 个 session config 方法均不在其中；过滤逻辑仅针对 snapshot 三方法，无 session config 专属开关。

实现理由：native 进程不组装 SDK session registry、permission-mode store 与 seed-read coordinator；这是已知隔离差异，先例见 `A8-NATIVE-SNAPSHOT-CAPABILITY-001.md`。

## 范围

本包 native isolated 生产入口（`src/pilotdeck-gateway/src/native_process/`）及 `methods/config.rs` 中注册的 7 个方法。仅限上述 7 个 session config 方法的接线与 CAPABILITY_UNAVAILABLE 响应；不涉及 TS 侧、proxy 分发、session runtime 生命周期或其他方法族。描述的 CAPABILITY_UNAVAILABLE 行为不依赖运行时配置，无法通过继承环境变量启用。不放宽判分，不改变对其他差异的豁免范围。

## 机器判据

`src/pilotdeck-gateway/src/native_process/methods/config.rs` 中的单元测试逐方法覆盖入口验证与 CAPABILITY_UNAVAILABLE 最终响应：`session_model_set_requires_session_key`、`session_model_set_unavailable_with_valid_key`、`session_model_clear_requires_session_key`、`session_model_clear_unavailable_with_valid_key`、`set_permission_mode_rejects_invalid_mode`、`set_permission_mode_unavailable_with_valid_mode`、`set_session_thinking_requires_session_key`、`set_session_thinking_unavailable_with_valid_key`、`output_styles_list_unavailable`、`set_output_style_requires_session_key`、`set_output_style_rejects_empty_name`、`set_output_style_unavailable_with_null_name`、`seed_read_state_requires_session_key`、`seed_read_state_requires_path`、`seed_read_state_rejects_negative_mtime`、`seed_read_state_rejects_fractional_mtime`、`seed_read_state_unavailable_with_valid_params`。

`requests.rs::describe_server` 的 capabilities 静态列表（含 snapshot 过滤分支）不包含上述 7 个方法名；对拍装置核 TS capabilities 子集时这 7 个方法不得出现。

## 批准记录

2026-11-04，AI 代批，待人复核。依据：#1310 实现单与 TS 基线行为一致（native 模式下 session config 不可用）；先例 A8-NATIVE-SNAPSHOT-CAPABILITY-001 对同类差异的处理方式。本条不将代批误写为人批，人复核应以持久决策指针登记。
