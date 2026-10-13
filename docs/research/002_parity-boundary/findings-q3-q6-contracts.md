# Q3 现成测试 · Q4 落盘 · Q5 模型请求 · Q6 非确定性 —— 核查结果

> 2026-10-14。被查：`resources/PilotDeck` @ `cc7c4e6`。由子 agent 产出，主会话抽查 4 条（✔）并更正 1 处计数方法（⚠）。行号相对 `resources/PilotDeck/`。
> 两条候选边界：**A** = 18789 gateway 帧协议；**B** = 3001 浏览器 API（见 `findings-q1q2-boundary.md`）。

## Q3 现成测试能不能当对拍用例

| 目录 | 文件数 | 怎么起被测对象 | 能否指向外部进程 |
|---|---|---|---|
| `tests/protocol` | 1 | 不起：静态核对 schema 与方法枚举（`module-protocol-contract.spec.ts:19-55`） | 不适用 |
| `tests/gateway` | 60 | 多数进程内（`InProcessGateway`）；5 个走 WS，如 `bridge-interaction-reconnect.spec.ts:125-140` 真起 `startGatewayServer` 后用 `GatewayWsClient` 连 | **走 WS 的那几个：换 url 即可** |
| `tests/sdk` | 6 | 进程内 `createLocalGateway` + 注入假模型 `__testModelFactory`（`model-usage-e2e.spec.ts:43-72`）；部分再套真 WS（`session-transcript-restore-e2e.spec.ts:116`、`agents-e2e.spec.ts:2040, 3252, 3340, 3398`） | **能，最好的一批** |
| `tests/e2e` | 1 | spawn 真 supervisor，打 `/api/uploads`；要真模型 + key（`legal-data-loop-real.spec.ts:13-17, 41-56`） | 能，但依赖真模型 |
| `ui/e2e` | 46 | 多数只起 vite、`page.route` 伪造 `/api`，不碰后端；`ui/e2e/real/` spawn 真 supervisor，要真模型（`real/launch.mjs:11-19, 78`） | `real/` 能，其余不碰后端 |
| `ui/server/*.test.*` | 80 | vitest 里自建 `express()` 只挂单个路由（`routes/messages.test.js:23-25`） | 不能直接用 |

假模型：18 个测试文件 `implements ModelRuntime`，5 个注入 `fetch`/transport；注入点 `src/cli/createLocalGateway.ts:244, 746`。

## Q4 落盘格式（第二类契约）

- 根目录 `~/.pilotdeck`，可由 `PILOT_HOME` 覆盖 ✔（`src/pilot/paths.ts:9, 21`）——**对拍时每边一个临时目录，跑完比目录**。
- 配置 `pilotdeck.yaml`（项目级 `<proj>/.pilotdeck/pilotdeck.yaml`），有 `schemaVersion`，缺省按 1（`src/pilot/config/loadPilotConfig.ts:107, 673`）。
- 会话 `projects/<projectId>/chats/<session>.jsonl`（`paths.ts:38`）；子 agent `<stem>/subagents/<id>.jsonl`（`src/session/transcript/JsonlTranscriptWriter.ts:316`）；项目 ID 由 `.cwd` 标记文件定（`paths.ts:129`）。
- 记忆 `memory/`：Markdown（`MEMORY.md`、`project.meta.md`）+ `control.sqlite`（`src/context/memory/edgeclaw-memory-core/src/core/file-memory.ts:31-32`、`.../storage/sqlite.ts:644`）；`ui/server` 也直读这个库（`ui/server/services/memoryService.js:62`）。
- `auth.db`（SQLite，`ui/server/database/db.js`，建表 `init.sql`；src 侧也读：`createLocalGateway.ts:957`）· `server-token`（32 字节 base64url，0600：`src/gateway/server/authToken.ts:14, 36-38`）。
- 其他 jsonl：`router/events.jsonl`、`gateway/sdk-task-budget-ledger.jsonl`、`telemetry/queue.jsonl`、cron `run-history.jsonl`、`llm/invocations.jsonl`。
- **迁移：没有统一框架**，各处零散（auth.db `ALTER TABLE`、记忆库 `pipeline_state_v2`、`CronStoreMigration.ts`）；transcript 无文件级版本号。

## Q5 发给模型的请求（第三类契约）

- 组装链：`PromptAssembler.ts:57`（system prompt）→ `agent/loop/modelRequestAssembly.ts:46-74`（汇总 prompt + tools）→ `compaction/CompactionEngine.ts:178`（压缩）→ `model/request/buildModelRequest.ts:17`（转 provider 请求体）。
- 统一接口 `ModelRuntime`（`stream` / `complete`）✔（`src/model/ModelRuntime.ts:14-21`）；实现在 `src/model/providers/{anthropic,google,openai,openai-responses}`。`src/adapters` 是消息渠道，**不是**模型 provider。
- **统一出口：有** ✔。`sendProviderRequest(..., options.fetch ?? fetch, ...)`（`src/model/streaming/streamModel.ts:124, 206`）——在这里注入 fetch 就能录制 / 回放。另有审计钩子 `ModelInvocationLogSink`（`src/storage/invocationStorage.ts:77`）。

## Q6 非确定性

| 写法 | src | ui/server |
|---|---|---|
| `Date.now(` | 124 | 59 |
| `new Date(` | 149 | 82 |
| `randomUUID` | 197 ⚠ | 31 |
| `Math.random` | 25 | 6 |

⚠ **计数方法更正**：上表是「命中行数」，`randomUUID` 的 197 行里有 **82 行是 import**，实际调用点约 115 处。其余几行未逐一复算，量级可信、精确数不可信。

- 集中的文件：`ui/server/routes/taskmaster.js`、`ui/server/pilotdeck-bridge.js`、`src/adapters/channel/protocol/ImLiveReplyController.ts`、`ui/server/index.js`、`src/sop/staffdeck/SopStateStore.ts`。
- **没有统一 Clock / IdGenerator**，但约 90 处构造参数接受 `now?: () => Date` / `uuid?: () => string`（例：`src/session/events/SessionRuntime.ts:41-42`、`CompactionEngine.ts:58-60`、`createLocalGateway.ts:179, 684, 703`）；默认值都回落到 `new Date` / `randomUUID`，覆盖不全。
- 序号：WS 帧 `seq` 每请求从 0 自增（`src/gateway/server/GatewayWsConnection.ts:129, 152`）；事件 `sequence` / `createdAt`（`SessionRuntime.ts:57-64`）；run seq（`src/gateway/run/RunRegistry.ts:247`）。

## 子 agent 的判断

- 预判基本成立，修正两处：sdk/gateway 已有一批测试打真 WS；时间 / ID 有分散的注入参数。
- **首选 A（18789）**：有类型帧；现成测试已能「起 gateway → `GatewayWsClient` 连真端口」；模型侧在 `options.fetch` 录制回放。
- **备选 B（3001）**：`ui/server` 测试是单路由内存 express，WS 无类型；能起真进程的只有 `ui/e2e/real` 与 legal e2e，且都要真模型。
- **风险**：① A 覆盖不到 `ui/server` 进程内引的 `src` 逻辑和对 `auth.db`、记忆库的直读 ② 时间 / ID 默认值大面积回落，要么全注入、要么比对前规范化 `createdAt` `entryId` `seq` 一类字段 ③ 落盘分散（jsonl、md、两个 sqlite）且无统一迁移 ⇒ 除了比协议输出，还要比磁盘副作用。
