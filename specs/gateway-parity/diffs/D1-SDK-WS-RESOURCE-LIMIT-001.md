---
id: D1-SDK-WS-RESOURCE-LIMIT-001
---

## 类别

bug 修复：安全 / 资源。依据 `.42cog/intent.md`「什么算 bug」③ 安全；#983 fix-r4 按人定要求修复对端长度头导致 CLI 无界预分配、abort 的 P0。

## 基线依据

固定基线 cc7c4e6 的 `packages/sdk/src/transport.ts:112-115` 只向运行时 WebSocket 构造器传 URL，SDK 未配置帧/消息上限（读码依据，不把它外推为运行时能接受任意大帧）。Node v22.22.0 / undici 6.23.0 的真实 SDK 探针 `src/pilotdeck-cli/tests/fixtures/sdk-transport-limits/generate.mjs` 实测 16,777,217 与 104,857,601 字节单帧均成功，结果 `{sessions:[]}`；同目录 `oracle.json` 固化版本、源码哈希和两格结果。`ws` 包的 100 MiB 默认值仅为诊断对照，不是 SDK 默认实现。

本轮复测原始输出 `_tmp/20261026-issue-983/fix-r4/node-observed.stdout` 与 `node-observed/oracle.json`；敌意头不是 TS 金样，不宣称其与 TS 对齐。

## Rust 行为

`src/pilotdeck-cli/src/sdk/transport.rs` 中 `MAX_RECEIVE_BYTES = 128 * 1024 * 1024`；frame 与 message 均使用该有限值。帧头先拒绝超过上限的声明，避免 tungstenite 按 2^62 预分配；消息上限同时拒绝跨帧累计超过 128 MiB。沿用 receive 的错误映射，CLI stdout 为空、stderr 为 `Gateway WebSocket closed.\n`，退出码 1，无 signal。

128 MiB 是严格覆盖 TS 最大已实测成功格 100 MiB+1 的紧邻常见二进制容量，避免把实测成功区间截断；该值是安全预算，不声称从 TS 推导出同值上限。

## 范围

D1 CLI SDK ordinary RPC 接收 WebSocket 帧/消息；当前调用方为 `sdk/sessions.rs` 的 sessions list。超过 128 MiB 的输入属于显式安全差异；保留原有 16 MiB+1 与 100 MiB+1 的成功行为。不是全局进程内存限额，不更改 gateway 服务端、TS 基线、超时/取消/租约机制或冻结装置判分。

## 机器判据

`src/pilotdeck-cli/tests/sdk_sessions_list_exit.rs`：

- `sessions_list_accepts_ts_large_frames`：重放真实 TS 两成功格。
- `sessions_list_rejects_frame_cap_plus_one_header`、`sessions_list_rejects_extreme_frame_header`：只发完整合法未掩码长度头（128 MiB+1 / 2^62），保留 TCP，验证无 payload 即干净退出。
- `sessions_list_rejects_fragmented_message_over_cap`：完整合法响应分片累计 128 MiB+1，单帧 1 MiB，验证累计消息上限。
- ignored `sessions_list_transport_limits_regenerates_identically`：Node v22.22.0 原生探针重生成字节一致。

负控 `src/pilotdeck-gateway/tests/fixtures/sdk-sessions-list/negative_controls.py` 的 `websocket-unlimited-frame` / `websocket-unlimited-message` 分别删除单帧/累计消息限制，精确测试名与专属 nonce 回执绑定；三个原有下限负控继续保护 TS 两成功格。记录每项编译结果，完整判杀在提交后宿主执行；本轮仅运行 frame-cap 样本，不能将其代表全部负控通过。

## 批准记录

依人定规则修复，待人复核。2026-10-29 用户 #983 fix-r4 协调者补充明确要求两个有限上限、覆盖 100 MiB+1、超限干净关闭，并按安全/资源差异登记；本次在已授权范围内执行，不新增对拍计分豁免，不提前宣称阶段实效验收完成。
