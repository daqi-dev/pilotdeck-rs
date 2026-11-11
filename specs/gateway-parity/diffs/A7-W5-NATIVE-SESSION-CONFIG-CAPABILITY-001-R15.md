---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R15
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R14`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R14 同法，承接关系由文件名与正文表达。
本件在分支内原编 R14，rebase 时 main 侧另一更正件已占 R14，顺位改号 R15，内容未变）。
登记 #1623（r1 + fix-r2）落地后 -R6 的三处陈述过时——用例名、负控绑定、
rewind_files 门。不改判分语义、不放宽任何口径；旧条目按只追加规则保留原样，
审计以本条与 -R6 并读为准（-R6 对 #1604 迁移本身的记录仍准确）。

## 基线依据

-R6 记录的 TS 依据不变（`?.trim()` 门形态、U+FEFF WhiteSpace），但 -R6
「迁移后」表中「孤立代理项（`sessionKey`）……放行并走有损转换（与 #1455
probe4 的 config 族 TS 口径一致）」一行只描述 #1604 到 #1623 r1 之间的过渡
窗口：#1623 人工拍板方案 B 后，经 `require_session_key` 的调用点一律拒绝
孤立代理 key（比 TS `?.trim()` 放行严，差异正本
`A7-W5-SESSION-KEY-SURROGATE-REJECT-001`）。该行现在只对仍走 `require_text`
的键位成立（如 `close_project_sessions` 的 `projectKey`，#2083/R13 已另钉）。
-R6 机器判据引用的两用例名
`require_session_key_keeps_lone_surrogates_and_rejects_blank` 与
`resume_session_accepts_lone_surrogate_session_key` 在 r1 已分别更名为
`require_session_key_rejects_lone_surrogates_and_blank` 与
`resume_session_rejects_lone_surrogate_session_key`。

## Rust 行为

-R6「Rust 行为」记 `tools.rs` `required_trimmed` 一行委派 `require_text`，
五个调用点含 `rewind_files` 的 `sessionKey`。#1623 fix-r2 把该调用点改走共享
`require_session_key`（`methods/tools.rs`）：孤立代理 `sessionKey` 由放行改为
拒绝（rewind 经 `create_session_storage` 读对方 transcript、把
`FileHistoryStore::rewind` 的快照还原进项目工作树，有磁盘别名面）；其余四个
键位（`userMessageId`、`background_task_stop` / `background_tasks` 的
`sessionKey` / `taskId`）仍如 -R6。同轮 `submit_turn`（`runtime/turn/submit.rs`
`validate()`）与 `native_archive` 族（`native_archive.rs` `session()`）各接
同一判定，详见 `A7-W5-SESSION-KEY-SURROGATE-REJECT-001` 的 fix-r2 段。

## 范围

本条只更正 -R6 的三处陈述：①「迁移后」表的孤立代理 sessionKey 行（见基线
依据）；②机器判据的两用例名（见基线依据）；③rewind_files 门（见 Rust 行为）。
-R6 的其余内容——非串 TypeError、U+FEFF 判空白、`params` null 读错、
负控锚随 #1604 迁移改指 `trim_guard`——仍准确。`steer.rs` / `active_turn.rs`
等 A7.8 豁免的 `string()` 门不受 #1623 影响，仍如 -R6。

## 机器判据

-R6 记 `session-dir-surrogate-rejected` →
`resume_session_accepts_lone_surrogate_session_key` 必红。#1623 r1 把该行重绑：
变异内容改为在 `trim_guard` 内重加 `to_utf8` 过滤（拒绝退化回 blank 报文），
绑定 `resume_session_rejects_lone_surrogate_session_key`。-R6 所记「41 条锚点」
计数随后续批次增长，复算命令
`python3 -I src/pilotdeck-gateway/tests/fixtures/native-config/negative_controls.py --check`，
以实报为准（fix-r2 时点 88 变异锚 / 75 literal anchor），本条不再固化数字。
fix-r2 机器判据新增
`rewind_files_rejects_lone_surrogate_session_key` /
`rewind_files_keeps_fffd_and_paired_surrogate_keys`
（`native_process::methods::tools::tests`），负控新增
`rewind-entry-surrogate-guard-removed`（门调用退回 `required_trimmed` 须
KILLED），详见 `A7-W5-SESSION-KEY-SURROGATE-REJECT-001` 机器判据节。

## 批准记录

未批准，非已接受差异。2026-11-11 #1623 fix-r2 实现轮 AI 代登记（-R6 的
引用已与实现相反，按只追加规则新增本条更正），待人复核。
