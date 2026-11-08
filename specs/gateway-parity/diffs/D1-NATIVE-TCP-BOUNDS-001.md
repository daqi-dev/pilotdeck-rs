---
id: D1-NATIVE-TCP-BOUNDS-001
---

## 类别

有意改进：原生 sidecar TCP 资源有界与已执行身份不重发。

## TS 依据

固定 cc7c4e6 的 AgentLoopSidecarServer/tcpAgentLoopSidecarServer 真套接字实测：连接共享 module instance、generation 各异、resumeSupport 为 none；跨连接 status/cancel 可用；活动请求重复 execute 返回 OPERATION_ALREADY_ACTIVE。已完成相同 execute 在 TS 再执行；断连中的执行在 TS 可继续完成。原件 mbp2 本轮 `_tmp/20261108-tcp-listener-oracle-01/run04`，入口端口十格 `_tmp/20261108-tcp-port-oracle-01/run01`。结果不是假 factory 能证明的完整 AgentLoop 兼容。

## Rust 行为

保持共享查询/取消及连接独立身份。完成请求沿用 Rust owner 的终态缓存，跨 generation 明确要求 reconciliation；断连退休本连接并标 result_unknown，不重发副作用，不宣称 resume。此处与 TS 完成后再执行/断连继续不同，单独登记，不能算一比一兼容。

## 范围

仅原生 CLI 所需 Rust sidecar TCP 监听入口；stdio 原合同与冻结判分不变。

## 机器判据

`pilotdeck-transport/tests/tcp_listener.rs` 用真实 TCP socket 验证隔离、重复执行、跨连接取消/状态与超限后其它连接仍可用；factory 只隔离运行时。端口转换沿用 JS trim/Number。32 连接、每连接32执行队列；输入行及全局在读8MiB；留存 ledger 4096项/64MiB计费上限；所有输出与未决 host call 合计64MiB计费，采用保守UTF-16/节点成本，计费值不是RSS。ledger不淘汰已执行身份，容量耗尽须重启；不静默截断协议。TCP deadline线程另受全局32许可约束，线程创建失败须在acceptance前拒绝。

## 批准记录

AI 代批，待人复核；未批准，非已接受差异。依据完成CLI/机器能力授权与先前Rust连接owner的副作用不重发合同，增加可撤回有界监听端。未获人类最终验收，不据此改变冻结判分或标阶段完成。
