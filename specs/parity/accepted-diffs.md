# 已接受差异清单

> 每条一行。没有登记的差异，一律算 Rust 的错。条目由 Issue #1 起追加；**只增不删**，作废的条目划线并写明原因。
> AI 可以追加条目，但状态一律是「待批」；**人批了才算已接受**（评审 A：追加不能当成免检豁免）。

| # | 场景 | 差异形状 | 类 | 依据 | 状态（待批 / 已批：人 · 日期） |
|---|---|---|---|---|---|

| 1 | `deadline` | Rust sidecar 收尾后，TS 宿主竞态可能额外写一条 `gateway_submit_failed` durable.status | 原版缺陷 | bug 第②条：基线 `src/gateway/client/InProcessGateway.ts:1727` catch 未排除 `timedOut`，与 `tests/gateway/operation-deadline.spec.ts:159-160` 的仅有 `turn_timeout` 断言矛盾；独立复现 `_tmp/probe-host-timeout.mjs`、`_tmp/probe-host-timeout.json`、`_tmp/probe-host-timeout.stderr`；阶段 A 替换 gateway 时修（协调者 AI 代拍，待人复核） | 待批 |
| 2 | `deadline_during_tool` | Rust sidecar 收尾后，TS 宿主竞态可能额外写一条 `gateway_submit_failed` durable.status | 原版缺陷 | bug 第②条：基线 `src/gateway/client/InProcessGateway.ts:1727` catch 未排除 `timedOut`，与 `tests/gateway/operation-deadline.spec.ts:159-160` 的仅有 `turn_timeout` 断言矛盾；独立复现 `_tmp/probe-host-timeout.mjs`、`_tmp/probe-host-timeout.json`、`_tmp/probe-host-timeout.stderr`；阶段 A 替换 gateway 时修（协调者 AI 代拍，待人复核） | 待批 |
| 3 | `deadline` / `deadline_during_tool` | #1 #2 依据更正：真实 TS 流程 0/10、Rust 多写 gateway_submit_failed，改按 Rust 产品问题处理；修复撤出 PR #27，挂起待人（3 轮未修好） | Rust 产品问题 | #1 #2 依据更正 | 待批 |
| 4 | `handshakeClose`（A6.5-pr10b，MCP 客户端） | 握手中 close：TS 基线让迟到成功的连接继续装上（`statuses:["ready"]`、子进程不退出）；Rust 关闭迟到连接，`start` 报 `mcp_handshake_failed`，状态保持 close 时的 `idle`，子进程回收 | 原版缺陷 | 子进程永不回收的资源泄漏；真实 TS 夹具 `src/pilotdeck-gateway/tests/fixtures/mcp-runtime/oracle.json` 的 `handshakeClose` 实测值；Rust 交错测试与负控见 `tests/mcp_runtime.rs`；#371 | 已批：郑达奇 · 2026-10-21 |
| 5 | `background-task-async-spawn`（A6.5-pr12，后台任务） | 异步报告的 spawn 失败（ENOENT/EACCES 等，Node 不发 exit）：TS 的 stop/dispose 永久挂起；Rust 在 stop/dispose 时立即结算为 cancelled（interrupted、endedAt 有值、exitCode undefined、发完成事件） | 原版缺陷 | TS 探针 `_build/wt-a67-pr2/_tmp/20261020-a65-pr12/probes/p1.out`、`p1-r1.out`：missing-cwd 时 stop 1 s 未返回、dispose 3 s 挂起；任务书「不复制挂起，清理补全」；#371 | 已批：郑达奇 · 2026-10-21 |
| 6 | `deadline` / `deadline_during_tool` | 更正 #1–#3：deadline 收尾时 TS 宿主外层 catch 与 timeout fence 竞态，可能额外写一条 `gateway_submit_failed` durable.status；Rust sidecar 收尾比 TS 快约 6 ms，更常踩中（真实流程 TS 0/10、Rust 1/10），受控交错下两侧都多写 1 条、fence 完成后两侧都是 0；两侧 sidecar wire 规范化后逐帧一致 | 原版缺陷 | 基线 `src/gateway/client/InProcessGateway.ts:1385-1436`（先发超时取消再等持久化）、`:1727-1750`（catch 只看 fence current）、`:1824-1848`（timeout 路径不等 pump 即完成 fence）；64 轮实测与受控交错原件 `_tmp/20261025-issue-19/delivery/`（repro-counts.json、diag-hold / diag-after、natural-failure-wire-analysis.json）；协议无「fence 已完成」通知，sidecar 单侧无可靠修法；修复归 A7 Rust 宿主（#723 选 A） | 已批：郑达奇 · 2026-10-26（#723） |
