---
id: D2-CLI-CHANNEL-WS-RESOURCE-LIMIT-001
---

## 类别

默认 CLI 远端 channel 的有限接收预算，属于安全/资源修复。采用共享 `pilotdeck_cli::ws_config::websocket_config()` 的128 MiB单帧与累计消息上限，拒绝超过预算的长度声明；它不是整个进程内存预算，也不外推至本地host。固定TS `GatewayWsClient.ts` 使用Node WebSocket未自行设限，不能宣称TS与这个上限相同。

## 基线依据

共享预算已在SDK与cron的大帧和超限长度头实测中覆盖，引用独立条目 `D1-SDK-WS-RESOURCE-LIMIT-001`、`D2-CRON-WS-RESOURCE-LIMIT-001`，不把它们的验收/批准外推为默认channel验收。默认channel真实子进程已测握手、流式输出、ping、终态和错误；本入口的大帧/分片边界仍待独立验证，当前不标全兼容或阶段完成。

## Rust 行为

默认channel使用共享配置：单帧/累计消息128MiB。超过预算导致握手不可用或流传输错误，绝不声称已成功。

## 范围

仅默认CLI远端连接的接收预算，不修改Gateway、SDK、cron、本地host与业务协议。

## 机器判据

共享配置的大帧/长度头有既有SDK和cron测试；默认channel路径由子进程握手与流测试核实，独立大帧/分片边界待补，不提前宣称验收通过。

## 批准记录

**AI 代批，待人复核**。可撤回，按意向的安全bug默认修复边界执行，决策见 `state/decisions-20261108-cli.md`。**未批准，非已接受差异**，不改冻结装置，不取得冻结判分豁免。
