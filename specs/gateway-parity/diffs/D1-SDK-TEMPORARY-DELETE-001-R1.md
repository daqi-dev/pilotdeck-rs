---
id: D1-SDK-TEMPORARY-DELETE-001-R1
original: D1-SDK-TEMPORARY-DELETE-001
---

## 类别

补充原条目的长驻宿主资源范围，不改变实现和冻结判分。本条仍为本分支新增、未批准的草稿；本次补齐账本必需字段，不改原条目。

## 基线依据

沿用 `D1-SDK-TEMPORARY-DELETE-001` 中固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的重复临时会话删除与 SDK 清理依据。4096 与 1048576 UTF-16 单元是 Rust 资源选择，不是 TS 阈值；本更正不新增 TS 等价主张。

## Rust 行为

4096 是每个宿主进程 retained identity 总预算，不只是同时活跃的会话数：连续使用互不相同 UUID 创建并删除临时会话，也会累积到上限；之后创建新身份明确失败。相同 key 的成功临时 acquire 或成功打开新的 durable RecordingPlan 能替换/解除对应身份；重新启动宿主会清空本进程身份记录，但没有跨进程重复删除保护。不会按时间或容量淘汰删除身份，以免重复 delete 落到同名持久原件。

## 范围

这是有意的资源与持久数据保护取舍，不能宣称无限长驻的新 UUID 临时会话吞吐。后续前端宿主应显式处理预算错误，不能静默重试同一不可用进程。

## 机器判据

r130 native 模块中的 identity_budget_tests 和实际 SDK 删除消费者已通过；精确原始记录在主仓 `_tmp/20261108-cli-completion/verify-r130/logs/native.log`。原条目的 count/UTF-16 精确阈值与重复删除保护判据不变。

## 批准记录

本更正仍未人批、非 accepted diff、非验收豁免；AI 代批待人复核。
