---
id: D4-TUI-ABORT-ORDER-001
---

## 类别

防止按 session 取消的旧请求迟到误取消新一轮。

## 基线依据

固定 TS TUI 的 abort_turn 按 sessionKey 发起；终结事件与 RPC 响应可以交错，本次 Rust 初版静态审查发现终结事件先清 running、新提交可早于旧abort确认的竞态。此条修复 Rust 新实现缺陷，不声称已复现 TS 的同类竞态；测试必须使旧terminal明确先于延迟abort响应。

## Rust 行为

同session abort在途期间阻止新submit、新建/切换session及权限决定；旧流终结不解除这一限制。成功收到abort响应后才放行后续提交；请求最长等待10秒，超时或错误即显示取消结果未知并要求重连，关闭本连接且不再允许新提交，不自动重试。Ctrl-C退出仍可用。

## 范围

仅客户端单连接发起的session-wide取消顺序，不提供跨客户端、跨宿主run所有权或持久化取消保证，不把关闭连接当成已确认服务器取消成功。

## 机器判据

`scenarios.py --case abort-late` 的真实PTY/fake WS先收到abort、再送旧terminal、延迟响应；期间输入第二轮不得出现第二次submit，响应后才可提交。abort-timeout 故意不回响应，超过期限仍不得提交，并能退出恢复终端。原权限Ctrl-C和背压场景回归。

## 批准记录

**AI 代批，待人复核**。不变更冻结尺子，不将未知取消结果报告为成功，不代表原生权限闭环或阶段验收。
