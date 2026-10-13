---
type: plan
number: "002"
date: 2026-10-15
title: 阶段 A：Rust 替换 18789 gateway——代理先行、装置其次、只读 method 首迁
tags: [phase-a, gateway, proxy, parity]
status: ready
related: [research/005_phase-a-gateway, research/002_parity-boundary, plan/001_phase-c-agent-loop-sidecar, experiments/exp002-connection-state]
---

# 阶段 A 计划

## Context

为什么这样设计见 `docs/research/005_phase-a-gateway/`（`must-know` / `gap` / `decision` / `method-inventory`，行号均已回源核对）。一句话：在 `ui/server` 与 `apps/desktop` 一行不改的前提下，Rust gateway 先以透明代理占位（实验与验收全走高位端口，绝不绑定 18789——用户本机的 PilotDeck 正占着它），配一套能机器判的对拍装置，再把冻结清单里的 (a) 类 **10** 个只读 method 逐个本地化。取舍卡五项技术项（粒度 / 装置 / 类型 / 编排 / crate）为 **AI 代拍，待人复核**；两项红线项（A1 是否提前、桌面端是否验收）**待人拍板，不代拍**（`005/decision.md` #5/#7）。

**阶段门（AI 代拍，待人复核——人 2026-10-14 夜授权 AI 代做全部判断，见 `CLAUDE.md` §五「授权扩大」）**：阶段 A **可以与阶段 C 并行**——两者替换的进程不同（C = sidecar，A = gateway），阶段 A 的对拍是「Rust gateway + TS sidecar」对「TS gateway + TS sidecar」，不依赖阶段 C 的进度。**合并进 main 的每一步仍须阶段 A 自己的对拍达标。**

## 现状（已确认）

| 项 | 状态 | 证据 |
|---|---|---|
| 边界与粒度 | 已定：连接 1:1 透传；(a) 类 **10** 只读可按 method 本地化（冻结清单 `005/method-inventory.md`）；(b) 46、(c) 4、待定 36 与写全局者在「轮次与会话运行时」整体迁走前一律透传 | exp002 + `005/method-inventory.md`（每行带行号）；`005/decision.md` #1 |
| 重定向钩子 | 齐备：`PILOTDECK_GATEWAY_PORT` / `PILOTDECK_GATEWAY_URL` / `PILOTDECK_GATEWAY_TOKEN(_PATH)`，三处入口都认 env，改代码零需求 | `src/cli/pilotdeck.ts:212-223`、`ui/server/pilotdeck-bridge.js:77-81`、`ui/server/webRuntimeSupervisor.js:102-109, 214`、`apps/desktop/src/main.ts:138, 162-163` |
| 驱动 | 现成：`GatewayWsClient`（url+token，request/stream/通知，hello 超时 10s），`src/gateway/index.ts:112-119` 一处导出；真端口测试样板 10 个 | `tests/gateway/bridge-interaction-reconnect.spec.ts:125-141` 等 |
| 协议类型 | TS-only，无运行时 schema；101 个 method 枚举在 `frames.ts:21-122`，已全部分类并**冻结**进 `005/method-inventory.md`；协议版本 `"1.2"`（`version.ts:1`） | `005/gap.md` 2.1 |
| 服务端 WS | 手写 RFC 6455：必须掩码、无 continuation、无服务端 ping、close code 4001/4002/4003 | `src/gateway/server/websocket.ts:31-50, 52-75, 107-148` |
| 18789 HTTP 面 | `/health`、`/auth/local-token`（仅 loopback）、`/feishu/webhook`、`/api/web/*`、`/api/module-host/{describe,call}`、`/api/module-host/approvals/*`、`ui/dist` 静态资源 | `GatewayServer.ts:96-131` 等，见 `005/gap.md` 1.4 |
| 本仓 | `src/` 仅占位 `pilotdeck-cli` / `pilotdeck-core`；阶段 C 将搬入 `pilotdeck-{agent,transport,protocol,tokenizer}`（`transport` 与网关无关，注意重名） | `docs/plan/001` |
| 环境红线 | 用户本机 PilotDeck 正占 18789 与 `~/.pilotdeck`；实验一律高位端口 + 隔离 `PILOT_HOME`；绝不 kill 用户进程 | `CLAUDE.md` §五 |

## 目标

