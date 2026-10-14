---
type: plan
number: "003"
date: 2026-10-14
title: 阶段 B：Rust 替换 ui/server——反代探针先行、对拍装置其次、逐路由接管
tags: [phase-b, ui-server, proxy, parity]
status: draft
related: [research/006_phase-b-ui-server, research/005_phase-a-gateway, research/002_parity-boundary, plan/001_phase-c-agent-loop-sidecar, plan/002_phase-a-gateway]
---

# 阶段 B 计划

## Context

为什么这样设计见 `docs/research/006_phase-b-ui-server/`（`must-know` / `gap` / `decision` / `route-inventory`，行号均已回源核对）。一句话：**浏览器 `ui/src` 一行不改**，Rust 以反代站上浏览器唯一入口（默认 3001；实验一律高位端口），**逐路由**从 TS `ui/server` 手里接管；未接管的 HTTP / SSE 与整条 WS 连接反代给跑在私有回环端口的 TS `ui/server`。取舍卡九条代拍（切法 / WS 切法 / 38 处 `src/` / 装置 / 鉴权 / 编排 / 迁移分组 / 分母与覆盖率 / 阶段门）均为 **AI 代拍，待人复核**（`006/decision.md`）。

**阶段门（AI 代拍 #7，待人复核）**：B1–B2（探针、装置、规约）只读基线、只写本仓 `scripts|docs|specs`，不写产品代码，可与阶段 A / C 并行；**B3 起（Rust `ui-server` crate 进 `src/`）严格等阶段 A 对拍达标**（`.42cog/intent.md`「不做什么」红线收窄的原话：「前一阶段对拍达标之前不进下一阶段」）。并行期全量对拍同一时刻只跑 1 个（`CLAUDE.md` §五）。

**D5 待批补充**：为避免“完整 UI 恢复等 B 修复、B3 又等 A 完整验收”的互等，[008 decision D5](../research/008_phase-a-session-domain/decision.md) 建议以 A8「gateway 退役验证完成（不含 UI 自动恢复）」作为 B3 的 A 侧闸门，证据清单见 [计划 005](005_phase-a-session-domain.md)；此解环建议**须人批准，未批准前维持上述原闸门**。上段按原文保留，其所引串行原话在当前 `.42cog/intent.md:24` 已划掉并另记并行说明；该现状不构成对 D5 的批准。

## 现状（已确认）

| 项 | 状态 | 证据 |
|---|---|---|
| 入口边界 | 浏览器只连 `ui/server`（3001，Express + `ws`）；vite dev 代理 `/api`、`/ws`、`/shell`、`/memory-dashboard` 四前缀 | `findings-q1q2-boundary.md:8, 13`；`ui/vite.config.js:131-144` |
| 分母 | 静态清单 264 条路由注册（1 条未挂载）+ 3 个 WS 端点 + 26 种入站 WS type + 5 组 SSE；**最终分母以运行时枚举（dump Express router stack）为准，与静态清单对账** | `006/route-inventory.md` 汇总表、§9-10 |
| 共享资源 | `pilotdeck.yaml` 由 `config.js` / `memory.js:79` / `gateway.js:113` / `onboarding.js:480` 多处写；`auth.db` 由 auth / settings / user / sessionNames 多组写；`pilotdeck:config-broadcast` 是进程内事件（`config.js:561` → `index.js:307`），**反代搬不动** | `006/decision.md` #7；`006/gap.md` 2.2, 2.5 |
| gateway token | bridge 读 `PILOTDECK_GATEWAY_TOKEN_PATH` 或 `<PILOT_HOME>/server-token`；gateway 侧 `ensureGatewayAuthToken` 写同一路径 → 两者必须共享同一 `PILOT_HOME` | `ui/server/pilotdeck-bridge.js:79-81`；`src/gateway/server/authToken.ts:14` |
| WS 连接级状态 | `/ws` 上有 `connectedClients`、`sessionWatchRegistry`、断线后帧交替补连接；**不能按 message type 拆** | `index.js:2364, 2388, 2337-2341, 2645-2651` |
| 可重定向 | `SERVER_PORT`（`index.js:3583`）、`DATABASE_PATH`（`db.js:26`）、`PILOT_HOME`（`index.js:190`）；非监督即 `unmanaged`（`runtimeCoordination.js:12-14`） | 上述行号 |
| 反代样板 | `handlePluginWsProxy` 双向逐帧搬运 + 双向传播 close，现成可抄 | `index.js:2250-2288` |
| 进程内 `src/` | 16 个非测试文件 38 处直接 import；分三桶（4 条 gateway 通道 / 其余 Rust 直接实现或随路由留守） | `006/decision.md` #2 表；`ui/server/index.js` 无直接 import |
| 鉴权 | JWT secret 在 `auth.db` 的 `app_config`；HS256 7d、过半衰期回 `X-Refreshed-Token`；bcrypt 12 轮；默认关本地鉴权 | `db.js:589-593`；`middleware/auth.js`；`routes/auth.js:40-140`；`constants/config.js:8-16` |
| SSE | 5 组流式 + 1 处 NDJSON；逐块转发、不得缓冲 | `006/route-inventory.md` §8 |
| 装置起点 | `ui/e2e/real/launch.mjs` 已用高位端口（3010/5180/18798）+ 临时 `PILOT_HOME`/`DATABASE_PATH` 编排整套进程，可直接当探针骨架 | `launch.mjs:11-14, 78, 83-84` |
| 环境红线 | 用户本机 PilotDeck 正占 3001 与 18789；实验一律高位端口 + 隔离目录；绝不 kill 用户进程 | `CLAUDE.md` §五 |

