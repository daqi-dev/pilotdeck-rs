---
id: D2-CRON-WS-RESOURCE-LIMIT-001
---

## 类别

安全 / 资源 bug 修复：cron 管理客户端显式限制单帧和整条 WebSocket 消息，避免对端长度头触发无界分配；同时不采用会截断基线已成功输入的库默认 16 MiB 单帧上限。

## 基线依据

固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/gateway/client/GatewayWsClient.ts` 使用运行时 `WebSocket`，没有自行指定接收上限。2026-11-08、mbp2、Node v22.22.0 对真实 `pilotdeck cron create` 发送 17,825,824 字节 hello_ok 帧后，正常输出参数用法；生命周期挂起另见 `D2-CRON-WS-EXIT-001`。原始脚本 / 输出为主仓 `_tmp/20261108-issue-124-cron/ts-large-frame.mjs` / `ts-large-frame.log`，SHA256 分别为 `29d2170045e55cc2d5f42d3b6d067427df1e59f5431264c7a7383595908274a0` / `9a5f095c492339dd68d22a1399d821ae314dc1c45e6d80baad7dd27f9dccb6be`。

既有 `D1-SDK-WS-RESOURCE-LIMIT-001` 的独立 SDK 真实金样证明同 Node 运行时可接受 100 MiB+1 单帧；它不是 cron 此格的实测，也不将其批准范围外推至 cron。128 MiB 是明确的资源预算，不声称 TS 恰好具有同值上限或支持任意大输入。

## Rust 行为

`src/pilotdeck-cli/src/cron.rs` 使用 `sdk::transport::websocket_config()`：单帧和累计消息上限均为 128 MiB。只共享资源预算与关闭动作，cron 仍使用自己的发现、凭据、hello 身份与等待规则。超过单帧上限的声明在读取 payload 前拒绝；累计分片不得绕过消息上限。hello 阶段失败沿用 server 不可用错误，普通 RPC 接收失败沿用 `Gateway WebSocket closed.`。

## 范围

仅旧 `cron` 管理连接的接收帧与累计消息；不修改 Gateway 服务端、SDK 既有行为、调度器、持久状态或冻结判分语义。资源上限不是进程总内存预算。

## 机器判据

`src/pilotdeck-cli/tests/cron_exit.rs` 的真实子进程测试覆盖大于 16 MiB 的成功帧及资源边界；编译和实际执行记录在主仓 `_tmp/20261108-issue-124-cron/`。SDK 既有 frame/message 上限测试继续保护共用配置。具体已执行范围按原始日志记载，不以测试存在代替通过。

## 批准记录

**AI 代批，待人复核**，依据 `.42cog/intent.md` 安全 bug 默认修复边界；判断记录在 `state/decisions-20261108-cli.md`。**未批准，非已接受差异**，不自动取得冻结判分豁免，不提前宣称 D2 或 CLI 阶段完成。
