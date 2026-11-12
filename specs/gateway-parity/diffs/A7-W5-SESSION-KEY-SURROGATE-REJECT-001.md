---
id: A7-W5-SESSION-KEY-SURROGATE-REJECT-001
---

## 类别

有意改进（**比 TS 严**，#1623 人工拍板方案 B）：Rust 对含孤立代理
（unpaired surrogate）的 `sessionKey` 直接拒绝；TS 的 `?.trim()` 门放行，
两侧都把 key 的落盘身份经由替换字符（U+FFFD）有损渲染，代理拼写因此与
显式 U+FFFD 拼写别名到同一份磁盘文件——拒绝是切断别名破坏面的手段，
不做落盘迁移、不改 sanitize 单射性（方案 A 否决）。

## 基线依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0 /
ICU 78.2。`src/cli/createLocalGateway.ts:1118` / `:1131` / `:1144` /
`:1164`（rename/tag/delete/export_session_transcript 各自的
`if (!input.sessionKey?.trim()) throw new DialogGatewayError("INVALID_SESSION_KEY",
"sessionKey is required.")`——fix-r2 更正：`:1164` 属
export_session_transcript，非 record；账本
`specs/gateway-coverage/ledger.json` 的 `record_agent_status_message`
错误分支原无 `INVALID_SESSION_KEY` 条目，fix-r5 补登记——TS 基线该入口
`InProcessGateway.ts:2068` 对 `input.sessionKey` 原样转发，连 trim 门也没有
（live 路径 `SessionRouter.ts:407-419` 精确 Map 查、冷路径
`GatewaySessionHistoryBundle.ts:205-213` 经 `sanitizeSessionIdForPath` 有损
落盘，Node v22.22.0 实测 `probe_ts_record_status_gate.log`））与
`src/model/session/NativeSessionModelSelectionPort.ts:118`（config 族同一门）：
`?.trim()` 对 `s\uD800` 不截、不空（`"\s\uD800".trim()` 仍真值），门放行。
`src/session/storage/ProjectSessionStorage.ts:239`
`sanitizeSessionIdForPath`：纯 `[\\/]+` → `-` 折叠，代理原样进路径；Node
fs 边界把孤立代理编码为 U+FFFD 字节（实测
`_tmp/20261111-issue-1623/probe1_ts_lone_surrogate.log`，Node v22.22.0：
`Buffer.from('s\uD800','utf8')` = `73 efbfbd`，与显式 `'s�'` 同字节；
两文件名先后写入同一目录后仅存 1 项、内容为后写者）——TS 侧
`s\uD800` 与 `s�` 即同一 transcript / file-history / tool-results /
subagents 路径：`deleteSession`（`createLocalGateway.ts:1142-1161`，含
`deleteSessionTranscripts ?? deleteTranscript ?? rm` 三级回退删 transcript
本体）可经代理拼写删掉另一拼写的全部会话文件。**Rust 侧修前实测**
（`probe2_rust_prefix_e2e.log`，本分支修前运行）：`sanitize_session_id`
同形别名（同一路径）、代理拼写的 `tag_session` 把自己的 metadata 追加进
显式 U+FFFD 拼写的 transcript、代理拼写的 `delete_session` 删掉其
file-history 目录。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/mod.rs:199`
`require_session_key`：先走家族共享 `require_text`（blank/非字符串门不变，
#1604），再对通过者的 UTF-16 单元做 `has_unpaired_surrogate`
（`mod.rs:228`，`char::decode_utf16(...).any(|unit| unit.is_err())`——与
存储层 `String::from_utf16_lossy` 同一次走查，判定不可能分歧）检查，含
孤立代理即以守卫族同构的 domain error 拒绝
（`INVALID_SESSION_KEY` + `sessionKey must not contain an unpaired surrogate.`，
同一 RequestError 构造，无第三种错误协议）。`session_dir.rs:66`（resume/
close/delete/rename/tag 五调用方）与 `config.rs:69`（session_model_set/
clear、permission/thinking/style 族写路径）的家族包装全部委托该共享守卫
（`:66`/`:69` 为包装 doc 首行；fn 定义行在 `session_dir.rs:76` /
`config.rs:81`，行号口径统一由跟进单 #2993 承接）。
fix-r2 追加三个入口：`runtime/turn/submit.rs` `validate()`（主写路径：
wire sessionKey 原样进 `RunRef`，经生产 `assembly.rs` create_storage 链落盘；
错误走该入口既有 error-event 通道
`{type:"error",code,message,recoverable:true}`，registry 与 session factory
之前短路。fix-r8 次序口径：该门位于 mode/basePermissionMode 与 runId 校验
之后、busy 拒绝之前——组合非法输入（如 `{mode:"bogus",sessionKey:"\ud800"}`）
保持 TS 的产码次序（TS 先答 INVALID_PERMISSION_MODE，`probe_ts_gates.log`
实测无 sessionKey 门），与同批 config 族的入口次序一致（mode → busy →
sessionKey 门）；先序钉
`submit_turn_surrogate_gate_precedes_the_busy_refusal` 不受影响（busy 在
validate 之后），另有 `submit_turn_combined_invalid_keeps_the_ts_mode_code`
钉组合次序）；`methods/tools.rs` `tools_rewind_files` 的 sessionKey 门改委托
共享 `require_session_key`（原 `required_trimmed` 门不拒代理，而 rewind 经
`create_session_storage` 读对方 transcript、把 `FileHistoryStore::rewind`
的快照还原进项目工作树）；`native_archive.rs` `session()` 在 blank 分支后
对 String 值追加同一 `has_unpaired_surrogate` 判定（该族 `session_id()`
有损渲染 `<id>.jsonl` 文件名）。三处共用同一谓词 `has_unpaired_surrogate`
与同一文案常量 `SESSION_KEY_SURROGATE_MESSAGE`（`methods/mod.rs` 定义、
经 `native_process.rs` re-export），仍无第二种错误协议。
fix-r4 追加第四个入口（#2992）：`session_model.rs` `execute()` 的
sessionKey 门在 blank/非字符串门后对 String 值追加同一
`has_unpaired_surrogate` 判定 + `SESSION_KEY_SURROGATE_MESSAGE`（该入口
`sanitize_session` 有损渲染 `<id>.jsonl` 文件名后读 saved/effective model
选择，与 `native_archive` 的 `session()` 逐字同构；错误码沿用账本该入口
既有映射 `ledger.json` `INVALID_SESSION_KEY` →
`NativeSessionModelSelectionPort.ts:120`，无新错误协议）。
fix-r5 追加第五个入口（#2998）：`session_dir.rs` `session_record_status`
（`record_agent_status_message`）在既有 `must be a string.` 门后追加同一
`has_unpaired_surrogate` 判定 + `SESSION_KEY_SURROGATE_MESSAGE`（该入口
无 trim 门，A7.8-INTERACTION-NONSTRING-OWNER-001-R1）；其交给 key 的每个
身份都是有损渲染——fence 路径 `session_transcript_path`、冷路径
`append_metadata_with_lease` 经 `sanitize_session_id`，活路径
`assembly.rs` `live_recorder` 亦按 `sanitize_session_id` 比对（`/`-vs-`-`
别名同一次走查），故代理拼写命中 `s\u{FFFD}` 会话的 transcript 与活写者
（fix-r5 修前探针 `_tmp/20261026-issue-1623/fix-r5/probe_prefix_e2e.log`：
代理拼写 recorded:true 且条目落 FFFD 会话 transcript、两拼写同一文件）。
账本 `ledger.json` 该入口错误分支补 `INVALID_SESSION_KEY` 条目（fix-r5，
如实标注为 Rust 侧本条登记差异，TS 基线无此分支）。fix-r6 更正：该账本
登记已撤销——`record_agent_status_message` / `submit_turn` /
`tools_rewind_files` 三入口的 Rust 侧孤立代理门均不入 TS 基线账本（无
`B/` 锚点、无 TS 侧产码，schema 无法表达 Rust-only 分支，fix-r5 补登记
即被 `check_ledger.py` 四项拒：error_group_order / refs 与 summary 的
reference_format / invented_error_code），正本在本 diff——防后人再犯。
成对代理（astral 字符，如 U+10000 = D800 DC00）与显式 U+FFFD 仍是合法
key；存储层 `sanitize_session_id`（`transcript_adapter.rs:2162`）保持有损
别名不动（方案 B 残余：防线是入口守卫，非磁盘身份单射化，
`transcript_locks.rs` 的 `session_transcript_path_keeps_the_surrogate_fffd_alias`
钉住该残余）。

## 范围

覆盖的 sessionKey 门：session_dir 五方法（resume/close/delete/rename/tag）
与 config 族写路径入口（r1，经共享 `require_session_key`）；fix-r2 追加
三个入口——`submit_turn`（主写路径）、`tools_rewind_files`（有磁盘别名面：
transcript 读 + file-history 工作树还原）、`native_archive` 族（有磁盘
别名面：`session_id()` 有损渲染 transcript 文件名）；fix-r4 追加第四个
入口——`session_model_get`（本地路由 `dispatch.rs:102-116`，读路径别名面：
`sanitize_session` 有损渲染 `<id>.jsonl` 后读 saved/effective model 选择，
#2992）；fix-r5 追加第五个入口——`record_agent_status_message`
（`session_dir.rs` `session_record_status`，写路径别名面：fence 路径、
冷路径 `append_metadata_with_lease` 与活路径 `live_recorder` 全部经
`sanitize_session_id` 有损渲染，#2998）。五处 TS 侧实测（fix-r2
`_tmp/20261111-issue-1623/fix-r2/probe_ts_gates.log`
与 fix-r4 `_tmp/20261026-issue-1623/fix-r4/probe_ts_session_model_gate.log`、
fix-r5 `_tmp/20261026-issue-1623/fix-r5/probe_ts_record_status_gate.log`，
Node v22.22.0）：submitTurn（`InProcessGateway.ts:764-845`）与
rewindFilesForSdk（`ProjectRuntimeRegistry.ts:1125-1131`）**均无**
sessionKey 门，native_archive 族仅 `?.trim()` blank 门
（`createLocalGateway.ts:1619-1620`，孤立代理放行），
session_model_get 的 `requireSessionKey`（`NativeSessionModelSelectionPort.ts:118-121`）
亦只有同一 blank 门（`"s\uD800".trim()` 非空放行），
recordAgentStatusMessage（`InProcessGateway.ts:2068`）连 trim 门也没有、
`input.sessionKey` 原样转发，冷路径经 `sanitizeSessionIdForPath` 有损落盘
（两种拼写同 `73 efbfbd` 字节）——五处拒绝均为本条
「比 TS 严」的延伸，非 TS 对齐。**不覆盖**：
① `close_project_sessions`
等的 `projectKey`（#2083 已钉代理放行，trim 门共享但拒绝只挂
sessionKey）；② `new_session` 生成的 key（channelKey 有 UTF-8 门，
前缀只含配置项目拼写，生产不可达，R13 已登记）；③ permission/
steer/user_dialog 各自的 `require_text` sessionKey 门与 tools 家族其余
sessionKey 门（background_task_stop / background_tasks：内存
subagents/tasks 注册表，`task.session_id == *session_key` 精确 JsString
匹配，无磁盘别名面）。另：Rust `session_delete` 缺 TS 的
`deleteSessionTranscripts ?? deleteTranscript ?? rm` 回退链
（生产 `JsonlTranscriptStore` 未实现 `delete_session` capability，
`session_dir.rs` 调用处静默跳过 transcript 文件本体、只删目录）——独立
于本条的既有差异，修前探针观测到（probe2：delete 后 transcript 文件
幸存、file-history 目录被删），本条不修，登记供跟进。

## 机器判据

- 行为 pin `native_process/methods/session_dir.rs` tests：
  `require_session_key_rejects_lone_surrogates_and_blank`（守卫级）+
  `require_session_key_keeps_fffd_and_paired_surrogates`（对照组：显式
  U+FFFD 与成对代理放行）+ `resume_session_rejects_lone_surrogate_session_key`
  + `{close,delete,rename,tag}_session_surrogate_key_gate`（四破坏性方法
  各一，错误形状断言）+ `surrogate_key_cannot_reach_the_fffd_transcripts_files`
  （#1623 验收：两拼写不解析到同一文件——A 的 `s�` transcript 与
  file-history 经 B 的代理拼写 tag/delete 后原样幸存）+
  `native_process/methods/config.rs` tests
  `session_model_set_surrogate_key_answers_the_session_key_gate`（config
  写路径入口）；fix-r2 三门各一组（拒绝 + FFFD/成对代理对照）：
  `runtime/turn/submit_tests.rs`
  `submit_turn_rejects_lone_surrogate_session_key`（error-event 形状 +
  session factory 未达）与 `submit_turn_keeps_fffd_and_paired_surrogate_keys`、
  `native_process/methods/tools.rs` tests
  `rewind_files_rejects_lone_surrogate_session_key` 与
  `rewind_files_keeps_fffd_and_paired_surrogate_keys`（对照过门走到
  NO_CHECKPOINT）、`native_archive/tests.rs`
  `archive_gate_rejects_lone_surrogate_session_key`（session() 门 +
  response() wire 层）与 `archive_gate_keeps_fffd_and_paired_surrogate_keys`；
  fix-r4 一组：`session_model/tests.rs`
  `session_model_get_rejects_lone_surrogate_session_key`（入口门 +
  code/共享文案断言）与 `session_model_get_keeps_fffd_and_paired_surrogate_keys`
  （对照：显式 U+FFFD 与成对代理各自 seed 的 saved 选择被自己的拼写读到），
  另 `runtime/turn/submit_tests.rs`
  `submit_turn_surrogate_gate_precedes_the_busy_refusal`（busy 会话上门先于
  `session_busy` 应答的次序钉：持有 turn 预留后 submit，断言
  INVALID_SESSION_KEY error-event 且无 `session_busy` 事件）；
  fix-r5 一组：`native_process/methods/session_dir.rs` tests
  `record_agent_status_message_rejects_lone_surrogate_session_key`
  （入口门 + code/共享文案断言）与
  `record_agent_status_message_keeps_fffd_and_paired_surrogate_keys`
  （对照：显式 U+FFFD 与成对代理各自的 status 条目落自己的
  `<id>.jsonl`，互不串写，`recorded:true`）。
  录制原件回放四面（`production_default_archive_wire_goldens` /
  `archive_native_materialized_138_rpcs_match_with_explicit_path_relocation` /
  pairing 的 `native_archives_match_production_wire_and_do_not_reach_upstream`
  与 `archive_native_materialized_138_pairs_are_local`）经
  `surrogate_gate_divergence` 期望覆盖：`session-unpaired` 的录制 TS 应答
  （SESSION_NOT_FOUND）保持原字节，期望改判本条登记的 INVALID_SESSION_KEY
  （生产 stringify 重建，`cases.json` SHA 钉不变）。
- 负控 `tests/fixtures/native-config/negative_controls.py`：
  `session-key-surrogate-guard-removed-{resume,delete,e2e,config}`（共享
  守卫 `if has_unpaired_surrogate(...) {` → `if false {`，一码点四绑，
  须 KILLED）、`session-key-surrogate-message`（错误 message 走形须 KILLED；
  fix-r2 重绑：文案已提升为共享常量 `SESSION_KEY_SURROGATE_MESSAGE`，变异
  改编辑常量定义）、重绑后的 `session-dir-surrogate-rejected`（`trim_guard`
  内重加 `to_utf8` 过滤——拒绝退化回 blank 报文、代理 projectKey 被 #2083
  门外折叠——须 KILLED）；fix-r2 三门各一行（绑各自拒绝用例）：
  `submit-entry-surrogate-guard-removed`（`submit.rs` 门条件 → `&& false`）、
  `rewind-entry-surrogate-guard-removed`（`methods/tools.rs` 门调用退回
  `required_trimmed`）、`archive-entry-surrogate-guard-removed`
  （`native_archive.rs` 门分支 `if false`）；fix-r4 追加
  `session-model-entry-surrogate-guard-removed`（`session_model.rs` 门条件
  → `if false`，绑 reject 用例的 expect_err 专属串）；fix-r5 追加
  `record-status-entry-surrogate-guard-removed`（`session_dir.rs`
  `session_record_status` 门条件 → `if false`，绑 reject 用例 expect_err
  专属串），均须 KILLED。计数不写死：
  `python3 -I src/pilotdeck-gateway/tests/fixtures/native-config/negative_controls.py --check`
  实报为准（fix-r5 时点 128 变异锚 / 82 literal anchor 全过；`--check` 为只读
  复算、不持树锁——该口径措辞由跟进单 #2993 承接）。
- 复算：`cargo test -p pilotdeck-gateway --lib -- surrogate` 须全绿
  （r1 十一例 + fix-r2 六例
  `submit_turn_{rejects,keeps}_*` / `rewind_files_{rejects,keeps}_*` /
  `archive_gate_{rejects,keeps}_*` + fix-r4 三例
  `session_model_get_{rejects,keeps}_*` /
  `submit_turn_surrogate_gate_precedes_the_busy_refusal` + fix-r5 两例
  `record_agent_status_message_{rejects_lone_surrogate,keeps_fffd_and_paired}_*`
  （fix-r5 时点 70 过）；该过滤器同时命中
  r1 守卫级与 #2083 projectKey 用例）；TS 侧
  `node _tmp/20261111-issue-1623/probe1_ts_lone_surrogate.mjs` 与 fix-r2
  `node _tmp/20261111-issue-1623/fix-r2/probe_ts_gates.mjs`、fix-r4
  `node` 实测 `_tmp/20261026-issue-1623/fix-r4/probe_ts_session_model_gate.log`
  （`"s\uD800".trim()` 非空放行 + 两种拼写同 `73 efbfbd` 字节）、fix-r5
  `node` 实测 `_tmp/20261026-issue-1623/fix-r5/probe_ts_record_status_gate.log`
  （recordAgentStatusMessage 的 key 原样到 fs 边界 + 两种拼写同
  `73 efbfbd` 字节、两文件名先后写仅存 1 项）。

## 批准记录

未批准，非已接受差异。人工方向拍板（方案 B：拒绝 + 登记）见 #1623
协调记录；正式批准状态待人复核。
