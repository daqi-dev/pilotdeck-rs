# exp003：真 ui/server 经 Rust 透明代理连接 TS gateway

2026-10-14，GitHub #15 / 计划 002 Issue A1，工作分支 `feat/a1-exp003-proxy`。基线为只读的 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck`，commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。

**结论：透明代理的连接、只读面、文本轮次、HTTP 面及等价帧对照通过；真实 ui/server 的自动交互重放失败，不能宣布七项全绿。** 保存旧 binding 的协议驱动在直连和代理两侧都成功重连、取回待决问题及 active snapshot；真实 bridge 在连接关闭后丢失旧 binding，未发出恢复请求。该失败需要后续单独修复基线客户端/bridge，A1 不修改 `resources/`。整体编排因此有意返回退出码 1。

## S0：隔离与边界

唯一监听拓扑是 `3301 ui/server → 28789 Rust proxy → 28790 TS gateway`，所有服务设置 `HOST=127.0.0.1`，TS gateway 显式设置 `PILOTDECK_GATEWAY_HOST=127.0.0.1`；Rust 代理拒绝任何非 `127.0.0.1` 的监听配置。**上一轮 UI 曾绑 0.0.0.0，已用 127.0.0.1 重跑。** 最终 R1 于北京时间 2026-10-14 03:59:01–03:59:38 执行，编排使用 `lsof` 按自有 PID 断言实际监听地址，仅允许指定的一个回环端口；[listeners.json](../../../_tmp/20261014-exp003/listeners.json) 中 7 次监听检查全部通过（含代理重启与两轮等价对照）。没有连接或绑定 3001 / 18789，没有操作用户的 PilotDeck 进程或 `~/.pilotdeck`。

本轮先每 60 秒检查协调者的 `/Users/da/ws/pilotdeck-rs/_tmp/ruler-snap/progress.txt`，上限 40 分钟；03:41:28 开始等待，03:55:28 第 15 次检查出现 `DONE`，实际等待约 840 秒。之后才启动高负载 Rust 校验和实验；原始等待记录在 [ruler-wait-r1.json](../../../_tmp/ruler-wait-r1.json)，最终运行时间与退出码在 [run-r1.json](../../../_tmp/20261014-exp003/run-r1.json)。

本轮实验运行产物位于本 worktree 的 [_tmp/20261014-exp003/](../../../_tmp/20261014-exp003/)：主实验 `home/`，等价对照 `home-direct/` / `home-proxy/`，项目夹具 `project/`，General 工作区 `workspaces/general/`，`TMPDIR=tmp/`，SQLite 鉴权库在各隔离 home 内。环境与 `pilotdeck.yaml` 的 `webui.runtime` 同时显式设置 host、serverPort、databasePath、workspacesRoot，防止基线 YAML 默认值覆盖工作区环境变量。**先写 `home*/server-token`（0600），再启动进程**；gateway 的显式 token 和 bridge 的 `PILOTDECK_GATEWAY_TOKEN_PATH` 指向同一份隔离 token。

`DISABLE_LOCAL_AUTH=1` 与基线实际识别的 `PILOTDECK_DISABLE_LOCAL_AUTH=1` 均显式设置；`DATABASE_PATH` 指向隔离 SQLite 文件。未启动 supervisor；使用基线的 standalone unmanaged 模式。`PILOTDECK_SKIP_BROWSER_OPEN=1`。按本任务约定，以 Node 直接请求 ui/server 的 HTTP 与 `/ws` 替代浏览器。

进程由 Python `Popen(start_new_session=True)` 持有句柄和自有进程组；退出时对自有组发 SIGTERM，再以 SIGKILL 清理剩余孙进程。清理超时/系统错误记入审计，不从 finally 覆盖原始场景失败；不会按端口查找并杀进程。最终轮的完整 PID、进程组、命令、端口、退出码在 [processes.json](../../../_tmp/20261014-exp003/processes.json)。前轮证据保存在 `attempt-07-before-r1/`，R1 的 TIME_WAIT 预检失败与 YAML 工作区默认值诊断保存在 `attempt-08-r1-preflight/`、`attempt-09-r1-workspace/`；更早尝试在 `attempt-01/` 至 `attempt-06/`。最终 [audit.json](../../../_tmp/20261014-exp003/audit.json) 确认全部自有进程停止、全部自有组消失、cleanupErrors 为空、监听全部回环、三个端口可重新绑定。端口预检/收尾探测使用与服务一致的 SO_REUSEADDR，避免把 TCP TIME_WAIT 误判为活跃监听。

