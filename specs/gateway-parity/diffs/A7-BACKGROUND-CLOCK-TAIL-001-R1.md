---
id: A7-BACKGROUND-CLOCK-TAIL-001-R1
original: A7-BACKGROUND-CLOCK-TAIL-001
---

## 类别

bug 修复：批量 shutdown 必须隔离单个子代理 stop 的 payload 析构 panic，避免后续取消和 BackgroundResources dispose 被跳过；补全原登记的矩阵消费与负控。

## 基线依据

固定 cc7c4e6 `BackgroundSubagentRuntime.ts` 的 shutdown 顺序调用 stop；真实 TS 探针 shutdownCase 等待两个 body 进入，再 shutdown，两个均收到 abort、done 完成、状态 cancelled。TS 没有可注入 Clock 或 Rust panic payload，双重 panic 为 Rust 专属故障；正常对照原始输出 `_tmp/20261026-issue-804c/fix-r1/ts-probe2.log`，金样 `background-clock/oracle.json`。

## Rust 行为

shutdown 对每个 stop 分别 catch_unwind；首个 Clock payload 的二次 panic 不跳过后续 entry，BackgroundResources dispose 继续等待 shell runtime 进入 Disposed。单独 stop 的 payload 析构传播语义不变，原有先取消后释放 payload 顺序不变。

## 范围

只吸收批量 shutdown 中 stop 的 unwind；不处理永不返回的同步 Clock、无视取消的 body、subagent 首次 Clock 或 run 构造 panic。普通 S-pending/ok/err/running 完整行与 TS 实测比较；B-zero/nonzero/never 按原登记差异断言。B-reject 全字段证据固定，但不冒称 Rust 原生 child.wait 错误路径可等价注入。

## 机器判据

`clock_tail_shutdown_survives_payload_drop` 覆盖两个子代理及显式 shutdown/直接资源 dispose；负控 tail-shutdown-payload。S-pending 用 flag 验证未调用 body 并消费全行，负控 tail-pending-body。tail_oracle_has_exact_matrix 精确 8 个 ID/数量，并固定全部 B 行字段；tail-matrix/tail-duplicate/tail-shell-evidence 配对。fallback 拆为 shell/finish/stop/no-pid 四个独立变异。重生成守卫 background_clock_regenerates_identically。

## 批准记录

2026-10-26 #804 pr2 fix-r1 用户指定本 P0 修复及 P1/P2 测试补齐。本更正收紧原 bug 修复范围内保证，不扩展产品或阶段完成声明。
