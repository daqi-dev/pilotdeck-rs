---
id: D2-CLI-CHANNEL-WS-EXIT-001
---

## 类别

挂起 bug 修复：默认 CLI 的远端 channel 在所有输入完成且当前 turn 已结束后释放它持有的 WebSocket，使进程能够退出。该差异不改变尚未结束的 turn、主动取消或本地宿主的生命周期。

## 基线依据

固定 PilotDeck `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/pilotdeck.ts:337-348` 和 `src/adapters/channel/cli/CliChannel.ts:25-45` 未在远端 channel 结束后关闭 `RemoteGateway`。Node v22.22.0、mbp2、隔离 home/project 与 fake HTTP/WS 实测：未知 argv 完成一轮、stdin 两行完成两轮，以及仅空白输入三格，在输出完成后观察 4.5 秒均未退出、WS 未关闭；发送 SIGTERM 清理。原始脚本及记录是主仓 `_tmp/20261108-cli-channel/probe.mjs`、`probe.ndjson`；只使用虚构凭据。观察窗口不是对业务请求新设的超时。

## Rust 行为与范围

本片 `src/pilotdeck-cli/src/cli_channel` 在远端成功执行全部输入或确定终态错误后，发关闭帧并释放 WebSocket；保留输入处理、流帧消费与渲染的既定合同。仅覆盖默认 `pilotdeck` 远端 CLI channel，不扩展到 cron、SDK、TUI、Gateway 调度或本地 fallback。断线、未完成的 turn 和资源清理各自按实际测试报告；CLI 本地 fallback 和完整启动前置仍待后续补齐。

## 机器判据

`src/pilotdeck-cli/tests/cli_channel_exit.rs` 在独占18789专用租约的隔离 fake HTTP/WS 和真实 Rust 子进程上，测试单次 argv、多个 stdin 行及空输入。对每格先核原始 TS 金样对应请求和渲染，再观察 WS close/EOF，最后断言进程退出；fake 服务不能主动关闭制造通过。测试不等于真实 native/provider 组合或 A8/#1146 的正式验收。

## 批准记录

**AI 代批，待人复核**：按 `.42cog/intent.md` 的挂起 bug 默认修复边界及用户持续推进授权执行。逐条人签前本条为 **未批准，非已接受差异**，不自动取得冻结判分豁免；不改冻结尺子、既有差异或阶段状态。
