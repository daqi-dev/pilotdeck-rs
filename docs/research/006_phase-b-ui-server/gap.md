# 差距表 · 阶段 B：用 Rust 替换 `ui/server`

> 2026-10-14。被查 `resources/PilotDeck` @ `cc7c4e6`，本 worktree 逐条回源核对；**给不出行号的结论不写**。
> 行号相对 `resources/PilotDeck/`。分级：P0 最高；P0/P1 清零才开工，P2 酌情。
> 上一轮（调研 002 的 `gap.md`）回答「边界选哪条」；本表只回答「阶段 B 的三条命门还差什么」。路由与 WS 的完整分母见 `route-inventory.md`。

## 一、基线对三条命门

### 命门 1 · 浏览器 `ui/src` 一行不改，Rust 对浏览器完全等价于原 `ui/server`

**状态：入口干净、端口 / 库 / 数据目录全部可由环境变量重定向；但「等价」从未实证，且 HTTP 面很宽。**

| # | 事实 | 证据 | 差距是什么 | 修起来要什么 | 分级 |
|---|---|---|---|---|---|
| 1.1 | 浏览器只连 `ui/server`（Express + `ws`，默认 3001），不直连 18789 gateway；开发态 vite 把 `/api`、`/ws`、`/shell`、`/memory-dashboard` 四个前缀代理到 3001 | `docs/research/002_parity-boundary/findings-q1q2-boundary.md:8, 13`；`ui/vite.config.js:131-144` | Rust 顶替 3001 后要复刻 Express 的**路由优先级与中间件顺序**（如 `/api/projects/files` 必须在 `/api/projects` 通用路由之前，`index.js:545-547, 589`）、错误 JSON 形状、CORS 暴露头 `X-Refreshed-Token`（`index.js:512`） | 探针先证「纯反代下浏览器一切照常」，再逐路由对拍；Express 语义差异用录放装置抓 | **P0** |
| 1.2 | 路由分母很大：264 条路由注册（含 4 条 `router.route(...).get().post()` 链式；其中 1 条未挂载，静态可用 263）+ 3 个 WS 端点（`/ws`、`/shell`、`/plugin-ws/*`）+ SSE；`/api` 下 22 组挂载在 `index.js:540-648`；**最终分母以运行时枚举为准（见 3.6）** | `route-inventory.md`；`index.js:540-648`；`index.js:2299-2304`（WS 路径分发） | 不可能一次整体替换：要有一条「Rust 在前、未实现反代给 TS」的渐进路径，且每次只动一组 | 见命门 2 与 `decision.md` #1 | **P0** |
| 1.3 | 端口、鉴权库、数据目录可重定向，且 `ui/server` 可脱离 supervisor 单独起：`SERVER_PORT`（`index.js:3583`）、`HOST`（`index.js:3584`，**默认 `0.0.0.0`**）、`DATABASE_PATH`（`database/db.js:26`）、`PILOT_HOME`（`index.js:190`）；非监督模式下 gateway 状态 `unmanaged`（`services/runtimeCoordination.js:12-14`） | 上述行号 | 无（这是探针可隔离运行的前提）；但「私有回环端口」不是自动的——`HOST` 不显式设 `127.0.0.1` 就仍监听所有网卡 | 探针与对拍装置沿用这套 env 并**显式设 `HOST=127.0.0.1`**；端口一律高位；就绪时断言监听只在回环 | P1 |
| 1.4 | 有一条**桌面旁路**：Electron 主进程直接 `require` 了 `ui/server/services/pilotdeckConfig.js`（不经 HTTP），该文件又 import `src/` | `apps/desktop/src/main.ts:941`；`ui/server/services/pilotdeckConfig.js:1, 8-10, 14` | 阶段 B 内 Rust 不接管这条旁路；只要该 TS 文件还在，TS 的 `src/` 就仍被桌面端依赖 | 本阶段只登记、不改桌面端；`decision.md` 编排项说明 | P2 |

### 命门 2 · 能逐路由从 TS 切到 Rust，任何时刻可回退

**状态：可回退的机制在基线里现成（`/plugin-ws/*` 就是一条纯反代样板）；硬点在 `/ws` 的连接级内存态与「Rust/TS 同时写 `~/.pilotdeck`」。**