## 目标

阶段 B 的终点（`.42cog/intent.md` 作品区）：`pilotdeck` 二进制起在浏览器唯一入口后，原版浏览器 `ui/src` 零改动即可用，全部路由（**分母以运行时枚举为准，静态清单 264 条**）+ 3 个 WS 端点 + SSE 由 Rust 承担，TS `ui/server` 进程退役。**本计划覆盖到「B3 完成、第一组路由（无状态只读 + 鉴权）在 Rust 里跑通、拿到第一份 Rust 前置的对拍全绿」为止**——会话/聊天大域与 PTY 终端（深水区）届时已有实测数据，另开计划 004。

## 执行步骤

> 拆分逻辑与计划 001 / 002 相同：issue 之间隔着人的闸门（B2 冻结规约、B3 等阶段 A 达标）。**只先开 B1**，后续 issue 在前一条合并后再开。

> D5 待批补充：B3 改用 A8「gateway 退役验证完成（不含 UI 自动恢复）」作 A 侧闸门须人批准；未批准前维持上述原闸门，B1/B2 前置要求仍保留。

### Issue B1 · 探针：Rust 反代站在高位端口前，真浏览器经它访问 TS `ui/server` 一切照常（可立即开工，与 A/C 并行）

**S1 最小 Rust 反代**（实验代码住 `_tmp/<日期>-exp008/`，不进 `src/`——B3 才建 crate）
- 单 bin：`tokio` + `hyper`（HTTP 全量反代，含 body 流式）+ `tokio-tungstenite`（WS upgrade 后双向逐帧搬运、双向传播 close）。监听 `127.0.0.1:13001`，上游 `http://127.0.0.1:13002`。
- 三件事必须做对：① HTTP 逐跳头剥离、`X-Forwarded-*` 不乱加；② SSE / NDJSON body **不缓冲**、逐块转发（保留 `text/event-stream`、`Content-Type: application/x-ndjson`、`X-Accel-Buffering: no`，`index.js:1043-1046`）；③ WS 全帧 tee 到 jsonl（方向 + 单调序号 + 原文）。
- 第四件：**运行时枚举**——隔离 env 下加载真 `ui/server`（或反代 bin 提供 `--dump-routes`），dump Express router stack + 加载期 hook，产出 `router-enumeration.json`，与 `006/route-inventory.md` 对账（做法见 `route-inventory.md` §10）。
- 处理侧标记：Rust 对自持路由加响应头 `X-PilotDeck-Handler: rust`，反代路径标 `ts`（B1 阶段全是 `ts`），录制层读它、比对时剥离（`006/decision.md` #3）。