## S1：代理实现

代码只存在于 [src/pilotdeck-gateway/](../../../src/pilotdeck-gateway/)，已加入 `src/` workspace；没有 `_tmp` 源码副本，没有本地业务 method。crate 继承 `rust-version.workspace` 与 workspace lints（含 `unsafe_code=forbid`）。直接依赖全部使用 `=版本`，传递依赖由 `src/Cargo.lock` 锁定。非致命 accept 错误（包括 EMFILE / ECONNABORTED）记录后以 100ms 间隔重试；无效监听操作或权限错误才终止监听。

HTTP/1 请求的方法、path/query、端到端头、请求体与响应状态/头/体经 hyper 全量反代；仅调整上游 Host、去除 hop-by-hop 头。请求与响应体流式转发，不集中读入内存。`/health` 也向 TS 上游转发。`/ws` 为一个下游连接建立一个上游连接，上游连通后才返回 101；透传上游 101 的端到端附加头，按下游 key 重算 `Sec-WebSocket-Accept`。缺 key、key 非 16 字节 base64 nonce、错误 method/version/Connection 等客户端握手错误返回 400，仅上游连接/握手失败返回 502。text/binary payload 不解析、不改写，hello / hello_ok 与 TS 下发的 binding 都原文传递。两侧统一限制单帧 4 MiB、完整消息 16 MiB（包含分片累积）；超过边界会断开连接，不宣称上限之外等价。

任何一侧 EOF/错误均结束配对、释放另一侧 socket。上游主动 close 的 code/reason 保留。客户端 close 时等待最多 1 秒取回 TS 的真实 close reply；等待期间先按序转发上游在途 text/binary/control 帧，再写回其未掩码 close 帧，避免 tungstenite 自动回显客户端 close 与基线“总回 1000”的行为冲突。这个分支有针对空 close / 4444 close 及在途 text/binary/pong 顺序的回归测试。

帧日志由 [preload.mjs](../../../scripts/exp003/preload.mjs) 在实际客户端侧录：真实 bridge 使用 Node 原生 WebSocket 包装器，独立驱动也使用同一包装器。每行包含全局单调 `seq`、`monotonicNs`、连接编号、方向、URL、原文 `raw` 和解析后的完整 `frame`；close code/reason 也记录。Rust 被测侧只输出诊断日志，不生成比对数据。包装器保留实际 send / message / close 行为，不替客户端保存或修复 binding。

## S2：启动与 mock

编排是 [run.py](../../../scripts/exp003/run.py)，驱动是 [driver.mjs](../../../scripts/exp003/driver.mjs)，比较器是 [compare.py](../../../scripts/exp003/compare.py)。

TS 使用已构建的 `dist/src/cli/pilotdeck.js server`。真实 `ui/server/index.js` 通过基线已有的 tsx loader 启动。provider 配置使用 OpenAI 协议，指向 `http://127.0.0.1:28790/exp003/v1`；preload 在 **TS gateway 现有 HTTP listener** 上增加测试专用 mock route，没有第四个端口或模型服务进程，没有修改基线文件。模型 HTTP 请求、SSE 解码及原生 agent/tool 运行均走基线实现，响应固定为 `EXP003_MOCK_OK`；`EXP003_RECONNECT` 会调用 `ask_user_question`，形成真正的待决交互。模型请求/响应见 `main-model.jsonl`、`direct-model.jsonl`、`proxy-model.jsonl`。

沙箱原生文件 watcher 的运行限制与替代方式见“意外”第 4 条。最终轮对两侧同样设置 `EXP003_POLL_WATCH=1`，这不是扩展/config 热加载的验收。

复现命令（从本 worktree 根目录运行）：

```sh
export CARGO_HOME="$PWD/_build/cargo-home"
cargo build --manifest-path src/Cargo.toml -p pilotdeck-gateway
python3 scripts/exp003/run.py
# 预期整体 exit 1：真实 UI 自动重放场景仍 FAIL。
# 脚本拒绝覆盖已有 manifest；重跑前将前轮运行产物归档在同一隔离目录内。
```

## S3：七个场景与证据

