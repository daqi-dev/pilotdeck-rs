# exp008：Rust 反代站在高位端口前，真浏览器经它访问 TS ui/server 一切照常

2026-10-14，GitHub #25 / 计划 003 Issue B1，工作分支 `feat/b1-ui-server-proxy`。基线为只读的 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck`（commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`）。

**结论：S1 最小 Rust 反代（HTTP / SSE / NDJSON / 整条 WS）与 S2 四进程编排成立；场景 1–4、6、8、9、10 通过。** 真 Chrome 经 Vite → Rust → TS 渲染出原版 PilotDeck 安装页并完成 register/login；只读 API、一轮 mock 模型对话、SSE/NDJSON 增量透传、直连/反代归一化帧序列、双注册/遮蔽探针均与直连一致；运行时枚举得 **263** 条唯一路由，与静态清单 264 条的差正好是未挂载的 `routes/update.js:112`。**场景 5（PTY）为本机沙箱阻塞，需在沙箱外跑**（见「未做」的哨兵命令）。**r1 补完（2026-10-14）**：`scripts/exp008/compare.py` 直连/反代录放比对（HTTP 7 / WS 11 / SSE 17，differences=[]）＋ 7 条阳性/等价对照全过——改 HTTP body 字段、改状态码、删 WS 帧、改 SSE 事件**每一种都判 FAIL 并指出是哪一处**；场景 7 的续传头/查询参数在反代层透传成立，真实 modules host 端到端续传因无 host fixture 未做（原因见下）。r1 编排 `run.py --only sse-resume,compare,controls` **退出码 0**。**r2 补完（2026-10-14）**：按 codex/GLM 两家评审逐条修——归一化按字段路径限定、头只剥可重算/逐跳并补改头/丢头/Set-Cookie/刷新令牌对照、反代转发上游 WS 端到端握手头（含 `Authorization`）、SSE 多行 data、`13003` 入端口审计、按进程组存活清理；`compare` HTTP 9 / 握手 1 / WS 11 / SSE 17 / close 1、`differences=[]`，`controls` 11/11，见下「r2 补完」。

## Issue #72：更正 WS 握手与 close（2026-10-14）

上述 B1 比较结果是旧尺子的历史结果，不能据此宣称 B2 零差异。B2 round6/round7 在七条 WS 连接上测得仅代理有 Date、客户端先 close 时下行多一条 close。根因是 Hyper 默认序列化补 Date，以及代理转发实际上游 close 回执后又 flush 了 tungstenite 自动排队的下游回执。

本次禁用 Hyper 自动 Date（上游已有 Date 仍原样保留），透传客户端 challenge，使上游 accept 值一致；继续按 B1 剥逐跳头、保留同名多值和端到端头字节、附加 handler 标记。close 两个发起方向都等待并转发另一端真实回执，随后退出而不 flush 发起端自动回执。正常消息处理、大小限制、扩展策略保持原样；现有一秒回执等待预算用于双向收尾。Hyper 仍负责 HTTP 头名大小写和顺序序列化，这里保证的是头集合与值，不是整个头块的原始排列。

最小 Node v22 实测脚本：`scripts/exp008/proxy/tests/node-ws-semantics.cjs <只读基线绝对路径>`，完全使用内存 Duplex、无 listen。实际 `http upgrade → ws.handleUpgrade` 的 101 无 Date；普通 `ServerResponse.writeHead(101)` 则有 Date。两方向、空 close、正常/自定义 code、UTF-8 reason 共八组，均各方向恰好一帧。Rust 内存测试另检验 Hyper 实际输出、重复 Set-Cookie/非 ASCII 头、上游已有 Date、旧双 close 阳性对照、真实回执与发起载荷不同及 EOF 无回执。

本次未跑需要监听的 B2 全量对拍。PR #73 的草稿装置将 proxy 源码计入 apparatus，且 recorder 与 DUT 共用二进制并计入 source；修复后旧 live3 池不再适用于正式定标。协调者应在合入 #72/#73 后固定装置，按同一 UTC 日、同一固定 fixture 路径重新录制 TS 20 池、全新 k=5，先通过 TS 定标再录 proxy；不能用历史重放或修改账本代替。一次性取证、原始输出及可执行协调命令见本 worker `_tmp/triage.md`、`_tmp/RESULT.md`、`_tmp/validate-b2.sh`，清理 worktree 前须归档。

## 编号与端口

- 实验编号 **exp008**（计划 003 里原先写的 `exp006-ui-server-proxy` 已改成 `exp008-ui-server-proxy`；`006_phase-b-ui-server/` 是调研目录名，不改）。之前 `docs/research/006_phase-b-ui-server/route-inventory.md` 里指向 `exp006` 的对账产物路径也一并改成 `exp008`。
- 端口只用本条线段：**Rust 反代 `13001`、TS ui/server `13002`、UI 路由枚举 `13003`、TS gateway `28890`、Vite `15180`**。全程未绑定 3001 / 18789（用户本机 PilotDeck 在跑），未触碰真实 `~/.pilotdeck`。`13003` 是路由枚举自起的 ui/server 监听，同上表的「仅四端口」旧说法已按 r2 更正。

## S0：隔离、回环与进程清理