**S2 编排四个进程**（全部高位端口 + 隔离目录；隔离目录与高位端口照 `ui/e2e/real/launch.mjs` 的约定，但 **env 必须按进程分别构造**——`launch.mjs` 把同一份 env 透给 supervisor 再分给所有子进程，我们这里不能照抄，见下面 vite 一条；**gateway 与 `ui/server` 必须共享同一个 `PILOT_HOME` / token**）
- 公共：`PILOT_HOME=<tmp>/home`（**四个进程统一**）、`TMPDIR=<tmp>/tmp`；gateway 的 token 落在 `<tmp>/home/server-token`（`src/gateway/server/authToken.ts:14` 的 `ensureGatewayAuthToken`）。
- TS `ui/server`：`SERVER_PORT=13002`、**`HOST=127.0.0.1`**（`index.js:3584` 默认 `0.0.0.0`；「私有回环端口」必须由绑定落实，不能只靠路径 + token）、`DATABASE_PATH=<tmp>/auth.db`、`PILOTDECK_CONFIG_PATH=<tmp>/pilotdeck.yaml`、`PILOTDECK_GATEWAY_URL=ws://127.0.0.1:28790/ws`、**`PILOTDECK_GATEWAY_TOKEN_PATH=<tmp>/home/server-token`**、`PILOTDECK_DISABLE_LOCAL_AUTH=0`，`node --import tsx resources/PilotDeck/ui/server/index.js`。
- TS gateway：`PILOTDECK_GATEWAY_PORT=28790`、**同一 `PILOT_HOME=<tmp>/home`**，配 mock 模型服务（照 exp001 / prime-agent 的 `mock_provider.py`，`findings-prime-agent.md:20`）。
- vite：**单独一份 env，`SERVER_PORT=13001`（= Rust），与 TS `ui/server` 的 `SERVER_PORT=13002` 分离**——vite 用 `SERVER_PORT` 算代理目标（`ui/vite.config.js:57, 132-141`），若像 `launch.mjs` 那样把含 `SERVER_PORT=13002` 的同一份 env 透给 vite，vite 会直连 TS 13002、绕过 Rust，场景 1-6 的「经 Rust」全部假通过。另给 `VITE_PORT=15180`；代理 `/api /ws /shell /memory-dashboard` 因此指向 **Rust 的 13001**。
- Rust 反代：`13001 → 13002`。
- **就绪断言**：起完先断言 `<tmp>/home/server-token` 存在且 `0600`、bridge 的 `PILOTDECK_GATEWAY_TOKEN_PATH` 指向它、四个进程的 `PILOT_HOME` 展开为同一绝对路径；否则场景 4（一轮对话）会因 gateway 鉴权失败假红。**再断言监听地址为回环**：`lsof -nP -iTCP:13001,13002 -sTCP:LISTEN`（或 `ss -ltn`）只出现 `127.0.0.1`，不得出现 `0.0.0.0`/`*`——「私有回环端口」是绑定保证，不是路径与 token 的推论。

**S3 过场景并落盘结论** → `docs/experiments/exp008-ui-server-proxy/readme.md`
1. 静态：`GET /`（经 Rust）返回 UI；`/memory-dashboard` 可达。
2. 鉴权：`/api/auth/status` → `register` → `login` 发 JWT，浏览器 localStorage 存 token；带 token 的 `/api/auth/user` 通过。
3. 只读 API：`/api/projects`、`/api/projects/:p/sessions`、`/api/sessions/:id/messages`、`/api/config`、`/api/models` 经 Rust 结果与直连一致。
4. 一轮对话：mock 模型下 `pilotdeck-command` → 事件流 → `final`；前端渲染正常（经 Rust 的 `/ws`）。
5. 终端：`/shell` 经 Rust 起 PTY，`init` → `input` 灌**固定命令输出哨兵**，且**哨兵的完整字节不得出现在注入的输入文本里**——PTY 会把输入回显，`index.js:2950-2953` 的 `output` 帧直接转发 PTY 输出，若只断言「哨兵出现」，`printf 'PDSENTINEL_<nonce>'` 会被回显本身满足、命令没执行也绿。改用 shell 拼接生成：输入 `printf 'PDSENTINEL_%s\n' '<nonce>'`，期望输出 `PDSENTINEL_<nonce>`（该整串不在输入中，回显无论怎么分帧都产生不了它），断言它出现在 PTY 输出帧；`resize` 生效；再验 `exit`（会话结束 / close 传播）与**重连**（`terminalSessions` 复用语义）。
6. SSE：`/api/search/conversations`、`/api/projects/clone-progress`、`/api/uploads/:id/events` 经 Rust **逐步到达**；录制每次 `read()` 的 byte offset + 到达时刻作为**诊断元数据**（不是硬门），硬门是**完整事件序列不丢 / 不并 / 不乱序**且**每个事件的到达延迟 ≤ `k × 上游间隔 + m`**（口径见 `006/decision.md` #5）。**不得用 `read()` 分块数当硬门**——传输层自然合并分块不等于语义聚合，会比出假红。
7. SSE 续传：续传头只在 GET 流分支读取（`routes/modules.js:334-339` 的 `streamOnly`）——用 `GET /api/modules/staffdeck-sdk/events?operation=job_events` 带 `Last-Event-ID`（或 `?lastEventId=`），`preview_job_events` 带 `?afterSeq=`；`POST /api/modules/staffdeck-sdk/call`（`routes/modules.js:449`）是写调用分支、不读这些头，别用错。断言从指定 id/seq 之后续传、不重放、不丢。
8. 阴性 + 阳性对照：同一驱动脚本「直连 13002」与「经 13001」各跑一遍，HTTP 响应 / WS 帧归一化后一致；在反代里故意改写一个响应头、丢一帧 WS、或对 SSE 丢一个事件 / 把两个事件并成一个 / 把整条流攒到结束才发（到达延迟超上界），断言录放比对判 FAIL。
9. 运行时枚举与对账：隔离 env 下 dump Express router stack + 加载期 hook，产出 `router-enumeration.json`，与 `006/route-inventory.md` 对账，差异写进 readme「分母对账」节。
10. 路由穿透探针：`GET /api/projects/files`（`:547` 先于 `:589`）、`GET /api/sessions/model`（`:637` 先于 `:640`）、`GET /api/mcp/config/read`（被 `mcp.js:36` 遮蔽）各断言实际命中的 handler，经 Rust 与直连一致。

