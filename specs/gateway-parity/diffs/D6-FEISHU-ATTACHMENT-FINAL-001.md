---
id: D6-FEISHU-ATTACHMENT-FINAL-001
---

## 类别

bug 修复：附件事件不得永久关闭同一回合的后续文字回复。固定 TS 自身吞文本，Native 有意修正；非无差异等价。

## 基线依据

固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，src/adapters/channel/feishu/FeishuChannel.ts:619-622 在 assistant_attachment 上调用 liveReply.flushFinal。真实 ImLiveReplyController.flushFinal 关闭 controller，后续 assistant_text_delta 被忽略。非冻结真实采集 _tmp/20261110-channel-difference-evidence/feishu-attachment-final/oracle.json（SHA 1eec6802141fd9e7c0f86d098326e13fb59f64841d0f8ef34d131a5fb58801de）在 Node22.22.0/ICU78.2 用真实 Feishu adapter、真实 attachment delivery 与 loopback REST 观察：前文字和文件已送达，AFTER-ATTACHMENT-FINAL 未送达，回合已结束。外层 gateway 是显式脚本事件源。

## Rust 行为

src/pilotdeck-gateway/src/channel/feishu/mod.rs 的 assistant_attachment 分支改为 live.flush_segment(true)，先结束附件前文字 segment，再发送真实附件，保持 controller 开放，允许后续同 run final。r43 原始 actual Server/A7 transcript 证明 final 已生成且 turn_result success，但渠道未发送，排除 provider 失败。补丁位于 _tmp/20261110-r43-channel-fixes/increment.patch；patched actual host 验证尚待新批运行。

## 范围

仅 Feishu 的非终态 assistant_attachment 与后续文本顺序；不更改终态 flush_final、timeout/abort、公开协议、共享 controller 或其他平台。已有历史 golden 原字节保留；本条不授权冻结尺子修改。

## 机器判据

真实 TS：_tmp/20261110-channel-difference-evidence/feishu-attachment-final/capture.mjs，绑定固定源码 SHA 与 Node/ICU。Native：native_process::feishu_actual_host_tests::feishu_actual_server_media_a7_upload_interactions_new_reload_shutdown，须实际观察 send_attachment 文件出站后 feishu-actual-final-0-1，再通过权限/问询/new/reload/shutdown。r43 首败保留，修后待复跑，不能仅靠源码或单测声称实际通过。

## 批准记录

未批准，非已接受差异。产品修复已由主协调按既有人批默认修 bug 原则授权继续；这不是差异豁免、独立评审或正式验收签字。由主协调复核真实证据后另行新增正式条目，原冻结 ledger 不改。

Evidence manifest: _tmp/20261110-channel-difference-evidence/manifest.json. Raw Native first failures and immutable r43/r46 source bindings are retained under first-failures/; r48 rerun and independent reviews remain pending.
