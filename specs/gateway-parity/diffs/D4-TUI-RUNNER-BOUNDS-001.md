---
id: D4-TUI-RUNNER-BOUNDS-001
---

## 类别

限制交互状态、输入与终端背压，防止内存无限增长和输出阻塞造成挂起。

## 基线依据

固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 的 TUI 状态在 reducer 中累积消息，当前基线不存在下述 Rust 特定阈值；已采普通 TS PTY 和纯 reducer/command 金样，不宣称对 TS 做过8MiB、64KiB或终端背压边界实测。该条不借其他客户端资源差异获取批准。

## Rust 行为

runner 对保留的消息、activity、pending permission、session 列表、viewer、输入及显示缓存的实际容量/行索引采用约8MiB计数，累计收到的事件/输入另有8MiB预算；消息最多4096，待决权限最多64，输入UTF8最多64KiB。超限显式报错，必要时中止旧流、清权限与activity并删除最新内容，保留可退出的终端。token以非阻塞同fd普通文件检查并最多64KiB读取；工具输出文件最多1MiB。stdout须为TTY，写入使用非阻塞fd，整帧共享绝对1秒期限；瞬时EAGAIN等待可写，持续堵塞返回失败并恢复termios/NONBLOCK。事件逐条核预算并每轮轮询输入，不固定15ms节流。显示按 Unicode 字素宽度连续硬换行，不承诺与 Ink 或 Ratatui 的按词折行逐像素一致；增量缓存与缩放重建每轮最多处理8192个UTF-16单元，代理对不从中切开。单个显示行/字素累计超过64KiB也明确触发资源失败；缓存不足逐轮移除最新内容并保留可显示的旧历史，不能静默清空全部历史。

## 范围

仅远端 TUI runner 的应用数据计数与本地输入/显示；8MiB不是RSS或解码临时分配的硬上限，传输已有独立4MiB计数与共享帧上限。不是磁盘故障、任意外部 pager、全部平台的通用期限保证；当前实测平台为mbp2 macOS。

## 机器判据

实际PTY的 budget/pending-budget 覆盖超限中止和权限清理；backpressure 停读终端，要求失败退出及终端标志恢复；output-pipe 要求明确拒绝、无WS连接及原终端属性保持。throughput 发送300个间隔10ms的小delta并要求尾部和final完成；此前固定15ms睡眠实测只到seq147便resource limit断连的失败证据保持在主仓 `_tmp/20261108-cli-tui/throughput-r1/`。另有真实FIFO token读取与70k长行尾部显示测试。

## 批准记录

**AI 代批，待人复核**。阈值是新实现安全边界，未获得冻结验收豁免；后续如调整必须更新明确判据并保留旧失败记录。
