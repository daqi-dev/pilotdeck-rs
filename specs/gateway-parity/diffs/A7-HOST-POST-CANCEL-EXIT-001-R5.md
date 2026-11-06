---
id: A7-HOST-POST-CANCEL-EXIT-001-R5
original: A7-HOST-POST-CANCEL-EXIT-001
---

## 类别

向安全偏离（沿用 R3「不可避免（有界期限的代价）」）。本更正只补 R3 末句「四格矩阵（#590）中涉及取消的场景按本偏离判定」的适用口径，不改行为、不改判分语义、不改既有冻结字节。

## 基线依据

同最新条目 `A7-HOST-POST-CANCEL-EXIT-001-R3` 的「TS 依据」，本更正不改该依据。

## Rust 行为

同最新条目 `A7-HOST-POST-CANCEL-EXIT-001-R3` 的「Rust 行为」（`POST_CANCEL_EXIT_TIMEOUT` 5 s，到点 fail-closed），本更正不改该行为。

## 范围

同 R3「范围」。R3 末句的适用口径按 **2026-11-06T02:58:40Z 人批「按窄读」**
（决策单 #1716 评论 https://github.com/daqi-dev/pilotdeck-rs/issues/1716#issuecomment-6675645168 ，
正文仅「按窄读」两字；仓内留档 `state/decisions-20261106-narrow-read-1716.md`）确定：

- 该偏离描述的是 **Rust 宿主**取消后的 5 s 墙钟期限，因此只在宿主为 Rust 的格里成立：格③ Rust 宿主 × TS sidecar、格④ Rust 宿主 × Rust sidecar。
- 宿主为 TS 的格① TS × TS、格② TS × Rust **不适用**。尤其格②里 `cancel_during_tool`、`stream_interruption`、`sidecar_one_shot_parent_abort_after_admission`、`sidecar_one_shot_parent_close_after_admission` 的差异，是「TS 宿主眼里观测到的 sidecar 侧差异」，不落在本条范围内，仍按 sidecar 侧信号记（四格账本②的 sidecar 侧信号为 23 条，不因本条降为 19–21）。
- ③④ 的 fixture（`model-stream` / `provider-error` / `provider-unconfigured` / `token-saver-judge`）不含取消场景，未被触发；④ 的 72 case 全部 `NOT_RUN`，也判不到。

不改产品代码、不改任何既有冻结文件；本条只新增本文件，登记口径见 #590 四格账本 §六.8。

## 机器判据

同 R3「机器判据」：`live_stream_tests.rs` 的 `slow_consumer_*` 三条与 host 层 `completed_application_is_collected_before_the_deadline_exit` 仍然有效。本更正不新增实现、不改判据。

## 批准记录

2026-11-06 人批「按窄读」（#1716 评论 2026-11-06T02:58:40Z），作为四格矩阵（#590）的适用口径；
仓内留档 `state/decisions-20261106-narrow-read-1716.md`（含评论 URL、时间、作者与正文原文）。
原偏离批准见 `state/decisions-20261025.md`「#643 取消退出到点后的有界收尾：选 A」；R4 登记见
`state/decisions-20261031-ledger-batch.md`。仅覆盖上文「范围」所列情况。