**怎么算做完**
- [ ] exp008 readme 各条场景各有证据（帧日志片段 / 截图路径 / SSE chunk 元数据）；失败场景如实记录并给出对架构的影响
- [ ] `router-enumeration.json` 与静态清单对账完成，`only-static` / `only-runtime` 差异逐条给原因，回填 `006/route-inventory.md`
- [ ] 全程未占用 3001 / 18789、未触碰真实 `~/.pilotdeck`、未 kill 用户进程（端口与目录清单写进 readme）
- [ ] 若有「读源码没料到」的行为差异（Express 路由优先级、SSE 缓冲、WS close 语义、gateway token 共享），逐条记入 readme「意外」节并回填 `006/gap.md`

### Issue B2 · 对拍装置（B1 合并后）

> Issue #47 分两次交付：本次 PR 只做 **S1 + S2**（录制、枚举、六场景与覆盖门），实验记录用 `docs/experiments/exp010-ui-server-parity-device/`；exp007 已被前端实验占用。**S3 比较器 + S4 规约/冻结/对照留给下一 PR**，下面的整条 Issue 验收项仍待完成。S1 请求/响应头必须保留同名多值与原始字节；Connection 提名头按字节解析，非法请求回 400；活动基准必须另建直连 TS 的带外观察连接，保留每个用户/会话的真实 snapshot 与 activity revision，不经被测入口取基准。

**S1 录制反代 + 运行时枚举**：在浏览器与被测入口之间放录制层（可与 B1 反代同一 bin）：落 (method, path, query, 去 token 值的 header, body) 与 (status, header, body / 流分块 + 每块 byte offset 与到达时刻；分块仅诊断) 到 jsonl；WS 按连接落全帧（含录放层单调 `seq`）。录制层读响应头 `X-PilotDeck-Handler`（`rust|ts`）标记处理侧，比对时剥离；分母来自 `scripts/ui-server-parity/enumerate_routes.mjs`（复用 B1 的 `enumerate_hook.mjs` stack dump）。**S2 场景集与覆盖矩阵**：从 `ui/e2e` 里不 mock 后端的支线 + exp008 场景提炼，先 6 个基础场景（鉴权、项目/会话只读、一轮对话（mock 模型）、断线重连 watch、配置写+广播、上传+SSE）；**同时为每条运行时枚举路由登记 ≥1 条直接 HTTP 用例或显式豁免**（`{route, reason, approver}`），形成「路由 × 用例」矩阵，遗漏即 FAIL；续传、PTY 哨兵/退出/重连、双注册/遮蔽探针各入场景。**S3 比对器**：HTTP 结构化 diff（ID 一致映射、时间/随机字段归一化，剥离 `X-PilotDeck-Handler`）；WS **先比同一连接内全序帧序列（`seq` 保留断言），再比按 `type` 的结构 + 确定字段**；SSE **完整事件序列（不丢/不并/不乱序）+ 事件到达延迟上界**（`read()` 分块元数据仅诊断，不作硬门）；落盘目录快照 diff；**处理侧覆盖检查**（读 `X-PilotDeck-Handler` 与矩阵，缺用例即 FAIL）。**S4 规约与对照**：`specs/ui-server-parity/{acceptance,normalization,accepted-diffs}.md` 草稿（**不动** `specs/parity/`）——`acceptance.md` 写「路由 × 用例覆盖矩阵」硬门，`normalization.md` 写 WS `seq` 全序与 SSE 完整事件 / 到达延迟口径；`controls/` 阳性对照（注入 HTTP 头差异 / **交换同一连接两帧顺序** / 删一帧 WS / **对 SSE 丢一个事件 / 并两个事件 / 把整条流攒到结束才发** / 截断 SSE / 改一处落盘 → 必 FAIL）；四项对照（自比 k 遍 / 逐规则负向 / 两侧同坏 / 双谱系评审）照计划 001 S3 结构做，`check_ruler.py` 输出 JSON 全 true。

