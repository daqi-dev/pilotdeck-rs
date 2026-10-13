# 取舍卡 · 阶段 A：用 Rust 替换 18789 gateway

> 2026-10-15。**一页纸。** 依据见 `must-know.md` 与 `gap.md`（行号均已回源核对）；上一轮全局拍板见 `docs/research/002_parity-boundary/decision.md`（先 C 再 A 再 B、驱动用 TS、默认修 bug）与 `docs/experiments/exp002-connection-state/`（连接级状态）。
> ⚠️ 夜跑规则（`CLAUDE.md` §五）：「需要你拍板的地方」按 **AI 建议**先走，每处标「AI 代拍，待人复核」。

## 这次不可妥协的几条

1. 原版 `ui/server`（含浏览器前端、桌面壳）一行不改就能连上——只允许用基线已有的环境变量重定向。
2. 对拍能机器判——帧与事件流、落盘、HTTP 响应三个观测点，归一化后可比，且有阳性对照。
3. 渐进切换时连接级状态不出错——一客户端连接 1:1 一条上游连接；binding 权威在持有交互运行时的一侧；断线双向同步关闭。

## 我选择什么（草稿）

> **Rust gateway 以「透明代理」姿势先站上 18789（高位端口做实验）：每条客户端 WS 连接 1:1 映射一条 TS 上游连接，HTTP 全量反向代理；对拍装置用 TS 驱动（复用基线 `GatewayWsClient`），比帧日志、落盘快照、HTTP 响应三样；协议类型从基线 `frames.ts`/`types.ts` 导出 JSON Schema 冻结为对拍基准，Rust 手写 serde、以 schema 校验实例；进程编排一律不改 `ui/` 与 `apps/desktop`，靠环境变量 + 装置自己拉起。**(a) 类约 35 个只读 method 达标后再逐个本地化；(b)(c) 类与写全局状态者在「轮次与会话运行时」整体迁走前一律透传。**

## 我放弃什么（草稿）

> 放弃「上来就在 Rust 里实现 gateway 业务」——先用代理把连接路径与对拍装置钉死；放弃按会话/按 method 切 (b)(c) 类（exp002 证明切不动）；放弃为桌面端改启动入口（阶段 B 再说）；放弃用代码生成器从 schema 生成 Rust 类型（手写 + 实例校验）；放弃动已冻结的 `specs/parity/`（阶段 A 另立规约）。

## 依据来自哪一类

- [x] 自有积累 —— 调研 002（边界选型，已拍板）、exp002（连接级状态，已核）、`docs/plan/001`（阶段 C 的尺子方法论，直接复用其「装置先于代码、阳性对照、冻结规约」三件套）
- [x] 他人积累 —— PilotDeck 基线 `cc7c4e6`（本轮逐条回源核对，见 `gap.md`）；codex-rs（app-server / -protocol / -transport 三分与 `start_websocket_acceptor` 的做法，`app-server-transport/src/lib.rs:37`、`Cargo.toml:51` 用 tokio-tungstenite）
- [ ] 自己跑的 —— exp003 还没跑，是第一条 issue

## 支持它的最强依据

重定向钩子在基线里**本来就齐**：gateway 监听端口、`ui/server` 的连接地址与 token 路径、supervisor 的就绪探测全部读环境变量（`src/cli/pilotdeck.ts:212-223`、`ui/server/pilotdeck-bridge.js:77-81`、`ui/server/webRuntimeSupervisor.js:102-109, 214`），supervisor 的 env 原样继承（`:103-109`）——「Rust 占 18789、TS 挪高位端口、`ui/server` 不改一行」不需要动基线任何代码。

## 反对它的最强依据

代理态的一致是**必要不充分**：它证明连接路径与装置，但 (a) 类本地应答、乃至最终 TS 上游退役，每一步都引入新的不一致面（`describe_server` 能力清单、落盘并发读写、广播时序）；且阶段 A 与阶段 C 共享机器与全量对拍串行的约束（`CLAUDE.md` §五），两条线并行可能互相制造假差异。

## 仍未补齐的缺口

exp003 没跑——「真的 `ui/server` 连代理」是全部方案的前提实证（计划 001 已把它留给阶段 A，作为 Issue A1）。

## ⚠️ 需要你拍板的地方（夜跑：按 AI 建议先走）