阶段 A 的终点（`.42cog/intent.md` 作品区）：Rust gateway 承担 18789 端口的全部职责（**验收一律在高位端口上做**，18789 是最终占位描述，不是任何实验或验收的绑定端口），原版 `ui/server` 零改动连上即可用，全部 101 个 method 与 HTTP 面由 Rust 承担、TS gateway 进程退役。**本计划覆盖到「A4 完成」为止**——(a) 类 10 个全部本地化 + 其余全量透传 + 装置冻结，拿到第一份「Rust 前置」的对拍全绿；**A4 是中间验收**（TS 退役属 A5+），会话域与轮次/交互运行时的整体迁移（深水区）届时已有实测数据，另开计划 003。

## 执行步骤

> 拆分逻辑与计划 001 相同：issue 之间隔着人的闸门——A1 可直接开工（`005/decision.md` #5 已 AI 代拍），A3 冻结 schema（AI 代批，待人复核），A4 合并前须 A2 装置对拍达标。后续 issue 在前一条合并后再开。

### Issue A1 · exp003：真 `ui/server` 连透明代理（GitHub 另开；可与阶段 C 并行，`005/decision.md` #5 AI 代拍）

**S0 前置确认**：`CLAUDE.md` §五的硬红线（高位端口、隔离 `PILOT_HOME`、不碰用户进程）逐条核对。

**S1 搭代理**（代理代码**只留 `src/pilotdeck-gateway` 一份**，铁律 7：不留 `_tmp` 副本、不留「先住 `_tmp` 再转正」的双份；`_tmp/<日期>-exp003/` 只放运行产物——帧日志、隔离 `PILOT_HOME`、token 文件）
- Rust 单 bin：`tokio` + `tokio-tungstenite`（codex-rs 同款选型，做法见 `app-server-transport/src/transport/`，不抄代码）。TCP 监听 → HTTP 请求全量反代到上游（`hyper` 客户端）→ `/ws` upgrade 成功后，客户端侧与上游侧各一条 WS，帧原文双向搬运。**帧日志由 driver 侧录制**（S3 的驱动脚本/编排各自落盘 jsonl：方向 + 单调序号 + 帧全文）——被测代理不负责自证；代理内部日志仅作调试辅助，不进比对口径。
- 关键复刻点：`hello`/`hello_ok` **原帧透传**（不自造 binding）；客户端断开 → 同步关上游，反之亦然（`GatewayWsConnection.ts:48-63` 的联动靠它）；上游 close code（4001/4002/4003）原样带回。

**S2 编排三个进程**（全部高位端口 + 隔离目录）
- 编排脚本先**生成隔离的 token 文件**：显式给 `PILOTDECK_GATEWAY_TOKEN` 时基线**不写** token 文件（`GatewayServer.ts:58-60`：给了 token 就不走 `ensureGatewayAuthToken` 的写文件分支），而 `ui/server` 只从文件读 token（`pilotdeck-bridge.js:204-218`）——必须把同一 token 写进 `<tmp>/home/server-token` 并让 `PILOTDECK_GATEWAY_TOKEN_PATH` 指向它，否则 bridge 永远拿不到 token。
- TS gateway：`PILOT_HOME=<tmp>/home`、`PILOTDECK_GATEWAY_PORT=28790`、`PILOTDECK_GATEWAY_TOKEN=<固定>` 起 `resources/PilotDeck` 的 `pilotdeck.ts server`（mock 模型服务按 exp001 的做法接，`tmp/home` 里 provider 指向它）。
- Rust 代理：监听 `28789`，上游 `ws://127.0.0.1:28790/ws`。
- `ui/server`：单独起（不经 supervisor，`runtimeCoordination.js:12-14` 非监督即 `unmanaged`）——`SERVER_PORT=3301`、`PILOT_HOME=<tmp>/home`、`PILOTDECK_GATEWAY_URL=ws://127.0.0.1:28789/ws`、`PILOTDECK_GATEWAY_TOKEN_PATH=<tmp>/home/server-token`。

**S3 过场景并落盘结论** → `docs/experiments/exp003-transparent-proxy/readme.md`
1. 连通：bridge 日志出现 `connected → ws://127.0.0.1:28789/ws`；`hello_ok` 帧日志在案。
2. 只读面：浏览器打开 `127.0.0.1:3301`，项目列表、会话列表、历史消息可读（(a) 类 method 经代理）。
3. 一轮对话：mock 模型下完成一条消息的 `submit_turn` → 事件流 → `final`，前端渲染正常。
4. 断线重连：杀代理 → `ui/server` 退避重连（`gatewayConnectionCache.js:10-21`）→ `reconnect_interaction` + `active_turn_snapshot` 重放成功。
5. HTTP 面：`/health`、`/api/web/*`、`/api/module-host/describe`（带 Bearer）经代理可达。
6. 等价对照：同一驱动脚本（S4 里 A2 的雏形，可先用手搓脚本；帧日志由该脚本自己录）直连 TS 与经代理各跑一遍，归一化后帧日志一致。
7. **阳性失败对照**：在 6 的管线里注入一处已知差异（改一帧内容或删一条事件），比对必须判 **FAIL**——证明「一致」不是恒真（第 1 轮评审要求：仅有等价对照=瞎判风险）。

