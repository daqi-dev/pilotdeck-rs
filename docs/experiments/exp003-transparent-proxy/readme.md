# exp003：真 ui/server 经 Rust 透明代理连接 TS gateway

2026-10-14，GitHub #15 / 计划 002 Issue A1，工作分支 `feat/a1-exp003-proxy`。基线为只读的 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck`，commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。

**结论：透明代理的连接、只读面、文本轮次、HTTP 面及等价帧对照通过；真实 ui/server 的自动交互重放失败，不能宣布七项全绿。** 保存旧 binding 的协议驱动在直连和代理两侧都成功重连、取回待决问题及 active snapshot；真实 bridge 在连接关闭后丢失旧 binding，未发出恢复请求。该失败需要后续单独修复基线客户端/bridge，A1 不修改 `resources/`。整体编排因此有意返回退出码 1。

## S0：隔离与边界

唯一监听拓扑是 `3301 ui/server → 28789 Rust proxy → 28790 TS gateway`，驱动只访问 `127.0.0.1`，TS 与 Rust listener 均绑定 loopback。本轮 UI 使用基线默认 HOST `0.0.0.0`，收尾已在脚本补设 `HOST=127.0.0.1` 供后续复现；该单行收窄没有重跑整轮。没有连接或绑定 3001 / 18789，没有操作用户的 PilotDeck 进程或 `~/.pilotdeck`。

所有运行产物位于本 worktree 的 [_tmp/20261014-exp003/](../../../_tmp/20261014-exp003/)：主实验 `home/`，等价对照 `home-direct/` / `home-proxy/`，项目夹具 `project/`，General 工作区 `workspaces/general/`，`TMPDIR=tmp/`，SQLite 鉴权库在各隔离 home 内。**先写 `home*/server-token`（0600），再启动进程**；gateway 的显式 token 和 bridge 的 `PILOTDECK_GATEWAY_TOKEN_PATH` 指向同一份隔离 token。

`DISABLE_LOCAL_AUTH=1` 与基线实际识别的 `PILOTDECK_DISABLE_LOCAL_AUTH=1` 均显式设置；`DATABASE_PATH` 指向隔离 SQLite 文件。未启动 supervisor；使用基线的 standalone unmanaged 模式。`PILOTDECK_SKIP_BROWSER_OPEN=1`。按本任务约定，以 Node 直接请求 ui/server 的 HTTP 与 `/ws` 替代浏览器。

进程由 Python `Popen` 持有句柄，退出时只终止本脚本启动的进程；没有按端口查找并杀进程。最终轮的完整 PID、命令、端口、退出码在 [processes.json](../../../_tmp/20261014-exp003/processes.json)，历次运行在 `attempt-01/` 至 `attempt-06/` 各自的同名 manifest。所有登记进程已停止，最终端口释放检查见 [audit.json](../../../_tmp/20261014-exp003/audit.json)。

## S1：代理实现

代码只存在于 [src/pilotdeck-gateway/](../../../src/pilotdeck-gateway/)，已加入 `src/` workspace；没有 `_tmp` 源码副本，没有本地业务 method。直接依赖全部使用 `=版本`，传递依赖由 `src/Cargo.lock` 锁定。

HTTP/1 请求的方法、path/query、端到端头、请求体与响应状态/头/体经 hyper 全量反代；仅调整上游 Host、去除 hop-by-hop 头。请求与响应体流式转发，不集中读入内存。`/health` 也向 TS 上游转发。`/ws` 为一个下游连接建立一个上游连接，上游连通后才返回 101；text/binary payload 不解析、不改写，hello / hello_ok 与 TS 下发的 binding 都原文传递。