- 运行产物在 [\_tmp/20261014-exp008/](../../../_tmp/20261014-exp008/)：`home/`（`PILOT_HOME`，四进程统一）、`home/server-token`（0600）、`home/pilotdeck.yaml`、`home/auth.db`、`project/`（夹具）、`tmp/`（`TMPDIR`）、`workspaces/`、`chrome-profile/`（Playwright `userDataDir`，本 worktree 的 `_tmp/` 下）。
- 四进程全部只绑 `127.0.0.1`：[listeners.json](../../../_tmp/20261014-exp008/listeners.json) 用 `lsof -a -p <自有PID> -iTCP -sTCP:LISTEN` 断言每个监听地址恰为 `127.0.0.1:<port>`，无 `0.0.0.0`/`*`。「私有回环端口」由绑定落实，不是路径 + token 的推论。
- 进程由 Python `Popen(start_new_session=True)` 持有自有进程组；退出对自有组发 SIGTERM→SIGKILL，**从不按端口查找或杀进程**。`stop()` 按**自有进程组是否存活**清理，不再因组长已退出就跳过同组子进程（r2）。最终 [audit.json](../../../_tmp/20261014-exp008/audit.json)：全部自有进程停止、自有组全部消失、`cleanupErrors: []`、五端口（含 `13003`）可重新绑定、监听全回环。
- gateway 与 ui/server 共享同一 `PILOT_HOME` 与同一 `server-token`（gateway `PILOTDECK_GATEWAY_TOKEN` / ui `PILOTDECK_GATEWAY_TOKEN_PATH` 都指向 `home/server-token`）；`PILOTDECK_CONFIG_PATH` 指向隔离 `pilotdeck.yaml`；`PILOTDECK_DISABLE_LOCAL_AUTH=0` 打开本地鉴权。未启动 supervisor，用基线 standalone unmanaged 模式；`PILOTDECK_SKIP_BROWSER_OPEN=1`。

## S1：Rust 反代

代码在 [scripts/exp008/proxy/](../../../scripts/exp008/proxy/)（单 bin，独立 crate，`hyper` + `hyper-util` + `tokio-tungstenite`）。**取舍说明**：计划 003 原话是「实验代码住 `_tmp/<日期>-exp006/`，不进 `src/`」。本实现把它放在 `scripts/exp008/proxy/` —— 仍在 B1/B2 允许的 `scripts|docs|specs` 写范围内、**不在 `src/`**，同时让 `run.py` 可复现（`_tmp/` 不入库，随 worktree 清理）。依赖版本对齐基线缓存用 `--offline` 构建，`CARGO_HOME=<worktree>/_build/cargo-home`、`CARGO_TARGET_DIR=<worktree>/_build/exp008-target`。

- **HTTP 全量反代**：method / path / query / 端到端头 / body 经 hyper 流式转发；只去逐跳头（含 `Connection` 点名的头），**不加任何 `X-Forwarded-*`**；上游 `Host` 改成上游 authority。请求/响应体是 `BoxBody` 流，不集中读入内存。
- **SSE / NDJSON 不缓冲**：`Content-Type: text/event-stream`、`application/x-ndjson` 与 `X-Accel-Buffering: no` 原样保留；body 逐块转发。场景 6 用注入的两条测试流实测：首字节 <1ms 到达，终结事件在 ~602ms 后到达（span 601.8/601.9ms），**证明不是读完才返回**。
- **整条 WS 反代**：任意 upgrade 路径（`/ws`、`/shell`、`/plugin-ws/*`）先连上游、成功后才回 101；上游 101 的端到端附加头保留、下游 key 透传上游；应用消息有效载荷保留，close 双向传播（读取另一端真实回执，转发后不再 flush 自动回执，见 Issue #72 更正）。
- **WS 全帧 tee**：每帧落 [ws-frames.jsonl](../../../_tmp/20261014-exp008/ws-frames.jsonl)，含全局单调 `seq`、方向 `c2u`/`u2c`、opcode、`raw`（text 原文，binary/ping/pong base64）、close code/reason。本轮 34 帧（9 上行 / 25 下行，28 text / 6 close）。
- **处理侧标记**：所有经反代的 HTTP 响应与 WS 101 加 `X-PilotDeck-Handler: ts`（B1 全是 ts）。浏览器与直连证据都读它；直连 TS 无此头。这个头是 B2 覆盖检查的抓手。

## S2：四进程编排

编排 [scripts/exp008/run.py](../../../scripts/exp008/run.py)，WS 驱动 [scripts/exp008/driver.mjs](../../../scripts/exp008/driver.mjs)，gateway mock 预载 [scripts/exp008/preload.mjs](../../../scripts/exp008/preload.mjs)，Vite 包装配置 [scripts/exp008/vite.config.mjs](../../../scripts/exp008/vite.config.mjs)，路由枚举 [scripts/exp008/enumerate_hook.mjs](../../../scripts/exp008/enumerate_hook.mjs) + [scripts/exp008/reconcile_routes.py](../../../scripts/exp008/reconcile_routes.py)。

拓扑：**Chrome → Vite :15180 → Rust :13001 → TS ui/server :13002 → TS gateway :28890**；mock 模型挂在 gateway 现有 HTTP listener 的 `/exp008/v1/chat/completions`（无第五个进程、无额外端口，照 exp003 做法）。