**怎么算做完**
- [ ] exp003 readme 七条场景各有证据（帧日志片段 / 截图路径）；失败场景如实记录并给出对架构的影响
- [ ] 阳性失败对照确实 FAIL 过（贴注入方式与 FAIL 输出）
- [ ] 全程未占用 18789、未触碰真实 `~/.pilotdeck`、未 kill 用户进程（端口与目录清单写进 readme）
- [ ] 若有「读源码没料到」的行为差异（手写 WS 怪癖、重连细节），逐条记入 readme「意外」节并回填 `005/gap.md`

### Issue A2 · 对拍装置（A1 合并后）

**S1 驱动与场景**：`scripts/gateway-parity/driver.mts`（tsx 起）从 `resources/PilotDeck/src/gateway/index.js` 导入 `GatewayWsClient` / `RemoteGateway` / `PILOTDECK_GATEWAY_PROTOCOL_VERSION`，参数：`--url --token --scenario --out`；**帧日志一律由 driver 侧录制**（驱动进程自己按连接落全帧 jsonl，含 notification 与交错顺序；被测代理不经手比对数据——不自证）。场景集：从 10 个真端口测试提炼（先 6 个：hello 鉴权、只读 method 混跑、`submit_turn` 全流、断线重连+重放、权限问答、cron/skill 写路径），再加 HTTP 面场景与** WS 怪癖场景**（RSV 非零帧、continuation 0x0 帧、unmasked 帧、客户端 close——预期分别为「当普通帧解 / 静默丢弃 / 异常断开 / 回 1000 不回显」，见 `005/gap.md` 1.5 四细目）。
**S2 编排与比对**：`run_round.py`（起 mock 模型 + 被测侧 + 隔离 `PILOT_HOME`，参数化端口与路径，风格照 `scripts/parity/round.py`；**每轮生成隔离 token 文件**并注入 `PILOTDECK_GATEWAY_TOKEN_PATH`，原因见 A1 S2；端口全高位）；另设**上游观测点**：`scripts/gateway-parity` 自有的一台录制代理，插在 Rust gateway 与 TS 上游之间，录上游向帧日志——用于 A4 的「本地应答未转发上游」断言（属装置，不属被测侧）。`compare.py` 比三样：帧日志（ID 一致映射、帧 `seq` 保留、路径对齐——原则沿用 002 #7，规则新写）、落盘快照（目录树 + 内容）、HTTP 抓取（status/头/体，含 ndjson/SSE）。
**S3 规约与对照**：`specs/gateway-parity/{acceptance,normalization,accepted-diffs}.md` 草稿（**不动已冻结的 `specs/parity/`**）；`controls/` **阳性失败对照**：注入帧差异 / 删一条事件 / 改一处落盘，必须判 FAIL——「注入一处差异必须 FAIL」是装置可信的必要条件，等价对照替代不了。四项对照（自比 k 遍 / 逐规则负向 / 两侧同坏 / 双谱系评审）照计划 001 S3 的结构做，`check_ruler.py` 输出 JSON 全 true。
**怎么算做完**
- [ ] `python3 scripts/gateway-parity/check_ruler.py` 四项全 true，结果进 `docs/experiments/`
- [ ] 规约草稿**交人批**后冻结（`FROZEN.md` + sha256，`check_frozen.py` 校验）；冻结前必须有人过目（CLAUDE.md 方法缺口 ⑬）
- [ ] TS 自比（直连 vs 直连）在冻结口径下 100% 一致，否则装置不可信

### Issue A3 · 协议 schema 导出与冻结（可与 A2 并行）

