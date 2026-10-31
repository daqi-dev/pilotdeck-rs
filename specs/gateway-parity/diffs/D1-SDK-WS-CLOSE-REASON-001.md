---
id: D1-SDK-WS-CLOSE-REASON-001
---

## 类别

人批协议关闭帧差异；仅 SDK CLI 的测试网关所见 WebSocket close reason，不改变 CLI 可见输出或退出行为。

## 基线依据

固定基线 cc7c4e6、Node v22.22.0 的真实 CLI 信号原件在 `src/pilotdeck-cli/tests/fixtures/sdk-signals/source.json`。`run` / `resume` × `query-hello-reject-sigint` / `query-hello-reject-sigterm` 四格中，网关向 connection 2 发送 code 4003、reason `auth_failed`；TS 客户端回复的关闭帧 code 为 4003、reason 为空字符串。原件包含 wire、hook、自然退出和清理，不以读码推断代替实测。当前重采证据见 `_tmp/20261026-issue-1145/phase3/RESULT.md`。

## Rust 行为

Rust 的 tungstenite 自动回显网关关闭帧，code 仍为 4003、reason 为 `auth_failed`。该 reason 是网关侧协议观测，用户在 CLI 端不可见；CLI 业务 stderr 仍按独立的字节规约比较，不因本条放行。

## 范围

仅上述四格的 connection 2，TS close code=4003、reason=""、cleanup=false 时允许 Rust reason="auth_failed"。完全相同的关闭帧仍可通过。其他场景、连接、code、cleanup、关闭数量/顺序及额外字段均不豁免；其他 reason 字符串也不豁免。

## 机器判据

`src/pilotdeck-cli/tests/common/signal_replay.rs` 的 `closes_match` 只改比较用期望的 reason，不改原件。四格 `signal_cases::{run,resume}_query_hello_reject_{sigint,sigterm}` 均执行四面对拍。离线 `signal_replay::comparison_tests::close_reason_exception_is_exact_and_scoped` 验证四格批准值与完全相等的正控，逐项损坏 code、reason、connection、cleanup、额外字段、关闭数量和场景名必须失败；最后恢复正控。业务 stdout/stderr、退出、文件树、abort 和连接计数继续独立比较。

## 批准记录

2026-10-31 人批（#1151 二 A），见 state/decisions-20261031-human-batch.md。本轮用户直接转述该批准并指示登记；本树从主仓原字节补齐既有批准卡。此条不表示 P08 或 #123 已完成人工验收。