- **env 按进程分别构造**（关键，不能像 `launch.mjs` 那样透同一份）：TS ui/server 得 `SERVER_PORT=13002`；**Vite 单独一份 `SERVER_PORT=13001`（= Rust）**，否则 Vite 直连 TS，场景 1–6 的「经 Rust」全部假通过。`VITE_PORT=15180`。
- gateway：`node --import preload.mjs dist/src/cli/pilotdeck.js server`，`PILOTDECK_GATEWAY_PORT=28890`、`PILOTDECK_GATEWAY_HOST=127.0.0.1`。
- ui/server：`node --import preload.mjs --import tsx/dist/loader.mjs ui/server/index.js`，`HOST=127.0.0.1`（基线默认 `0.0.0.0`）、`DATABASE_PATH` 指向隔离 `auth.db`、`PILOTDECK_GATEWAY_URL=ws://127.0.0.1:28890/ws`。
- Rust：`13001 → http://127.0.0.1:13002`。
- Vite：`node <ui>/node_modules/vite/bin/vite.js --config scripts/exp008/vite.config.mjs --configLoader runner`，代理 `/api /ws /shell /memory-dashboard` 指向 Rust 13001。
- **就绪断言**：起完先断言 `home/server-token` 存在且 `0600`；每个进程 `wait_http` 通过后再 `lsof` 断言回环。

## S3：场景与证据

| # | 场景 | 结果 | 证据与判据 |
|---|---|---|---|
| 1 | 静态入口 | **PASS** | [static.json](../../../_tmp/20261014-exp008/static.json)：`GET /` 经 Rust → 302 到 `http://localhost:15180`（`X-PilotDeck-Handler: ts`）；Vite `/` 200、3506 字节 UI 壳；`/memory-dashboard` 经 Rust 401（需鉴权，非 404） |
| 2 | 鉴权（真 Chrome） | **PASS** | [browser.json](../../../_tmp/20261014-exp008/browser.json) + [browser.png](../../../_tmp/20261014-exp008/browser.png)：页面标题 `PilotDeck`、`#root` 非空；`/api/auth/status` `needsSetup:true` → `POST /api/auth/register` 200 且 `registerHandler:ts` → Bearer `/api/auth/user` 200（`exp008`）；15 条 API 响应带 `ts` 标记 |
| 3 | 只读 API 直连/反代一致 | **PASS** | [readonly.json](../../../_tmp/20261014-exp008/readonly.json)：`/api/projects`、`/api/projects/exp008-fixture/sessions`、`/api/config`、`/api/models`、`/api/auth/user` 直连(13002) 与经 Rust(13001) status/body/header（除 `date`、`x-pilotdeck-handler`）完全一致 |
| 4 | 一轮对话（mock 模型，经 Rust `/ws`） | **PASS** | [chat.json](../../../_tmp/20261014-exp008/chat.json) + [ws-frames.jsonl](../../../_tmp/20261014-exp008/ws-frames.jsonl)：驱动经 `:13001/ws?token=` 发 `pilotdeck-command`，10 帧到 `kind=complete`，含 `EXP008_MOCK_OK`，无 error；gateway mock 日志 8 次模型调用 |
| 5 | `/shell` PTY | **需沙箱外跑** | [pty.json](../../../_tmp/20261014-exp008/pty.json)：本机沙箱 node-pty `posix_spawnp failed`（直连与反代同样），哨兵/退出/重连**未验**；`/shell` 的 WS 双向反代已实测（`init` 到 TS、错误 `output` 帧经 Rust 回浏览器）。沙箱外命令见「复现」 |
| 6 | SSE / NDJSON 增量 | **PASS** | [sse.json](../../../_tmp/20261014-exp008/sse.json)：注入的 `/exp008-stream/{sse,ndjson}` 经 Rust，status 200、`text/event-stream` / `application/x-ndjson`、`X-Accel-Buffering: no` 保留，首字节 0.7–0.9ms、终结事件 ~602ms（span 601.8/601.9ms），两事件不丢不并，直连/反代 body 相同 |
| 7 | SSE 续传 `Last-Event-ID`/`afterSeq` | **反代层 PASS；真实路由待 B2** | [sse-resume.json](sse-resume.json)：注入路由四种续传输入直连==反代、从指定 id/seq 之后续传不重放不丢；真实 `GET /api/modules/staffdeck-sdk/events` 两侧同为 501（无 StaffDeck modules host），端到端续传待 B2。 |
| 8 | 直连/反代录放比对 + 阳性对照 | **PASS** | [comparison.json](comparison.json)（HTTP 9 / WS 11 / SSE 17 / WS 握手 1 / WS close 1，differences=[]）＋ [controls.json](controls.json)（11 条全过：改 body 字段/状态码/**改头/丢头/注入 Set-Cookie/注入 X-Refreshed-Token**/删 WS 帧/改 SSE 事件均 FAIL 且指位置，同侧自比 PASS）；旧 [equivalence.json](../../../_tmp/20261014-exp008/equivalence.json) 仍为独立 session 的基线证 |
| 9 | 运行时枚举与静态对账 | **PASS** | [router-enumeration.json](router-enumeration.json) 263 条唯一路由；[route-reconciliation.json](route-reconciliation.json)：`onlyStatic=2`（静态把 `/api/agent` 标 `SSE`，真实注册是 `POST`）、`onlyRuntime=0`；264−263=1 即未挂载的 `routes/update.js:112`（与 `update.js:108` 同路径，去重） |
| 10 | 路由穿透/遮蔽探针 | **PASS** | [shadow.json](../../../_tmp/20261014-exp008/shadow.json)：`/api/projects/files`（须先于 `/api/projects`）、`/api/sessions/model`（须先于 `/api/sessions`）、`/api/mcp/config/read`（被 `/api/mcp/config/:scope` 遮蔽）三条直连与经 Rust 的 status/body 一致且都带 `ts` |