| # | 场景 | 结果 | 证据与判据 |
|---|---|---|---|
| 1 | 真实 bridge 连通 | **PASS** | [ui.log](../../../_tmp/20261014-exp003/ui.log) 出现 `connected → ws://127.0.0.1:28789/ws`；[bridge-frames.jsonl](../../../_tmp/20261014-exp003/bridge-frames.jsonl) seq 1/2 为 hello/hello_ok，含 TS binding；重连后的第二个 hello_ok 为 seq 45。 |
| 2 | 项目、会话、历史只读面 | **PASS** | [ui.json](../../../_tmp/20261014-exp003/ui.json) 中项目列表、会话列表、历史均 HTTP 200，包含真实项目、`web:exp003-chat` 和持久化回复。bridge 录到 `list_projects`、`list_sessions`、`describe_project`、`read_session_messages`。注意后三者中有会话运行态 method；没有误称它们都属于冻结 (a) 类。 |
| 3 | 一轮文本对话 | **PASS** | 同一 `ui.json` 记录 14 条浏览器侧 UI 帧，含 `EXP003_MOCK_OK` 文本及 `kind=complete`；[ui-frames.jsonl](../../../_tmp/20261014-exp003/ui-frames.jsonl) 记录浏览器侧原帧。bridge seq 11 发 submit_turn，seq 20 为 `seq=8, final=true, event.type=turn_completed`。未出现 UI error。 |
| 4 | 杀代理、退避重连、自动重放 | **FAIL（基线 bridge）** | 自有 proxy PID 49858 被终止，PID 49895 在同一 28789 端口重启；约 1.764s 后替代代理就绪，约 9.969s 后驱动结束。两个时长与 observedMethods 在 [reconnect-process.json](../../../_tmp/20261014-exp003/reconnect-process.json) 中只写一次，FAIL 路径也保留。bridge 录到 close 1006 与新的 hello_ok，ui.log 记录断线及恢复连接；[reconnect.json](../../../_tmp/20261014-exp003/reconnect.json) 和 [reconnect-frames.jsonl](../../../_tmp/20261014-exp003/reconnect-frames.jsonl) 记录 pending question、gateway_unavailable，以及 `timeout: interaction replay`。真实 bridge 的全量录帧里没有 `reconnect_interaction` / `active_turn_snapshot` 请求。 |
| 5 | HTTP 全量反代 | **PASS** | [http.json](../../../_tmp/20261014-exp003/http.json)：`/health`、`/auth/local-token`、`/api/web/projects`、带 Bearer + principal 的 POST `/api/module-host/describe`、文件读取，直连/代理的 status、body、content-type 一致且均 200；文件写入 POST 的 body 完整落盘；无 Bearer 返回 401。测试专用 SSE/NDJSON 路由的两段数据间隔约 501–503ms（判据 ≥350ms），证明增量流式转发而非读完才返回。 |
| 6 | 同驱动直连/代理等价 | **PASS** | [direct-frames.jsonl](../../../_tmp/20261014-exp003/direct-frames.jsonl) vs [proxy-frames.jsonl](../../../_tmp/20261014-exp003/proxy-frames.jsonl)，各 19 条：hello、hello_ok、三个只读 method、完整 submit_turn 流及 close。官方 `GatewayWsClient`、同一场景、两个干净 home；[comparison.json](../../../_tmp/20261014-exp003/comparison.json) 为 `PASS, differences=[]`，包含正常关闭码 1000。 |
| 7 | 阳性失败对照 | **PASS（比较器正确判 FAIL）** | 从成功的代理侧日志复制出独立对照记录，将 hello_ok 的 `serverVersion` 改为 `INJECTED_KNOWN_DIFFERENCE`，并同步更新 raw；实际 TS 与代理未被改动。[positive-frames.jsonl](../../../_tmp/20261014-exp003/positive-frames.jsonl)、[positive.json](../../../_tmp/20261014-exp003/positive.json)、[positive.log](../../../_tmp/20261014-exp003/positive.log) 显示 **FAIL、exit 1、index=1 唯一差异**。 |

机器原始运行结果在 [scenarios.json](../../../_tmp/20261014-exp003/scenarios.json) 和 [orchestrator.log](../../../_tmp/20261014-exp003/orchestrator.log)。1–3 共用一次真实 UI 驱动，编排中以 `S2-S3-ui` 聚合记录；上表分开给出各自的证据。

额外关闭码对照见 [close.json](../../../_tmp/20261014-exp003/close.json)：`hello_required=4001`、`protocol_mismatch=4001`、`auth_failed=4003`、`invalid_json=4002`；客户端空 close 与 4444 close 都取回 TS 的 `1000, reason=""`。

