# 两类发现 · Rust 后端怎么接上原版前端、怎么证明行为一致

> 2026-10-14。依据见同目录 4 份 `findings-*.md`（均带行号，主会话共抽查 15 条）。
> 分级：P0 最高；**P0 / P1 / P2 清零，P3 酌情**。

## 一、它们做到的 —— PilotDeck 基线对三条命门

| # | 命门 | 状态 | 差距是什么 | 修起来要什么 | 分级 |
|---|---|---|---|---|---|
| 1 | 前端一行不改就能接上，能逐条切 | **部分** | 浏览器之下还有一层 `ui/server`（约 5 万行），它进程内 import `src/` 38 处、直读写 `~/.pilotdeck`。只有 18789 gateway 这条边界干净（单一客户端、有类型帧）。但单条 WS 上有连接级状态（`interactionBinding`、`seq`、权限 / elicitation 绑在连接上），**按 method 拆给两个后端会对不上** | A 边界：接一根线（Rust 终止 `hello`，TS 挪端口）；连接级状态：**要先盘点**（探针） | **P0** |
| 2 | 「一致」能由机器判 | **部分** | 三个观测点都在：帧协议有类型（`frames.ts` / `types.ts`）· 落盘根目录可由 `PILOT_HOME` 整体换掉 · openai / anthropic 协议的模型请求有统一出口 `options.fetch`（**google 流式不经它**，`streamModel.ts:172-181`）。**缺的是对拍装置本身**——现成测试多数是进程内构造，只有 `tests/sdk` 与少数 gateway 测试打真端口 | 观测点：接一根线。装置：**得从头造**，但 prime-agent 给了样板 | **P0** |
| 3 | 同一输入跑两次结果相同 | **部分** | 约 90 处构造参数可注入 `now` / `uuid`，但默认值大面积回落；而且这些注入点**只在进程内可用**，黑盒对拍两个进程时用不上 | 黑盒对拍：靠 **mock 模型服务 + 归一化**（prime-agent / Diffy 的做法），不靠注入。基线能否指向 mock 模型服务：**待探针** | **P1** |

## 候选之间 —— 两条边界

| 命门 | A：18789 gateway 帧协议 | B：3001 浏览器 API |
|---|---|---|
| 1 接得上、能逐条切 | 部分：单一客户端、帧带 `id` 可分派；连接级状态是风险（`frames.ts:18`、`GatewayWsConnection.ts:129, 152`） | 部分：280 条路由 + 25 种无类型 WS 消息；SSE；JWT 要读 `auth.db` |
| 2 机器可判 | **原生**：有类型帧；`tests/sdk` 已有「起 gateway → `GatewayWsClient` 连真端口」（`session-transcript-restore-e2e.spec.ts:116`） | 部分：`ui/server` 测试是单路由内存 express；能起真进程的只有 `ui/e2e/real`、legal e2e，都要真模型 |
| 3 可复现 | 同 | 同，且 `ui/server` 自己也有大量 `Date.now` / `randomUUID` |
| 范围 | `src/`（约 18 万行） | `src/` + `ui/server`（约 23 万行） |
| 覆盖不到的 | `ui/server` 进程内调 `src` 的那部分、对 `auth.db` / 记忆库的直读、桌面端直连 | 桌面主进程直接 `require` 的 `pilotdeckConfig.js`（`apps/desktop/src/main.ts:941`） |

## 参考仓做到的（可借）

| 做法 | 谁做的 | 对我们 | 证据 |
|---|---|---|---|
| 共享一份脚本的 **mock 模型 HTTP 服务**，会话级队列，记录每个请求 | prime-agent | **直接可用** | `findings-prime-agent.md` 二 |
| 比三样：事件流 · 会话文件 · 发给模型的请求体 | prime-agent | 正好是我们的三类契约 | 同上 一 |
| 按键名归一化 timestamp / id / pid | prime-agent | 直接可用；**但正则要窄**（宽松正则会掩盖真差异） | 同上 一、判断 |
| provider 层录制回放 | goose | 真模型跑一次录下来，两边回放 | `findings-ref-architecture.md` goose 段 |
| 协议 / 服务 / 传输三 crate；ts-rs 导出类型；wiremock + insta；`TimeProvider` | codex-rs | Rust 侧分层与测试样板 | 同上 |
| 集中的「已接受差异」清单 | tsgo | prime-agent **没有**（散在注释），我们要有 | `../001_rewrite-case-studies.md` |

## 二、它们没做的

| 找过什么 | 结论 | 是哪一种 |
|---|---|---|
| 集中、可审计的允许差异清单 | prime-agent 无（散在约 20 个文件的注释里） | **没顾上做** → 我们做 |
| 验证对拍装置本身会失败（阳性对照） | prime-agent README 自己承认「全绿 ≠ 一致」，没见到故意注入差异的检查 | **没顾上做** → 我们做 |
| 服务端「Rust 在前、转发给 TS」逐条切 | prime-agent 是分支上渐进 + main 上一次 squash 切换；Cloudflare 做过但领域不同 | 有但不完全适用：连接级状态让「按 method 切」不一定成立 |
| 切换后保留对拍 | prime-agent 切换次日删装置，改成对 sha 固定的旧版二进制 | 有，可借 |

### 该不该自己跑 —— 要跑两个探针（第 7 步）

| 探针 | 为什么必须跑 | 会改变哪个取舍 |
|---|---|---|
| **exp001**：基线能否在临时 `PILOT_HOME` 下无头起 gateway、指向一个 mock 模型服务、同一脚本跑两遍并比出噪声字段 | 「输出清楚，却不知道拿什么喂它」；整个对拍方案都压在这一条上 | 命门 2、3 的方案成不成立 |
| **exp002**：盘点 18789 连接上的连接级状态，哪些 method 依赖它 | 决定渐进的粒度：按 method、按会话、还是整条连接 | 命门 1 的切法 |
