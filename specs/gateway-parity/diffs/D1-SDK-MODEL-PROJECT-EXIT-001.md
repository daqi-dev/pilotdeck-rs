---
id: D1-SDK-MODEL-PROJECT-EXIT-001
---

## 类别

SDK 本地参数校验已报错后仍保留 query 连接的挂起 bug 修复。

## 基线依据

固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的真实 CLI 在 mbp2、Node v22.22.0 / ICU 78.2、隔离 home 与动态 loopback WS 上实测：`run/resume --model fixture/model` 不带 project，先完成 control 的 new/resume，再完成 query 的 resume，随后输出 `projectKey is required when options.model is set.`，control 已关闭但 4.5 秒观察点 query 仍开、进程未退出，须逐 PID SIGTERM 清理。原件为主仓 `_tmp/20261108-cli-sdk-gaps/ts-mbp2-01/`；仓内汇集和可复采脚本位于 `src/pilotdeck-cli/tests/fixtures/sdk-model-project/`。

## Rust 行为

按原顺序保留 control 和 query 的两次会话动作；query resume 成功后发现 model 有值而 project 缺省，以内部类型标记本地校验失败，主动关闭两条连接并返回原错误文本、exit 1。缺 prompt、缺 resume ID 的优先级不变；远端返回同样错误文本不触发本地校验清理。已有信号中断处理保持原路径。

## 范围

仅 SDK run/resume 的本地 model 缺 project 分支；空串 project 按旧参数解析视为缺省，空白 project 仍作为实际 key 查询 catalog。其他初始化错误和流、SDK 信号、TUI、默认 CLI channel 不由本条改变。

## 机器判据

`src/pilotdeck-cli/tests/sdk_model_project_exit.rs` 启动真实 Rust 子进程，复放真实 TS 四格的 argv、请求和 stderr，且 fake server 不主动关闭本地错误分支；断言无 catalog/submit、query/control Close 及子进程 exit 1。同时验证空串/空白 project 和远端同文错误继续保留 query，直到 peer 主动关闭。全部 Rust 编译测试在 mbp2，测试不代表真实 provider 或完整 D1 验收。

## 批准记录

**AI 代批，待人复核**：按 intent 默认修复挂起 bug 与本轮全力实现 CLI 的授权执行。人签前为 **未批准，非已接受差异**，不取得冻结判分豁免，不修改既有冻结装置或阶段状态。