| # | 需要拍板的 | 选项 | 我的建议 | 拍了吗 |
|---|---|---|---|---|
| 1 | **切换粒度**：(a)(b)(c) 怎么从 TS 手里接 | (i) 全部按 method 逐个切；(ii) 只读按 method、其余整连接透传；(iii) 按会话整块归属某一侧 | **(ii)**，与 exp002 结论一致：每条客户端连接 1:1 映射一条 TS 上游连接，`hello`/`hello_ok` 原帧透传、沿用 TS 下发的 binding；(a) 类约 35 个只读 method 可由 Rust 本地应答（`describe_server` 暂不本地化，见 `gap.md` 3.4）；(b) 会话域约 45 个、(c) 连接状态 5 个、写全局状态那批，在「轮次与会话运行时」整体迁走前**一律透传**。**不选 (iii)**：`ui/server` 全程只开一条共享连接（`pilotdeck-bridge.js:244`），「按会话拆到两侧」要求拆连接，做不到 | **AI 代拍：(ii)，待人复核**（exp002 已给出依据，本卡确认） |
| 2 | **对拍装置**：驱动怎么写、比什么 | 驱动：TS（复用 `GatewayWsClient`）/ Python / Rust；比对物：帧 / 帧与落盘 / 帧与落盘与 HTTP | 驱动 **TS**（002 拍板过：协议客户端现成、与基线同源，从 `src/gateway/index.ts:112-119` 导入；真端口样板 `tests/gateway/bridge-interaction-reconnect.spec.ts:125-141`）；拓扑：每侧各一个 mock 模型服务实例 + 各自隔离的临时 `PILOT_HOME`，TS 侧 = 基线 gateway 直连，Rust 侧 = Rust gateway（前置）+ TS 上游；**比三样**：① 帧与事件流（按连接录 jsonl：方向、帧全文、单调序号；含 notification 与交错顺序）② 落盘快照（目录树 + 文件内容归一化 diff）③ HTTP 响应（status/头/体，含 ndjson/SSE 流）；归一化沿用 002 的原则（ID 一致映射、帧 `seq` 保留、路径对齐），落**阶段 A 自己的规约**（不动冻结的 `specs/parity/`）；**阳性对照必做**（注入差异必须 FAIL）；装置住 `scripts/gateway-parity/`，规约草稿住 `specs/gateway-parity/`（人批后冻结） | **AI 代拍：按建议，待人复核** |
| 3 | **协议类型怎么来** | (i) 从 `types.ts`/`frames.ts` 导出 JSON Schema 作 Rust 类型的对拍基准；(ii) 纯手写 serde 不立 schema；(iii) 由 schema 代码生成 Rust | **(i) + 手写**：用 `ts-json-schema-generator`（或等价物）在基线上生成帧与载荷的 JSON Schema，提交进仓、冻结为**对拍基准**（校验两侧录到的帧都合法、Rust 序列化结果符合 schema）；Rust 类型**手写 serde**，靠「schema × 实例」的 fixtures 测试对拍，不做代码生成（codex-rs 是反方向 Rust→TS，`app-server-protocol/src/export.rs:123, 202`，做法可鉴、方向相反）。已知坑：`GatewayChannelKey = … \| (string & {})`（`types.ts:74-81`）这类开放联合，schema 必须保持开放字符串，不得收敛成闭枚举；生成器输出的 schema 也要经人过目再冻结 | **AI 代拍：(i)，待人复核** |
| 4 | **进程编排**：supervisor / 桌面端写死起 TS gateway | (i) 改两处启动入口；(ii) 不改，包装脚本/环境变量绕；(iii) 不改，Rust 只在对拍装置里起 | **(ii)**（002 第 6 行已按此拍板，本卡细化）：阶段 A 不碰 `ui/` 与 `apps/desktop`（红线，`.42cog/intent.md`「不做什么」）。对拍装置自己拉起全部进程（端口全高位，**绝不碰用户正占着的 18789 与 `~/.pilotdeck`**）；人肉验收走 Web UI：导出 `PILOTDECK_GATEWAY_PORT=<高位>`（supervisor 会把 TS gateway 挪过去并按它探测就绪，`webRuntimeSupervisor.js:102, 214`）+ Rust gateway 起在 18789（或同样高位）+ `PILOTDECK_GATEWAY_URL` 指向 Rust。**桌面端在阶段 A 内不会用上 Rust**（它总是自己 spawn TS gateway，`apps/desktop/src/main.ts:250-259`），接入留给阶段 B 或人另行拍板 | **AI 代拍：(ii)，待人复核** |
| 5 | **阶段门**：C 达标前进 A 到哪一步 | (i) 严格串行，A 的一切等 C 达标；(ii) A 的实验与装置（不写产品代码）可与 C 的修复期并行，A 的 method 迁移等 C 达标 | **(ii)**：exp003（A1）与装置/schema（A2/A3）只读基线 + 只写本仓 `scripts|docs|specs`，与 C 的产物零耦合；严格串行会白等数周。但 A4（`pilotdeck-gateway` 进 `src/`、真迁 method）**严格等 C 达标**（红线收窄的原话：「前一阶段对拍达标之前不进下一阶段」）；并行期全量对拍仍串行（机器负载，`CLAUDE.md` §五） | **AI 代拍：(ii)，待人复核** |
| 6 | **Rust 侧 crate 形态** | (i) 一步到位三分（protocol / transport / server）；(ii) 先单 crate `pilotdeck-gateway`，长痛了再拆 | **(ii)**：codex 三分是长出来的，不是设计出来的；阶段 A 初期只有「acceptor + 1:1 上游 + 透传 + 少量本地应答」，单 crate 够。注意与阶段 C 搬入的 `pilotdeck-transport`（module protocol）**重名不同物**，网关侧命名避开（`pilotdeck-gateway` 无冲突） | **AI 代拍：(ii)，待人复核** |

## 两把尺子

**复用分级**

| 来源 | 权限 | 这次用到哪一步 |
|---|---|---|
| PilotDeck 基线（AGPL，被移植对象） | 逻辑可移植，衍生作品同为 AGPL | 驱动复用其 `GatewayWsClient`（本仓已 AGPL，无额外义务）；协议类型作为对拍基准 |
| codex-rs（Apache-2.0） | 只读借鉴，读懂再自己写；复制须保留 NOTICE | 借 tokio-tungstenite + 手写握手校验的**做法**；不搬代码 |

**满意与停止线**：四个拍板项各有建议、每条依据带行号、exp003 的验收口径写进计划——**到此为止**。运行时行为不再读材料猜，交给 exp003 与对拍装置去量。