**S1 生成**：`scripts/gateway-parity/export_schema.mjs` 用 `ts-json-schema-generator`（或等价物）从 `src/gateway/protocol/frames.ts` + `types.ts` 导出 JSON Schema，产物提交进 `schema/`。
**S2 静态断言**：method 枚举 = 101 且与 `frames.ts:21-122` 逐字一致，**且与 `005/method-inventory.md` 冻结清单的 method 集合一致**（防清单与代码漂移——分母动了指标就假）；`GatewayChannelKey` 等开放联合保持开放字符串（`types.ts:74-81`）；协议版本常量出现在 `hello`/`hello_ok`。
**S3 冻结与用途**：人过目 → 冻结进 `specs/gateway-parity/FROZEN.md`；此后两侧（TS 基线、Rust）录到的帧先过 schema 校验再进比对；Rust 侧 serde 类型以「schema × 实例 fixtures」对拍（A4 用）。
**怎么算做完**：[ ] 断言脚本进 CI 可跑；[ ] schema 冻结登记；[ ] `gap.md` 2.1 的「无运行时 schema」关闭。

### Issue A4 · `pilotdeck-gateway` crate：透传 + (a) 类本地化（可与阶段 C 并行；合并前须本阶段 A2 装置对拍达标）

**S1 crate 骨架**：`src/pilotdeck-gateway`（单 crate，AI 代拍 #6；A1 的代理代码已在此，无转正搬运）；`pilotdeck-cli` 加 `server` 子命令委派之。模块：`acceptor`（仅 `/ws` upgrade，复刻手写 WS 的可见行为——**怪癖清单**：不要求分片、continuation(0x0) 静默丢弃、RSV 不校验、回 pong、客户端 close 回 1000 不回显、unmasked 帧按异常路径断开、close code 4001/4002/4003 语义含 `GatewayWsConnection.ts:89` 的 4002 `invalid_frame`，细目见 `005/gap.md` 1.5）、`upstream`（连 TS 的客户端，帧掩码）、`http_proxy`（全量反代 + `/health` 本地）、`pairing`（连接 1:1 + 双向关闭）、`dispatch`（本地 method 白名单，其余透传）。
**S2 (a) 类逐个本地化**：本地化清单 = `005/method-inventory.md` 冻结的 **(a) 类 10 个**（改清单=改分母，须人过目）。每个 method 一小步：读同一 `PILOT_HOME` 实现 → 场景组对拍绿 → 上游观测点录帧证明**该 method 的 request 没有转发到 TS** → 登记差异（bug 按_intent_「默认修」）。**`describe_server` 保持透传**（`gap.md` 3.4，不在 (a) 内），hello_ok 仍由 TS 生成；每迁一个在 `specs/gateway-parity/accepted-diffs.md` 对应登记。
**S3 阶段验收（A4 = 中间验收）**：Rust 前置 + TS 后置跑全部场景。**指标（防假通过口径）**：分子 = 「**由 Rust 本地应答**且对拍一致」的 (a) 类 method 数——每个计分项须同时满足：① 响应由 Rust 生成、与 TS 侧对拍一致；② **上游观测点的录帧里没有该 method 的 request**（纯透传响应不计分，哪怕输出一致）；分母 = **10**（冻结清单）。清单里没本地化的不计分。透传面（(b)(c) 与待定）回归零差异，按整块计（exp002 的结论）。
**怎么算做完**
- [ ] CI `check` 绿；阳性失败对照仍会 FAIL（装置没被顺手改瞎）
- [ ] (a) 类 10 个全部本地化、对拍一致且录帧证明未转发上游；透传面回归零差异
- [ ] 人肉验收：Web UI 走通（此步不交给 AI）；**桌面端明确标注「未验收」**（`005/decision.md` #7，待人拍板）

### Issue A5 起 · 深水区（另开计划 003，本计划不展开）

会话域 (b) 类整块 → 轮次/交互运行时整体迁移（binding 权威随之移到 Rust）→ TS 上游退役。前置一道架构决策：Rust gateway 如何承载 agent 循环（进程内链接阶段 C 的 `pilotdeck-agent`，还是同 TS 一样以 Module Protocol 拉 sidecar——两种拓扑的语义等价性要用阶段 C 的尺子量过再定）。

## 验收（端到端怎么测）

```bash
# 装置自检（A2 之后）
python3 scripts/gateway-parity/controls/run_all.py        # 阳性失败对照：注入差异必须 FAIL
python3 scripts/gateway-parity/check_ruler.py             # 四项对照 JSON 全 true
python3 scripts/gateway-parity/check_frozen.py            # 冻结文件哈希
# 对拍（A4 之后）
python3 scripts/gateway-parity/run_round.py --side ts   --label ts-r1
python3 scripts/gateway-parity/run_round.py --side rust --label rust-r1   # Rust 前置 + TS 上游
python3 scripts/gateway-parity/compare.py ts-r1 rust-r1   # 帧与事件流、落盘、HTTP 三样
# schema（A3 之后）
node --import tsx scripts/gateway-parity/export_schema.mjs --check
```

