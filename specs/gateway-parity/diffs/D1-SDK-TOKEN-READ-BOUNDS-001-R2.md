---
id: D1-SDK-TOKEN-READ-BOUNDS-001-R2
original: D1-SDK-TOKEN-READ-BOUNDS-001
---

## 类别

追加 cron/TUI 远端发现的 Unix token 资源边界与明确错误传播。原条目和 R1 均保留原字节；它们此前明确排除的入口不能靠读器复用自动算入验收。本修订不改冻结判据或批准状态。

## 基线依据

固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/pilotdeck.ts:363-367` 在 cron 调用 `connectRemoteGatewayIfAvailable`；`src/adapters/channel/tui/TuiChannel.ts:70-75` 同样通过它发现远端。`src/gateway/client/probeServer.ts:16-29` 在健康检查成功后调用 `readGatewayAuthToken`，后者 `src/gateway/server/authToken.ts:17-23` 对存在的文件执行无上限 `readFile(..., "utf8").trim()`。普通 IO 失败被 probe catch 转成不可用；cron 报需要运行 server，TUI 选择传入的 in-process fallback。健康检查的 AbortController 没有绑定文件读取。

旧 TS `pilotdeck.ts:318-323` 先检查交互终端，本片真实 Rust TUI 回归也提供 PTY stdin 才能进入健康检查，不以非 TTY 提前退出假冒资源拒绝。没有新采这两个 TS 入口的 FIFO/超限时序；原 SDK 19 格实采与既有 cron/TUI 成功金样不冒称本片新实采。

## Rust 行为

cron 的健康检查后读取与 TUI 的健康检查后读取统一直接调用 `sdk::token::read(&path)?`。Unix O_NONBLOCK、同 fd 普通文件判定、65536 原始字节上限与读取 65537 字节增长检测均复用原实现。已打开特殊文件输出 `Gateway auth token file must be a regular file.`，超限输出 `Gateway auth token file exceeds the 65536-byte limit.`；均 stderr 单行、退出 1、无 WebSocket，不把这两类资源拒绝静默换成另一个宿主。

普通路径解析/打开/元数据/读取失败、目录或空 token 保持各入口原不可用分支：cron 使用原 `pilotdeck cron requires a running pilotdeck server.`，TUI 回退 LocalHost。普通文件 UTF-8 lossy 与 JS trim 保持；不新增 argv/env token 限制。

server 继续使用既有严格 raw-byte reader（现名 `read_tui_token`，已注释其真实 server 用途）。它将目录和普通读取失败返回错误，仅不存在路径允许首次生成凭据；不能把发现客户端的空值回退策略套给凭据创建者，避免读取失败后换掉已有身份。server 的既有错误文案、原始字节与创建语义不属于此次更改。

## 范围

新增范围仅 `pilotdeck cron` 与 `pilotdeck tui` 的发现文件读取；SDK 与默认 channel 分别按原条目/R1 记载，serve/其他显式读器不能据此泛称已修。资源保证仅 Unix reader，新增真实进程矩阵仅 macOS；非 Unix 读器仍原实现，不宣称 Windows 编译或同等保护。

不承诺跨写者原子快照、故障文件系统内核 IO 可中断；不改 terminal 生命周期、WS wire、正常输出、server 身份生成、配置写入或原 oracle。

## 机器判据

`cli_channel_exit::cron_and_tui_token_resources_fail_before_connection_or_local_fallback` 在独占原固定端口租约并 bind 成功后，分别真实启动 cron list/TUI，先返回 health 200，再核无 writer FIFO、持有 writer FIFO、65537 字节、32 MiB 稀疏文件共八格。TUI 使用已有 rustix 同版本的安全 PTY API 提供 stdin，master 持有至子进程退出且 CLOEXEC；stdout/stderr 保持独立 pipe。逐格核 8 秒有界退出、精确 rc=1/空 stdout/单行错误、零 WS、未创建 LocalHost 状态、原配置字节与 token inode/mode/长度/mtime 不变。每格只清理自己的 Child 与临时目录，不碰已有服务。

共用夹具从原默认七格提取，七格完整 wire/stdout/取消与不变性断言仍在；普通 cron 成功和 TUI 交互继续使用既有金样/PTY 回归。新增测试/依赖候选仅完成 mbp2 standalone rustfmt 与 patch 格式检查，未由作者编译/运行；实际 Rust 由协调者排期，收到该轮原始记录前不登记通过。普通 IO/目录回退本片无新增进程格，仍由保留原分支及共享 reader 单测约束。

## 批准记录

**AI 代批，待人复核；未批准，非已接受差异，无冻结评分豁免。** 当前完整 CLI 目标中明确授权修复真实挂起和资源错误静默回退；server 保留严格凭据读取是既有身份合同，不是把安全问题豁免。测试/依赖候选实现方为 Codex，生产接线作者为协调者；只本机提交，不推 GitHub。未实测或待人签项不得计为阶段 D 完成。