## 重连失败对架构的影响

基线 `src/gateway/client/GatewayWsClient.ts:93-100` 在 close 回调内先清空 `hello`，再调用 disconnect handlers；`RemoteGateway.ts:111-112` 的 binding getter 直接委托 client。因此 `ui/server/pilotdeck-bridge.js:255-258` 的 `onInvalidated` 与 `:2127-2130` 的 catch 路径已经拿不到退休 binding。后者将 `awaitingGatewayReconnect` 设为 false，最终清理本地 active 状态，重连成功后没有恢复候选。

为区分“代理状态丢了”和“真实 UI 没发恢复请求”，同一个官方客户端补充驱动显式在关闭前保存 binding：直连结果在 [protocol-direct.json](../../../_tmp/20261014-exp003/protocol-direct.json)，代理结果在 [protocol-proxy.json](../../../_tmp/20261014-exp003/protocol-proxy.json)。两侧均 `outcome=reconnected`、返回原 pending requestId、`active=true`，说明 TS 的待决状态经连接 1:1 断线后仍可恢复。两侧后续 `elicitation_respond` 均返回 `delivered=false`；driver JSON 与 scenarios.json 均明确标记 **PASS-with-note** 并附基线限制，未把“恢复请求成功”宣称为“恢复轮次完成”。全量驱动帧在对应的 `protocol-*-frames.jsonl`。

**架构判断：保留 Rust 前置、TS 持有 binding/交互运行时的方案；自动 UI 重放仍是 P0 未闭合。** A1 的实验与实现已交付，不能把真实 UI 恢复缺陷通过改驱动、抹帧或改 baseline 变绿。

## 比较口径与限度

比较器保留连接内完整记录的顺序、direction、所有语义字段、event.seq、final 与 close code/reason；验证 recorder.seq 严格递增、raw 与 parsed frame 一致，空日志不能 PASS。非 JSON raw、格式损坏、重复序号或 raw/parsed 不一致会产出带 side/line/error 的 FAIL 差异并 exit 1，不再抛裸异常或依赖 assert；即使两侧同样损坏也不能 PASS。只对 UUID 做一致首次出现映射、binding generation 做一致映射、显式时间字段归一化。项目路径在两轮完全相同，未通过删路径来消掉差异。两轮隔离 home 和 port/URL、录制单调时钟不作为语义帧字段比较。

这是 **A1 临时口径**，没有修改或冻结 `specs/parity/`，也不是 A2 的全量帧/落盘/HTTP 三面尺子。阳性对照只覆盖本任务要求的一处已知帧差异；SSE/NDJSON 是独立驱动证据，不混进 WS 帧序列。

## 意外

1. **真实自动重放与源码推断不一致**：gateway 重连成功，但旧 binding 在 bridge 获取前已丢失，恢复请求根本没有发出。另有两侧一致的 post-reconnect answer `delivered=false`，尚未在本 issue 内修复/定因。
2. **标准库 close 自动应答不是透明搬运**：初版配对在 client close 后立即 drop，实际 1000 变 1006（`attempt-05/comparison.json`）；仅 flush 自动应答又变为 1005（`attempt-06/comparison.json`）。最终改为读 TS 真正的 close reply 再写回，保持 close code 为比较字段，并增加空/custom close 测试。没有把差异归一化掉。
3. **单有目录不算注册项目**：初轮存在 `project/` 却返回 `Unknown projectKey`，需 home 下注册目录的 `.cwd` 标记；General 的 cwd 默认取 `~/general`，需显式 `WORKSPACES_ROOT`。初轮 mkdir 被沙箱拒绝，未成功创建用户目录。R1 首次回环重跑（attempt-09-r1-workspace）再次暴露 UI 的 YAML 默认 `workspacesRoot=os.homedir()` 会覆盖环境变量，已将隔离路径补进 `webui.runtime` 并重新执行全部场景；最终轮 General 目录实际存在于隔离 `workspaces/general/`，UI 日志不再出现该 mkdir 失败。
4. **沙箱 fs.watch 会触发 gateway 静默退出**：即使文件描述符上限为 1048576，native watcher 报 EMFILE；CLI 的 uncaughtException handler 只记遥测并 exit 1。`attempt-04/gateway-direct.log` 的监听前置诊断确认了异常。最终 preload 对两侧同样显式替换为浅层 stat 轮询、使用相同固定夹具；不覆盖递归扩展热加载。`attempt-03` 已留下未替换 watcher 时的 UI 正常轮次、HTTP 通过及自动重放失败证据。
5. **fetch mock 会被 CLI 初始化覆盖**：首个 preload global-fetch 替身未拦到真实请求，失败记录在 `attempt-02/`。最终改为同 listener 的真实 HTTP mock provider。`/api/module-host/describe` 还必须 POST 并提供 principal；protocol_mismatch 对应 4001，不是 4002。
6. **UI 生产前端未构建**：启动日志明确提示未发现生产 frontend build。任务指定的 HTTP/WS 驱动完成数据与浏览器消费帧验证，但没有 DOM/视觉渲染验收，没有访问其提示的 Vite 5173。