机器原始结果在 [scenarios.json](../../../_tmp/20261014-exp008/scenarios.json)，进程/端口在 [processes.json](../../../_tmp/20261014-exp008/processes.json)，清理审计在 [audit.json](../../../_tmp/20261014-exp008/audit.json)。

## r1 补完（2026-10-14）：比较器 + 阳性对照 + SSE 续传

上一轮只证明了「搬运层透明」，没证明「裁判能抓错」。r1 补上裁判与对照，证据在 [\_tmp/20261014-exp008-r1/](../../../_tmp/20261014-exp008-r1/)；编排 `python3 scripts/exp008/run.py --out _tmp/20261014-exp008-r1 --only sse-resume,compare,controls` 一次跑通、**退出码 0**，清理审计 [audit.json](../../../_tmp/20261014-exp008-r1/audit.json) 同样干净（四监听回环、自有进程组全消失、四端口全部 `released`）。小体积证据已入库到本目录：[comparison.json](comparison.json)、[controls.json](controls.json)、[sse-resume.json](sse-resume.json)。

### 比较器 `scripts/exp008/compare.py`

以下 r1/r2 结果为历史证据。r2 的“按字段路径”表述经 #41 复核仍是按键名递归，首值基准和头自洽检查存在漏判；#41 收紧后的规则见文末。旧录制缺少独立基准和请求时间，须重录才能验收 B2 前置。

- **录制**（`run.py` 的 `compare` 场景）：同一组请求在两侧各录一遍——**直连** TS `ui/server` `13002`，**经反代** Rust `13001`；两侧共用同一 `PILOT_HOME` 与同一 `out/project`，满足 `specs/parity/normalization.md` 的「两侧同一路径」。落 `out/compare/<side>/{http,handshake,ws,sse,close}.jsonl`：HTTP/握手 `{case,method,path,status,headers,body}`、WS 按连接全序帧 `{case,dir,opcode,raw}`、SSE 完整事件序列 `{case,index,sseId,event,data}`、WS close `{case,sentCode,sentReason,observedCode,observedReason}`（`handshake`/`close` 为 r2 新增）。
- **归一化（r2 起按字段路径限定，不再全局抹）**：身份键（`runId`/`sessionId`/`id`/`blockId`/`timeline.turnId`…）里内嵌的 UUID 按**每侧首次出现顺序**映射成 `<uuid:N>`，前缀（`status_`/`text_`/`web:`）保留，引用关系保留；时钟键（`timestamp`/`startedAt`/`lastActivity`…）与进程键（`pid`…）塌缩 `<VOLATILE>`；会话相关计数器（`revision`/`generation`）按同 case 首个值取**相对差**（所以「漏加/多加一次 revision」仍会现形）；**其余一律逐字比**——`used`/`displayUsed`/`ratio`/token 预算不再塌缩，任意业务字符串里的 UUID 不再被替换。`out/project` 绝对路径替换成 `<PATH>`。SSE 事件的 `sseId` 是**续传锚点，不归一化**。
- **头（r2 起只剥可重算/逐跳）**：白名单仅 `date`/`content-length`/`connection`/`keep-alive`/`transfer-encoding`/`x-pilotdeck-handler`；`set-cookie`/`x-refreshed-token`/`etag` 及其余端到端头必须参与比对。`etag` 额外按 body 重算校验（`<ETAG-VALID>`），丢了或改坏都算差异；`x-projects-revision` 是**每次请求现取墙钟**（`projects.js:70-73`），按「正整数形式」归一为 `<REVISION>`，丢失或非数字仍算差异。
- **判决**：结构化 diff，差异带精确位置 `http[<case>] status|header.<name>|body.<path>` / `handshake[<case>] …` / `ws[<case>] #<index>` / `sse[<case>] #<index>` / `close[<case>] #<index>`；PASS 退出 0，FAIL 退出 1。r2 直连 vs 反代：**HTTP 9（含 `/api/projects` 与 `/api/projects/exp008-fixture/sessions` 同快照）/ WS 11 / SSE 17 / 握手 1 / close 1，differences=[]**。
- 只读 HTTP 覆盖 `/health`、`/api/auth/status`、`/api/auth/user`、`/api/config`、`/api/models`、`/api/projects`、项目会话列表、真实 modules 路由。项目/会话为**同快照连读**（`readonly` 场景另断 `sameStatus/sameBody/sameHeaders` 全等，见下）；唯一随请求变的是墙钟 `X-Projects-Revision`，按上一条的形式归一处理，而不是把整个端点排除。

### 阳性对照（[controls.json](controls.json)，11 条全过）

在比对管线里对录制副本注入已知差异，每条都必须 FAIL 且指出位置；同侧自比必须 PASS：

| 对照 | 期望 | 实际 | 指出的位置 |
|---|---|---|---|
| `equivalence:left-vs-right` | PASS | PASS | — |
| `equivalence:left-vs-itself` | PASS | PASS | — |
| `equivalence:right-vs-itself` | PASS | PASS | — |
| `inject:http-body-field` | FAIL | FAIL | `http[health] body.installMode` |
| `inject:http-status-code` | FAIL | FAIL | `http[health] status` |
| `inject:http-header-change`（r2） | FAIL | FAIL | `http[health] header.content-type` |
| `inject:http-header-drop`（r2） | FAIL | FAIL | `http[health] header.etag` |
| `inject:http-set-cookie`（r2） | FAIL | FAIL | `http[health] header.set-cookie` |
| `inject:http-refreshed-token`（r2） | FAIL | FAIL | `http[health] header.x-refreshed-token` |
| `inject:delete-ws-frame` | FAIL | FAIL | `ws[chat-raw] #3` |
| `inject:change-sse-event` | FAIL | FAIL | `sse[sse-incremental] #0` |

