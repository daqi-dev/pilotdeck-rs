---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R15
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

编号保全：原整合候选的未批准 CLI R13 与后来 main 的已登记 R13 同名，本件将前者顺延为 R15，正文历史引用不重写，批准状态保持未批准；原提案原字节 SHA 92644ff65ae77f84b2406c5c4b599ceb8a82a4bc856383ea90fb332902ef2bd0，原件保存在 `_tmp/20261111-gateway-closeout/main-fe497-private/cli-r13-original.md` 及 96ea28c58 提交。main R13 原字节保留。R14 所指旧“原 R13”就是本件来源。

共同核心整合追加更正（保全 CLI 来源的同名 R11；主线 R11 原字节保留）：多项目 native 会话目录写入先校验真实注册项目；旧单项目入口拒绝已移除。不可表示的孤立 UTF-16 项目路径按原值拒绝，避免有损别名误操作。可撤回的安全偏离；不修改 R9/R10 原文或冻结判分。

本件保留来源记录；其中 list_sessions 与 record_agent_status_message 的注册范围已由 R12 更正，最终行为以 R12 为准。R12 原文的 R11 指 CLI 来源条目，本件明确这一承接关系。

## 基线依据

R9 记录真实固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85、Node 22.22 / ICU 78.2：createLocalGateway 与 GatewayWsConnection 真实链；_tmp/20261107-issue-2090/ts-probe/probe.out 47 条，probe-null.out 20 条。delete/rename/tag 的 provider 先项目解析再 session key；项目未注册为 PROJECT_NOT_FOUND；nullish 回退。resume/close 不解析项目。#2083 原始 probe 的 closeProjectSessions 对孤立代理返回空列表；本条不把 native 的严格拒绝说成 TS 原行为。

## Rust 行为

项目选择不再以旧单项目 gate 拒其他已注册项目。list/delete/rename/tag 使用有界 marker 注册解析；delete/rename/tag 注册检查先于 session key；nullish 使用选中请求视图的项目，合法词法别名按原 UTF-16 归一后匹配。未知项目拒绝且不创建文件。record_agent_status_message 先保留其 key/turn 类型门，再验证项目，才允许冷存储创建。close_project_sessions 将 raw UTF-16 规范化，保留无法表示的代理单元于 PROJECT_NOT_FOUND 报文，不把它替换成 U+FFFD 去命中别的项目。resume/close 自身 key gate 及输出差异仍按 R9/R10，未在本轮宣称对齐。

## 范围

本条只更正多项目入口、首错顺序及身份安全；不更改一般 replay、冻结尺子或既有差异批准状态。注册检查使用有界读取及受限 blocking worker；不保证任意内核存储故障可被中断或提供跨进程文件系统事务。其他会话方法不据此推导。

## 机器判据

R9 原始 TS 复算命令（从主仓执行，输出另存独占文件）：
cd resources/PilotDeck && node_modules/.bin/tsx ../../_tmp/20261107-issue-2090/ts-probe/probe.mjs
cd resources/PilotDeck && node_modules/.bin/tsx ../../_tmp/20261107-issue-2090/ts-probe/probe-null.mjs

候选 Rust 应在当前源码指纹下运行五个 blank_key_gate_order、两 surrogate 入口、registered_project_lexical_alias_is_resolved_before_session_write、unregistered_project_never_creates_session_storage、真实 marker FIFO 与旧负控；编译及实测未完成前不得标通过。应保存各源码、bin、日志哈希；原 R9 输出不覆盖。

## 批准记录

AI 代批，待人复核；未批准、非已接受差异，无冻结判分豁免。本件在整合时追加为 R13，来源为 CLI PR #2725 3637484a 的 R11；不修改主线 R11 与 CLI 已有 R12，不构成任何判分豁免；实效人签和阶段完成另办。