新发现已回填 [005/gap.md](../../research/005_phase-a-gateway/gap.md)。已知 RSV / continuation / unmasked 等手写 WS 怪癖仍留给 A2/A4；A1 的 tungstenite acceptor 不能据此宣称原始 RFC 帧头全行为等价。

## 后续

1. **基线 UI 自动交互恢复闭环**：单独跟踪 `GatewayWsClient` 清空 hello 与 bridge 保存旧 binding 的时序；修复后重新验收真实 UI 自动 `reconnect_interaction` / `active_turn_snapshot` 和轮次完成。A1 不修改只读 `resources/`。同时定因两侧一致的 post-reconnect answer `delivered=false`，当前补充协议探针必须标记 `PASS-with-note`。
2. **A2 夹具分离（GLM P3，preload.mjs）**：将 mock provider 移到独立上游进程，拆开 watcher 替代与 HTTP mock，重新明确被测面。A1 仍受三端口约束，沿用已披露的同 listener mock 和浅层轮询。
3. **A2 比较口径收窄（GLM P3，compare.py）**：冻结尺子前，按协议字段路径审查 `durationMs` / `elapsedMs` 的归一化；不要用同名键集合抹掉业务语义时间。本轮保留 A1 临时口径，非 JSON raw 已改为明确 FAIL 差异。
4. **A2/A4 原始 WebSocket 边界**：覆盖 RSV、continuation、unmasked 等基线怪癖，以及完整握手协商、生产 DOM、递归扩展/config 热加载。当前消息/帧内存上限是 A1 的保护边界，不构成这些面的全量等价承诺。

## Rust 校验与交接

按任务要求在 `src/` 跑 `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`，全部通过，原始输出在 [rust-checks.log](../../../_tmp/20261014-exp003/rust-checks.log)。本 worktree 当前全仓共 696 个测试通过、0 失败，其中 gateway 共 8 个测试通过。gateway 覆盖大于 64KiB 的原文载荷、上游 4002 code/reason、空/custom 客户端 close 取回真实上游 code、关闭窗口在途 text/binary/pong 的顺序、hop-by-hop 头处理、101 附加头与重复 Set-Cookie 保留、畸形握手及实际 HTTP 400、accept 错误分类、4 MiB 单帧与 16 MiB 分片累积消息上限。Rust 测试使用内存 duplex，不绑定额外端口。

Python 评审回归 5 项全部通过，输出在 [python-regressions-r1.log](../../../_tmp/python-regressions-r1.log)：损坏录帧产出 FAIL、cleanup 超时不覆盖原始失败、已退出父进程的组仍被清理、实际孙进程继承独立自有组、通配监听被断言拒绝。真实代理边界实测在 [proxy-boundary-r1.json](../../../_tmp/proxy-boundary-r1.json) / [proxy-boundary-r1.log](../../../_tmp/proxy-boundary-r1.log)：缺 key 与畸形 key 均 400，上游不可用 502，实际 EMFILE 后代理存活并恢复接收请求，0.0.0.0 配置在绑定前 exit 1。该实测只占用 28789，结束后仅清理自有组。非 JSON close 原帧的比较器 FAIL 输出在 [comparator-invalid-r1.json](../../../_tmp/comparator-invalid-r1.json)。

未做 git commit / push，未修改 `resources/` 或 `specs/parity/`。未修复真实 UI 自动重放及 post-reconnect answer，未验收桌面端、生产前端 DOM、递归热加载或 101 method 全量对拍。本轮逐条评审处理、验证输出和七场景结果在 [_tmp/RESULT-r1.md](../../../_tmp/RESULT-r1.md)。