注入器刻意避开易变键（否则两个不同值都会被映射成同名占位符而漏判），并断言差异位置包含被改处的确切路径。计划 003 S3 第 8 条里 B2 才做的「交换帧序 / SSE 丢事件 / 并事件 / 攒流到结束才发」四种对照**不在本轮**，留给 B2 的 `controls/`。

### 场景 7：SSE 续传（[sse-resume.json](sse-resume.json)）

- **真实路由探测**：`GET /api/modules/staffdeck-sdk/events?operation=job_events` 带 `Last-Event-ID: 2`，直连与经反代都返回 **501 `SOP_MANAGEMENT_UNAVAILABLE`**、body 一字不差。原因：真实流分支先在 `ui/server/routes/modules.js:334-339` 读 `Last-Event-ID`/`?lastEventId=`/`?afterSeq=`，但要走到流式返回必须先过 `readSopManagement(config)`（`modules.js:568`），要求 `modules.sop.management` 与 `webui.staffdeckCopy` 配好**同一 origin** 的 StaffDeck 正式服务并通过 `/api/auth/me` 与 `/api/auth/me/api-credentials` 校验。本轮隔离环境没有 StaffDeck modules host，故**真实路由端到端续传做不了**——这就是 `realRouteResumable:false` 的确切原因，不是反代丢了头。
- **反代层续传透传（B1 可证部分）**：在 UI app 注入 `/exp008-stream/resume`，按真实路由同样的输入读续传点、**只从该点之后**发事件；直连/反代两侧事件序列逐一相等、无重放无丢：

| 输入 | 续传来源 | 期望/实得 seq |
|---|---|---|
| 无 | `none` | 1,2,3,4,5 |
| `Last-Event-ID: 2` | `header` | 3,4,5 |
| `?lastEventId=4` | `lastEventId` | 5 |
| `?operation=preview_job_events&afterSeq=1` | `afterSeq` | 2,3,4,5 |

即「续传头与查询参数由 Rust 原样转发、事件序列不丢不并不乱序」在 B1 成立；「真实 modules host 上的续传语义」待 B2 有 host fixture 后补。

## r2 补完（2026-10-14）：按两家评审逐条修

两家评审无 P0（codex：6×P2 + 1×P3 及 driver 附注；GLM：1×P1 + 2×P2 + 4×P3）。r2 逐条处理，**PTY 按嘱不重跑**。证据目录：`_tmp/20261014-exp008-r2/`（`readonly,compare,controls`，[audit.json](../../../_tmp/20261014-exp008-r2/audit.json)）与 `_tmp/20261014-exp008-r2-extra/`（`equivalence,routes,sse-resume`）；两次均**退出码 0**、五监听全回环、五端口全 `released`、`cleanupErrors:[]`。小体积证据已回填本目录。

| 评审项 | 落点 | r2 处理 |
|---|---|---|
| **P1** 头白名单把 `set-cookie`/`x-refreshed-token`/`etag`/`x-request-id` 一并剥掉 | `compare.py` | 白名单只留 `date`/`content-length`/`connection`/`keep-alive`/`transfer-encoding`/`x-pilotdeck-handler`；其余端到端头全部参与比对。`etag` 按 body 重算 sha1 校验（`<ETAG-VALID>`），丢失/改坏都判差异；补改头、丢头、注入 Set-Cookie、注入 X-Refreshed-Token 四条阳性对照 |
| **P2** 全局塌缩 `revision`/用量/`token`、任意字符串 UUID 被替换 | `compare.py` | 归一化按字段路径限定：身份键内嵌 UUID 逐侧首现映射（保留前缀）；时钟/进程键塌缩；`revision`/`generation` 取同 case **相对差**；`used`/`displayUsed`/`ratio`/token 预算**逐字比**；非身份字符串里的 UUID 不替换 |
| **P2** 上游 WS 只用 URL 建连，丢 `Authorization` | `proxy/src/main.rs` | `ClientRequestBuilder` 转发下游端到端握手头（`Authorization`/`Origin`/`Cookie`/子协议…），只跳过逐跳与 WS 自身握手头；新增 `handshake.jsonl` 的 `ws-header-auth` 用例：固定 `Sec-WebSocket-Key`，两侧都须 101（丢头会变 401/502） |
| **P2** 多行 SSE `data` 被覆盖，前行被改/丢仍 PASS | `run.py` | `parse_sse_events` 按规范把多条 `data:` 用 `\n` 拼接，并先把 CRLF/CR 归一为 LF |
| **P2** 组长已退出就跳过清理，同组子进程残留 | `run.py` | `stop()` 改按**自有进程组是否存活**发 SIGTERM→SIGKILL；组仍在则记 `cleanupErrors`，不再因组长先退就跳过 |
| **P2** 排除 `/api/projects`、算了头差却不断言 | `run.py` | `HTTP_CASES` 恢复 `/api/projects` 与项目会话同快照连读；`readonly` 用比较器同款头规则断言 `sameStatus`/`sameBody`/`sameHeaders`，并记录两侧 revision |
| **P2** 枚举进程实际监听 `13003`，不在端口审计/回环断言 | `run.py` | `PORTS` 增 `enum:13003`（入 `ALL_PORTS` 与 released 审计），枚举起后 `lsof` 断言 `127.0.0.1:13003` 且回环 |
| **P3** 序号在锁外自增、同步写阻塞运行线程 | `proxy/src/main.rs` | 单一日志写线程持有文件并在出队时统一编号，`tee` 只入队（先入先写，磁盘顺序与 `seq` 一致） |
| **P3** 空闲链路的自动 Pong 只在关闭分支 flush | `proxy/src/main.rs` | 每轮 relay 后 flush 上下游两侧 |
| **P3** `equivalence` 自带一份 scrub，双轨归一 | `run.py` | 删掉本地 scrub，改调 `compare.compare_sides`；会话 id 改 UUID 供身份映射 |
| **P3** `BASELINE` 绝对路径硬编码、`RUST` 无自建步骤 | `run.py` | `EXP008_BASELINE_ROOT` 可覆盖默认基线根；`ensure_proxy_built()` 在二进制缺失时用 `CARGO_HOME`/`CARGO_TARGET_DIR` 自动 `cargo build --offline` |
| codex 附注：driver 在 close 前落盘，无双向关闭码/原因对照 | `driver.mjs`/`compare.py` | 新增 `ws-close` 驱动模式与 `close.jsonl` 通道：记录发出的 code/reason 与对端回执，两侧逐字比（实测均 `4001 / exp008-close`，`clean:true`） |