**怎么算做完**
- [ ] `python3 scripts/ui-server-parity/check_ruler.py` 四项全 true，结果进 `docs/experiments/exp010-ui-server-parity-device/`
- [ ] `enumerate_routes.mjs` 产出 JSON 与 `006/route-inventory.md` 对账，差异逐条有原因并回填（分母以枚举为准）
- [ ] `python3 scripts/ui-server-parity/check_coverage.py` 通过：枚举出的每条 Rust 路由都有直接 HTTP 用例或显式豁免，且用例日志里处理侧为 `rust`
- [ ] 阳性对照含「换序」「SSE 丢事件/延迟聚合」两项，注入后必 FAIL
- [ ] 规约草稿**交人批**后冻结（`FROZEN.md` + sha256）；冻结前必须有人过目
- [ ] TS 自比（直连 vs 直连）在冻结口径下 100% 一致，否则装置不可信
- [ ] `ui/e2e` 的 `page.route` 支线作为**前端回归门**接入：切到 Rust 入口后重跑仍绿（证明前端没改）

### Issue B3 · Rust `pilotdeck-ui-server` crate：反代 + 鉴权 + 无状态只读（**阶段 A 达标后才开工**）

**S1 crate 骨架**：`src/pilotdeck-ui-server`（单 crate，先不拆）；`pilotdeck-cli` 加 `ui-server` 子命令。模块：`proxy`（HTTP/SSE/WS 反代，B1 代码转正）、`router`（路由归属表加载）、`auth`（读 `auth.db`、HS256、bcrypt、`X-Refreshed-Token`、`DISABLE_LOCAL_AUTH`）、`static`（`ui/dist` 与 `/memory-dashboard`）。
**S2 鉴权与静态接管**：`/api/auth/*`（register / login 写 `auth.db`，属 G-AUTHDB）、静态资源、`/health` 切 Rust。**接管前先落 G-AUTHDB 写者协调**：要么把 `/api/settings/{api-keys,credentials,notification-preferences,push}/*`、`/api/user/git-config`、`/api/sessions/:id/rename`（`index.js:995` `sessionNamesDb.setName`）、**`DELETE /api/projects/:projectName/sessions/:sessionId`（`index.js:954` 入口、`index.js:963` `sessionNamesDb.deleteName`）** 同批纳入，要么明确 TS 为唯一写者、Rust 只读 secret 不写（`006/decision.md` #7）。**注意删除会话是跨三组的路径**：除 G-AUTHDB 的 `sessionNamesDb` 外还删 transcript（G-SESS）并向同用户广播 `session-deleted`（G-EVT，`index.js:964-968`）；「同批纳入」必须连这两组一起处理，否则该路由整条留 TS。对拍绿。
**S3 无状态只读路由组接管**：`/api/projects`（GET）、`/api/projects/:p/sessions`、`/api/sessions/:id/messages`、`/api/models`、`/api/config`（GET）、`/api/ccr/*`、`/api/always-on/events` 等；每组一小步，对拍绿才把路由表条目（连同共享资源组）从 `ts` 翻 `rust`；每条路由登记 1 条直接用例。`getProjects`/`readSessionMessages`/`listSessions` 改走阶段 A 的 gateway 协议（`006/decision.md` #2 的 B 桶）。
**怎么算做完**
- [ ] **分母以运行时枚举为准**：路由表覆盖枚举集，标注每条的 `rust` / `ts` 与所属共享资源组；已切组对拍一致、未切组反代零差异
- [ ] **覆盖率硬门**：`check_coverage.py` 通过——每条 `rust` 路由 ≥1 条直接 HTTP 用例或显式豁免，且用例日志证明处理侧为 `rust`（`X-PilotDeck-Handler`）
- [ ] 鉴权对拍含 register / login / 刷新 / WS token / 默认关鉴权分支；G-AUTHDB 写者协调落地（同组同批或单一写者代理）
- [ ] CI `check` 绿；阳性对照仍会 FAIL
- [ ] 人肉验收：真浏览器经 Rust 入口走通（此步不交给 AI）