任何一侧 EOF/错误均结束配对、释放另一侧 socket。上游主动 close 的 code/reason 保留。客户端 close 时等待最多 1 秒取回 TS 的真实 close reply；直接写回其未掩码控制帧，避免 tungstenite 自动回显客户端 close 与基线“总回 1000”的行为冲突。这个分支有针对空 close / 4444 close 的回归测试。

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
| 4 | 杀代理、退避重连、自动重放 | **FAIL（基线 bridge）** | 自有 proxy PID 14721 被终止，PID 14882 在同一 28789 端口重启。bridge 录到 close 1006 与新的 hello_ok，ui.log 记录断线及恢复连接；[reconnect.json](../../../_tmp/20261014-exp003/reconnect.json) 和 [reconnect-frames.jsonl](../../../_tmp/20261014-exp003/reconnect-frames.jsonl) 记录 pending question、gateway_unavailable，以及 `timeout: interaction replay`。真实 bridge 的全量录帧里没有 `reconnect_interaction` / `active_turn_snapshot` 请求。 |
| 5 | HTTP 全量反代 | **PASS** | [http.json](../../../_tmp/20261014-exp003/http.json)：`/health`、`/auth/local-token`、`/api/web/projects`、带 Bearer + principal 的 POST `/api/module-host/describe`、文件读取，直连/代理的 status、body、content-type 一致且均 200；文件写入 POST 的 body 完整落盘；无 Bearer 返回 401。测试专用 SSE/NDJSON 路由的两段数据间隔 ≥350ms，证明增量流式转发而非读完才返回。 |
| 6 | 同驱动直连/代理等价 | **PASS** | [direct-frames.jsonl](../../../_tmp/20261014-exp003/direct-frames.jsonl) vs [proxy-frames.jsonl](../../../_tmp/20261014-exp003/proxy-frames.jsonl)，各 19 条：hello、hello_ok、三个只读 method、完整 submit_turn 流及 close。官方 `GatewayWsClient`、同一场景、两个干净 home；[comparison.json](../../../_tmp/20261014-exp003/comparison.json) 为 `PASS, differences=[]`，包含正常关闭码 1000。 |
| 7 | 阳性失败对照 | **PASS（比较器正确判 FAIL）** | 从成功的代理侧日志复制出独立对照记录，将 hello_ok 的 `serverVersion` 改为 `INJECTED_KNOWN_DIFFERENCE`，并同步更新 raw；实际 TS 与代理未被改动。[positive-frames.jsonl](../../../_tmp/20261014-exp003/positive-frames.jsonl)、[positive.json](../../../_tmp/20261014-exp003/positive.json)、[positive.log](../../../_tmp/20261014-exp003/positive.log) 显示 **FAIL、exit 1、index=1 唯一差异**。 |

机器原始运行结果在 [scenarios.json](../../../_tmp/20261014-exp003/scenarios.json) 和 [orchestrator.log](../../../_tmp/20261014-exp003/orchestrator.log)。1–3 共用一次真实 UI 驱动，编排中以 `S2-S3-ui` 聚合记录；上表分开给出各自的证据。

额外关闭码对照见 [close.json](../../../_tmp/20261014-exp003/close.json)：`hello_required=4001`、`protocol_mismatch=4001`、`auth_failed=4003`、`invalid_json=4002`；客户端空 close 与 4444 close 都取回 TS 的 `1000, reason=""`。

## 重连失败对架构的影响

基线 `src/gateway/client/GatewayWsClient.ts:93-100` 在 close 回调内先清空 `hello`，再调用 disconnect handlers；`RemoteGateway.ts:111-112` 的 binding getter 直接委托 client。因此 `ui/server/pilotdeck-bridge.js:255-258` 的 `onInvalidated` 与 `:2127-2130` 的 catch 路径已经拿不到退休 binding。后者将 `awaitingGatewayReconnect` 设为 false，最终清理本地 active 状态，重连成功后没有恢复候选。

为区分“代理状态丢了”和“真实 UI 没发恢复请求”，同一个官方客户端补充驱动显式在关闭前保存 binding：直连结果在 [protocol-direct.json](../../../_tmp/20261014-exp003/protocol-direct.json)，代理结果在 [protocol-proxy.json](../../../_tmp/20261014-exp003/protocol-proxy.json)。两侧均 `outcome=reconnected`、返回原 pending requestId、`active=true`，说明 TS 的待决状态经连接 1:1 断线后仍可恢复。两侧后续 `elicitation_respond` 均返回 `delivered=false`；这是额外暴露的基线行为，未把“恢复请求成功”宣称为“恢复轮次完成”。全量驱动帧在对应的 `protocol-*-frames.jsonl`。