**一处有意取舍**：`etag` 与 `x-projects-revision` 不是逐字相等，而是按「可重算/每次请求现取墙钟」校验——前者是 body 的 sha1（body 含时钟字段时两侧合法不同），后者是 `Date.now()`。判据是「存在且自洽」：丢了、改坏、非数字都判差异。其余头（含 `set-cookie`/`x-refreshed-token`）逐字比。

**r2 结果**：`compare` HTTP **9** / 握手 **1** / WS **11** / SSE **17** / close **1**，`differences=[]`；报告的 `normalisation` 块自述覆盖率（剥哪些头、塌缩/身份/相对键各是什么、其余逐字比）；`controls` **11/11**（改头、丢头、注入 Set-Cookie、注入 X-Refreshed-Token 等）；`readonly` 5 条 `sameStatus/sameBody/sameHeaders` 全真；`equivalence` 复用比较器后 10 帧 PASS；`routes` 仍 263（`static=264`，差 1 未挂载）且 `13003` 回环断言与 released 通过；`sse-resume` 四条续传用例 PASS、真实路由仍两侧同 501。`cargo fmt --check`、`clippy -D warnings` 全过。

## S3：运行时路由枚举（分母对账）

- 做法：`node --import enumerate_hook.mjs --import tsx ui/server/index.js`（隔离 env，`SERVER_PORT=13003`）。hook 在 `http.createServer(app)` 处接住 app，模块加载完 8s 后递归 dump `app._router.stack`：`layer.route` 取 `.path` + `.methods`，挂载 router 递归进 `layer.handle.stack`，挂载前缀从 `layer.regexp` 还原（Express 4 对普通 Router 不设 `mountpath`）。
- 结果：**263 条唯一 (method, path)**；静态清单 **264** 条注册，差 1 = `routes/update.js:112` 的工厂路由未挂载（且与已挂载 `update.js:108` 同路径，去重后仍 263）。逐条对账 `onlyStatic=2`（上表的 `SSE` 标签行）、`onlyRuntime=0`。
- 已回填 [006/route-inventory.md](../../research/006_phase-b-ui-server/route-inventory.md) §10。对账产物已入库到本目录：[router-enumeration.json](router-enumeration.json)（263 条完整清单）、[route-reconciliation.json](route-reconciliation.json)；运行时的完整快照另见 [\_tmp/20261014-exp008/](../../../_tmp/20261014-exp008/)。

## 意外

1. **Express 4.22 的 Router 实例原型 ≠ `express.Router.prototype`**：`Object.getPrototypeOf(express.Router()) !== express.Router.prototype`，因此加载期 hook 包 `Router.prototype.use/.route` **一条都拦不到**（首版 0 命中）。改用 stack dump + `layer.regexp` 还原前缀后得 263 条。已回填 `006/gap.md` 4.1；B2 的 `enumerate_routes.mjs` 应以 stack dump 为准。
2. **基线树只读，Vite dev 默认写不进去**：Vite 7 会在 config 所在 `node_modules` 下写 `.vite-temp`、在 `cacheDir` 写 `.vite/deps_temp_*`，直接起报 EPERM。用 `--config scripts/exp008/vite.config.mjs --configLoader runner` 包装基线配置：`runner` 避免 config bundling 的 `.vite-temp`，包装里把 `cacheDir` 指到 `_tmp/` 下，基线 `vite.config.js` 一行不改。已回填 `006/gap.md` 4.2。
3. **沙箱内 node-pty 起不来**：`posix_spawnp failed`，与代理无关（直连同样）。`/shell` 只能验到「WS 双向搬运」。已回填 `006/gap.md` 4.3；PTY 哨兵/退出/重连需换能起 PTY 的环境。
4. **真浏览器没看到应用自己的 `/ws`**：Playwright 的 `websocket` 事件只捕到 Vite HMR socket（`browser.json` `appWebSockets: []`）。UI 是按需开 WS，加载安装页时不开。`/ws` 的全帧对拍改由 `driver.mjs` 经 Rust 完成，Rust tee 记录原帧。已回填 `006/gap.md` 4.4。
5. **静态清单里的「SSE」不是 HTTP 方法**：`/api/agent` 的流式分支在静态表里记作 `SSE`，真实注册是 `POST`；对账脚本需按方法标签兼容，否则会造两条假 `only-static`。

