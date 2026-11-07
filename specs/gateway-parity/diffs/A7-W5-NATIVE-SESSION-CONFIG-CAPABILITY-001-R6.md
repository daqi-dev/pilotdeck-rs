---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R6
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正登记（实现落地）。-R2 是 #1465 收口时的现状记录：`tools.rs` 的 `required_trim` 与 `session_dir.rs` 的门仍是 `to_utf8` + Rust `str::trim` 的旧形态，非字符串答 `"<field> must be a string."` / `INVALID_SESSION_KEY`。**该记录已过时**：#1604 把这两处（以及同族 `close_project_sessions` 的 `projectKey` 门）并入 `native_process/methods/mod.rs` 的 `require_text` / `trim_guard`，行为随之改变。本条只登记这次改变，不改判分语义、不放宽任何口径。旧条目按只追加规则保留原样，审计请以本条为准。

## TS 依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（本仓外 checkout，本轮不可读）。本条采信的 TS 形状来自 -R2:33、-R2:37 已登记的两句：

- 门形态是 `!input.<key>?.trim()`：非字符串时可选链**不短路**，`.trim` 不是函数，Node 抛 `TypeError: input.<key>?.trim is not a function`，经边界序列化为 `code: gateway_request_failed` 的请求错误；
- `String.prototype.trim` 的 WhiteSpace 含 U+FEFF，而 Rust `str::trim` 不含。

**未闭合项（如实登记）**：-R2:33 要求的「session_dir 族 projectKey 形态」与 -R2 记的「lone-surrogate 仅为推断，落地时用真实 TS 探针闭合」，本轮仍**没有**跑真实 TS 探针——TS 基线不在本仓可读范围。因此本条对这两点的表述是「与 #1455 probe4 同族口径（config 族已实测）」，不是 session_dir 族的实测等价。该缺口已登记为跟进单 #2068，前置于 #1604 关单。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/tools.rs`：`required_trimmed` 改为一行委派 `super::require_text`（保留具名接缝，五个调用点不变：`rewind_files` 的 `sessionKey` / `userMessageId`、`background_task_stop` 的 `sessionKey` / `taskId`、`background_tasks` 的 `sessionKey`）。旧形态 `string()` + Rust `str::trim` 删除。

`src/pilotdeck-gateway/src/native_process/methods/session_dir.rs`：`require_session_key` 改为委派 `super::require_text`（`resume_session` / `close_session` / `delete_session` / `rename_session` / `tag_session` 共用）。

`close_project_sessions` 的 `projectKey` 门改走 `super::require_text`（-R2:33 明确把它列在 #1604 范围内）。缺键 / 空白 / 非 UTF-8 的报文与错误码**保持原值**（`gateway_request_failed` + `projectKey is required.`）；只有非字符串从原来的域名报文改答可选链 TypeError。

迁移后的三种输入：

| 输入 | 迁移前 | 迁移后 |
|---|---|---|
| 非字符串（如 `7`） | `"<field> must be a string."`（tools）/ `INVALID_SESSION_KEY`（session_dir） | `gateway_request_failed` + `input.<field>?.trim is not a function` |
| `"U+FEFF"`（BOM-only） | tools：Rust `str::trim` 不剥 BOM → 放行（漏判空白）；session_dir：已走 `js_trim` → 拒 | 一律判空白 → 域名错误 |
| 孤立代理项 | session_dir 旧门 `to_utf8` 过滤 → 拒 | 放行并走有损转换（与 #1455 probe4 的 config 族 TS 口径一致） |

`params` 为 `null` 时报 `Cannot read properties of null (reading '<field>')`（可选链读属性的 TS 形状），不再是域名错误。

## 范围

只覆盖 `native_process/methods/` 下这两族已迁的调用点。**不在本条范围**（均已在 -R2 或跟进单登记，语义未变）：

- `session_dir` 族其余 `projectKey` 读点：`resolve_project`（`list_sessions` / `delete_session` / `rename_session` / `tag_session` / `record_session_status` 的可选回落门）与 `session_new` 的前缀门，仍是旧形状 → **#2068**；
- `session_record_status` / `session_new` 刻意保留的 `must be a string.` 键门（TS 该处无 `?.trim()`，见 `A7.8-INTERACTION-NONSTRING-OWNER-001-R1:37`）；
- `steer.rs` / `active_turn.rs` / `runs.rs` / `runtime/turn/gateway.rs` 的 `string()` 门（同一条 A7.8 豁免）；
- `runtime/mcp/sdk.rs` 的同语义手写门（`pub(super)` 边界外的平行实现，行为已对齐，收敛属独立重构）。

负控夹具随之调整：`tests/fixtures/native-config/negative_controls.py` 的 `session-dir-blank-gate-removed` / `session-dir-surrogate-rejected` 两条变异锚从被删除的 `.filter(trimmed_string_is_non_empty)` 改指 `methods/mod.rs` 的 `trim_guard`（绑定测试不变），另新增 `session-dir-project-key-domain-message`（改 `close_project_sessions` 的域报文）绑定新用例。

## 机器判据

`cargo test -p pilotdeck-gateway --lib native_process::methods::tools::tests`：`a_non_string_session_key_throws_the_optional_chain_type_error`（非串 → `gateway_request_failed` + `input.sessionKey?.trim is not a function`）、`a_bom_only_session_key_is_blank`（U+FEFF 判空白 → `INVALID_SESSION_KEY`）、`a_real_session_key_passes_through`（正控）。

`cargo test -p pilotdeck-gateway --lib native_process::methods::session_dir::tests`：`a_non_string_session_key_throws_the_optional_chain_type_error`、`a_null_params_object_reports_the_optional_chain_read_error`、`a_bom_only_session_key_is_blank`、`a_non_blank_session_key_passes_the_gate`、`close_project_sessions_non_string_project_key_throws_the_type_error`、`close_project_sessions_blank_or_surrogate_project_key_is_the_domain_error`（空白 / BOM / 孤立代理项三态钉 `gateway_request_failed` + `projectKey is required.`）；既有 `require_session_key_keeps_lone_surrogates_and_rejects_blank`、`resume_session_accepts_lone_surrogate_session_key`、`resume_session_rejects_blank_session_key` 继续钉守卫本身。

负控：`python3 src/pilotdeck-gateway/tests/fixtures/native-config/negative_controls.py --check`（41 条锚点）；`session-dir-blank-gate-removed` → `resume_session_rejects_blank_session_key` 必红（实测：`session_dir.rs:854`）、`session-dir-surrogate-rejected` → `resume_session_accepts_lone_surrogate_session_key` 必红（实测：`session_dir.rs:833`）、`session-dir-project-key-domain-message` → `close_project_sessions_blank_or_surrogate_project_key_is_the_domain_error` 必红（实测 KILLED，`session_dir.rs:971`）。

## 批准记录

未批准，非已接受差异。2026-11-07 #1604 实现轮由 AI 代登记（-R2 的现状记录已与实现相反，按只追加规则新增本条更正），待人复核。session_dir 族的 TS 实测等价尚未闭合，见上文「未闭合项」与 #2068。
