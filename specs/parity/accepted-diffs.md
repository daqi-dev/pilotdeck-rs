# 已接受差异清单

> 每条一行。没有登记的差异，一律算 Rust 的错。条目由 Issue #1 起追加；**只增不删**，作废的条目划线并写明原因。
> AI 可以追加条目，但状态一律是「待批」；**人批了才算已接受**（评审 A：追加不能当成免检豁免）。

| # | 场景 | 差异形状 | 类 | 依据 | 状态（待批 / 已批：人 · 日期） |
|---|---|---|---|---|---|

| 1 | `deadline` | Rust sidecar 收尾后，TS 宿主竞态可能额外写一条 `gateway_submit_failed` durable.status | 原版缺陷 | bug 第②条：基线 `src/gateway/client/InProcessGateway.ts:1727` catch 未排除 `timedOut`，与 `tests/gateway/operation-deadline.spec.ts:159-160` 的仅有 `turn_timeout` 断言矛盾；独立复现 `_tmp/probe-host-timeout.mjs`、`_tmp/probe-host-timeout.json`、`_tmp/probe-host-timeout.stderr`；阶段 A 替换 gateway 时修（协调者 AI 代拍，待人复核） | 待批 |
| 2 | `deadline_during_tool` | Rust sidecar 收尾后，TS 宿主竞态可能额外写一条 `gateway_submit_failed` durable.status | 原版缺陷 | bug 第②条：基线 `src/gateway/client/InProcessGateway.ts:1727` catch 未排除 `timedOut`，与 `tests/gateway/operation-deadline.spec.ts:159-160` 的仅有 `turn_timeout` 断言矛盾；独立复现 `_tmp/probe-host-timeout.mjs`、`_tmp/probe-host-timeout.json`、`_tmp/probe-host-timeout.stderr`；阶段 A 替换 gateway 时修（协调者 AI 代拍，待人复核） | 待批 |
