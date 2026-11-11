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
`:1164`（rename/tag/delete/record 各自的
`if (!input.sessionKey?.trim()) throw new DialogGatewayError("INVALID_SESSION_KEY",
"sessionKey is required.")`）与
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
（`mod.rs:221`，`char::decode_utf16(...).any(|unit| unit.is_err())`——与
存储层 `String::from_utf16_lossy` 同一次走查，判定不可能分歧）检查，含
孤立代理即以守卫族同构的 domain error 拒绝
（`INVALID_SESSION_KEY` + `sessionKey must not contain an unpaired surrogate.`，
同一 RequestError 构造，无第三种错误协议）。`session_dir.rs:66`（resume/
close/delete/rename/tag 五调用方）与 `config.rs:69`（session_model_set/
clear、permission/thinking/style 族写路径）的家族包装全部委托该共享守卫。
成对代理（astral 字符，如 U+10000 = D800 DC00）与显式 U+FFFD 仍是合法
key；存储层 `sanitize_session_id`（`transcript_adapter.rs:2162`）保持有损
别名不动（方案 B 残余：防线是入口守卫，非磁盘身份单射化，
`transcript_locks.rs` 的 `session_transcript_path_keeps_the_surrogate_fffd_alias`
钉住该残余）。

## 范围

只覆盖经 `require_session_key` 的 sessionKey 门：session_dir 五方法
（resume/close/delete/rename/tag）与 config 族写路径入口。**不覆盖**：
① `record_agent_status_message`（冷路径，`session_dir.rs:499` 侧 plain
`as_string` 读、TS 本身无 sessionKey 门——两侧同险，TS 一致面，非本条
制造）；② `close_project_sessions` 等的 `projectKey`（#2083 已钉代理
放行，trim 门共享但拒绝只挂 sessionKey）；③ `new_session` 生成的 key
（channelKey 有 UTF-8 门，前缀只含配置项目拼写，生产不可达，R13 已登记）；
④ permission/steer/user_dialog/tools 各自的 `require_text` sessionKey 门
（内存键，精确 JsString 匹配、无磁盘别名面）。另：Rust `session_delete`
缺 TS 的 `deleteSessionTranscripts ?? deleteTranscript ?? rm` 回退链
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
  写路径入口）。
- 负控 `tests/fixtures/native-config/negative_controls.py`：
  `session-key-surrogate-guard-removed-{resume,delete,e2e,config}`（共享
  守卫 `if has_unpaired_surrogate(...) {` → `if false {`，一码点四绑，
  须 KILLED）、`session-key-surrogate-message`（错误 message 走形须 KILLED）、
  重绑后的 `session-dir-surrogate-rejected`（`trim_guard` 内重加 `to_utf8`
  过滤——拒绝退化回 blank 报文、代理 projectKey 被 #2083 门外折叠——
  须 KILLED）。`--check` 85 锚 / 72 literal anchor 全过。
- 复算：`cargo test -p pilotdeck-gateway --lib -- surrogate`
  （本轮 11 过）；TS 侧 `node _tmp/20261111-issue-1623/probe1_ts_lone_surrogate.mjs`。

## 批准记录

未批准，非已接受差异。人工方向拍板（方案 B：拒绝 + 登记）见 #1623
协调记录；正式批准状态待人复核。
