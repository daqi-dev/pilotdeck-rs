---
id: D1-SDK-WS-CLOSE-ORDER-001-R1
original: D1-SDK-WS-CLOSE-ORDER-001
---

## 类别

同原条目 `D1-SDK-WS-CLOSE-ORDER-001`（跨连接 close 观测噪声的精确范围豁免）。本更正只同步 #1178 矩阵扩容后的分母事实，不改变豁免类别、判分语义与豁免范围。

## 基线依据

同原条目：真实 TS checkout cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85、Node v22.22.0。原条目所述「96 格中全部 88 个多关闭格、TS / Rust 各重复十次」是 #1252 fix-r2 当时的实验事实，历史记录不变。#1178（PR #2353）入库 4 个 abort-rst 格（run/resume × SIGINT/SIGTERM）后矩阵为 100 格，多关闭格 88 → 92，观察超时格仍为 26（新增 4 格均为自然退出双关闭格）。当前分母由 `signal_replay.rs` 离线测试断言 `(total, timeout) == (92, 26)` 钉住（真实 source.json 机检）。

## Rust 行为

同原条目。4 个 abort-rst 格的关闭序为 query 连接 1006 先于 control 1005，顺序因果稳定（对端 RST 先于客户端 finally 关闭），按严格序比较并在两轮回放中均 PASS（证据 `_tmp/20261026-issue-1178/replay-rst-{1,2}/`），不入 31 格序不敏感清单。

## 范围

原条目 31 个完整格名的豁免清单不变（清单见原条目 `close-order-ids` 块，未新增、未删除）；本更正不扩大或缩小豁免范围，仅把叙述分母从 96/88 更正为 100/92。

## 机器判据

`src/pilotdeck-cli/tests/common/signal_replay.rs` 的 `comparison_tests::all_multiclose_cells_follow_registered_order_semantics` 现动态枚举全部 92 个多关闭格（含 26 个观察超时格）；`stable_query_before_control_cells_keep_concrete_order` 名单由 16 格扩为 20 格（含 4 个 abort-rst 格）；`rst_cells_keep_verified_original_shape` 钉住 4 格原件形状。`unordered_close_scope_equals_registration` 钉住精确清单，与原条目一致。负控绑定见 `sdk-signals/negative_controls.py`（79 个变异）。

## 批准记录

未单独报批：本更正为分母事实性修订（矩阵 96 → 100 的直接后果），不改变原条目已批豁免的范围与判分语义；新分母全部由本 PR 机器断言钉住并经双谱系评审（minimax + deepseek）。AI 代批，待人复核。
