---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R2
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

补充登记（事实层，随 #1455 P2 跟进）：原条目与 -R1 均未记录 7 方法的 SESSION_BUSY 分支分布与非字符串参数的 TypeError 形状。本条登记四组实测差异：① set_permission_mode 在 TS 侧不校验 sessionKey；② set_output_style / seed_read_state / 保留守卫族对非字符串参数，TS 抛裸 TypeError 而非对话框错误码；③ session_model_set / session_model_clear 的 busy 语义是 turn-replacement 转写预约而非 hasActiveTurn；④ lone-surrogate sessionKey 被 TS `?.trim()` 族接受。均为已登记事实差异，不放宽判分。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0 / ICU 78.2，探针原文存 `_tmp/20261106-issue-1455/probes/probe{1..4}.log`（假 router / 注入式 permissionModes 端口，覆盖 IPG 方法体；不等价于 stdio 帧全链路）。要点原文：

- setPermissionMode：`{"label":"missing-key","value":{"applied":true},"busyKeys":[null],"writes":[[null,"plan"]]}`；`{"label":"nonstring-key","value":{"applied":true},"busyKeys":[42]}` —— 缺失/空白/非串 sessionKey 一律放行到 `hasActiveTurn(input.sessionKey)`（收到原始值）再落 permissionModes，无 INVALID_SESSION_KEY（probe1）。
- setOutputStyle（回调已装配）：`{"label":"number","code":"TypeError","message":"input.name.trim is not a function"}`；`{"label":"undefined","code":"TypeError","message":"Cannot read properties of undefined (reading 'trim')"}`；空/空白串才是 `INVALID_OUTPUT_STYLE "name must be non-empty or null."`；busy 门在 name 门之前（probe2）。
- seedReadState：`{"label":"key-nonstring","code":"TypeError","message":"input.sessionKey?.trim is not a function"}`（probe4）；mtime 门在 busy 门之前。
- busy 分布（probe3，active=true、回调装配、合法参数）：`sessionModelSet`/`sessionModelClear` `busyCalls:0` 且返回 applied；`outputStylesList` `busyCalls:0`；`setPermissionMode`/`setSessionThinking`/`setOutputStyle`/`seedReadState` 各 `busyCalls:1` 且 SESSION_BUSY。sessionModelSet/Clear 的 busy 语义来自 `GatewayTurnReplacementCoordinator.reserveTranscriptWrite`（GatewayTurnReplacementCoordinator.ts:57-63，SESSION_BUSY "Cannot change the session model while another transcript update is pending."；该冲突分支为读码推断——单次调用 reserve→release，IPG 层无法直构冲突态）。
- lone-surrogate：`{"label":"thinking-lone-surrogate-key","value":{"applied":"callback"},"busyKeys":["\ud800"]}`（probe4）—— `\ud800` 通过 `?.trim()` 并进入 busy 检查。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/config.rs`（#1455 后）：

- set_permission_mode：mode 门 → busy 门（has_active_turn）→ CAPABILITY_UNAVAILABLE；**不设 sessionKey 门**，与 TS 一致（缺失/非串 key 因无处预留 turn 而跳过 busy 查询，可观察结果与 TS 相同：不 busy、落到 unavailable）。
- seed_read_state：sessionKey/path/mtime 门 → busy 门 → unavailable；非串 sessionKey 答 `INVALID_SESSION_KEY`（TS 为 TypeError，差异见「范围」）。
- session_model_set / session_model_clear / set_session_thinking / output_styles_list / set_output_style：capability 门第一步即 unavailable，busy 门不可达；session_model_set/clear 即使补装回调也不应加 hasActiveTurn 门（TS 侧本就没有，见 TS 依据第 4 点）。
- 保留守卫 `require_session_key` / `require_output_style_name`：非串 sessionKey 答 INVALID_SESSION_KEY、非串 name 静默通过——两者与 TS 的 TypeError 形状不一致，待 #1457 装配回调时定夺（TypeError 无法映射为 RequestError 码）。
- `methods/config.rs::is_trimmed_non_empty` / `trimmed_string_is_non_empty`（`pub(super)`）：session_dir 与 config 共享的 `?.trim()` 实现，lossy 转换放行 lone-surrogate（对齐 TS）；session_dir 的 `require_session_key` 据此由「to_utf8 拒绝」改为「放行」，返回类型仍为 JsString（config 侧守卫返回 ()，按各自调用形状保留）。

## 范围

仅 A7-W5 会话配置 7 方法及共享 trim 守卫。四组差异中：① 已按 TS 对齐（无需改码）；②③ 为已登记事实差异，属 -R1 所述「未实现」范围的一部分，随 #1457 实现单闭合，本条不扩大豁免、不放宽判分；④ 为对齐性修复（session_dir 收敛）。不涉及 proxy 分发、session runtime 生命周期、其他方法族；tools.rs 的 `required_trimmed`（to_utf8 + Rust trim，另一处同语义实现）不在本条范围，待独立跟进。

## 机器判据

`cargo test -p pilotdeck-gateway --lib -- native_process::methods::config::tests native_process::methods::session_dir::tests`（32 项全过）：busy 门配对 `set_permission_mode_busy_during_active_turn` / `seed_read_state_busy_during_active_turn`（SESSION_BUSY + TS 报文）；顺序 `set_permission_mode_invalid_mode_wins_over_busy` / `seed_read_state_rejects_negative_mtime_during_active_turn` / `set_permission_mode_missing_session_key_hits_unavailable`；边界 `seed_read_state_unavailable_with_zero_mtime`（mtime 0）；G7 `resume_session_accepts_lone_surrogate_session_key` / `resume_session_rejects_blank_session_key`。负控 `tests/fixtures/native-config/negative_controls.py` 18 条变异（含两个 busy-gate-removed、mtime 边界/整数性、空白串、INVALID_SESSION_KEY、CAPABILITY_UNAVAILABLE 终态码与报文）须全部 KILLED；锚点漂移报错退出。

## 批准记录

未批准，非已接受差异。2026-11-06，#1455 P2 跟进单登记（AI 代登记）。本条承袭 -R1 对原条目的定性（未实现，非有意收窄）；②③ 两组差异的闭合由 #1457 装配单承接，人复核时以持久决策指针登记。