**D5 待批补充（B3）**：建议在人批准后，以 A8「gateway 退役验证完成（不含 UI 自动恢复）」连同 B1/B2 前置作为 B3 开工条件，再由 B 后续工作包显式承担客户端恢复修复，最后完成真实 UI 验收。**此解环建议须人批准，未批准前维持原闸门「阶段 A 达标后才开工」**；A7 或一次协议探针不足以代替 A8，完整 UI 门继续保留，本轮不放行 B3。

### Issue B4 · 写路由组按共享资源接管（B3 合并后，一组一 issue）

**按共享资源组推进（同组同批，`006/decision.md` #7）**：
- **B4a 无 G-EVT 耦合组**：G-GIT（`/api/git/*` 写路由）→ G-PLUGINS（`/api/plugins/*`）→ G-SKILLS（`/api/skills/*`、`/api/commands/execute` 的 `/skill_install`）→ G-TASKDIR 读（写待桥）。
- **B4b 跨进程广播桥（G-EVT 前置）**：TS `ui/server` 加仅回环内部端点（`POST /__internal/broadcast`；**`HOST=127.0.0.1` 绑定断言 + `<PILOT_HOME>/server-token` 鉴权**双保险），payload 带 `type`，**TS 侧按事件类型分发**——`config:reloaded` 走全员 `broadcastConfigReloaded`（`index.js:298-306`），`session-deleted` 等按用户过滤（`index.js:964-968`，只发 `client.__pilotdeckUserId === userId` 的连接），**不得一律走全员广播**（`006/decision.md` #7）；Rust 写提交成功后调用；探针先验文件 watcher 二次广播语义（`006/gap.md` §三）。桥默认关、带 env 开关，B5 `/ws` 接管后删除。
- **B4c 经桥迁移的写组**：G-YAML（`/api/config` 写、`/api/memory/settings`、`/api/gateway/*` 保存、`/api/v1/model-configuration`）同批；G-SESS 的删除（`index.js:954, 1004`）同批；然后 G-UPLOAD、G-MEMDB、G-PERMS。
每个写组都过「同组同批 / 单一写者」约束（`006/gap.md` 2.5）；组内任一路由未过对拍，整组不翻 `rust`。

### Issue B5 · `/ws`、`/shell`、`/plugin-ws/*` 与 SSE 的接管（B3/B4 之后）

`/plugin-ws/*` 本就是纯代理，最先可整段迁；`/ws` 必须在 Rust 自持连接状态（`connectedClients`、watch 注册、断线替补）并对拍全部 23 种 type + **同一连接全序** + 断线/重连/watch 场景；Rust 自持 `connectedClients` 后**删除 B4b 广播桥**。SSE 各自持流（口径见 `006/decision.md` #5，含 `Last-Event-ID` 续传）。`/shell`（PTY）单列——先长期保持整条反代；若迁则单开 issue，PTY 比握手 + **固定命令输出哨兵 + 退出 + 重连**，不只比 resize。

### Issue B6 · 进程编排（B5 之后）

改 `webRuntimeSupervisor.js:31-38`：起 Rust（`SERVER_PORT`）+ TS `ui/server`（私有回环端口，仅当有未接管路由时）；桌面端 `apps/desktop/src/main.ts:260-267` 同改；`apps/desktop/src/main.ts:941` 的 `require(pilotdeckConfig.js)` 旁路单列处理。**这一步会动 `ui/`（`ui/server` 正在被替换，允许）与 `apps/desktop`——按 `.42cog/intent.md`「阶段 B 才动 `ui/server`」，但要人过目。**

