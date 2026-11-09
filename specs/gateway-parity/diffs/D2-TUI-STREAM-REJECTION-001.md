---
id: D2-TUI-STREAM-REJECTION-001
---

## 类别

TUI 流式提交收到明确的服务端拒绝后继续等待的挂起修复。

## 基线依据

固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`、Node 22.22.0 / ICU 78.2 下，真实 RemoteGateway 连接动态 loopback fake，submit_turn 收到同 ID `response ok:false` 后，500ms 观察期内 iterator 仍未结束、WS 仍打开；之后主动关闭客户端用于清理。原件 `src/pilotdeck-cli/tests/fixtures/tui/rejection.json` SHA-256 `003e34c20abb469cd22b4effd9f622dc277ac4a2f556da3402140de693d5d134`，可复采脚本同目录 `generate-rejection.mjs`；原件亦保存在 mbp2 `/Users/da/ws/pdrs-runner/_tmp/20261108-tui-rejection-capture/`。观察期不是无限挂起证明；源码 `GatewayWsClient.ts` 的 response 分支只查询 pending，不处理 streams。

## Rust 行为

仅当流 ID 收到 `response ok:false`，关闭该流并向消费者交付原 Gateway 错误 envelope；其他请求继续同连接运行。不改变普通 RPC 或最终 event 的语义。已取消、尚未发送的请求不会发出，已发送的请求取消只清理本地 waiter，不承诺撤销服务端副作用或自动重试。

## 范围

仅TUI远端流的明确拒绝与本地waiter清理；已发送业务的服务端执行不由取消本地future撤销。

## 机器判据

`src/pilotdeck-cli/tests/tui_transport.rs` 用真实 socket 对拒绝与超过 64 次已发送后取消做回归；transport 单元测试在泵启动前排队并取消副作用请求，断言没有请求帧。测试使用 fake 服务，不代表已完成真实 native 的权限请求闭环或 TUI 验收。

## 批准记录

**AI 代批，待人复核**：本轮用户授权范围内的挂起修复；人签前为未批准，非已接受差异，不取得冻结判分豁免。
