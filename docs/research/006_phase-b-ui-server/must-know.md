# 决策点 · 阶段 B：用 Rust 替换 `ui/server`

> 2026-10-14。**这一份是给 AI 用的裁刀，不是给你答的题。**
> 阶段 B 的边界在调研 002 里已经定过：B = 3001 浏览器 API（`findings-q1q2-boundary.md:8, 20-24`），顺序先 C 再 A 再 B（`docs/research/002_parity-boundary/decision.md:58`）。本目录只回答「阶段 B 自己怎么落地」。
> 行号均相对 `resources/PilotDeck/`（基线 @ `cc7c4e6`，本 worktree 已回源核对）。

## 我要作的决定

**Rust 以什么形态站上浏览器唯一入口 3001、按什么粒度从 TS 手里接路由、进程内 import `src/` 的 38 处怎么处置、拿什么装置证明「接过去之后浏览器行为没变」。**

## 一、这件事上，我不可妥协的是哪几条

| # | 不可妥协的 | 为什么它是命门 | 怎么算满足 |
|---|---|---|---|
| 1 | **浏览器 `ui/src` 一行不改**，Rust 对浏览器完全等价于原 `ui/server`：HTTP、无类型 WS、SSE、静态资源、鉴权语义全保真 | 本次重构的红线（`.42cog/intent.md`「不做什么」）。浏览器唯一入口是 3001（`findings-q1q2-boundary.md:8`）；入口以下随便改，入口以上（`ui/src`）不许动 | `ui/vite.config.js:131-144` 代理的 `/api` `/ws` `/shell` `/memory-dashboard` 四个前缀 + `ui/dist` 静态资源全部经 Rust 可用；`ui/e2e` 里不 mock 后端的支线可跑；人用真浏览器验收 |
| 2 | **能逐路由从 TS 切到 Rust，任何时刻可回退**；未接管的 HTTP / SSE，以及**整条** WS 连接，原样反代给 TS `ui/server` | `ui/server` 有 264 条路由注册（见本目录 `route-inventory.md`）与 3 个 WS 端点，不可能一次写完；`/ws` 上有连接级内存态（`index.js:2364` `connectedClients`、`index.js:2388` `sessionWatchRegistry`、`index.js:2337-2341` 断线把帧交给替补连接），**只能整条连接转发，不能按消息 type 拆**；**路由之间还共享资源**：`pilotdeck.yaml` 的写者不止 `/api/config`（`routes/memory.js:79` 写同一文件），进程内事件 `pilotdeck:config-broadcast`（`routes/config.js:561`、`routes/gateway.js:124` 发出，`index.js:307` 监听 → `broadcastConfigReloaded`）反代搬不动 | Rust 站 3001、TS `ui/server` 挪私有高位端口（**显式 `HOST=127.0.0.1`**，`index.js:3584` 默认 `0.0.0.0`，回环靠绑定不靠路径+token）；**迁移组按共享资源（同一文件 / 同一 SQLite / 同一进程内事件）划分，同组同批，或先补跨进程广播桥**；每接管一组路由，对拍一致才删对应转发；关掉 Rust 直接回 TS |
| 3 | **「一致」能由机器判、同一输入可复现**：HTTP 请求/响应、无类型 WS 帧、SSE 事件、落盘副作用四类观测点可录可比，且有阳性对照 | 3001 的 WS 是**无类型** JSON（`index.js:2373` 起 23 种，见 `route-inventory.md`），SSE 是流式（`index.js:1043`、`routes/projects.js:542`），进程内还有 38 处 `src/` 依赖；分母是静态手工清点、随 env / 工厂实例化变；判不了就只能凭感觉（阶段 C 的教训：装置先于代码，`docs/plan/001`） | 录放装置 + 窄归一化（ID / 时间 / 随机字段一致映射；**录放层同一连接单调帧序 `seq` 保留并断言全序**）+ mock 模型服务 + 阳性对照（含换序、SSE 丢事件/攒流延迟等注入）；**分母以运行时枚举（dump Express router stack）为准并与静态清单对账**；**验收含「路由 × 用例」覆盖矩阵硬门**（每条 Rust 路由 ≥1 条直接 HTTP 用例或显式豁免，装置记录处理侧）；规约**另立**（`specs/ui-server-parity/`），**不动**已冻结的 `specs/parity/` |

