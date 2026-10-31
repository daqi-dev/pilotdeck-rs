---
id: D1-SDK-SIGNAL-UNHANDLED-REJECTION-001
---

## 类别

人批错误呈现差异：仅 SDK CLI 信号矩阵中八个明确格子的 stderr。用户都能看到同一句错误文字，退出码均为 1；不改变产品取消与清理行为。

## 基线依据

固定基线 cc7c4e6、Node v22.22.0 的真实 CLI 原件位于 `src/pilotdeck-cli/tests/fixtures/sdk-signals/source.json`。八格去除既有规约允许的 SQLite 启动警告后，没有前置业务行，直接以 Node 未处理 rejection 的崩溃形式输出错误：`PilotDeckError: <msg>` 夹在源码位置与栈帧中，最后为 `Node.js v22.22.0`。abort-error 的 `<msg>` 为 `d1 fixture rejected`；terminal-during-abort-final 为 `Gateway client closed.`。退出码均为 1。2026-10-31 用真实 Node v22.22.0 与原探针重采八格，原始 stdout/stderr、wire、hooks、自然退出与清理记录见 `_tmp/20261026-issue-1145/phase4/ts-eight/`，结论见同目录上级 `RESULT.md`。

## Rust 行为

Rust stderr 只输出一行 `<msg>\n`：abort-error 四格为 `d1 fixture rejected\n`，terminal-during-abort-final 四格为 `Gateway client closed.\n`，退出码仍为 1。错误文字与 TS 相同，Rust 不复刻 Node 的异常栈及版本尾行；不允许丢掉错误文字、输出其他错误、增加额外行或省略行尾换行。

## 范围

仅以下八个完整 ID 的 stderr 面：

- `run--abort-error-sigint`
- `run--abort-error-sigterm`
- `resume--abort-error-sigint`
- `resume--abort-error-sigterm`
- `run--terminal-during-abort-final-sigint`
- `run--terminal-during-abort-final-sigterm`
- `resume--terminal-during-abort-final-sigint`
- `resume--terminal-during-abort-final-sigterm`

不按 ID 前后缀、正则或 stderr 形状扩展范围。stdout、退出码与信号、abort 次数、连接、关闭帧、监听器、文件树及 milestone 仍照常严格比较。两格动态 runId 差异保留 FAIL，不纳入本条。

## 机器判据

`src/pilotdeck-cli/tests/common/signal_replay.rs` 的 `approved_signal_stderr` 精确列出八个 ID 及对应 Rust 字节，`stderr_matches_for_case` 对其他 ID 继续既有规约。真实回放与离线检查共用 `compare_surfaces`；原始 TS 记录及原 stderr 形状归一化函数不改。

`signal_replay::comparison_tests::unhandled_rejection_stderr_is_exact_and_scoped` 逐格验证批准单行与错误文本、缺换行、空串、Node 栈负例，对矩阵其余格及近似名称拒绝相同豁免；`signal_replay::comparison_tests::unhandled_rejection_preserves_other_surfaces` 在八格逐一损坏 stdout、退出码、abort、连接、监听器、文件树等观察面，必须得到对应差异。`src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py` 的六个 `rejection-*` 源码变异绑定上述完整测试名与专属失败消息，覆盖扩到第九格、放松 Rust 文本、改错两类批准值以及放松退出码/stdout；每项先编译，跑完全部项再汇总。

## 批准记录

2026-10-31 人批（#1164 选 B），见 state/decisions-20261031-human-batch.md。本轮用户直接转述批准并指示登记；本条不表示 P08 或 #123 已完成人工验收。