## 未做 / 限度

- **PTY 哨兵/退出/重连**：本机沙箱 node-pty `posix_spawnp failed`，**需在沙箱外跑**（`/shell` 的 WS 双向反代已在沙箱内验过）。确切命令（先按「复现」起好四进程，再只跑 pty 场景）：`python3 scripts/exp008/run.py --only pty`；若要单独留证据目录：`python3 scripts/exp008/run.py --out _tmp/20261014-exp008-pty --only pty`。脚本会断言固定命令输出哨兵（`printf 'PDSENTINEL_%s\n'` 由 shell 拼出整串，回显造不出它）、`resize`、`exit`/close 传播与 `terminalSessions` 重连复用。**不要再在沙箱内重试。**
- **真实 modules host 的 SSE 续传语义**：`realRouteResumable:false`（两侧同为 501）。需配置 StaffDeck `modules.sop.management` + `webui.staffdeckCopy`，并起一个能回 `/api/auth/me`、`/api/auth/me/api-credentials` 及流式 operation 的 host fixture，属 B2。
- **计划 003 B2 的其余阳性对照**：交换同一连接两帧顺序 / SSE 丢一个事件 / 并两个事件 / 把整条流攒到结束才发 / 截断 SSE——本轮只做了任务要求的四条（改 body 字段、改状态码、删 WS 帧、改 SSE 事件），其余留 B2 的 `controls/`。
- **SSE 到达延迟上界口径（`k × 上游间隔 + m`）**：未冻结；B1 只做「首字节不缓冲 + 终结事件 ~602ms 后到」的定性证明。阈值留给 B2（且按规矩不看 Rust 结果定）。
- **运行时枚举对账是粗对账**：静态 264 的口径含 4 条链式与 1 条未挂载；脚本按 markdown 行解析，`onlyStatic` 的归类对格式敏感，已在 readme 说明。
- **未做**：断线重连 watch、配置写+广播、上传+SSE、`ui/e2e` 前端回归门（均属 B2 场景集）；未改基线任何文件；未动 `ui/src`、`apps/desktop`、`specs/`。

## Rust 校验

独立 crate 在 `scripts/exp008/proxy/`，本轮 `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全过（0 测试）。它不进 `src/` workspace，所以不影响 `src/` 的 CI。原始输出：

```sh
export CARGO_HOME="$PWD/_build/cargo-home"
export CARGO_TARGET_DIR="$PWD/_build/exp008-target"
cargo build  --offline --manifest-path scripts/exp008/proxy/Cargo.toml
cargo fmt --manifest-path scripts/exp008/proxy/Cargo.toml -- --check
cargo clippy --offline --manifest-path scripts/exp008/proxy/Cargo.toml --all-targets -- -D warnings
cargo test  --offline --manifest-path scripts/exp008/proxy/Cargo.toml
```

## 复现

```sh
export CARGO_HOME="$PWD/_build/cargo-home"
export CARGO_TARGET_DIR="$PWD/_build/exp008-target"
cargo build --offline --manifest-path scripts/exp008/proxy/Cargo.toml
# 默认 out=_tmp/20261014-exp008；已有同名目录会自动归档为 -attempt-N，不覆盖。
python3 scripts/exp008/run.py
# 只跑一部分：--only static,browser,readonly,chat
# 预期整体 exit 0（pty 为「需沙箱外跑」，沙箱内只验 /shell 的 WS 双向反代）。
```

r1 的比较器/阳性对照/续传（产物不覆盖上一轮，单独落目录）：

```sh
python3 scripts/exp008/run.py --out _tmp/20261014-exp008-r1 --only sse-resume,compare,controls
# 等价单独重判已有录制（不再起进程）：
python3 scripts/exp008/compare.py _tmp/20261014-exp008-r1/compare/direct \
    _tmp/20261014-exp008-r1/compare/proxy --out /tmp/comparison.json \
    --path "$PWD/_tmp/20261014-exp008-r1/project"
python3 scripts/exp008/compare.py _tmp/20261014-exp008-r1/compare/direct \
    _tmp/20261014-exp008-r1/compare/proxy --controls --out /tmp/controls.json \
    --path "$PWD/_tmp/20261014-exp008-r1/project"