### Issue B7 起 · TS `ui/server` 退役（另开计划 004，本计划不展开）

全路由对拍通过后停 TS 上游、删 `ui/server`。前置：38 处 `src/` 全部有归宿（`006/decision.md` #2）；**枚举集上每条路由的 `rust` 与覆盖门通过（无 `ts` 残留、无未豁免缺例）**；共享资源组全部整组迁完；`/shell` 与桌面旁路有结论。

## 验收（端到端怎么测）

```bash
# 分母与覆盖（B2 S1/S2；协调者在允许 loopback listen 的环境跑）
python3 scripts/ui-server-parity/run.py --out _tmp/exp010-run
# run.py 在隔离 env 下 --import enumerate_routes.mjs 加载真实 ui/server；复用 B1 stack dump 与对账
python3 scripts/ui-server-parity/check_coverage.py --enumeration _tmp/exp010-run/enumeration/router-enumeration.json --evidence _tmp/exp010-run/cases.jsonl
# 装置自检（B2 S3/S4，下一 PR 实现；本 PR 不提供下面这些脚本）
python3 scripts/ui-server-parity/controls/run_all.py        # 阳性对照：注入差异（含换序、SSE 丢事件/延迟聚合）必须 FAIL
python3 scripts/ui-server-parity/check_ruler.py             # 四项对照 JSON 全 true
python3 scripts/ui-server-parity/check_frozen.py            # 冻结文件哈希
# 对拍（B3 之后）
python3 scripts/ui-server-parity/run_round.py --side ts   --label ts-r1
python3 scripts/ui-server-parity/run_round.py --side rust --label rust-r1   # Rust 前置 + TS 上游
python3 scripts/ui-server-parity/compare.py ts-r1 rust-r1   # HTTP / WS / SSE / 落盘
# 前端回归门（复用 ui/e2e 的 page.route 支线）
cd resources/PilotDeck/ui && PILOTDECK_E2E_SERVER_PORT=13001 npx playwright test ...
```

最后一道是人：用原版浏览器实测（Rust 入口，`ui/src` 未改），这一步不交给 AI。

## 风险与权衡

| # | 风险 | 应对 |
|---|---|---|
| R1 | Express 路由优先级 / 中间件顺序在反代或 Rust 实现里被改变（`/api/projects/files` 必须先于 `/api/projects`，`index.js:545-547, 589`；`/api/sessions/model` 先于 `/api/sessions`，`index.js:637-640`；`mcp.js:413` 被 `mcp.js:36` 遮蔽） | 路由表按**注册顺序 + 挂载穿透**生成（`006/decision.md` #1）；每条双注册 / 遮蔽各配 1 条探针用例；逐组对拍 |
| R2 | 无类型 WS 的比对口径不稳（自比就有噪声），且按 `type` 分组会掩盖错序 | B2 S4 自比 k 遍定口径（照计划 001）；**冻结「同一连接全序 + `seq` 断言」**，加「交换两帧顺序」阳性对照；不比跨连接 |
| R3 | 38 处 `src/` 里依赖阶段 A 未就绪的 gateway method | B3 起等 A 达标；未就绪的条目路由表留 `ts`（`006/decision.md` #2 的 C 桶） |
| R4 | `/shell` PTY 跨连接/跨平台状态复杂 | 初期保持整条反代；迁移单列 issue，比握手 + **固定命令输出哨兵 + 退出 + 重连** |
| R5 | 与阶段 A / C 并行造成机器负载假差异 | 全量对拍串行锁沿用（`mkdir /tmp/pdrs-parity.lock` 模式）；B1/B2 小规模手跑 |
| R6 | 误碰用户环境（3001、18789、`~/.pilotdeck`、用户进程） | 一律高位端口（130xx/151xx/287xx）+ `PILOT_HOME`/`DATABASE_PATH`/`TMPDIR` 指向 `_tmp/`；每次实验把端口清单写进 readme；绝不 kill 用户进程 |
| R7 | `auth.db` 双写（Rust 与 TS 同时处理鉴权/登录/设置） | 按 **G-AUTHDB 共享资源组**同组同批；或指定单一写者、另一侧经协议代理；接管前 Rust 只读 secret（`006/decision.md` #7） |
| R8 | SSE 在反代里被缓冲、改写 framing，或事件被延迟 / 合并 / 丢 | B1 S3 用**事件到达时刻**证明逐步到达；保留 `X-Accel-Buffering: no` 等头；**完整事件序列 + 到达延迟上界**口径 + `丢事件 / 并事件 / 攒流到结束` 阳性对照；`read()` 分块元数据仅诊断、不作硬门（`006/decision.md` #5） |
| R9 | B3 被迫在阶段 A 未达标时开工（进度压力） | 阶段门是红线；B1/B2 的产出（探针结论、装置、规约）本身可合并，不越界 |
| R10 | 改 `ui/server` 与 `apps/desktop` 时越界改到 `ui/src` | 每次提交前 `git diff --stat` 核对路径；`ui/src` 出现改动即回退 |
| R11 | 跨进程广播桥不一致 / 有漏洞（G-EVT） | 桥仅回环（**`HOST=127.0.0.1` 绑定断言**）+ `<PILOT_HOME>/server-token` 鉴权、**按事件类型/用户分发**、默认关；payload 与 `broadcastConfigReloaded` 同形；探针先验文件 watcher 二次广播；`/ws` 接管后删桥 |
| R12 | 覆盖率门形同虚设（用例不驱动真实路由，反代冒充 Rust） | 装置记录处理侧 `X-PilotDeck-Handler`；直接 HTTP 用例直连 Rust 端口；缺用例且无豁免即 FAIL；分母以运行时枚举为准 |

