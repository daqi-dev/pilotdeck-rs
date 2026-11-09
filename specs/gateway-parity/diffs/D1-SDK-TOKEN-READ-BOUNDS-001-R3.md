---
id: D1-SDK-TOKEN-READ-BOUNDS-001-R3
original: D1-SDK-TOKEN-READ-BOUNDS-001
---

## 类别

追加 R2 后的测试平台注册与普通回退进程矩阵。原条目、R1、R2 保留原字节；本修订不修改产品行为、冻结判据或既有批准状态。

## 基线依据

沿用 R2 所列固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 cron/TUI 发现合同：token 为空或普通读取失败时，cron 报需要运行 server，TUI 选择本地宿主。此次没有重新采集 TS 的新异常文件时序，不以 Rust 进程格替代 TS 基线。

## Rust 行为

产品沿用 R2 的共享 reader 与错误传播。新增测试不改变空文件/目录的普通不可用语义，也不把普通 IO 回退改成资源错误；已打开特殊文件/超限仍明确拒绝。

## 范围

资源八格与普通回退四格注册在 Unix。固定 rustix 1.1.5 公开 API 下，夹具当前实现 PTY 的平台集合为 Apple/Linux/Android/FreeBSD/Fuchsia/illumos；其余 Unix 显式 ignore 并写出原因，强制执行则明确失败。本次只在 mbp2 macOS 排期实测，不宣称 Linux 或其他 Unix 已编译运行，平台注册不是平台验收。

SDK 与默认 channel 的原矩阵保持；默认 channel 普通回退并未因新四格获得新的进程证据，继续按 R1 原范围单列未测项。server 的严格凭据创建策略不变。

## 机器判据

保留 `cron_and_tui_token_resources_fail_before_connection_or_local_fallback` 两入口各四类资源共八格，以及默认 channel 原七格。增加 `cron_and_tui_empty_and_directory_tokens_keep_ordinary_fallback`：两入口各空文件/目录共四格，独占原端口并响应 health 200；cron 核原 UNAVAILABLE、TUI 真 PTY stdin 与 stdout pipe 核精确输出终端错误及唯一 owned-project `.cwd`，两者核零 WS 和输入身份保全。

普通回退四格的输出不能单独区分客户端成功消费 health 200 与健康检查超时；不可用输出及状态是其直接判据，严格资源八格另以精确资源错误防止健康检查提前回退而空过。新平台注册/四格尚待协调者 mbp2 编译运行，不登记通过；最终结果绑定原始日志和提交，不回写 R2 当时未实测的历史记录。

## 批准记录

**AI 代批，待人复核；未批准，非已接受差异，无冻结评分豁免。** 本修订只补齐后续测试范围与证据限制，不把新增测试或独审通过当作用户接受行为差异，也不表示阶段 D 完成。