```

r2 的评审修复（`compare`/`controls` 为必跑；`readonly` 验头断言与同快照项目用例，`routes` 验 `13003` 回环，`equivalence` 验比较器复用）：

```sh
python3 scripts/exp008/run.py --out _tmp/20261014-exp008-r2 --only readonly,compare,controls
python3 scripts/exp008/run.py --out _tmp/20261014-exp008-r2-extra --only equivalence,routes,sse-resume
```

PTY（**只在能起 node-pty 的沙箱外环境跑**，不要在本沙箱重试）：

```sh
python3 scripts/exp008/run.py --out _tmp/20261014-exp008-pty --only pty
```

浏览器用本机已装 Chrome（Playwright `channel: 'chrome'`，`userDataDir=_tmp/20261014-exp008/chrome-profile`），**不下载浏览器、不用用户自己的 Chrome 配置**。

## 对架构的影响

- 「Rust 前置逐路由接管、未接管整条反代、浏览器 `ui/src` 一行不改」在探针层面站住了：HTTP / SSE / NDJSON / WS 都能字节透传，处理侧标记可用，分母 263 与静态对账吻合。
- 三个需要在 B2 先解决的环境/装置问题：Express hook 不可靠（改 stack dump）、Vite 缓存需可写目录、PTY 需可起进程的环境。
- r1 后：比较器能判差异并指位置（4 类注入必 FAIL、同侧自比 PASS），续传头/查询参数透传成立；仍未验的是 B2 的换序/丢事件/并事件/攒流对照与真实 modules host 上的续传语义。
- r2 后：比较器归一化按字段路径限定（不再抹 revision/用量/token/任意 UUID），头只剥可重算/逐跳且补了改头/丢头对照；上游 WS 转发端到端握手头（头鉴权用例成立）；SSE 多行 data、`13003` 端口审计、按进程组清理均已补齐。

## 协调者补跑：场景 5 PTY（沙箱外，2026-10-14 04:45）

`python3 scripts/exp008/run.py --out _tmp/20261014-exp008-pty --only pty` → exit 0：`{'status': 'PASS', 'sentinel': 'PDSENTINEL_58db9e1e7ca1', 'proxyOutputBytes': 446}`；端口 13001 / 13002 / 28890 / 15180 无残留。**只验了哨兵**（哨兵字节不在输入文本里），计划 B1 S3 第 5 条的「`exit` 与重连」本轮未验，留给 B2。


## #41：B2 前置收紧（2026-10-14，本地规则层已验，服务重跑待协调者）

- `compare.py` 只对明确 channel、事件/方向、完整字段路径归一；数组索引也参与路径。未知 HTTP/WS/SSE payload、错误嵌套位置的身份/时钟、timeline revision 与 generation 都按原值比。`GET /health` 仅归一顶层合法 ISO 时间、正整数 pid 和 instanceId；已知 WS kind/event 仅归一列出的身份和 timestamp。
- `session-activity.activity.revision` 使用驱动在发 command 前请求的 `session-activity-snapshot`，验证活动已结束、revision 为唯一正整数，取 snapshot 最大值作独立基准；只有同 case/session 的后续活动允许比较增量。无基准比较绝对值；显式坏基准、不前进的 revision 直接 FAIL。此规则适用于 exp008 隔离的单用户、无并发夹具，B2 若扩大场景应提供相应独立基准，不能用被比较的首值作基准。`record_ws` 与 `equivalence` 均保留基准和发送帧。
- 相同 wire body 的 ETag 逐字比较；不同 body 的 ETag 必须各自匹配 Express SHA1 body hash，保留 strong/weak 属性。只有 body 的结构比较也通过时，合法不同 digest 才不会产生差异。
- `x-projects-revision` 仅对成功 `GET /api/projects` 允许墙钟归一；ASCII 正整数、落在记录的请求起止时间范围内（上界最多允许 1000ms 的 `max(Date.now(),previous+1)` 计数领先），并要求 proxy revision ≥ direct revision。缺时间/丢头/Unicode/超范围/旧快照均 FAIL；readonly 有同样的范围检查及显式顺序断言。
- WS 上游请求克隆头并复用 `strip_hop_headers`，剥全部 Connection 提名头与 Proxy-Authorization 等，再生成握手；Authorization 与子协议保留。日志采用容量 8 的异步背压队列，先预留队列空间再编码 payload，单写线程按最多 8 条批量写入/flush，写失败使 relay 失败。
- `controls.py` 为无监听离线对照，`compare.py --controls` 自动包含新增对照及 Rust 阳性单测；`--before` 可载入旧比较器验证“旧 PASS → 新 FAIL”。Rust 单测实际构造旧静态转发和旧无界队列，再验证违规注入被新断言拒绝，合规行为通过。

协调者在沙箱外重录（不要用旧 r2 录制补造基准）：

```sh
cd /Users/da/ws/pilotdeck-rs/_build/wt-b2pre
export CARGO_HOME="$PWD/_build/cargo-home"
export CARGO_TARGET_DIR="$PWD/_build/exp008-target"
cargo build --locked --offline --manifest-path scripts/exp008/proxy/Cargo.toml
python3 scripts/exp008/run.py --out _tmp/20261014-exp008-issue41 --only readonly,compare,controls,equivalence,sse-resume,routes
```

本地原始输出与边界见本 worktree `_tmp/RESULT.md`；本次没有更新既有 `comparison.json`/`controls.json`，它们仍是 r2 历史结果。没有启动服务、修改冻结尺子、提交或修改 state。

### #41 收紧后协调者重录（2026-10-14 上午，沙箱外）
`run.py --only readonly,compare,controls,equivalence,sse-resume,routes`（隔离端口 13001/13002/13003/15180/28890，只绑 127.0.0.1）：compare **PASS、0 差异**；controls **46/46**（含 #41 新增的 35 条旧行为假通过对照）；equivalence 11 帧 PASS；sse-resume 四种续传 PASS；routes 运行时 263 / 静态 264。审计：监听全回环、五端口 released、自有进程组全部消失、cleanupErrors=[]。