| # | 事实 | 证据 | 差距是什么 | 修起来要什么 | 分级 |
|---|---|---|---|---|---|
| 2.1 | `/ws` 是**无类型** JSON，且连接上挂进程内状态：`connectedClients` 集合、`sessionWatchRegistry` 的 session→socket 注册、`WebSocketWriter.send` 在原连接断开后把后续帧交给同用户的替补连接 | `index.js:2373-2635`（消息处理）；`index.js:2364`；`index.js:2388, 2395-2399, 2519`；`index.js:2337-2341`；`index.js:2645-2651`（断线清理） | 不能按 message type 拆给两个后端——连接一拆，watch 注册与断线接管语义就错 | **整条 WS 连接反代**；基线已有可抄的样板：`handlePluginWsProxy` 双向逐帧转发、双向传播 close（`index.js:2250-2288`） | **P0** |
| 2.2 | HTTP 路由与 WS 广播互相依赖：HTTP 处理里往 WS 广播（删会话广播 `session-deleted`、满额被删的队列项广播给同用户所有 socket、配置写 / 重载广播 `config:reloaded`、taskmaster CLI 路由广播） | `index.js:966`；`index.js:2462-2466`；`routes/config.js:527, 756, 995, 1038`；`routes/gateway.js:124`；`routes/taskmaster.js:1025, 1133, 1210, 1263, 1363` | 反代层必须**字节级透传**、不能重排或合并；**且 `pilotdeck:config-broadcast` 是进程内 EventEmitter 事件**（`routes/config.js:561`、`routes/gateway.js:124` 发出，`index.js:307` 监听 → `broadcastConfigReloaded`，`index.js:298-306` 写 `connectedClients`），反代搬不动——写路由先迁 Rust、WS 仍留 TS 时广播直接断 | 反代只做搬运；把「同一路由只由一侧处理」升级为**按共享资源分组、同组同批**，或先补**跨进程广播桥**（`decision.md` #7） | **P0** |
| 2.3 | 鉴权与 `auth.db` 绑定：JWT secret 由 `app_config` 表按需生成（64 字节 hex），HS256、claims `{userId, username}`、`expiresIn 7d`、过半衰期回 `X-Refreshed-Token`；密码 bcrypt 12 轮；默认关本地鉴权（`DISABLE_LOCAL_AUTH` 非 `0`）；WS 用 query `?token=` 或 `Authorization` 头，`verifyClient` 鉴权 | `database/db.js:26, 589-593`；`middleware/auth.js`（`getOrCreateJwtSecret` 使用、`jwt.verify`、`generateToken`、auto-refresh）；`routes/auth.js:40-140`（register/login，`bcrypt` 导入 :2、`hash` :71）；`constants/config.js:8-16`；`index.js:478`；`index.js:491-504` | Rust 接管鉴权路由时必须读**同一个** `auth.db`、用**同一套** JWT 语义；两侧绝不能同时处理同一条鉴权路由（写 `app_config` / `last_login` 会竞争） | Rust 侧 SQLite + bcrypt + HS256 实现；按 G-AUTHDB 共享资源组协调写者（同组同批或单一写者代理，`decision.md` #7）；对拍含 login/register/刷新 | **P0** |
| 2.4 | SSE 是流式、要逐块转发且不缓冲：搜索会话推送 `result/progress/done/error`；项目 clone-progress 推 `data:` JSON；上传事件推 `upload_*`；`/api/agent` 在 `stream=true` 时把同一 POST 升级成 event-stream；模块 host 能力透传上游 SSE/NDJSON；`GET /api/modules/staffdeck-sdk/events`（`operation=job_events`，`routes/modules.js:450` 的 `streamOnly` 分支）支持 `Last-Event-ID` / `?lastEventId=` 与 `preview_job_events` 的 `?afterSeq=` 续传（`POST .../call` 是写调用分支、不读这些头） | `index.js:1033-1060`（`text/event-stream` 在 :1043）；`routes/projects.js:542-555`；`routes/uploads.js:137-155`；`routes/agent.js:910, 1184`；`routes/modules.js:100-110, 336-339, 449-450` | 反代若缓冲或改写 framing，前端 EventSource 行为即变；`X-Accel-Buffering: no` 等头要保留（`index.js:1046`）。**比对口径原先未定事件到达延迟**：反代把整条流攒到结束才发时，仅比事件序列仍会绿（B1 的手工时间戳不进自动对拍）；反过来把 `read()` 分块数/分块边界当硬门，又会被传输层自然合并判红（假红）；`Last-Event-ID` 续传未入场景集 | Rust 用流式 body 透传、不丢 / 不并事件；规约冻结 SSE 比对口径（**完整事件序列** + **到达延迟上界**；`read()` 分块元数据仅诊断，不作硬门）；续传补一条 GET 场景 | P1 |
| 2.5 | 同一份 `~/.pilotdeck` 被多处直读写，且**共享资源的写者横跨多条路由**：写 `pilotdeck.yaml` 的有 `routes/config.js:518, 747, 916, 979, 1517`、`routes/memory.js:79`、`routes/gateway.js:113`、`routes/onboarding.js:480`；写 `auth.db` 的有 `routes/auth.js`（register/login 的 last-login）、`routes/settings.js:2` 的 `apiKeysDb/credentialsDb/notificationPreferencesDb/pushSubscriptionsDb`、`routes/user.js:2` 的 `userDb`、`index.js:963, 995` 的 `sessionNamesDb`；此外删 transcript 与项目目录（helper 模块 `ui/server/projects.js:568, 576, 593`，HTTP 入口 `index.js:954, 1004`）、插件目录（`routes/plugins.js:152, 300`）、上传落 `<projectKey>/.tmp/chat-uploads`（`routes/uploads.js:106` + `src/gateway/dialog/UploadStore.ts:92-93`） | 上述行号；落盘根可由 `PILOT_HOME` 覆盖（调研 002 findings-q3-q6:21） | Rust 与 TS 同时写同一文件 / 同一 SQLite = 竞争；**「一条路由只有一个写者」不够**——`routes/memory.js:79` 与 `/api/config` 写同一个 `pilotdeck.yaml`，`auth.db` 更被多组路由共享 | **按共享资源（同一文件 / 同一 SQLite / 同一进程内事件）分组，同组同批迁移；或指定单一写者侧、另一侧经协议代理**（`decision.md` #7）；对拍用各自临时 `PILOT_HOME`；回退即整组回退到 TS | **P0** |

