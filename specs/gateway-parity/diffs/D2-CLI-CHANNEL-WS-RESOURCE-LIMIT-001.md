---
id: D2-CLI-CHANNEL-WS-RESOURCE-LIMIT-001
---

## 类别与范围

默认 CLI 远端 channel 的有限接收预算，属于安全/资源修复。采用共享 `pilotdeck_cli::ws_config::websocket_config()` 的128 MiB单帧与累计消息上限，拒绝超过预算的长度声明；它不是整个进程内存预算，也不外推至本地host。固定TS `GatewayWsClient.ts` 使用Node WebSocket未自行设限，不能宣称TS与这个上限相同。

## 依据与机器检查

共享预算已在SDK与cron的大帧和超限长度头实测中覆盖，引用独立条目 `D1-SDK-WS-RESOURCE-LIMIT-001`、`D2-CRON-WS-RESOURCE-LIMIT-001`，不把它们的验收/批准外推为默认channel验收。默认channel真实子进程已测握手、流式输出、ping、终态和错误；本入口的大帧/分片边界仍待独立验证，当前不标全兼容或阶段完成。

## 批准记录

**AI 代批，待人复核**。可撤回，按意向的安全bug默认修复边界执行，决策见 `state/decisions-20261108-cli.md`。**未批准，非已接受差异**，不改冻结装置，不取得冻结判分豁免。
