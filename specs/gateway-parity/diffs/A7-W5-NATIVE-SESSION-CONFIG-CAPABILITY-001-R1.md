---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R1
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正（原条目 TS 依据事实性错误）：原条目称「native 进程的 `createLocalGateway` 不传入 session configuration callbacks」，据此把差异登记为「原生隔离宿主的会话配置能力收窄」。实测固定基线 TS native 传入全部 session config 回调且 `describe_server` 广告全部 7 项，TS 侧不存在该收窄；Rust 侧 7 方法 stub `CAPABILITY_UNAVAILABLE`、不广告，实为**未实现**（功能缺失），非有意收窄。类别由「能力收窄」更正为「未实现」，不放宽判分。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0。`src/cli/createLocalGateway.ts:1096-1097` 传入 `sessionModelSet`/`sessionModelClear` 回调；`:1371-1382` 的 `setPermissionMode` 为完整实现（校验 mode、落 `sessionOverrides`）；`:1403-1409` 传入 `setSessionThinking`、`outputStylesList`、`setOutputStyle` 回调；`seedReadState` 经 `src/gateway/client/InProcessGateway.ts:2423-2442` 的 SessionRouter 生效。真实 describeServer 捕获 `src/pilotdeck-gateway/tests/fixtures/native-management/oracle.json`（`:2` sourceCommit 与基线一致）`contract.capabilities` 含全部 7 项：`session_model_set`（`:2772`）、`session_model_clear`（`:2773`）、`set_permission_mode`（`:2790`）、`set_session_thinking`（`:2794`）、`output_styles_list`（`:2795`）、`set_output_style`（`:2796`）、`seed_read_state`（`:2807`）。原条目所引 IPG.ts:2191-2209 等行号是 InProcessGateway 在回调缺位时的 capability 门行为，不能作为「TS native 不配置回调」的依据。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/config.rs`：7 个方法（session_model_set、session_model_clear、set_permission_mode、set_session_thinking、output_styles_list、set_output_style、seed_read_state）接线为 stub。fix-r4 起对 5 个 TS 带回调门的方法（IPG.ts:2192,2202,2365,2375,2380）capability 门提前到方法入口第一步，报文对齐 TS 缺回调形状（`<method> is unavailable.`）；set_permission_mode 与 seed_read_state 保留先参数验证后 unavailable（与其 TS 原文顺序一致，IPG.ts:2321-2330、2423-2442）。`src/pilotdeck-gateway/src/native_process/requests.rs:124` 的 `describe_server` 静态 capabilities 仅含 `session_model_get`，其余 6 项不广告。SDK session registry、permission-mode store 与 seed-read coordinator 未组装进隔离 native 进程，属功能缺失，待后续实现单补齐。

## 范围

同原条目：本包 native isolated 生产入口（`src/pilotdeck-gateway/src/native_process/`）及 `methods/config.rs` 中注册的 7 个方法。`session_model_get` 的广告/实现已由 #1305 合入、不在本更正范围。更正不改变原条目对判分的收紧方向：不放宽判分、不增加豁免；TS 侧（含 proxy 分发、session runtime 生命周期）与其他方法族不受影响。能力缺失无法通过继承环境变量启用。

## 机器判据

`cargo test -p pilotdeck-gateway config`（树内 `src/` 下）逐方法覆盖：5 个回调门方法对边输入（缺 sessionKey、空 name）先答 `CAPABILITY_UNAVAILABLE` 且报文为 `<method> is unavailable.`（`session_model_set_missing_session_key_hits_capability_gate_first` 等 5 项）；`set_permission_mode`/`seed_read_state` 保留参数验证类期望（`INVALID_PERMISSION_MODE`、`INVALID_SESSION_KEY`、`INVALID_FILE_PATH`、`INVALID_FILE_MTIME`）；保留的参数守卫 `require_session_key`/`require_output_style_name` 直接单测。负控：`src/pilotdeck-gateway/tests/fixtures/native-dispatch/negative_controls.py` 的 `omit-config` 变异（`config: &[]`）须被 `#1305 registry contains every wired method` 抓为 INVALID。`requests.rs::describe_server` 静态列表仍不含这 7 个方法名（`session_model_get` 除外）。

## 批准记录

未批准，非已接受差异。原条目 2026-11-04 的「AI 代批，待人复核」基于错误的 TS 依据，其批准状态作废、不为本更正承袭；本条登记的是未实现的功能缺失，待实现单落地后按更正流程闭合。