**架构判断：保留 Rust 前置、TS 持有 binding/交互运行时的方案；自动 UI 重放仍是 P0 未闭合。** A1 的实验与实现已交付，不能把真实 UI 恢复缺陷通过改驱动、抹帧或改 baseline 变绿。

## 比较口径与限度

比较器保留连接内完整记录的顺序、direction、所有语义字段、event.seq、final 与 close code/reason；验证 recorder.seq 严格递增、raw 与 parsed frame 一致，空日志不能 PASS。只对 UUID 做一致首次出现映射、binding generation 做一致映射、显式时间字段归一化。项目路径在两轮完全相同，未通过删路径来消掉差异。两轮隔离 home 和 port/URL、录制单调时钟不作为语义帧字段比较。

这是 **A1 临时口径**，没有修改或冻结 `specs/parity/`，也不是 A2 的全量帧/落盘/HTTP 三面尺子。阳性对照只覆盖本任务要求的一处已知帧差异；SSE/NDJSON 是独立驱动证据，不混进 WS 帧序列。

## 意外

1. **真实自动重放与源码推断不一致**：gateway 重连成功，但旧 binding 在 bridge 获取前已丢失，恢复请求根本没有发出。另有两侧一致的 post-reconnect answer `delivered=false`，尚未在本 issue 内修复/定因。
2. **标准库 close 自动应答不是透明搬运**：初版配对在 client close 后立即 drop，实际 1000 变 1006（`attempt-05/comparison.json`）；仅 flush 自动应答又变为 1005（`attempt-06/comparison.json`）。最终改为读 TS 真正的 close reply 再写回，保持 close code 为比较字段，并增加空/custom close 测试。没有把差异归一化掉。
3. **单有目录不算注册项目**：初轮存在 `project/` 却返回 `Unknown projectKey`，需 home 下注册目录的 `.cwd` 标记；General 的 cwd 默认取 `~/general`，需显式 `WORKSPACES_ROOT`。初轮 mkdir 被沙箱拒绝，未成功创建用户目录；后续所有工作区留在隔离目录。
4. **沙箱 fs.watch 会触发 gateway 静默退出**：即使文件描述符上限为 1048576，native watcher 报 EMFILE；CLI 的 uncaughtException handler 只记遥测并 exit 1。`attempt-04/gateway-direct.log` 的监听前置诊断确认了异常。最终 preload 显式替换为浅层 stat 轮询，fixture/config 不变；不覆盖递归扩展热加载。`attempt-03` 已留下未替换 watcher 时的 UI 正常轮次、HTTP 通过及自动重放失败证据。
5. **fetch mock 会被 CLI 初始化覆盖**：首个 preload global-fetch 替身未拦到真实请求，失败记录在 `attempt-02/`。最终改为同 listener 的真实 HTTP mock provider。`/api/module-host/describe` 还必须 POST 并提供 principal；protocol_mismatch 对应 4001，不是 4002。
6. **UI 生产前端未构建**：启动日志明确提示未发现生产 frontend build。任务指定的 HTTP/WS 驱动完成数据与浏览器消费帧验证，但没有 DOM/视觉渲染验收，没有访问其提示的 Vite 5173。

新发现已回填 [005/gap.md](../../research/005_phase-a-gateway/gap.md)。已知 RSV / continuation / unmasked 等手写 WS 怪癖仍留给 A2/A4；A1 的 tungstenite acceptor 不能据此宣称原始 RFC 帧头全行为等价。

## Rust 校验与交接

按任务要求在 `src/` 跑 `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`，全部通过，原始输出在 [rust-checks.log](../../../_tmp/20261014-exp003/rust-checks.log)。已有 core 的 1 个测试和 gateway 新增的 3 个测试均通过；新增测试覆盖大于 64KiB 的原文载荷、上游 4002 code/reason、客户端 close 取回上游 code，以及 hop-by-hop 头移除/端到端头保留。测试使用内存 duplex，不绑定额外端口。

未做 git commit / push，未修改 `resources/` 或 `specs/parity/`。未修复真实 UI 自动重放及 post-reconnect answer，未验收桌面端、生产前端 DOM、递归热加载或 101 method 全量对拍。交接总表在 [_tmp/RESULT.md](../../../_tmp/RESULT.md)。