### 命门 3 · 「一致」能由机器判、同一输入可复现

**状态：四类观测点都在，但 3001 侧没有任何现成对拍装置；现成测试一半是内存 express、一半是 `page.route` 伪造。**

| # | 事实 | 证据 | 差距是什么 | 修起来要什么 | 分级 |
|---|---|---|---|---|---|
| 3.1 | HTTP 观测点：全量 JSON API（含 50mb body、`multipart/form-data` 走 multer 跳过 json 解析）、错误 JSON、SPA 静态兜底 | `index.js:513-524`；`index.js:3461`（`/api` 404 兜底）；`index.js:3471`（SPA 兜底） | 无运行时 schema；只能靠录制金样本 + 归一化 diff；**且默认不知道请求到底由谁处理**——Rust 前置时反代全绿会掩盖「路由其实还没接管」 | 录 (method, path, query, 认证后 header, body, status, resp header/body)；ID/时间/token 窄归一化；**装置记录处理侧**（Rust 对自持路由加 `X-PilotDeck-Handler: rust`，反代路径标 `ts`；比对前剥离该头、覆盖检查读取该头） | **P0** |
| 3.2 | 无类型 WS：`/ws` 23 种入站 type、`/shell` 3 种；出站帧同样无 schema | `route-inventory.md`；`index.js:2379-2635`；`index.js:2683, 2981, 2994` | 没有类型可校验；只能录成金样本，并对连接级时序（watch/断线/替补）单独建场景。**原先「按 `type` 分组比结构」会把顺序打散**，同一连接内的错序（如 `final` 先于 `delta`）可能被掩盖 | 帧级录制 jsonl（方向 + **录放层同一连接单调帧序 `seq`**）。比对两层、顺序优先：① **同一连接内全序帧序列**（含 `type` 交错）；② 每种 `type` 的归一化结构。**录放层 `seq` 是排序键，保留并断言，绝不归一化**；仅应用负载里的 `createdAt`/`entryId` 归一化；阳性对照加「交换两帧顺序 → 必 FAIL」 | **P0** |
| 3.3 | 现成测试**不能直接当对拍**：`ui/server/*.test.*` 80 个是 vitest 里自建 `express()` 只挂单个路由；`ui/e2e` 46 个多数 `page.route` 伪造 `/api`、不碰后端；`ui/e2e/real/` 才 spawn 真 supervisor 且要真模型 | `docs/research/002_parity-boundary/findings-q3-q6-contracts.md:11-15`；`ui/e2e/real/launch.mjs:16-19, 66-84`；本 worktree 统计 `ui/e2e/` 11 个文件用 `page.route` | 后端对拍装置要从零搭；`ui/e2e` 的 `page.route` fixture **可以复用**，但用途是「证明前端渲染不变」，不是后端一致性 | `decision.md` #3：另立 `scripts/ui-server-parity/` 与 `specs/ui-server-parity/` | **P0** |
| 3.4 | 落盘副作用观测点齐：transcript `projects/<projectId>/chats/<session>.jsonl`、`pilotdeck.yaml`、`auth.db`、`plugins.json`、上传目录；根目录可由 `PILOT_HOME` 整体替换 | 调研 002 findings-q3-q6:21-26；`database/db.js:26`；`routes/plugins.js:115`；`routes/uploads.js:106` | 格式混合（jsonl / yaml / sqlite / 目录），且无统一迁移 | 对拍步骤：目录树 + 归一化内容快照 diff | **P0** |
| 3.5 | 非确定性集中在 `ui/server`：`Date.now` 59 处、`new Date` 82 处、`randomUUID` 31 处、`Math.random` 6 处 | `docs/research/002_parity-boundary/findings-q3-q6-contracts.md:38-41` | 黑盒对拍不能靠注入 `now`/`uuid`（进程内注入点用不上） | mock 模型服务 + 窄归一化（照阶段 C / prime-agent 的做法） | P1 |
| 3.6 | 分母（264）是静态手工清点，随 env / 实例化变：工厂路由（`routes/modules.js:51`、`routes/models.js:22`、`routes/update.js:110`）、条件挂载（`routes/modules.js:58` 的 `STAFFDECK_PROJECT_BINDING_ENABLED`）、4 前缀循环挂载（`index.js:669`）、未挂载/遮蔽（`routes/update.js:112`、`routes/mcp.js:413`）；且 6 个场景驱动不到 git 20 条、memory 27 条等大量路由 | `route-inventory.md` 汇总与口径；上述行号 | 按文档计数验收 = 假覆盖：静态清单可能漏项、工厂可多次实例化、条件分支改变可达性；没有「每条 Rust 路由 ≥1 用例」的门，整组未驱动也会绿 | 装置在隔离 env 下加载真 `ui/server`，**dump Express router stack 运行时枚举**并与静态清单对账（只静态 / 只运行时 / 前后缀不一致），**以枚举为分母**；验收加**路由 × 用例覆盖矩阵硬门**（遗漏或未豁免即 FAIL） | **P0** |

