# Q1 协议边界 · Q2 能否转发 —— 核查结果

> 2026-10-14。被查：`resources/PilotDeck` @ `cc7c4e6`。由读源码的子 agent 产出（冒烟那一路），主会话抽查了 3 条（标 ✔）。
> 行号均相对 `resources/PilotDeck/`。

## 预判被证实：前端之下还有一层 Node 服务

浏览器只连 **`ui/server`**（Express + ws，默认 3001），不直连 `src/gateway`（默认 18789）。

| 进程 | 谁起 | 监听 | 证据 |
|---|---|---|---|
| `ui/server/index.js` | `webRuntimeSupervisor.js` | 3001 | `ui/server/webRuntimeSupervisor.js:31-38` |
| vite（开发） | 同上 | — | `:41-48`；`/api` `/ws` 代理到 3001：`ui/vite.config.js:131-144` |
| `pilotdeck.ts server`（gateway） | 同上 | 18789 ✔ | `:50-56`；`src/gateway/server/GatewayServer.ts:71` |

`ui/server` 是 gateway **唯一的客户端**：`ws://127.0.0.1:18789/ws` ✔（`ui/server/pilotdeck-bridge.js:77-78, 221`）。

## 两条边界，长得完全不一样

| | 浏览器 ↔ `ui/server`（3001） | `ui/server` ↔ gateway（18789） |
|---|---|---|
| HTTP | 23 组 `/api/...`，约 280 处路由注册（`ui/server/index.js:540-648`） | 只有 `/health` `/auth/local-token` `/feishu/webhook` `/api/web/*`（`GatewayServer.ts:98-120`） |
| WS | `/ws` `/shell` `/plugin-ws/*`；`/ws` 约 25 种**无类型** JSON（`index.js:2373` 起） | **有类型**帧：hello / request / response / event / notification（`src/gateway/protocol/frames.ts:5, 123-162`），约 100 个 method，载荷类型 `types.ts`（1666 行） |
| 鉴权 | JWT，secret 存 SQLite `auth.db`（`ui/server/database/db.js:589-593`）；桌面端默认关鉴权（`apps/desktop/src/main.ts:172`） | 本地密钥 `~/.pilotdeck/server-token`（`src/gateway/server/authToken.ts:35-38`），`hello` 帧里比对 |

## 会让「透明转发」失效的东西

| 项 | 有没有 | 证据 |
|---|---|---|
| `ui/server` 进程内直接 import `src/` | **有，最严重**：16 个非测试文件、38 处 ✔ | 例：`pilotdeck-bridge.js:60`、`routes/config.js:34-37`、`services/modelFreeHistory.js:1-3` |
| `ui/server` 直接读写后端数据目录 | **有** | 写 `~/.pilotdeck/pilotdeck.yaml`（`services/pilotdeckConfig.js:37, 956`）；删 transcript 与项目目录（`projects.js:568-593`） |
| WS 状态与 HTTP 互相依赖 | **有** | HTTP 路由往 WS 广播（`index.js:966`、`routes/taskmaster.js:1024`）；`sessionWatchRegistry` 内存态（`index.js:207-217`）；`hello_ok` 带连接级 `interactionBinding`（`frames.ts:18`） |
| 同源 cookie | 没有 | Bearer JWT 存 localStorage（`ui/src/utils/api.js:37, 55`） |
| 后端回调前端端口 | 没有 | 但有 IPC 门控：`services/runtimeCoordination.js:46-50` → supervisor 决定是否起 gateway（`webRuntimeSupervisor.js:188`） |
| 桌面端绕过 | 部分 | 窗口加载 `ui/server`（`main.ts:622`），但主进程直接 `require` 了 `ui/server/services/pilotdeckConfig.js`（`:941`），后者又引 `src/` |

## 规模更正

`ui/server` 自身约 **5 万行** JS/TS（含测试）✔，之前被算进「前端 13.7 万行」里。

## 子 agent 的判断：部分可行

- **首选**：在 18789 gateway 边界切。只有一个客户端、协议有类型、帧带 `id`，可按 `method` 分派；Rust 用同一个 `server-token` 终止 `hello`，TS gateway 挪到别的端口，先接 `/health` 与只读方法，其余原帧转发。
- **备选**：Rust 站在 3001 前面反代整条 `/ws` `/shell` SSE，先切无状态 HTTP 路由。
- **风险**：① `ui/server` 进程内引 `src/`，gateway 全换成 Rust 后 TS 的 `src/` 仍删不掉 ② 单条 WS 上有连接级状态（`interactionBinding`、事件 `seq`、权限与 elicitation 绑定在连接上），按 method 拆到两个后端会对不上 ③ Rust 与 TS 同时读写 `~/.pilotdeck`，且 supervisor 与桌面端硬编码了 `pilotdeck.ts server`（`webRuntimeSupervisor.js:53`、`main.ts:251-258`）。
