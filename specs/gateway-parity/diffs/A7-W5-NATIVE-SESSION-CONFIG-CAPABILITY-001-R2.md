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

- set_permission_mode：mode 门 → busy 门（has_active_turn）→ CAPABILITY_UNAVAILABLE；**不设 sessionKey 门**，与 TS 一致（TS 只把原始 key 传给 hasActiveTurn，不做形态校验——probe1 实测 `busyKeys:[null]` / `busyKeys:[42]`）。非串 key 在 Rust 侧无处查 turn 而跳过 busy 查询，**「结果与 TS 相同」是读码推断**：probe1 的 `hasActiveTurn` 是注入假件（忽略 key 直接返回场景标志），无法判定真实 router 对 null/42 的查表结果，需在 #1457 装配回调后用真实 router 实测。
- seed_read_state：sessionKey/path/mtime 门 → busy 门 → unavailable；非串 sessionKey 答 `INVALID_SESSION_KEY`（TS 为 TypeError，差异见「范围」）。
- `INVALID_PERMISSION_MODE` 报文按 JS ToString 渲染 `input.mode`：缺失字段渲染 `undefined`（probe1 missing-mode，四组场景一致），非串 42 渲染 `42`。#1455 r1 前缺失字段渲染成空串（`"Unsupported permission mode: "`），属移植偏差而非基线差异，本轮已改为对齐 TS 并加断言测试 + 绑定变异。
- session_model_set / session_model_clear / set_session_thinking / output_styles_list / set_output_style：capability 门第一步即 unavailable，busy 门不可达；session_model_set/clear 即使补装回调也不应加 hasActiveTurn 门（TS 侧本就没有，见 TS 依据第 4 点）。
- 保留守卫 `require_session_key` / `require_output_style_name`：非串 sessionKey 答 INVALID_SESSION_KEY、非串 name 静默通过——两者与 TS 的 TypeError 形状不一致，待 #1457 装配回调时定夺（TypeError 无法映射为 RequestError 码）。
- `methods/config.rs::is_trimmed_non_empty` / `trimmed_string_is_non_empty`（`pub(super)`）：session_dir 与 config 共享的 `?.trim()` 实现，lossy 转换放行 lone-surrogate（对齐 TS）；session_dir 的 `require_session_key` 据此由「to_utf8 拒绝」改为「放行」，返回类型仍为 JsString（config 侧守卫返回 ()，按各自调用形状保留）。**注意**：lone-surrogate 的 TS 实测只覆盖 config 族（probe4 的 setSessionThinking/setOutputStyle）；session_dir 族（resume/close/delete/rename/tag）按同一 `?.trim()` 形状**推断**，尚未对 TS 侧跑探针——落地前不得把这段当「已实测等价」。

## 范围

仅 A7-W5 会话配置 7 方法及共享 trim 守卫。四组差异中：① 已按 TS 对齐（无需改码）；②③ 为已登记事实差异，属 -R1 所述「未实现」范围的一部分，随 #1457 实现单闭合，本条不扩大豁免、不放宽判分；④ 为对齐性修复（session_dir 收敛）。不涉及 proxy 分发、session runtime 生命周期、其他方法族；`native_process/tools.rs::required_trimmed`（`to_utf8` + Rust `str::trim`，另一处同语义实现，JS WhiteSpace 集合与 Rust `trim` 在 U+FEFF / U+0085 上分叉）不在本条范围，前置于 #1604（P2-跟进：`tools.rs` / `session_dir.rs` 迁 `require_text`）。session_dir 族的 `projectKey` 也不在 `require_session_key` 覆盖内（该守卫只查 `sessionKey`），其形态校验与 TS 的对齐同属未收敛项，一并前置于 #1604；`session_dir` 的 lone-surrogate 放行仅为推断（见上），同样由 #1604 落地时用真实 TS 探针闭合。

## 机器判据

`cargo test -p pilotdeck-gateway --lib -- native_process::methods::config::tests native_process::methods::session_dir::tests`（38 项全过）：busy 门配对 `set_permission_mode_busy_during_active_turn` / `seed_read_state_busy_during_active_turn`（SESSION_BUSY + TS 报文）；顺序 `set_permission_mode_invalid_mode_wins_over_busy` / `seed_read_state_rejects_negative_mtime_during_active_turn` / `set_permission_mode_missing_session_key_hits_unavailable`；边界 `seed_read_state_unavailable_with_zero_mtime`（mtime 0）、`seed_read_state_rejects_blank_session_key`、`require_output_style_name_rejects_js_whitespace_name`（U+FEFF）、`set_permission_mode_accepts_every_ts_mode`（default/plan/bypassPermissions 白名单）、`require_session_key_keeps_lone_surrogates_and_rejects_blank`（守卫层，5 个调用点共用）；报文 `set_permission_mode_missing_mode_message_matches_ts` / `set_permission_mode_nonstring_mode_message_matches_ts`；G7 `resume_session_accepts_lone_surrogate_session_key` / `resume_session_rejects_blank_session_key`。负控 `tests/fixtures/native-config/negative_controls.py` 24 条变异（含两个 busy-gate-removed、两处门序反转、插入 TS 没有的 sessionKey 门、mtime 边界/整数性、空白串、缺失/非串 mode 报文、mode 白名单收窄、INVALID_SESSION_KEY、CAPABILITY_UNAVAILABLE 终态码与报文）须全部 KILLED；锚点漂移报错退出。**证据边界**：空白 sessionKey 用例直接构造请求参数，不经真实 router 的字符串归一化；「真实链路上空白键是否原样抵达」尚未实测（与上面 session_dir 的推断同批，前置于 #1604）。

## 批准记录

未批准，非已接受差异。2026-11-06，#1455 P2 跟进单登记（AI 代登记）。本条承袭 -R1 对原条目的定性（未实现，非有意收窄）；②③ 两组差异的闭合由 #1457 装配单承接，人复核时以持久决策指针登记。