## 二、候选之间 —— Rust 前置的两种姿势

| | 甲：Rust 反代站 3001，**逐路由接管**、未实现反代给 TS | 乙：**整体替换**（一次写完 264 条路由 + WS） |
|---|---|---|
| 改动面 | Rust 加一个上游代理 + 一张路由归属表；TS `ui/server` 原样跑在私有端口 | 一次性重写全部 HTTP/WS/SSE/鉴权，TS 进程立即退役 |
| 风险面 | 低：单条路由不一致可单独回退；反代层透明 | 极高：任何一条路由不一致都会同时暴露；无回退点 |
| 能证明什么 | 命门 1（入口等价）与装置本身（纯反代下应零差异）先行 | 只有「全部写完」才知道行不行 |
| 进程内 38 处 `src` | 宿主路由被接管前继续可用（TS 还活着） | 必须一次性全部有归宿 |
| 定位 | **第一步**（探针 / Issue B1） | 不选 |

## 三、该不该自己跑 —— 要跑的实验

| 实验 | 为什么必须跑 | 会改变哪个取舍 |
|---|---|---|
| **探针（计划 003 的 Issue B1）**：真 `ui/server` 跑高位端口，Rust 反代站在另一个高位端口前，真浏览器经 Rust 访问 | 命门 1 的放行条件。调研 002 与 exp002 全是读源码，运行时行为（Express 路由优先级、WS 反代、SSE 透传）没验证过 | 若走不通，「逐路由接管 + 未实现反代」与「前端一行不改」都要重想 |
| **鉴权互通实验**：Rust 读同一 `auth.db`、签发/校验 JWT、过半衰期刷新 | 鉴权是每条 `/api` 路由的前置；不先证通，任何路由接管都跑不起来 | 决定 Rust 是「自己签 JWT」还是「反代鉴权给 TS」（`decision.md` #4） |
| **无类型 WS 录放实验**：录一条真实会话的 `/ws` 帧序列，Rust 反代与 TS 直连各跑一遍，归一化 diff | 命门 3 的放行条件；无类型消息的比对口径只能从实测里长出来 | 决定 WS 接管策略与对拍口径 |
| **运行时枚举对账**：隔离 env 下加载真 `ui/server`，dump Express router stack + 加载期 hook，与 `route-inventory.md` 静态清单对账 | 分母直接决定「覆盖率门」有没有意义；静态清点已出现过漏链式路由的错 | 决定分母、路由归属表生成规则、覆盖矩阵的条目集 |
| **广播桥与文件 watcher 实验**：Rust 写 `pilotdeck.yaml` 后，TS 的 chokidar watcher（`index.js:3720s`、事件从 `index.js:3724` 发）是否已广播、payload 与 `config.js:527/995` 的 `ui-save` 是否同形、是否与显式桥调用重复 | 决定 G-YAML 能否在 `/ws` 之前迁、跨进程广播桥要不要显式调用、`suppressNextWatchEvent`（`routes/gateway.js:115`）语义怎么复刻 | `decision.md` #7 的桥契约 |

