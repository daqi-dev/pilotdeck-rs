---
id: D2-CRON-WS-EXIT-001
---

## 类别

挂起 bug 修复：一次性 cron 管理命令在终态后释放自己的远端连接。不是 cron 调度/执行语义变更。

## 基线依据

固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/pilotdeck.ts` 中 `handleCronCommand` 建立 RemoteGateway 后直接返回，没有关闭连接；`src/gateway/client/probeServer.ts` 返回持有 WS 的 RemoteGateway。

2026-11-08 在 mbp2、Node v22.22.0、隔离 home 和 fake HTTP/WS 服务实测 `cron list`：服务端收到 `cron_list` 并返回成功；stdout 为 `{\n  "ok": true\n}\n`；4 秒后进程尚未退出、WS 尚未关闭，发送 SIGTERM 后才清理。原始脚本和输出保留在主仓 `_tmp/20261108-issue-124-cron/ts-lifecycle.mjs`、`ts-lifecycle.log`；测试值均为虚构凭据。4 秒是该次观测窗口，不作为其他请求的新超时阈值。

## Rust 行为

`src/pilotdeck-cli/src/cron.rs` 在已经收到业务成功/失败响应或已完成参数错误输出后释放管理 WebSocket，使 CLI 返回相应退出码。业务成功为 0，业务错误和 usage 为 1；其他消息、参数、错误顺序和业务副作用仍按基线核验。

## 范围

仅 `pilotdeck cron list/create/delete/stop` 及 cron 未知/缺失子命令在建立连接后的终态生命周期。连接探测失败仍按旧不可用错误返回；未收到业务结果的 RPC 不引入新请求超时。不会停止已创建的 cron 任务，不改变 Gateway scheduler、任务停止操作或旧 SDK run/resume 的关闭语义。

## 机器判据

`src/pilotdeck-cli/tests/cron_exit.rs::cron_real_process_discovery_and_wire_contract` 使用真实 Rust CLI 子进程和独立 fake HTTP/WS，成功、usage 和业务错误各自先观察服务端收到 close/EOF，再核进程退出与 stdout/stderr。测试服务器收到结果后保持连接，不能由服务端主动断开制造假通过。四个成功叶命令均须满足退出约束。

在端口 18789 空闲的隔离宿主运行 `cargo test --manifest-path src/Cargo.toml -p pilotdeck-cli --test cron_exit`；端口已占用则拒绝启动被测 CLI，避免接入用户服务。正式 D0/P09 的完整四面比较与 A8 前置不被此新增产品测试替代。

## 批准记录

**AI 代批，待人复核**：按 `.42cog/intent.md` 已有人定的挂起 bug 默认修复边界执行；判断及可撤回范围见 `state/decisions-20261108-cli.md`。本条仅新增精确差异，不修改任何冻结判分文件、既有差异或阶段完成条件。
