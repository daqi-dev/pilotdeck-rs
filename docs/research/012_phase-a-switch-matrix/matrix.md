---
type: research
number: "012"
date: 2026-11-07
title: A8.5-pr2 切换状态机矩阵：状态×事件×时机逐格 TS 实测（#1353）
tags: [phase-a, switch, shutdown, matrix, parity, drain]
status: 主体逐格实测；A10 例外格与 background 三格（S1–S3 接纳、后代残留、start/存储写挂起）为源码推导，待 #2251 实测；断言表待主实现轮次落成确定性测试
related: [research/005_phase-a-gateway, research/008_phase-a-session-domain/must-know, research/008_phase-a-session-domain/decision, research/008_phase-a-session-domain/four-cell-matrix]
---

# 012 · 切换状态机矩阵：状态 × 事件 × 时机（#1353 / A8.5-pr2）

> 目标切换序：**停接纳 → drain 客户端+自动化入口（E1–E4）→ flush → 停旧 owner（含 E5 background dispose）→ Rust 恢复 → 新握手**——入口分组口径见 §三；E5 不在 flush 前 drain（A10 例外，随 S4 dispose 收敛）。
> TS 基线没有显式的切换状态机——它的停机组合根就是切换序在旧 owner 侧的投影。
> 本页把「切换的每一阶段 × 每个入口 × 到达时机」拆成格，**每格用 Node v22.22.0 对只读 TS 基线实跑取证**，
> 并给出主实现轮次可直接落成确定性测试的断言表。凡 TS 实际行为与切换目标冲突的格，登记为**基线差异**，不是照抄的规格。

## 一、方法与装置

