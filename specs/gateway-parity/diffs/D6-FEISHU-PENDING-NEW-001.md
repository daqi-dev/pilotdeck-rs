---
id: D6-FEISHU-PENDING-NEW-001
---

## 类别

bug 修复：显式 /new 必须能够清理挂起互动并重建会话。固定基线将其吞成权限/问询回答；Native 按既有 reset 产品意图修正，不声称无差异等价。

## 基线依据

固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，src/adapters/channel/feishu/FeishuChannel.ts:448-463 在 mapper.resolve 前先检查 pending elicitation/permission 并 return，fast pending 路径也先消费输入。真实固定 TS FeishuChannel + ImPermissionHelper，mock 外层 gateway 和 loopback REST：挂起权限后经 handleWebhook 输入 /new，实际发送“请回复 1 ...”非法回答提示；sessionBefore/sessionAfter 均 feishu:chat=room:general，pendingAfter=true，resetReply=false，modelSubmits=0。证据_tmp/20261110-channel-difference-evidence/feishu-pending-new/oracle.json，包含固定源码 SHA 与 Node22.22.0/ICU78.2。

## Rust 行为

src/pilotdeck-gateway/src/channel/feishu/mod.rs 的快速互动和 batch 路径，都在 pending helper 前识别 mapper 已有的 /new 或 /new 后接空格文本。沿用现有 mapper/new 分支：清 pending、递增 chat generation、取消所属旧 run、发布状态与新会话回复；不是新增命令语法。修复候选 _tmp/20261110-feishu-owned-stop-new/increment.patch。Native 实际 A7 原 r46 首败明确记录 /new 被当非法权限答案；修后完整消费者重跑待根协调新批。

## 范围

仅 Feishu 现有 mapper /new 命令优先级。普通数字互动回复、未知命令、其他渠道、公开协议、冻结 golden 均不改。stop 生命周期修复独立解释，不以本条作为全局 abort 授权。

## 机器判据

真实 TS _tmp/20261110-channel-difference-evidence/feishu-pending-new/capture.mjs：seed session + real permissions.capture/confirmInitialPrompt + real handleWebhook(/new)，检查相同 session/pending 尚在及实际 REST 提示。Native：native_process::feishu_actual_host_tests::feishu_actual_server_media_a7_upload_interactions_new_reload_shutdown 的 pending-new→/new→post-new，须观察新会话回复、旧 bash 未执行、新 session 实际 A7 final；后续还验证 reload/shutdown。原 r46 失败 JSON/日志保留，不改名、不覆盖、不放宽 timeout。

## 批准记录

未批准，非已接受差异。主协调明确按既有默认修 bug 授权推进可逆产品修复，并要求真实 TS 证明；该授权不自动授予判分豁免、双谱系终审或正式人签验收。本文件是未批准差异的新增登记；不修改历史 ledger 或判分语义。

Evidence manifest: _tmp/20261110-channel-difference-evidence/manifest.json. Raw Native first failures and immutable r43/r46 source bindings are retained under first-failures/; r48 rerun and independent reviews remain pending.