最后一道是人：用原版 Web UI 实测（`ui/server` env 指向 Rust gateway，`PILOTDECK_GATEWAY_PORT`/`PILOTDECK_GATEWAY_URL` 全高位 + 隔离 token 文件，绝不绑定 18789），这一步不交给 AI；**桌面端不在验收范围**（`005/decision.md` #7 待人拍板），Web 通过不得宣称桌面兼容。

## 风险与权衡

| # | 风险 | 应对 |
|---|---|---|
| R1 | 手写 WS 的怪癖在真客户端上爆（分片、>64KB 大帧、ping/pong、close 时序、RSV/continuation/unmasked 的非常规处置） | exp003 就是撞这个的；四细目已进 acceptor 清单与对拍场景（`005/gap.md` 1.5）；帧日志逐条对；对拍场景含长 transcript 大载荷 |
| R2 | notification 与事件流的**交错时序**不稳定，TS 自比就有噪声 | A2 S3 自比 k 遍定口径（照计划 001 的做法），时序断言按「同连接全序」比对，不比跨连接时序 |
| R3 | 若 A1 提前获人批准，与阶段 C 并行造成机器负载假差异 | 全量对拍串行锁沿用（`mkdir /tmp/pdrs-parity.lock` 模式）；A1 手工跑、小规模；批准范围外的任何提前都回到红线 |
| R4 | schema 生成器对 TS 高级类型（开放联合、条件类型）出错 | 静态断言 + 人过目才冻结；生成器输出只是**基准**，错了改生成配置，不改基线 |
| R5 | (a) 类本地化后 `describe_server`/能力清单与 TS 漂移 | `describe_server` 与 `hello_ok` 保持 TS 生成；一致性断言进对拍 |
| R6 | Rust 读盘与 TS 写盘并发（transcript 半行、sqlite 锁） | (a) 类只读；读实现容忍截断行（`read_subagent_messages` 备注，`005/method-inventory.md` #82）；对拍在落盘快照上比最终态 |
| R7 | 误碰用户环境（18789、3001、`~/.pilotdeck`、用户进程） | 一律高位端口（287xx/33xx）+ `PILOT_HOME`/`TMPDIR` 指向 `_tmp/` + 编排生成的隔离 token 文件（`PILOTDECK_GATEWAY_TOKEN_PATH` 指向它）；绝不 kill 用户进程；每次实验把端口清单写进 readme |
| R8 | 阶段门被进度压力侵蚀（「先写一点替换代码」「实验顺便把 dispatch 也做了」） | 并行已由 AI 代拍（`005/decision.md` #5），但**合并闸门不放松**：每条 PR 必须本阶段对拍达标 + 双谱系评审；超出 issue 范围的改动另开 issue |

## 范围外

阶段 B（`ui/server` 替换）与桌面端入口改动（**桌面端验收是否纳入待人拍板**，`005/decision.md` #7）；(b)(c) 类的真实迁移（计划 003）；`specs/parity/` 已冻结文件的任何改动；性能优化；上游升级；对外提供服务。

## 拍板记录

上一轮已拍（见 `002/decision.md`）：先 C 再 A 再 B · 驱动用 TS · 默认修 bug · 编排这一阶段不改入口。
本轮技术项 AI 代拍（待人复核，详据见 `005/decision.md`）：① 粒度 = 连接 1:1 + (a) 类（冻结清单 10 个）按 method、其余透传；② 装置 = TS 驱动比三样 + driver 侧录帧 + 上游观测点 + 阳性失败对照 + 新规约；③ 类型 = TS 导 JSON Schema 冻结、Rust 手写 serde；④ 编排 = 不改入口、env + 装置自编排 + 隔离 token 文件；⑤ crate = 先单 crate `pilotdeck-gateway`、代码只留 `src/` 一份。
本轮红线项**待人拍板（不代拍）**：⑥ 阶段门——A1（只跑 exp003、不写替换代码）是否提前（`005/decision.md` #5）；⑦ 桌面端不纳入阶段 A 验收（`005/decision.md` #7）。两项未拍板前均按最严字面执行。

## 相关

`docs/research/005_phase-a-gateway/` · `docs/research/002_parity-boundary/` · `docs/experiments/exp002-connection-state/` · `docs/plan/001_phase-c-agent-loop-sidecar.md`