- **被测**：只读 TS 基线 `/home/clawbox-08/ws/pdrs-runner/resources/PilotDeck` @ `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（下文 `B/` = 该目录；行号相对其根）。探针跑 **dist 构建**（构建时间晚于 src 最后修改，已核对 mtime）；tsx 不可用（`edgeclaw-memory-core` 顶层 await 与 CJS 冲突），这是选 dist 的原因。
- **运行时**：`node --version` → `v22.22.0`（原始输出见 `RESULT.md`）。
- **探针**（脚本与原始输出在 `_tmp/20261026-issue-1353/`，不进提交）：

| 探针 | 覆盖格 | 手段 |
|---|---|---|
| P1 | E1/E2 × S0 基线 | 进程内组装真实 `startPilotDeckServer`（假 Gateway）+ 真实 HTTP/WS 探测 |
| P2a | E1/E2/E4 × S1–S4 各阶段进行中 | 真实 `PilotDeckServerShutdownBundle`，步骤可控放行，阶段中途探测各入口 |
| P2b | t3：`server.close()` 阻塞规则 | 无连接 / 一条 WS / 空闲 keep-alive 三种对照 |
| P2c | E1 × S3/S4（既有 WS × flush/dispose 后） | stub `closeServer` 保住连接（真实组合到不了该状态，见 §4.1 的死锁说明） |
| P3 | t4：drain 失败 / 挂起注入 | 真实子进程复刻 `B/src/cli/pilotdeck.ts:257-281` 信号接线，注入 reject / never-resolve 的 `automation.stop` |
| P4 | E3 × 全阶段 + 边界并发 | 真实 `CronManager`（native storage，隔离 home）+ 假 agentGateway（可注入 abort 失败） |
| P5 | E5 × 全阶段 | 真实 `BackgroundTaskRuntime`（隔离 diskSpillDir；TERM 免疫任务拉宽 draining 窗口） |
| P6 | t5：端到端真实进程 | 真实 `node dist/src/cli/pilotdeck.js server`（高位端口 39888、隔离 home、假 provider 配置），SIGTERM 全时序 + 双 owner 并存格 |
| P7 | E4 × S2 + 启动完成竞态 | 真实 `startPilotDeckServer` + 受控假 channel（停机挂起、webhook） |

- 纪律：仅 loopback 高位端口、隔离 `PILOT_HOME`；探针不外发网络（provider 指向不可路由的 `127.0.0.1:9`，且不发起模型请求）；子进程记 pid、退出前收尸（本页全部原始输出采集后 `pgrep` 复核为零残留）。

## 二、状态轴（S）—— TS 停机组合根的实际顺序

信号接线：SIGINT/SIGTERM → `shutdownAndExit(0)`（`B/src/cli/pilotdeck.ts:275-280`）；`createShutdownAndExit` 用 `requestedExitCode = max(...)`、`stop().finally(() => exit(code))`（`B/src/cli/shutdownCoordinator.ts:3-19`）。
五步固定顺序在 `B/src/cli/PilotDeckServerShutdownBundle.ts:31-39`：`automation.stop` → `closeServer` → `flushChannelState`+telemetry snapshot → `disposeGateway` → `telemetry.shutdown`；失败收集、末尾聚合抛出（`:41-45`）。

| 阶段 | 切换语义 | TS 对应步 |
|---|---|---|
| S0 | 稳态 | 服务器正常运行 |
| S1 | 停自动化 stop 面（cron + always-on drain——命名域：E 轴「自动化入口」= E3/E4，不含 always-on，归属口径见 §三） | step1 `automation.stop()`：`B/src/cli/ProjectAutomationBundle.ts:260-269`/`:334-356`（先 cron 后 always-on）。cron 侧 abort 在飞 run，`B/src/cron/runtime/CronRuntime.ts:197-215` |
| S2 | 停接纳 + 停 channel | step2 `closeServer()`：`B/src/cli/pilotdeckServer.ts:234-259` —— 同步置 `closing=true`、清 `activeFeishu`，再停全部 channel（`server-shutdown`），最后 `closeGatewayServer()` = Node `server.close()`（`B/src/gateway/server/GatewayServer.ts:184-188`） |
| S3 | flush | step3 `flushChannelState()` + telemetry snapshot：`B/src/cli/ChannelAdapterBundle.ts:124-126` |
| S4 | 停旧 owner（gateway 域） | step4 `disposeGateway()`：`B/src/cli/LocalGatewayLifecycleBundle.ts:30-77`（watchers → gateway.dispose → subagents → `SessionRouter.shutdown`，含 stop-new 闸门 `B/src/gateway/SessionRouter.ts:110-113,421-459` → MCP → project runtimes，含 `BackgroundTaskRuntime.dispose` `B/src/task/runtime/BackgroundTaskRuntime.ts:434-449` → telemetry） |
| S5 | Rust 恢复 / 新握手 | TS 无对应：step5 `telemetry.shutdown()` 后 `process.exit`。旧进程退出 → 既有连接被内核断开。「新 owner 放开」格 TS 侧不存在，由断言表定义 Rust 侧义务（见 §六 A8/A9） |

**两处阶段语义必须先钉死**（后续每格都依赖）：

1. **S2 的「停接纳」分两层、两个时点。** `closing=true` 与 `activeFeishu=undefined` 在 `closeServer()` 入口同步生效（`B/src/cli/pilotdeckServer.ts:236-237`），channel 层闸门自此拒新；但 **TCP 停接纳发生在 S2 末的 `closeGatewayServer()`**（`B/src/cli/pilotdeckServer.ts:244`，即 Node `server.close()`）——S2 入口到该调用之间（channel 停机可无限挂起）**新 WS/HTTP 连接仍被接纳**（§4.2 closing 窗口、§4.4）。`server.close()` 之后只拒新 TCP，**既有 WS 连接不主动关、请求继续分发**（`B/src/gateway/server/GatewayWsConnection.ts:95-257` 无任何 shutdown 闸门）。
2. **S2 会无限期阻塞在 `server.close()` 上。** 只要有活着的 WS 连接（含关闭窗口内新建立的，见 P7），`close()` 永不 resolve，**无超时**。S3/S4 在真实组合里因此到不了——这是本矩阵最重要的一条基线事实（§4.1）。

## 三、事件轴（E）—— 入口

| # | 入口 | TS 依据 |
|---|---|---|
| E1 | WS：新 TCP+upgrade、hello、既有连接 request | upgrade 仅 `/ws`：`B/src/gateway/server/GatewayServer.ts:137-172`；hello→`hello_ok`：`B/src/gateway/server/GatewayWsConnection.ts:95-119`；request 分发 `:121-257`，**无 shutdown 闸门** |
| E2 | HTTP：public-approvals / public-host / `/health` / `/auth/local-token` / `/feishu/webhook` / `/api/web/*`（Bearer）/ static / 404 | `B/src/gateway/server/GatewayServer.ts:88-135`（顺序即回退链） |
| E3 | cron：tick 触发、`runTaskNow`、`createTask` 等管理方法 | scheduler `B/src/cron/runtime/CronRuntime.ts:166-194`；管理方法**无 started 闸门** `B/src/cron/runtime/CronManager.ts:113-176` |
| E4 | channel：启动完成竞态、hot-start、reconcile、cron result delivery、feishu webhook | closing 闸门与竞态处理 `B/src/cli/pilotdeckServer.ts:93-119`；`stopRunningChannel` `:121-152`；`hotStartChannel`/`reconcileChannels`/`deliverCronResult` `:298-310` |
| E5 | background：新任务提交、输出读取、stop、dispose drain | 运行时自有接纳闸门（draining/disposed 拒新任务）`B/src/task/runtime/BackgroundTaskRuntime.ts:240-242`；dispose `:377-449`。channel 层另有 closing 闸门（E4）、gateway 域 stop-new 闸门（S4 行） |

**入口分组口径（断言表 A3/A4/A10/A11 一律引用本口径，不再各自列举）**：客户端连接 **E1（WS）/ E2（HTTP）**——停 owner 超时与终态归 A4；自动化入口 **E3（cron）/ E4（channel）**——drain 挂起超时归 A3；background **E5**——不在 flush 前 drain，唯一 drain 点是 S4 dispose（A10 例外、A11 管辖），失败面见 A11。**always-on drain**：step1 `automation.stop()` 实况先 cron 后 always-on（`B/src/cli/ProjectAutomationBundle.ts:260-269`/`:334-356`——外层不吞：各自 stop 失败被收集后聚合抛出，`alwaysOn.stop()` 本身 reject 会经 `automation.stop` 抛出、走 A2 的 drain-throw 路径），但 always-on 不在 E1–E5 任一格——归属（并入自动化入口随 A3 管 drain 超时，或单列）由主实现轮对基线核对后钉死；在此之前 A3/A10 的覆盖声明（本口径所列组）不含 always-on、不宣称覆盖。**（r3 登记，#2260 实测）always-on 在飞 run 的失败面归 A3（drain 挂起超时面）承接，A2 不触发**：`AlwaysOnRuntime.stop()`（`B/src/always-on/runtime/AlwaysOnRuntime.ts:296-315`）对在飞 run **只等待、从不 abort**（cron 的 stop 显式 `abortTurn` 且吞 abort 失败，`CronRuntime.ts:197-214`；always-on 的 stop 无任何 abort 调用）——scheduler 在飞 tick 的失败被 `.catch(() => undefined)` 吞掉（`B/src/always-on/runtime/DiscoveryScheduler.ts:64-66`；timer 回调侧仅 warn 日志 `:80-84`），control run（rerunPlan/applyCycle）由 `Promise.allSettled` 等待、rejection 直接丢弃且无日志（`AlwaysOnRuntime.ts:301`）。推论（Node v22.22.0 实测，dist，基线 `cc7c4e65`）：在飞 run 失败（reject）⇒ `alwaysOn.stop()` 照常 resolve ⇒ `automation.stop` 不抛 ⇒ **A2（仅由 drain 抛出触发）不触发**，切换照走 flush；在飞 run 挂起 ⇒ stop 无超时等待 ⇒ `automation.stop` 挂起、停机卡 step1（该面 P3 已实测无超时、旧 owner 持续可服务）。探针原始输出：`_tmp/20261108-issue-2260-r1/raw/probe_always_on_stop.log`（失败注入下 stop() 均照常 resolve；挂起注入下 1200ms 观察窗不落定、全程 0 次 abortTurn，run 落定后 stop 才 resolve；#2260）。（r2 登记：此前三处「自动化入口」集合互斥——A3 写 E3/E4/E5、A10 写 E1–E4、§二 S1 又指 cron/always-on——统一钉死于此，拆开命名域。）

## 四、主矩阵：E × S 逐格

> 每格：**TS 实际行为**（实测）+ 原始输出摘录（`_tmp/20261026-issue-1353/raw/`）+ 源码行号。t1（稳态到达）与 t2（阶段进行中）折进各列。
> **行集约定**：各表只列到「行为发生最后一次变化」的阶段为止；未单列的格分两类，不是「未测」——S3/S4 在真实组合不可达（D3，P2c 仅 stub 可达）、S5 为进程退出（§5.3），行为沿用表内最后一行。

### 4.1 E1 WS × S

| 阶段 | TS 实际行为 | 证据 |
|---|---|---|
| S0 | hello `hello_ok`（protocolVersion 1.2）；request 正常应答 | P1：`[ws@S0/describe_server] {"type":"response","id":"probe-1","ok":true,...}` |
| S1（drain 中） | **无闸门**：新 WS 可连、`hello_ok`、请求照常。listener 未动 | P2a：`[ws@S1/hello] {"type":"hello_ok",...}`、`[ws@S1/describe_server] {"ok":true,...}` |
| closeGatewayServer() 后（S2 末，listener 已关） | **新 TCP 被拒**（ECONNREFUSED）；**既有 WS 的 request 与 hello 均继续被服务**——服务端从不主动发 close 帧 | P2a：`[tcp@after-close/refused] true`、`[ws@after-close/existing-describe] {"ok":true,...}`、`[ws@after-close/existing-hello] {"type":"hello_ok",...}`；P6 进程级复核：SIGTERM 同轮 `{"kind":"existing-ws","ok":true}` |
| S2 的阻塞效应 | 有活 WS ⇒ `close()` 不 settle（**无超时**）；客户端断开后 1ms 内放行，flush 9ms 后进入。**有活 WS 时 S3/S4 在真实组合里不可达** | P2a：`[closeServer-blocked-by-ws] {"flushEnteredWhileWsOpen":false}` → `[flush-after-client-close] {"flushEnteredMs":9,"stepOrder":["automation.stop:enter","closeServer:enter","flush:enter"]}`；P2b 对照：无连接 settle / 一条 WS 2000ms 不 settle、客户端关闭后 1ms resolve / 空闲 keep-alive settle |
| S3/S4（仅 stub 可达） | flush 与 gateway dispose **之后**，既有 WS 的 request 与 hello 仍被分发（连接层无闸门、`InProcessGateway.dispose` 也不设后续调用闸门 `B/src/gateway/client/InProcessGateway.ts:2061-2065`） | P2c：`[ws@S3/describe] {"ok":true,...}`、dispose 后 describe 调用照进 fake gateway 的账本 |
| S5（进程退出） | 内核断开既有连接；未退出前一切照旧（P3 hang 格） | P6：`[ws-final] {"closed":false,...}`（15s 仍无 close 帧）直到 SIGKILL |

**登记：S2 的阻塞效应是「切换状态机」目标序的第一处基线冲突。** 目标序要求 drain 完成后停旧 owner；TS 基线的 S2 把「停旧 owner」无限期押后到「最后一个 WS 客户端自愿离开」，drain 与 flush 反而排在其后。主实现轮次必须显式决策（断言表 A4/A10）。

### 4.2 E2 HTTP × S

| 阶段 | TS 实际行为 | 证据 |
|---|---|---|
| S0 | `/health` 200；`/api/web/*` 无 token 401、带 token 200；static 命中；未知路径 404 | P1：`[http@S0/health] {"ok":true,"status":200,...}`、`[http@S0/api-web-401] {"ok":true,"status":401,...}`、`[http@S0/api-web-authed] {"ok":true,"status":200,"body":"[]"}` |
| S1（drain 中） | 与 S0 完全一致（listener 未动，路由无闸门） | P2a：`[http@S1/health] {"ok":true,"status":200,...}` |
| closeGatewayServer() 后（S2 末，TCP 已拒新） | 新连接 refused；**空闲 keep-alive socket 被 `server.close()` 主动关闭**；关闭后复用报 ECONNREFUSED——该错误来自客户端 agent（`node:http.Agent`，`probes/common.mjs` keepAliveProbe；非 fetch——fetch 同场景观测为 `TypeError: fetch failed`）丢弃已关 socket 后**重建连**、撞已关 listener（错误串 `connect ECONNREFUSED` 是新 TCP 连接失败、非同 socket 报错；重建连解释为机制推断，**需实测**）——复用态错误码不写成确定性断言，可确定性落的只有 A14①②两条；SIGTERM 同轮的 keep-alive 在途请求得到 socket hang up（ECONNRESET，同 agent 复用、未控时序单样本）——P6 单样本（n=1，未控制请求与 SIGTERM 先后），只登记观测，断言按 A14 不变量（不分别锁错误码） | P2a：`[ka@after-close/reuse] {"ok":false,"error":"ECONNREFUSED..."}`；P6：`{"kind":"keepalive-http","ok":false,"error":"ECONNRESET: socket hang up"}`、`{"kind":"new-http","ok":false,"error":"TypeError: fetch failed"}` |
| S2 的 closing 窗口（channel 停机挂起、listener 未到 close） | `/health` 仍 200 | P7：`[closing-window/health] {"ok":true,"status":200,...}` |
| S5 | 端口在进程存活期间即已释放（listener 已关），见 §5.3 | P6：`[port-after-exit] {"refused":true,...}`（**listener 释放后、旧进程仍存活时**探测——紧随 `exitCode:null` 的 exit 观测 1 ms 后（`p6_real_server.json`）；round2 干净退出后未再探测，**退出后端口释放无直接证据**）＋ `[concurrent-takeover]`（存活期间新进程已监听，直接证据见 §5.3；注意时序：listener 释放 ≠ 进程退出） |

> r3：closeGatewayServer 行按 A14 口径改写——原「复用态可确定性断言」与 A14 的撤回相矛盾，删除；复用态错误码降为观测登记（未控时序单样本），只保留「服务端关闭空闲 socket」「已关 listener 的新连接 refused」两条可确定性断言。

### 4.3 E3 cron × S

| 阶段 | TS 实际行为 | 证据 |
|---|---|---|
| S0 | `createTask` → scheduled；`runTaskNow` → `{started:true}`，~300ms 后 `submitTurn` 到达 gateway；在飞时再 `runTaskNow` 建新副本（无去重闸门），`listTasks` 出现 `["running","scheduled",...]` | P4：`[S0/runTaskNow] {"started":true,...}`、`[S0/fired] {"submitTurnCalls":1,"ms":301}`、`[S0/listTasks] {"statuses":["running","scheduled",...]}` |
| S1（= `CronRuntime.stop()` 内部） | scheduler 先停 → 在飞 run 逐个 `abortTurn({reason:"system:cron_shutdown"})` → release 全部 task session。**abort 失败被 `.catch(() => undefined)` 吞掉，stop 仍成功** | P4：`[gw.abortTurn] {...,"reason":"system:cron_shutdown"}`、`[stop()] {"stopResult":{"resolved":true},...}`；注入 abort reject 后 `[stop-with-failing-abort] {"stop2":{"resolved":true},...}`。源码 `B/src/cron/runtime/CronRuntime.ts:207-212` |
| stop 后（S2 起） | **CronManager 管理方法无 started 闸门**：`createTask` 成功落库；`runTaskNow` 返回 `{started:true}` 但 **scheduler 已停，永不触发**（静默不执行） | P4：`[after-stop/createTask] {"ok":true,...}`、`[after-stop/runTaskNow] {"started":true,...}`、`[after-stop/fired?] {"submitTurnCallsAfterStop":1}`（计数未涨） |

### 4.4 E4 channel × S

| 阶段 | TS 实际行为 | 证据 |
|---|---|---|
| S0 | 启动完成入 `runningHandles`；`deliverCronResult` true；feishu webhook 由 `activeFeishu` 分发（自定义 200） | P7：`[S0/started] {"feishu":["feishu"],"test":["test"]}`、`[S0/feishu-webhook] {"ok":true,"status":200,"body":"feishu-probe-ok"}` |
| S2 的 closing 窗口 | `hotStartChannel` / `reconcileChannels` 拒绝 `"channel server is closing"`；`deliverCronResult` 返回 **false**（handle 先被摘走）；**feishu webhook 退化到 static 回退**（`activeFeishu` 在 `closeServer()` 入口同步清空，`B/src/cli/pilotdeckServer.ts:237`），表现为 200 + `index.html`——「关闭中」没有任何显式信号 | P7：`[closing-window/hot-start] {"error":"channel server is closing"}`、`[closing-window/deliverCronResult] {"delivered":false}`、`[closing-window/feishu-webhook] {"ok":true,"status":200,"body":"<html>probe-static</html>\n"}` |
| S2 之后 | `closing` 永久为真，同一错误文案 | P2a：`[reconcile@after-close] {"error":"channel server is closing"}`、P7：`[after-close/hot-start] {"error":"channel server is closing"}` |
| 启动完成竞态（t3） | channel 的 `start` 在 close 之后才完成 ⇒ handle 立即被 `stop("server-shutdown")`、状态报 `startup completed after server shutdown`、start 记为 failed——竞态有终态，不悬挂 | P7：`[race/started] {"started":["test"]}`、`[race/stops] {"stops":[{"key":"test","reason":"server-shutdown"}]}`；源码 `B/src/cli/pilotdeckServer.ts:99-108` |
| closing 窗口内的新 WS | 被接纳且 `hello_ok`，随后同样阻塞 `server.close()`（与 §4.1 同因） | P7：`[closing-window/new-ws-hello] {"type":"hello_ok",...}`、`[close-blocked-by-new-ws]` |

### 4.5 E5 background × S

| 阶段 | TS 实际行为 | 证据 |
|---|---|---|
| S0 | `start` → running；`getOutput` 增量可读；单任务 `stop` 正常 | P5：`[S0/start] {"taskId":"...","status":"running"}`、`[S0/stubborn-output] {"slice":"{\"content\":\"stubborn-start\\n\",...}"}` |
| S1–S3（flush 前 · dispose 前） | **运行时尚未 draining**：接纳闸门只查运行时自身的 draining/disposed 标志（S4 dispose 内才置位），经既有 WS 提交的新任务**照常被接纳**（「被接纳」窗口）——**源码推导、未实测**，待 #2251 补格。被接纳任务的后续去向两态（同为源码推导）：提交连接在 `server.close()` settle 前断开 ⇒ S4 可达、任务由 dispose 杀；提交连接保持打开 ⇒ D3 使 `close()` 永不 settle、S4 不可达，任务不被 dispose 杀，旧进程被 SIGKILL 后 detached 启动的任务子进程不随父进程退出、成为孤儿（#2251 探针须控制提交连接的去留，两态各补一格） | 源码 `B/src/task/runtime/BackgroundTaskRuntime.ts:240-242`（gate 只看运行时自身状态）；P5 未覆盖 S1–S3 的提交路径 |
| dispose 进行中（S4 · `disposeGateway()` 内） | **运行时自有接纳闸门**（唯一以任务运行时状态 draining/disposed 拒绝的入口；channel 层 closing 闸门见 §4.4、TCP 停接纳见 §二）：新任务抛 `Background task runtime is draining; refusing a new task.`；**输出在 drain 期间照常可读**；TERM 免疫任务把窗口拉满 5s grace 后 SIGKILL 兜底（实测 `dispose()` 发起到 resolve ≈5.0s：探针在 dispose 后 300ms 才起 t0，`ms:4705` 是尾段）。`draining` 态只在 S4 出现——运行时自身在 S1–S3 无状态变化（dispose 由 `disposeGateway` 调起，非 closeServer；但新任务提交在 S1–S3 仍被接纳，见上一行） | P5：`[during-dispose/start] {"admitted":false,"error":"Background task runtime is draining; refusing a new task."}`、`[during-dispose/getOutput]`（同上可读）、`[dispose-resolved] {"ok":true,"ms":4705,...}`（`S0/stubborn-output` 08:11:41.129 → `dispose-resolved` 08:11:46.135，全程 ≈5.0s）。源码 `B/src/task/runtime/BackgroundTaskRuntime.ts:240-242`（gate）、grace 兜底 `:377-449`、dispose 由 `B/src/cli/LocalGatewayLifecycleBundle.ts:30-77` 调起 |
| dispose 后 | 新任务抛 `... is disposed; refusing a new task.`；任务终态 `cancelled`+`interrupted:true`；`list()` 残留记录 | P5：`[after-dispose/start] {"admitted":false,"error":"Background task runtime is disposed; refusing a new task."}`、`[after-dispose/task2] {"status":"cancelled","interrupted":true}`、`[after-dispose/list] {"count":3}` |

## 五、时机专项

### 5.1 t3 边界并发（SIGTERM 同一轮：既有 WS / 新 WS / keep-alive / 新 HTTP）

P6 实测（真实进程）：

```
[sigterm-concurrent] [
  {"kind":"existing-ws","ok":true},                                  ← 既有连接继续服务
  {"kind":"new-ws","error":"connect ECONNREFUSED 127.0.0.1:39888"},  ← 新连接被拒
  {"kind":"keepalive-http","ok":false,"error":"ECONNRESET: socket hang up"},
  {"kind":"new-http","ok":false,"error":"TypeError: fetch failed"}]
```

即：**同一轮里「既有入口继续服务、新入口关闭」并行成立**——停接纳生效于新连接，对存量连接完全豁免。其中 `keepalive-http` 的 ECONNRESET 是单样本观测（n=1，未控制请求与 SIGTERM 先后），断言口径见 A14。

### 5.2 t4 注入：drain 失败 / 挂起

| 注入 | TS 实际行为 | 证据 |
|---|---|---|
| `automation.stop` reject（fail） | **其余步骤照走**（close-server-enter → flush-enter → dispose-enter → telemetry-shutdown 全打印），`stop()` 聚合抛出，但 `shutdownAndExit` 的 `.finally(() => exit(0))` 先到——**退出码 0**。计划阶段曾预测 unhandledRejection 会把码升为 1，实测不成立：`process.exit` 同步执行，unhandledRejection 事件没有机会再触发 | P3：`[fail/exit] {"exitCode":0,"signal":null,"ms":400}`、`[fail/child-stdout]`（五步标记齐全，`child-exit code=0`）。机制在 `B/src/cli/shutdownCoordinator.ts:10-13` + `B/src/cli/pilotdeck.ts:277-280` |
| `automation.stop` never-resolve（hang） | **无超时**：SIGTERM 后 2s 仍存活，**listener 保持开放、`/health` 200、新 WS hello+describe 照常**——旧 owner 完整可服务（观察窗即驱动脚本的 2s，随后 SIGKILL 收尸；「无超时」的依据是源码无超时逻辑 `B/src/cli/shutdownCoordinator.ts:3-19` + Node `server.close()` 无超时参数，而非长时间观察——P6 有 WS 客户端场景另行实测 15s 窗口 exitCode 恒 null，见 §5.3） | P3：`[hang/health-after-2s] {"ok":true,"status":200,...}`、`[hang/ws-after-2s] {"helloType":"hello_ok","respOk":true}`、`[hang/still-alive] {"exitCode":null,"signal":null}`（2s 时点；`p3_driver.mjs` SIGTERM 后 2000ms 即 SIGKILL）；P2b 空闲对照同为 2000ms 观察窗（`settledAfterHoldMs:false` 只证明 2000ms 内未 settle） |
| cron abort 失败 | 被吞，`stop()` 仍成功（§4.3） | P4 |
| background 任务 TERM 免疫 | 5s grace 后 SIGKILL 兜底；dispose 收敛（实测 ≈5.0s，§4.5）——但「必然收敛」只对 running 任务且 output/snapshot 写入不挂起成立：`whenStartsDrained()` 无超时、SIGKILL 后 `await done` 与 flush 无二次超时（A11 注；start/存储写挂起格待 #2251 实测） | P5 |

**登记：drain 失败在 TS 基线里不阻断切换、不改退出码；drain 挂起无超时且旧 owner 持续可服务。** 目标序「drain 失败必须停切换且旧 owner 仍可服务」两条基线都不满足（断言表 A2/A3）。

### 5.3 t5 旧进程退出 / 新 owner 放开（P6 端到端）

| 观察 | TS 实际行为 | 证据 |
|---|---|---|
| 有 WS 客户端时 SIGTERM | 进程**不退出**（15s+ 窗口 exitCode null，`server.close` 等既有连接）；WS 侧无 close 帧（`closed:false`、closeEvent null） | `[exit] {"exitCode":null,...}`、`[ws-final] {"closed":false,"closeEvent":null}` |
| 双 owner 并存 | 旧进程未退出时，**新进程已在同端口完成监听并可服务新 WS**（`newListened:true`、`newWsHello:"hello_ok"`），同时旧进程仍服务其既有 WS（`ok:true`）——**同端口双 owner 并存窗口** | `[concurrent-takeover] {"oldProcessAlive":true,"newListened":true,"newWsHello":"hello_ok"}`、`[old-still-serving-existing-ws] {"ok":true}` |
| 无客户端时 SIGTERM（对照） | ~125ms 干净退出，退出码 0，telemetry snapshot 打印 | `[round2-exit] {"exitCode":0,"signal":null}` |

**登记：「旧进程（及其后台 writer）退出前不放开新 owner」在 TS 基线被直接违反**：listener 在进程存活期间即释放，端口可被新进程抢占，而旧进程的既有 WS 仍在服务、background 任务尚未 dispose（S4 未到）、channel/telemetry 状态尚未 flush（S3 未到）——cron 已在 S1 停止、channel 已在 `closeServer()` 内停止（P7：`closing-window/*` 均落在 close-settled 之前、`stops` 在 close 流程内完成；P1/P2：`stepOrder` 为 automation.stop → closeServer → flush）。这是 A8.5 切换序里最尖锐的基线差异（断言表 A8/A9）。

## 六、断言表（主实现轮次的确定性测试期望）

> 每条给出：**Rust 侧期望**（切换目标）与**TS 基线实际**（本页实测）。凡两者相左的，测试必须锁 Rust 行为、并注明基线差异编号；与基线一致的，直接断言等价。编号供 issue 引用。**本页 A1–A14 / D1–D10 是页内局部编号**，与差异账本 ID（`specs/gateway-parity/diffs/` 的 `<area>-<topic>-NNN` 族）、交付区号（A8.5 等）、阶段码是不同命名空间，跨文档引用须写 `012:A10` 形式。

| # | 断言 | Rust 期望 | TS 基线实际 | 差异 |
|---|---|---|---|---|
| A1 | 停接纳对新入口原子生效 | 停接纳点（channel 闸门 + TCP listener）同点生效：此后新 WS/HTTP 连接被拒，既有行为与 §5.1 一致 | **差异**：channel 层闸门自 S2 入口生效，TCP 停接纳在 S2 末 `closeGatewayServer()`——closing 窗口内（S2 入口→该调用）新 WS/HTTP 仍被接纳（§4.2 closing 窗口 `/health` 200、§4.4 closing 窗口新 WS `hello_ok`） | **D10** |
| A2 | drain 失败必须停切换 | drain 步骤失败 ⇒ 不进入 flush/停 owner，进程保留可服务性并报错 | 失败被吞，后续步骤照走，退出码 0 | **D1** |
| A3 | drain 挂起有界 | drain 超时 ⇒ 记录并**停切换**、保持旧 owner 可服务，**不得带着在飞入口进 flush/dispose**（与 A2/A10 同一失败面；不设「放弃等待后继续」分支）。**A3 只管自动化入口（按 §三口径）**；客户端连接的超时归 A4；**E5 不在 flush 前 drain**（唯一 drain 点是 S4 dispose 本身，A10 例外）——E5 失败面不属 A3，两支终态承接在 A11（dispose 失败 ⇒ 退出路径由决策卡钉死；dispose 挂起 ⇒ 无界、不宣称 exit，孤儿路径见 §4.5）。always-on stop 的失败面（在飞 run 失败被吞不抛、挂起无超时且从不 abort——A2 因此不触发）按 §三 r3 登记归本条挂起超时面承接（#2260 实测）。超时值与终止在飞入口的手段（取消/abort 在飞 run）由主实现轮次决策卡钉死；可先落的边界断言：超时值为正有限值且不高于上限常量（单位显式钉死；0、负数、NaN、无穷大、溢出值（如 `u64::MAX` 毫秒、`1e300`）显式拒绝——「正有限值」无上限会把实际无界的值判过；上限值与单位由决策卡钉死；「同一界」指与 A4/A11 共用同一校验谓词，非数值相等） | 无超时；挂起时旧 owner 持续可服务（唯一「可用性保留」来源是没退出） | **D2** |
| A4 | 停 owner 不被客户端绑架 | 切换序不得等待「最后一个 WS 客户端自愿离开」；**A4 管全部客户端连接（按 §三口径——A3 转授的客户端超时责任在此承接）**：停接纳后既有 HTTP 连接同样有界等待——在飞请求按 A14③ 的不变量处理（完成或显式 abort，不得静默丢弃），零字节/半截头/慢 body 连接不得无限挂起（终止口径由决策卡钉死）。应有 close 帧与超时——close 帧 code/reason、超时值、以及**超时到期后的终态**（强断连接并继续切换 / 停切换并入 A3 失败面，二选一）由主实现轮次决策卡钉死，且终态须与 A3/A10 自洽（若强断并继续，在飞入口须先被终止，不得违反 A10）；超时起算点 = 停接纳点（A1），E1/E2 同一起算点，覆盖 close 帧发送与等待全程。可先落的边界断言：超时值为正有限值且不高于上限常量（0、负数、NaN、无穷大、溢出显式拒绝；单位与上限值由决策卡钉死，与 A3/A11 共用同一校验谓词） | `server.close()` 无限期等既有 WS，无 close 帧，S3/S4 不可达；零字节/半截头 HTTP 连接同样使其不 settle——同类 node:http 机制已由 `A8.3-CH-SMS-012-R1` 对 `SmsChannel` 实例实测（零字节 `stopWaitedMs=28999`（30 s 窗，**只证到 30 s**——Node `headersTimeout` 默认 60 s，30 s 内未 resolve 区分不了「永不 settle」与「60 s 才 settle」，≥65 s 观测待补（#2251 同族））、半截头 `199001`（200 s 窗），均右删失下界、未 resolve；close 后 `httpServerPreClose` 清 connections checker，408 不再驱动），**GatewayServer（39888）实例级未实测，留主实现轮补测** | **D3** |
| A5 | 既有 WS 在停 owner 前收到显式 close | drain 边界通知连接（close code/reason），之后不再分发 | 从不主动 close；flush 与 dispose 之后 request 仍被分发（P2c） | **D4** |
| A6 | cron：stop 即拒绝管理面 | stop 后 `createTask`/`runTaskNow` 显式报错 | 无闸门；`runTaskNow` 返回 `started:true` 静默不触发 | **D5** |
| A7 | cron abort 失败可观测 | abort 失败计入失败面（至少记日志/上报） | `.catch(() => undefined)` 全吞 | **D6** |
| A8 | 旧进程退出前不放开新 owner | 端口/所有权在旧进程（含后台 writer）完全退出前不交给新 owner | listener 先于进程释放；同端口双 owner 并存，旧进程仍服务既有 WS | **D7** |
| A9 | 新握手前置条件 | 新 owner 只在旧 owner drain+flush+退出完成后接受握手 | TS 无此概念（新进程可立即接管） | **D8** |
| A10 | flush 前无在飞入口（E1–E4） | flush 与 dispose 只在**E1–E4 入口（按 §三口径，不含 always-on）** drain 完成后发生（含 join channel 层全部在飞 stop/operation，`closing=true` 后失败的 stop 不得把 handle 回填为「仍在运行」）。**E5 background 例外**：TS 基线与目标序一致——flush（S3）在前、background dispose（S4）在后，S3 flush 时允许 background 任务在飞，由「flush 后、dispose 前」窗口的独立断言覆盖：S4 dispose 须收敛全部 background 任务（A11）。E5 在 S1–S3 的接纳行为见 §4.5 对应行（源码推导，待 #2251） | flush 被既有 WS 无限押后；dispose 后仍有入口活着（P2c）。另有**源码推导、未实测**的例外：`closeServer()` 只等快照时的 handle——并发在飞的 channel stop 不被 join、失败 stop 在 `closing=true` 后仍回填 handle（`B/src/cli/pilotdeckServer.ts:121-152`；待 #2251 补探针登记） | 与 D3/D4 同根 |
| A11 | background 闸门等价 | draining/disposed 拒新任务、drain 期间输出可读、grace+SIGKILL 兜底；grace 值为正有限值且不高于上限常量（0、负数、NaN、无穷大、溢出显式拒绝；与 A3/A4 共用同一校验谓词；默认 grace 是否对齐基线 5 s（§4.5 实测 ≈5.0s）由决策卡钉死）；grace 到期杀**任务所在进程组**（POSIX `kill(-pid)` 只覆盖同进程组；Windows `taskkill /T /F` 为树杀）——后代若 `setsid`/改 pgid 即逃逸，**不在杀灭断言内**；「零残留」检测口径必须**覆盖**杀灭口径（主口径：杀前全 pid 快照 + 杀后存活差集（或 subreaper/pidfd/cgroup 等价手段）；ppid 链/组、会话枚举只作辅助、**不得单独判 PASS**——任务进程被 SIGKILL 后未 `setsid` 的后代 reparent 到 1、`setsid` 后代改会话，两者都让以任务 pid 为锚的链/会话枚举漏检、把假 PASS 当 PASS；Windows 树枚举单独钉死），跨组逃逸负控（`setsid` 子进程）以快照差集判活，是「零残留」成立的前置，待 #2251 实测登记；「SIGKILL 后 dispose 收敛」以任务 running 且 output/snapshot 写不挂起为前提（`whenStartsDrained` 无超时、`await done`/flush 无二次超时——**E5 dispose 失败面在此承接**（不属 A3）：E5 dispose 是否加界由主实现轮决策卡钉死；失败（抛出）⇒ 是否尽力走完 telemetry/exit 及退出码由决策卡钉死；挂起 ⇒ 无界、telemetry/exit 到不了，**不宣称 exit/telemetry 终态**，亦**不宣称保持旧 owner 可服务**（flush 已完成、owner 已在 dispose 中拆，回不到「停切换保可服务」的失败面；孤儿路径见 §4.5）） | 基线按进程组杀（`B/src/tool/execution-world/DetachedShellPort.ts:128-162`）、`done` 只绑直接子进程 exit；闸门文案**一致**（`Background task runtime is ...; refusing a new task.`）。P5 未测任务派生子进程的后代残留与 start/存储写挂起格（待 #2251） | — |
| A12 | channel closing 闸门等价 | closing 后 hot-start/reconcile 报 `channel server is closing`、`deliverCronResult` false、启动完成竞态立即 stop | **一致**（含 webhook 退化到 static 回退的怪相——Rust 侧建议改为显式 503，见 D9） | D9（低优） |
| A13 | 无客户端 SIGTERM 干净退出 | 退出码 0、遥测 snapshot、端口释放 | 一致（~125ms） | — |
| A14 | keep-alive 语义 | 停接纳后空闲 keep-alive socket 被服务端关闭；**关闭后复用**报连接已关（ECONNREFUSED）——此态的 ECONNREFUSED 来自测试客户端 agent（`node:http.Agent`，`probes/common.mjs` keepAliveProbe；非 fetch——fetch 同场景新连接观测为 `TypeError: fetch failed`，P6 `new-http`）丢弃已关 socket 后**重新建连**、撞已关 listener，不能当纯服务端契约锁死（重建连解释是机制推断，**需实测**——裸 socket 对照，归并 #2251）。断言拆两条可确定性落：①服务端关闭空闲 socket；②对已关 listener 的新连接 refused（ECONNREFUSED）——②预设停接纳 = 关 listener，与 A8/A3 的相容性取决于停接纳机制（bind 保留/accept 拒绝 vs 关 listener），由主实现轮决策卡钉死（P2-跟进 #2258）；复用同一连接的错误码形态未在受控窗口实测（P6 `keepalive-http` 为同 agent 复用、未控时序的单样本 ECONNRESET，裸 socket 写失败未测）——要锁复用态错误码须钉死客户端与等待窗（或改用裸 socket 观测）。**SIGTERM 同轮在途请求**不写成契约（终局取决于请求字节是否已被解析派发，单样本竞态）：断言不变量——①停接纳后不得再被接纳、任何请求不得挂起；②未派发的在途请求允许被重置（ECONNRESET 是 P6 的单样本观测，非唯一期望）；③已派发处理的请求须完成或收到显式 abort，不得静默丢弃。要锁具体错误码须用受控时序（先发请求后 SIGTERM / 反之）分别实测，不把一次竞态样本当契约 | 复用态实测一致（P2a after-close 复用 ECONNREFUSED——探测走 `node:http.Agent` 重建连路径，错误串 `connect ECONNREFUSED` 为新 TCP 连接失败、非同 socket 报错；fetch 路径观测为 `TypeError: fetch failed`，P6 `new-http`）；在途态 P6 单样本（n=1，未控制请求与 SIGTERM 先后）观测同 agent 复用 `ECONNRESET: socket hang up`，「已派发请求跑完」为源码推导（`server.close()` 等活跃请求），未实测 | — |

差异清单汇总：**D1** drain 失败被吞且退出码 0；**D2** drain 无超时；**D3** `server.close()` 被既有 WS/HTTP 连接无限阻塞、S3/S4 不可达（HTTP 侧同类机制实测注见 A4，GatewayServer 实例级待补测）；**D4** 从不主动发 WS close 帧、dispose 后仍分发；**D5** cron 管理面 stop 后无闸门且静默不触发；**D6** cron abort 失败全吞；**D7** 双 owner 并存（listener 先于进程释放，旧进程仍服务）；**D8** 新握手无前置；**D9** closing 期 webhook 退化 static 200 无显式信号；**D10** 停接纳非原子——channel 闸门在 S2 入口、TCP 停接纳在 S2 末，closing 窗口内新 WS/HTTP 仍被接纳。其中 D3/D4/D7/D8/D10 是切换序的结构性冲突，主实现轮次须先定方案再落测试。

## 七、本页边界

- 全部结论固定于 Node v22.22.0 + 基线 `cc7c4e65` 的 dist 构建；`server.close()`/keep-alive 语义是 Node 版本敏感的。
- S3/S4 的入口行为（P2c）靠 stub `closeServer` 保连接取得——真实组合到不了该状态（D3），测试只能各自单独构造，不能在同一条真实停机流里同时观察 D3 与 D4。
- 探针的假 Gateway / 假 channel 只替换「请求最终去向」，不替换连接层、路由层、闸门逻辑本身；那些层全部真实。
- **源码推导、未实测的格**（A10/§5.2 已标注，待 #2251 补探针）：①「hot-reload stop 在飞时 SIGTERM」与「channel stop 失败」两格——`closeServer()` 只等快照时的 `runningHandles`，失败 stop 在 `closing=true` 后仍回填 handle（`B/src/cli/pilotdeckServer.ts:121-152`）；②P5 background 未覆盖三格：「任务派生子进程的后代残留」（基线按进程组杀，`done` 只绑直接子进程）、「start 未落定/快照写挂起」（`whenStartsDrained`、`await done` 均无超时）、以及 §4.5 的「S1–S3 新任务接纳」格（闸门只看运行时自身标志，源码推导；含被接纳任务的两态去向——提交连接断开 ⇒ S4 dispose 杀 / 提交连接保持 ⇒ S4 不可达、detached 子进程成孤儿，#2251 探针须控制提交连接去留）。
- 本页只登记事实与断言方向，不替主实现轮次做方案决策（D3/D4/D7/D8 的解法留给后续决策卡）。
- **待钉死的断言前提**：A14② 与 A8/A3 的相容性取决于停接纳机制（bind 保留/accept 拒绝 vs 关 listener），见 P2-跟进 #2258；零字节 ≥65 s 观测与裸 socket 复用态观测归并 #2251。
- **与既有差异账本的映射**（本页 D/A 编号与 `specs/gateway-parity/diffs/` 条目同源时以账本为准，主实现轮落测试/登记前先对账本核对，避免重复登记或与已批准口径冲突）：D1 ≈ `A7.2-NATIVE-PROCESS-SHUTDOWN-001`（automation.stop 抛错后仍走完清理、exit callback 收 0；该条未批准）；D3/D4 ≈ `A7.2-NATIVE-WS-DRAIN-001` + `-R1`（2026-10-31 #1158 人批「全部按默认」：停监听拒新 TCP、旧 WS 继续服务、`server.close()` 等连接）；A14 ≈ `A8.4-NATIVE-HTTP-001`（停机/空闲关闭按 Node 实测复刻）与 `A7.4-HTTPSTORE-KEEPALIVE-001-R3`（`close()` 与 keep-alive 连接生命周期）；§4.4 channel stop 阻塞 ≈ `A8.3-CH-SMS-012-R1`（Node 侧 `stop()` 阻塞在 `server.close()` 上）。`A8.4-NATIVE-HTTP-002-R1` 已声明其范围覆盖 A8.5 切换状态机 #1353。