## 二、知道了它们，我要作的是哪一个决定

> **Rust 站在 3001 前的姿势（逐路由接管 + 未实现反代给 TS，还是整体替换）、迁移按什么共享资源分组、分母怎么来、38 处进程内 `src/` 依赖怎么落地、对拍装置搭在哪、鉴权 / `auth.db` / SSE / 进程编排怎么对齐。** 见 `decision.md`。

## 三、哪些看起来相关，但知道了也不改变这个决定

- 浏览器 `ui/src` 的内部实现（组件、状态管理、样式）——只看它**怎么调后端**（HTTP 路径、WS 消息字段、SSE 事件名），不改它
- `ui/server` 各业务功能（StaffDeck、Always-on、记忆、TaskMaster）的内部语义——按路由组迁移时再逐组读
- `apps/desktop`（Electron 壳）——它直接 `require` 了 `ui/server/services/pilotdeckConfig.js`（`apps/desktop/src/main.ts:941`），阶段 B 要登记这个旁路，但不改桌面端
- 性能、并发、Docker / 安装脚本
- 参考仓里与「HTTP/WS 反代、无类型消息对拍、JWT 兼容」无关的功能

## 四、我的预判（待核实）

| 我怀疑 | 核实结果 |
|---|---|
| 浏览器会直连 18789 gateway | **不成立**：只连 `ui/server` 3001，`ui/server` 是 gateway 唯一客户端（`findings-q1q2-boundary.md:8, 16`） |
| 3001 上的 WS 可以按 message type 拆给两个后端 | **不成立**：连接级内存态在 `ui/server` 进程内（`index.js:2364, 2388, 2337-2341`），拆连接即丢状态 |
| `ui/server` 可以脱离 supervisor 单独起 | **成立**：非监督模式下 gateway 状态 `unmanaged`（`ui/server/services/runtimeCoordination.js:12-14`），监听端口读 `SERVER_PORT`（`ui/server/index.js:3583`） |
| 端口 / 数据库 / 数据目录都能重定向 | **成立**：`SERVER_PORT`（`index.js:3583`）、`DATABASE_PATH`（`ui/server/database/db.js:26`）、`PILOT_HOME`（`ui/server/index.js:190`） |
| JWT 能与 Rust 兼容 | **成立但要对齐细节**：secret 在 `auth.db` 的 `app_config`（`db.js:589-593`），HS256、claims `{userId, username}`、`expiresIn 7d`、过半衰期回 `X-Refreshed-Token`（`ui/server/middleware/auth.js`） |
| `ui/e2e` 的 fixture 能直接当后端对拍 | **部分**：`page.route` 伪造 `/api`、根本不碰后端（`ui/e2e/` 11 个文件）；只有 `ui/e2e/real/` 碰真后端且要真模型（`real/launch.mjs:16-19`） |
| 进程内 38 处 `src/` import 是同一类，可以一把改走 gateway 协议 | **不成立**：至少分三类（协议/类型与映射、gateway 背书的数据访问、本地配置与 Always-on 服务），处置方式不同（`decision.md` #2） |
| 写路由先迁 Rust、WS 仍留 TS，配置写触发的广播照样能到浏览器 | **不成立**：广播是进程内 EventEmitter 事件 `pilotdeck:config-broadcast`（`routes/config.js:561`、`routes/gateway.js:124` 发出，`index.js:307` 监听），反代只能搬字节、搬不动进程内事件；必须与 `/ws` 同组迁移或补跨进程广播桥（`decision.md` #7） |
| 264 是稳定可复算的分母 | **不成立**：工厂路由、条件挂载、4 前缀循环挂载、未挂载/遮蔽会让分母随 env 与实例化变；以运行时枚举（dump Express router stack）为准并与静态清单对账（`route-inventory.md` §9-10） |

---

**放行**：三条命门都能被「有 / 部分 / 没有」回答、每条证据带行号，可以进 `gap.md`。