> 已冻结的 `specs/parity/`（阶段 C 的尺子）**不动**；阶段 B 的对拍规约另立（`specs/ui-server-parity/`），人批后冻结。

## 四、B1 探针带回的新事实（2026-10-14，exp008）

| # | 事实 | 证据 | 对阶段 B 的影响 | 分级 |
|---|---|---|---|---|
| 4.1 | Express 4.22 的 `express.Router()` 实例原型**不等于** `express.Router.prototype`（`Object.getPrototypeOf(r) !== Router.prototype`），加载期 hook 包 `Router.prototype.use/.route` **一条都拦不到** | exp008 `enumerate_hook.mjs` 首版 0 命中；改用 dump `app._router.stack` + `layer.regexp` 还原挂载前缀后得 263 条 | B2 的 `enumerate_routes.mjs` 不能只靠原型 hook，必须以 stack dump 为准、hook 仅作交叉校验（或改从 `layer.handle` 反推） | P2 |
| 4.2 | 基线树只读且无 `ui/dist`；Vite dev 默认在 `ui/node_modules/.vite` 写缓存/临时配置，直接启动即 EPERM | exp008 `vite.log`：`.vite-temp` / `.vite/deps_temp_*` mkdir 失败 | B2 的浏览器场景要走 dev server 时，必须把 `cacheDir` 指到可写目录并用 `--configLoader runner`（否则配置 bundling 会写在基线内）；生产静态面要另想（B3 由 Rust 服务 `ui/dist`） | P1 |
| 4.3 | 沙箱内 node-pty `spawn` 报 `posix_spawnp failed`，`/shell` 无法真正起 PTY | exp008 `ui.log`：「Error spawning process: Error: posix_spawnp failed.」 | PTY 哨兵/退出/重连（场景 5）在本机沙箱内无法完整验收；B2 的 PTY 用例需在能起 PTY 的环境跑，或显式豁免并留原因 | P1 |
| 4.4 | 真浏览器加载 UI 后 `/ws` 未立即建立（Playwright 只看到 Vite HMR socket）；应用按需开 WS | exp008 `browser.json` `appWebSockets: []` | 浏览器场景证明 HTTP 面经 Rust；`/ws` 全帧对拍仍需驱动脚本（B1 用 `driver.mjs` + Rust tee 证明） | P2 |
| 4.5 | 直连与经反代两侧**共用一个 TS 进程**时，列表类端点会漂移：`/api/projects` 的 `x-projects-revision`/`lastActivity` 随墙钟变，`sessionMeta.total` 会把另一侧刚建的会话算进来（直连先录 WS 后，反代再读就多 1） | r1 `comparison.json` 首跑差异：`header.x-projects-revision`、`body[0].lastActivity` 类型/值、`body[0].sessionMeta.total` 0↔1 | B2 的 `compare.py` 要判 HTTP 面，须先决定「两侧各自冷启动后端」，否则不能把有状态列表端点纳入硬门；r1 先排除、只比确定性只读，并在 readme 记明 | P2 |
| 4.6 | 中继录放归一化里，**单调计数器**（`revision`/`generation`）不能和身份 ID 一样做「首次出现序号映射」：两个 fresh session 的绝对 revision 起点不同，序号会发散成 `<revision:20>` vs `<revision:24>`，制造假红 | r1 `compare.py` 首跑 `ws[chat-raw] #8.body.activity.revision`；改为塌缩 `<VOLATILE>` 后 PASS | B2 的 `specs/ui-server-parity/normalization.md` 应把「身份类→有序映射、计数/时钟类→塌缩、SSE `sseId` 是续传锚点不归一化」写成条目 | P2 |
