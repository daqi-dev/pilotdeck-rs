---
id: D1-SDK-TOKEN-READ-BOUNDS-001-R1
original: D1-SDK-TOKEN-READ-BOUNDS-001
---

## 类别

补记默认 CLI channel 的 Unix token 文件资源拒绝。原条目明确仅覆盖 SDK，不能据此把默认 channel 计为已保护；本修订仅扩充该入口的适用范围，不改原条目字节、冻结判据或批准状态。

## 基线依据

固定 TS 基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/gateway/client/probeServer.ts:16-29` 在固定默认地址健康检查成功后调用 `readGatewayAuthToken`；后者在 `src/gateway/server/authToken.ts:17-23` 对存在的 token 文件执行无字节上限的 `readFile(..., "utf8").trim()`。普通 IO 异常由 probe 的 catch 转成不可用，空 token 同样不建立远端连接，随后进入原有本地回退。健康检查的 AbortController 没有传给文件读取，不能作为 token 读取期限。

原 SDK 条目的 19 格真实 TS 记录只证明 SDK 路径的实际行为，不是默认 channel 的异常文件时序实采。本入口普通成功 wire/stdout 使用既有固定 TS `src/pilotdeck-cli/tests/fixtures/cli-channel/ts-oracle.json`，本轮未改 oracle；上述默认入口异常文件差异依据真实源码，不冒称新增 TS 实采。

## Rust 行为

`cli_channel/remote.rs::connect` 仍先完成原健康检查，再解析原 token 路径，并直接复用 `sdk/token.rs::read`；不复制第二套读取策略。Unix 使用 O_NONBLOCK 打开，在同一 fd 检查普通文件，元数据大于 65536 原始字节即拒绝，实际读取至多 65537 字节以检测增长。FIFO 等已打开特殊文件输出 `Gateway auth token file must be a regular file.`；超限输出 `Gateway auth token file exceeds the 65536-byte limit.`，均 stderr 一行、退出 1，不建立 WebSocket，也不把资源拒绝误当作 LocalHost 回退。

缺失、打开/元数据/普通读取失败、目录及空 token 仍返回空值并走原回退；文件 UTF-8 lossy 解码、JS trim、普通成功 hello/submit/渲染路径不改。非 Unix 继续既有文件读取，不声称获得本修订的资源边界。

## 范围

仅 `pilotdeck` 默认 channel 的健康检查成功后 token 文件分支；SDK 范围仍由原条目记载，cron、TUI、serve 等其他读器不由本文证明。限额按读取前原始字节计，不是 trim 后字符数；argv/env 来源没有因此新增限制。普通 IO 回退与明确资源拒绝必须区分，不能统一返回 SDK 的 token-required 文案。

本修订不增加路径预读、不改配置或 token 文件，不承诺跨写者原子快照、故障文件系统内核 IO 可中断或非 Unix 等效保护。此次实际运行平台为 macOS/mbp2；Unix cfg 本身不是所有 Unix 平台实测证明。

## 机器判据

`src/pilotdeck-cli/tests/cli_channel_exit.rs::default_channel_token_files_are_bounded_and_preserve_ordinary_wire` 在独占固定端口租约并成功 bind 后启动隔离真实 CLI，每格先实际返回 health 200。七格为无 writer FIFO、持有 writer FIFO、65537 字节、32 MiB 稀疏文件、含 JS 空白的正常 token、65536 字节边界及正常连接后的 SIGTERM。资源格核有界退出、精确 stdout/stderr/退出码、零 WebSocket 连接、未创建 LocalHost 状态、配置字节和 token 身份不变；普通两格对既有 TS oracle 核完整 hello/submit 与 stdout，取消格核自有子进程及一条 WS 回收，不宣称与 TS 信号行为逐字对拍。

同 fd 增长/替换路径防护复用现有 SDK reader 单测，不把它写成本入口重复实采。本片没有新增普通 IO 回退进程格；该路径由保持原逻辑及共享 reader 既有单测约束。全部 Rust 检查由协调者在 mbp2 r82 执行，本文候选形成时尚未收到结果，不能计通过；实际日志与冻结源码 hash 待该轮结果登记。

## 批准记录

**AI 代批，待人复核；未批准，非已接受差异，无冻结评分豁免。** 属用户完整 CLI 目标内修复真实挂起/无界读取的可撤回安全边界，协调者已明确授权复用 SDK reader。原条目与 SDK 金样不回写，D1 完成历史、A8 前置和阶段 D 实效/人签不因此变化；仅本机提交、不推 GitHub，未运行或待签项不得冒充完成。
