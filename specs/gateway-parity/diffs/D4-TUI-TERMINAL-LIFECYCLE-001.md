---
id: D4-TUI-TERMINAL-LIFECYCLE-001
---

## 类别

修复退出后进程残留与只有 final 时交互状态不终结的挂起风险。

## 基线依据

固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85；主仓 `_tmp/20261108-cli-tui/pty/run-04/out/` 在最终空闲画面后发送 Ctrl-C，1秒后终端已恢复但子进程仍存活；随后只为清理对其 PID 发送 SIGTERM。该有限观察不证明 socket 是唯一活跃句柄。`_tmp/20261108-cli-tui/final-only/out/` 实际 TS 收到仅 final=true 的 error 后画面仍 thinking，后续输入回显并发送 Enter，1秒内没有第二次提交；结合 TuiApp 的 running guard 与旧 transport 丢 final 路径说明挂起风险，不把有限等待表述为永不恢复。原始 driver、wire、PTY、源码/文件哈希均保留。

## Rust 行为

正常空闲 Ctrl-C 或 /exit 关闭本客户端连接并退出，恢复终端 raw mode 和原 NONBLOCK 位。旧事件 reducer 和普通非 final 事件仍保留；流结束但未见非 final 终结事件时清 running/待决权限并显示未确认终态的错误，允许后续交互，不把被丢弃的 final 内容误认成功。断连/失败同样清待决交互。

## 范围

仅新 Rust 远端 TUI runner。没有改变旧 Gateway 协议，未让旧 transport 开始 yield final；不涵盖默认本地宿主、真实 native 权限发射、人眼 UI 验收或整个 D4 完成。

## 机器判据

`src/pilotdeck-cli/tests/fixtures/tui-pty/smoke.py` 使用实际 Rust bin 与 PTY，验证普通/小屏启动、提交、最终回复、空闲 Ctrl-C、socket关闭、退出0及完整 termios 恢复；`scenarios.py --case final` 验证 final-only 后能第二次提交，permission-disconnect 验证断连后的退出。每次独占目录保留原始字节，不能用假服务代替真实 provider。

## 批准记录

**AI 代批，待人复核**。依据本轮产品实现与默认修 bug 授权执行；未人批前不属于已接受差异，不改冻结评分/阶段状态。
