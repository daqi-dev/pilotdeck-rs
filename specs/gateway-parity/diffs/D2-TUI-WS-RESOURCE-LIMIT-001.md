---
id: D2-TUI-WS-RESOURCE-LIMIT-001
---

## 类别

TUI 远端传输资源预算与有界写入：防止慢消费者或不响应的 WebSocket 使交互客户端无限积压或永久停在写入。

## 基线依据

固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/gateway/client/GatewayWsClient.ts` 为 pending 和 `AsyncEventQueue` 使用无显式上限的 Map/数组，发送不提供此处的 10 秒写入守卫。本地固定 TS 远端客户端通过隔离 fake loopback 采得的提交、permission_decide、abort 交织原件见 `src/pilotdeck-cli/tests/fixtures/tui/wire.json` 与同目录 `generate-wire.mjs`；该原件不证明超限情况下 TS 的实测表现。不得把 SDK/cron 的资源差异批准外推到本条。

## Rust 行为

TUI 单连接 transport 对命令通道最多 32 项、待决 RPC 64 项、活动流 16 条、每流待消费事件 64 项，并对排队的请求字节、待收响应、事件及尚未消费的流拒绝错误使用共享 4 MiB 序列化字节预算；超过预算或队列上限会显式报告 resource limit，pending/stream 满时仅拒绝新请求，事件积压或接收字节预算不足时退出泵并通知等待方，不静默丢失权限事件或伪装流完成。WS 发送和 ping flush 最长 10 秒，超时后终止连接并报错；显式 close 有界。入站单帧和消息仍复用共享 128 MiB 配置，预算与解码临时内存并非同一限制。普通 RPC 和流等待本身不新增超时。

## 范围

本差异仅涵盖 TUI 传输，不改变其他客户端。4 MiB 约束传输层保留的序列化字节计数，不是解码树、当前接收帧、调用者已取走的数据或整个进程的内存上限。

## 机器判据

`src/pilotdeck-cli/tests/tui_transport.rs` 使用动态 loopback fake 连接，验证真实 TS wire 中的流/权限/中止、乱序 RPC、空闲 ping、断连等待方、消费者丢弃及事件队列超限显式失败；新增同时64个pending后拒绝新工作/取消释放槽位、3个未消费大错误后第4个触发预算、hello flush同deadline测试。真实 native 权限闭环未完成：当前 host 工具装配尚不能触发 permission_request，记录见主仓 `_tmp/20261108-cli-tui/native-permissions/README.md`。该记录不属于真实 provider 冒烟或 TUI 完成验收。

## 批准记录

**AI 代批，待人复核**：按本轮授权在产品范围内修复可造成挂起或内存耗尽的问题；人签前为未批准，非已接受差异，不改冻结判分或阶段状态。