**D5 待批补充（R3 / R9）**：风险表原文保留；解环建议为以 A8「gateway 退役验证完成（不含 UI 自动恢复）」作为 B3 的 A 侧闸门，须人批准。未批准前维持原闸门，不凭 A8 报告自动放行；完整 UI 恢复仍须 B 修复后单独验收。

## 范围外

浏览器 `ui/src` 的任何改动；阶段 C（agent 循环 sidecar）与阶段 A（18789 gateway）本身；把前端重构成 Rust（人在意向里另排的顺序，AI 理解为改成 Rust，待人复核）；`specs/parity/` 已冻结文件的任何改动；性能优化；上游升级；对外提供服务；发版。

## 拍板记录

上一轮已拍（见 `002/decision.md`）：先 C 再 A 再 B · 驱动用 TS · 默认修 bug · 阶段 A 编排不改入口。
本轮 AI 代拍（待人复核，详据见 `006/decision.md`）：① 切法 = Rust 前置逐路由接管、未实现反代给 TS、WS 整条连接反代；② 38 处 `src/` = 分三桶，4 条走 gateway、其余 Rust 直接实现或随路由留守；③ 装置 = 黑盒录放反代 + Playwright 场景 + 本地 mock 模型 + 阳性对照，另立 `specs/ui-server-parity/`；④ 鉴权 = Rust 自实现、读同一 `auth.db`、G-AUTHDB 写者协调；⑤ SSE = 流式透传、逐路由接管，冻结**完整事件序列 / 到达延迟**口径并覆盖 `GET .../staffdeck-sdk/events` 的 `Last-Event-ID` 续传；⑥ 编排 = 先装置自编排、B6 再改 supervisor/桌面端；⑦ 阶段门 = B1/B2 可与 A/C 并行、B3 起严格等 A 达标；⑧ 迁移分组 = 按共享资源（同一文件 / 同一 SQLite / 同一进程内事件）同组同批，G-EVT 走过渡期跨进程广播桥；⑨ 分母与覆盖 = 运行时枚举为准 + 路由 × 用例覆盖矩阵硬门（每条 Rust 路由 ≥1 直接用例或显式豁免，装置记录处理侧）。

**D5 待批补充（拍板记录）**：以上历史记录原文保留；以 A8「gateway 退役验证完成（不含 UI 自动恢复）」作为 B3 闸门只是解环建议，须人批准，未批准前维持原闸门。D5 仍为「等人」，本轮没有新增拍板结果。

## 相关

`docs/research/006_phase-b-ui-server/` · `docs/research/005_phase-a-gateway/` · `docs/research/002_parity-boundary/` · `docs/experiments/exp002-connection-state/` · `docs/plan/001_phase-c-agent-loop-sidecar.md` · `docs/plan/002_phase-a-gateway.md`
