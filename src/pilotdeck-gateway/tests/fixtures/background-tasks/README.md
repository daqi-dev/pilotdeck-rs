# 后台任务夹具（A6.5-pr12）

`oracle.json` 来自固定 TS 基线（`cc7c4e6`）的真实运行：真实 `createTaskTools` 五个工具、
真实 `BackgroundTaskRuntime`（原生 detached shell，`/bin/bash`，macOS）、真实
`BackgroundSubagentRuntime`，以及 `ProjectRuntimeRegistry.prototype` 上的
`stopBackgroundTaskForSdk` / `backgroundTasksForSdk`（`resolve` 换成返回本场景的两个注册表）。
`sourceSha256` 覆盖 `taskTools.ts`、`BackgroundTaskRuntime.ts`、`BackgroundSubagentRuntime.ts`、
`ProjectRuntimeRegistry.ts` 的相对 import 闭包；生成器用 `bindSourceTree` 校验基线干净树与 Git blob，
并断言 locale 为 `en-US`、平台为 `darwin`。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/background-tasks/generate.mts /abs/output-parent
```

每次运行在输出父目录下 `mkdtemp`，`flag:'wx'` 写入；进程临时目录带归属令牌，只删本次自建的树。
两次运行逐字节一致才更新正本（提交时
`sha256(oracle.json) = 05859dac4ca547029e51be66b917583383884c749b1f398b8e6bc636415181c3`）。
条数：definitions 5 · unsupported 5 · validations 12 · sequences 11 · steps 167。输入在 `cases.mjs`。

## 序列

| 序列 | 内容 |
|---|---|
| cursor-polling | 多次游标读取（offset/maxBytes 取整、越界、负数、非数字）、task_wait 超时与完成、完成后读尽 |
| exit-statuses | 非零退出、SIGINT（failed）、SIGTERM（cancelled）、空输出、shell 退出后后台子进程继续写入 |
| stop | 运行中 stop（进程组确认消失）、忽略 TERM 走 grace→SIGKILL、`graceMs` 非数字、重复 stop、未知 id |
| list-filters-and-sessions | agentId/kind/status（串、数组、空串、空数组）过滤，160 单元命令截断，会话隔离 |
| wait-abort-and-timeout | 预先 abort、中途 abort（`tool_aborted`）、超时、未知 id |
| dispose-drains | dispose 终止并排空运行中任务、关闭后输出不可读、dispose 后拒绝新任务、完成事件预览字节 |
| max-tasks | 上限拒绝与 stop 后再准入 |
| spawn-errors | Node 同步抛出的 spawn 失败（ENOTDIR、shell 不存在、NUL 字节 argv/env/cwd 含孤立代理）→ failed |
| non-string-command | 非字符串 command 被 `String()` 成 argv（`probes/p2.out`），**仅 macOS** |
| async-spawn-error | Node 异步报告的 spawn 失败（cwd 不存在）：无 pid、无 exit，任务停在 running |
| sdk-projections | SDK stop/list 投影：子代理与 shell 任务两路、会话归属、observer 不可停、各种结束状态 |

每个序列结束（`async-spawn-error` 除外，它没有子进程）断言所有任务进程组已不存在；`groupGone`
步骤在中途对指定任务断言同样的事。

## 归一化

- 任务 / 子代理 id（UUID）→ `<task:N>` / `<sub:N>`；场景根目录 → `<root>`。
- `waitedMs`（墙钟）→ `<ms>`；数值 `pid` 与文本 `pid=<数字>` → `<pid>`。
- 完成事件按任务序号排序后比较：同时放行的任务结束顺序本身是竞态（TS 两次运行也不同）。
- 时钟：`now` 固定为 2026-01-01T00:00:00Z（`startedAt` / `endedAt`）。

## 平台

前 10 个序列与平台无关，Linux CI 照常回放；`non-string-command` 的输出是 bash 自己的
`command not found` 文本（bash 5 多 `line 1:`），只在 macOS 回放，测试精确断言跳过名单与运行条数
（macOS 11，其它 10）。其它路径都已改成与路径长度、bash 版本无关的文本。

## 已知偏离与缺口

- **磁盘落盘与快照恢复**未移植：输出只在 1 MB 环形缓冲；`unknown` 状态无法产生（代码路径保留，未覆盖）。
- **异步 spawn 失败**（ENOENT 等）：TS 不会发 exit，任务永远 running，`stop` / `dispose` 永远等待
  （`probes/p1.out`：`missing-cwd stop hang`、`dispose-hang`）。Rust 不复制这个挂起：stop 之前可见状态
  与 TS 相同（running、无 pid）；`stop` 置 `interrupted`，在没有进程可发信号时立即结算为 `cancelled`
  （`endedAt` 有值、`exitCode` 为 undefined），触发完成事件并唤醒 waiter；`dispose` 同样结算后返回。
  该差异须登记进 `specs/parity/accepted-diffs.md`（原版缺陷 / 待批）。夹具只做读取。
- **未调用 dispose 就丢弃运行时**：与 TS 的 `child.unref()` 一致，子进程继续运行。
- **子代理执行入口**：只移植注册表与投影；`run` 由调用方注入，真正的 AgentLoop fork 不在本单。
- **bash `run_in_background`**：固定基线的 bash 工具没有该参数（`additionalProperties:false`），后台
  执行只走 `task_create`；本单不新增。
