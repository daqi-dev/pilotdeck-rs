---
id: D1-SDK-WS-RESOURCE-LIMIT-001-R1
original: D1-SDK-WS-RESOURCE-LIMIT-001
---

## 类别

范围说明更正，沿用原条目的安全 / 资源 bug 修复分类。原条目「当前调用方为 sessions list」在 #1028 引入 messages 后不完整；本条不改变上限或判分语义。

## 基线依据

原条目的 Node v22.22.0 / undici 实测覆盖 sessions list 的 16 MiB+1、100 MiB+1 单帧，证据仍见 `D1-SDK-WS-RESOURCE-LIMIT-001.md`。messages 的真实 52 格见 `src/pilotdeck-cli/tests/fixtures/sdk-sessions-messages/oracle.json`；这些格没有大响应，不能据此声称 messages 在上述区间已与 TS 对拍。

## Rust 行为

`src/pilotdeck-cli/src/sdk/sessions.rs` 的 list 与 messages 均调用 `sdk/transport.rs` 的 ordinary RPC，共用 `MAX_RECEIVE_BYTES = 128 * 1024 * 1024` 的帧及消息上限。本轮未修改 transport。

## 范围

原条目适用的 D1 CLI SDK ordinary RPC 接收现在包含 sessions list 与 sessions messages。messages 大响应未对拍，100 MiB+1 及 128 MiB 邻域仍未验；不得用 list 的成功格替代完整 messages 输出验收。超过 128 MiB 的既有安全预算不变，不新增判分豁免。

## 机器判据

原条目列出的 list 大帧测试与负控继续保护共享上限；`sdk_sessions_messages_exit::sessions_messages_matches_ts_matrix` 只保护 messages 的 52 个小响应/参数/错误格。本条是适用范围及证据限制登记，没有新增行为检查或声称新的大响应测试通过。messages 大响应实测前置于 D1 messages 实效验收收尾。

## 批准记录

本条依据用户 #1028 fix-r1 明确要求逐条处理评审，执行可撤回的范围说明更正，AI 代批，待人复核；不重新批准或扩大原条目的安全差异，不改变阶段完成状态。
