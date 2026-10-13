# 前端依赖面与差距 · 逐条核查

> 2026-10-14。被查：`resources/PilotDeck` @ `cc7c4e6`（只读）。行号均相对 `resources/PilotDeck/`。
> 规模数字由 shell 实测（`find` + `wc -l`，统计口径：`.ts/.tsx/.js/.jsx` 为「代码」，`.css` 单列）。
> 给不出 `路径:行号` 的结论一律不写——被删掉的那类见文末「删掉的结论」。

## 一、模块划分与规模

**总量**：`ui/src` 841 个文件（含资源与 css）、代码 136,940 行、css 15,504 行。任务简报里的「约 13.7 万行、841 个文件」与此吻合。

### 一级目录（代码行）

| 目录 | 代码文件 | 代码行 | 是什么 |
|---|---|---|---|
| `components/` | 521 | 94,130 | 全部界面组件（下拆） |
| `composition/` | 138 | 25,697 | 模块/Profile 装配系统：7 个 slot 的绑定契约（`ui/src/composition/contracts.ts:5-7`）、装配运行时（`runtime.ts` 234 行）、20+ 个模块实现（`composition/modules/`，StaffDeck 业务域为主） |
| `stores/` | 9 | 5,961 | 全局状态（会话/流式消息等，e2e fixture 直接驱动它们，见 §三） |
| `hooks/` | 21 | 4,209 | 通用 hooks |
| `utils/` | 23 | 2,295 | 含对后端 HTTP 封装 `utils/api.js`（545 行） |
| `types/` | 9 | 1,459 | `types/app.ts:4` 定义 `AppTab = 'home'｜'chat'｜'always-on'｜'cron'｜'files'｜'shell'｜'git'｜'tasks'｜'memory'｜'skills'｜'preview'｜'dashboard'｜'plugin:*'` |
| `contexts/` | 13 | 1,247 | 主题/WS/插件/Toast 等 Provider（`App.tsx:37-46` 九层嵌套） |
| `shared/` | 12 | 875 | 与 `ui/server`、`ui/shared` 共享的代码 |
| `lib/` | 15 | 735 | 工具函数 |
| `i18n/` | 3 | 216 | i18next 配置（zh/en，中文是生产文案） |
| 根文件 | 6 | ~130 | `App.tsx`（64 行）、`main.jsx`（30 行）等 |

### `components/` 内部（代码行，取 ≥400 行者）

| 子目录 | 文件 | 行 | 是什么 |
|---|---|---|---|
| `chat/` | 99 | 19,319 | 聊天面组件（Markdown 渲染 `chat/view/subcomponents/Markdown.tsx`、消息流、权限/授权 UI） |
| `settings/` | 144 | 17,384 | 设置中心（表单型，144 个文件） |
| `chat-v2/` | 60 | 16,215 | 聊天搜索控制器等增强层（`app-shell/MainAreaV2.tsx:13-14` 引用） |
| `code-editor/` | 65 | 12,347 | CodeMirror 6 封装 + 二进制文件预览（PDF/表格/DOCX/PPTX，见 §四） |
| `main-content-v2/` | 18 | 9,921 | 各专属页：CronV2/SkillsV2/FilesV2/GitV2/TasksV2/DashboardV2 等 |
| `app-shell/` | 15 | 4,342 | 壳与侧栏：`AppShellV2.tsx:79-84` 用 `useMatch` 解析 `/p/:name`、`/p/:name/c/:id`、`/session/:id`、`/cron`、`/settings/*` |
| `onboarding/` | 26 | 3,927 | 首次引导 |
| `main-content/` | 11 | 2,557 | **在用的聊天容器**：`app-shell/MainAreaV2.tsx:9` 引 `main-content/view/MainContent` |
| `shell/` | 15 | 1,873 | 终端页（xterm + `/shell` WS，见 §二） |
| `project-creation-wizard/` | 17 | 1,796 | 建项目向导 |
| `auth/` | 16 | 1,239 | 登录/鉴权 |
| `git-panel/`、`file-tree/`、`ui/`、`task-master/`、`desktop/`、其余 | 38 | ~4,000 | 杂项 |

**聊天面合计**（chat + chat-v2 + main-content + main-content-v2 的聊天部分）≈ 4.8 万行，是最大的一块；其中互有引用：`main-content-v2/RunDetail.tsx:13` 反过来引 `chat/view/subcomponents/Markdown`。

**路由形态**：整个应用是**单通配路由**——`ui/src/App.tsx:50` `<Route path="*" element={<AppShellV2 />} />`（`:33-35` 注释明说 URL 变化不重挂壳）。URL 只在 `AppShellV2` 内部用 `useMatch` 分辨（`AppShellV2.tsx:79-84`）。「按路由渐进」因此可行但对象是**页面/Tab**，不是 React Router 的路由组件。

**跨页导航是 SPA 客户端跳转，不是整页加载**：`AppShellV2.tsx:5` 引 `useNavigate`、`:73` 取实例，`:387`（`onShowSettings`）、`:257, :326`（项目切换）、`:358, :361`（通知跳转）等全部走 `navigate(...)`——URL 变了但**不触发整页加载**。且服务端对全部 HTML 路由是单一通配：`ui/server/index.js:3471` `app.get('*')` 一律回旧 `dist/index.html`（`:3480` 拼路径、`:3483-3488` 存在即 `sendFile`）。⇒ **「静态托管按路径前缀选 bundle」只在整页加载时生效**：不新增托管分流，旧 SPA 内点进已迁移页面时永远还是旧 bundle；反向从新壳跳未迁移页也一样。渐进共存必须补两件事：**双入口托管**（改 `app.get('*')` 加前缀分支，或前置反代分流）与**双向导航拦截**（旧壳到已迁前缀的客户端导航改硬跳 `window.location`——拦截在路由/历史层做，覆盖 `navigate` 调用、`<Link>` 组件跳转（如 `auth/view/ModelConfigurationErrorScreen.tsx:40`，绕过 `navigate`）与前进/后退；新壳跳未迁路径时硬跳回旧 bundle）。跨壳硬跳会丢 SPA 内存态（WS 连接、流式中途文本、滚动位置；**composer 草稿不在其列**——草稿落 localStorage 键 `draft_input_<projectName>:<sessionId|'new'>`，`chat/utils/chatStorage.ts:6-9`），详见 `accepted-diffs.md`。

## 二、对后端的依赖面（浏览器侧）

浏览器只连 `ui/server`（3001），不直连 gateway——这一层结论沿用调研 002（`docs/research/002_parity-boundary/findings-q1q2-boundary.md`），此处只列浏览器侧的三条通道。

### 1. HTTP（`/api/...`）

- 统一入口 `authenticatedFetch`：`ui/src/utils/api.js:36-76`。JWT 存 localStorage 键 `auth-token`（`api.js:29, 37`）；`/api/modules/` 请求额外带会话头 `X-PilotDeck-Project`（读 sessionStorage 键 `pilotdeck-active-project`，`api.js:44-47`）；响应头 `X-Refreshed-Token` 会静默换 token（`api.js:65-68`）；≥500 统一弹 toast（`api.js:69-73`）。
- 平台分支 `IS_PLATFORM`：平台模式下不带 Bearer（`api.js:54`）。
- **端点面是动态拼接的，静态 grep 穷举不完整**：实测出现频次最高的前缀为 `/api/config`（43 处）、`/api/projects/`（29）、`/api/config/test-connections`（23）、`/api/enterprise/skills*`（44）、`/api/enterprise/knowledge*`（30+）、`/api/models`（12）、`/api/sessions/`（7）、`/api/plugins/`、`/api/uploads/`、`/api/always-on/cron-jobs/`、`/api/auth/*`、`/api/user/*`、`/api/settings/permissions`、`/api/skills/*`、`/api/update/*`、`/api/taskmaster/*`、`/api/modules/*`、`/api/mcp/config/`、`/api/sop/*`、`/api/commands/execute`、`/api/gateway/weixin/qr-begin`、`/api/v1/providers` 等 40+ 个前缀（grep `'/api/` 全量，2026-10-14）。服务端注册面约 280 处路由（调研 002：`ui/server/index.js:540-648`）。⇒ **对拍要靠录制兜底**：以网络层录到的请求集合为准，静态清单只当索引。

### 2. 主 WebSocket `/ws`（无类型 JSON）

- 连接：`ui/src/contexts/WebSocketContext.tsx:41-46`——`${proto}://${host}/ws?token=<JWT>`。
- 心跳：每 30s 发 `{type:'ping'}`（`WebSocketContext.tsx:128-132`）。
- 重连：指数退避 1s 起、×2、封顶 30s（`WebSocketContext.tsx:48-50, 172-186`）。
- 离线队列：**只**缓存 `check-session-status`（按 sessionId 去重，上限 100 条；`WebSocketContext.tsx:53-75, 229-232`）。
- 高频通道：`subscribe` 同步分发绕开 React 状态批处理（`WebSocketContext.tsx:21-28` 注释、`:239-244` 实现）——**这是旧代码为 React 打的补丁，重写时是对拍重点**。
- 出站消息类型（grep `{ type: '...' }` 于 sendMessage 调用点）：`pilotdeck-command`、`check-session-status`、`permission-response`、`elicitation-response`、`watch-session`、`unwatch-session`、`abort-session`、`regenerate-last-message`、`session-permission-grant`、`get-input-queue`、`get-pending-permissions`、`get-session-activity`、排队输入操作（`queue-input`/`move-queued-input`/`delete-queued-input`）、`model-selection-saved`（含 `ping`）。
- 入站约 25 种无类型 JSON（`stream_delta`、`projects_updated`、`agent_status`、`config:reloaded`、`input-queue-*` 等；服务端分发起点 `ui/server/index.js:2373`，清单以调研 002 为准）。接收端处理点**分散在各组件**，没有集中 switch——重写时要先做一张「消息类型 → 处理点」表（探针 Issue 的产出之一）。

### 3. 终端 WebSocket `/shell`

- 连接：`ui/src/components/shell/utils/socket.ts:4-13`——`/shell?token=<JWT>`，平台模式无 token（`:8-10`）。
- init 帧：`{type:'init', projectPath, sessionId, hasSession, provider:'plain-shell'|'pilotdeck', cols, rows, initialCommand, isPlainShell}`（`ui/src/components/shell/hooks/useShellConnection.ts:152-162`；类型定义 `shell/types/types.ts:9`）。
- 键入帧 `{type:'input',...}`：`useShellTerminal.ts:185, 217`（类型 `shell/types/types.ts:27`）。
- 入站 `{type:'output'|'auth_url'|'url_open'}`（`useShellConnection.ts:92-105`）；前端自己解析 ANSI 转义与 `Process exited with code N`（`useShellConnection.ts:9-11, 58-82`）——**退出码判定在前端**，对拍时不能漏。

### 4. 桌面端如何消费前端

`apps/desktop`（Electron 42，4,925 行 TS/JS 产品+脚本）窗口直接 `loadURL` 到 `ui/server` 端口（`apps/desktop/src/main.ts:622`）；默认关本地鉴权（`main.ts:172`）；gateway 启动命令写死 TS 入口（`main.ts:250-258`）；主进程还直接 `require` `ui/server` 的服务（`main.ts:921, 930, 941`）。⇒ 桌面端把「web 前端」当一张网页加载，**前端换成 Rust/Wasm 产物（同为静态资产）对它透明**；它对 TS 后端的硬依赖属于阶段 B/桌面 Rust 化的既有问题（调研 002 已登记）。

## 三、e2e 装置盘点（对拍能不能用，取决于用例怎么驱动前端）

任务简报说「46 个 Playwright 用例」；实测口径如下（2026-10-14，grep `^\s*test(` 逐文件数 + 循环展开人工核对）：

- **15 个 spec 文件、61 个 `test()` 调用**（grep `^\s*test(` 数得 60，漏掉 `model-selection.spec.mjs:108` 的同行调用 1 处——`for (const ready of [...]) test(...)` 的 `test(` 不在行首）：顶层 `ui/e2e/*.spec.mjs` 10 个文件 35 例；`ui/e2e/real/*.spec.mjs` 5 个文件 26 例。
- **循环展开后实际注册 72 例**：`model-selection.spec.mjs:108`（`for (const ready of [false, true]) test(...)` 同行注册 2 例）、`:193`（preset 循环 ×2）、`streaming-lifecycle.spec.mjs:94, 154`（各 ×2）、`streaming-search.spec.mjs:16, 41`（×3、×4）、`real/session.spec.mjs:35`（provider 循环 ×3）。**注意用例内层的 `for` 循环（如 `real/settings.spec.mjs:44, 51, 82`、`real/onboarding.spec.mjs:17`）不增例数。**引用计数一律以展开后的 72 为准。
- **另有 10 个独立 smoke 脚本**（`*.smoke.mjs`，Playwright 库 API 直写、无 `test()`）：`appearance`、`desktop-chrome`、`desktop-layout-regression`、`dialog-appearance`、`file-picker`、`settings-material`、`settings-navigation-theme`、`settings-refinement`、`update-actions`、`update-download`。
- 61 + 10 = 71 与「46」都对不上；简报的数应是某种子集口径。**以本表为准，引用时带文件名。**

按「新前端验收时这例怎么才能跑起来」分四类（展开后口径）：

| 类 | 注册数 | 文件与证据 | 怎么才能跑 |
|---|---|---|---|
| A. fixture 页耦合 | 34 | `streaming-lifecycle.spec.mjs`（7 例 + `:94, :154` 两处循环各 ×2 = 9）、`streaming-scroll.spec.mjs`（5）、`streaming-search.spec.mjs`（3 + `:16` ×3 + `:41` ×4 = 8）、`model-selection.spec.mjs`（8 + `:108` ×2 + `:193` ×2 = 11）、`composition.spec.mjs`（1，`:54` 循环在用例体内不增例）。全部 `page.goto('/e2e/fixtures/*.html')` 或 `/composition.html`（`streaming-lifecycle.spec.mjs:18, 49`、`model-selection.spec.mjs:32`、`composition.spec.mjs:12`），fixture 页在 `ui/e2e/fixtures/*.jsx` 挂旧组件并把 store 挂到 window，用 `page.evaluate` 直接调（`streaming-lifecycle.spec.mjs:19-28`） | **离线自跑**（vite dev 起 fixture 页、无真实后端：`streaming-lifecycle`/`model-selection` 用 `page.route` mock `/api`；`streaming-scroll`/`streaming-search`/`composition` 直驱 window store API 无网络）——但只对旧前端有效。断言的是旧 store API 与旧组件内部，新前端要跑必须先移植测试钩子或改写黑盒 |
| B. 黑盒页面级（**env 门控的真实部署用例，非 mock**） | 9 | `formal-composition.spec.mjs`（4）、`formal-route-matrix.spec.mjs`（1）、`real-auth-composition.spec.mjs`（3）、`real-auth-native.spec.mjs`（1）。**没有一个 `page.route` mock**：四件全部 `test.skip(!process.env.*_URL)` 门控（`formal-composition.spec.mjs:6`、`formal-route-matrix.spec.mjs:5`、`real-auth-composition.spec.mjs:4, 7`、`real-auth-native.spec.mjs:4`），对着**真实部署**打真路由真 API；`real-auth-native.spec.mjs:7` 还会注册真实账号（`browser-evidence`）。另有页内二次 skip：`formal-composition.spec.mjs:8-9`（`FORMAL_REPLACEMENT` 换 profile）、`real-auth-composition.spec.mjs:53, 135, 151`（profile 三选一，单轮只执行 1/3） | **要真实部署 + env 矩阵**：跑全 9 例需起 formal/real-auth 部署并按 profile 矩阵（staffdeck/minimal/replacement）多轮设置 env。跑起来后对任何前端一视同仁（同源同路由同文案即可） |
| C. API 级 | 1 | `history-fork.spec.mjs`（1）只用 `request` fixture 打 HTTP，不碰浏览器 | 与前端无关，属后端验收 |
| D. 真端到端 | 28 | `ui/e2e/real/*.spec.mjs` 26 个 `test()` + `real/session.spec.mjs:35` provider 循环 ×2 = 28；`real/launch.mjs:9-14` 起 ui/server + vite + gateway（高位端口 + 临时目录），`real/launch.mjs:17-19` 强制要求 mock LLM center 的 env（`PILOTDECK_E2E_LLM_CENTER_URL` / `PILOTDECK_E2E_API_KEY`，缺失即 throw），会话用例走真实 LLM 调用 | **要 mock LLM 环境 + 三进程编排**：`real/launch.mjs` 是现成编排但 vite 管线写死（`launch.mjs:13`），新前端要接入需 F2 runner 改造。跑起来后对任何被测前端一视同仁 |
| E. smoke 脚本 | 10 | 其中 3 个起 Electron（`appearance.smoke.mjs:2, 15`、`desktop-chrome.smoke.mjs:3, 15`、`desktop-layout-regression`）；其余 chromium 直连 5187 的 fixture 页（`update-download.smoke.mjs:7, 13`） | 3 个只对桌面验收有效；7 个同 A 类（耦合旧 fixture 页） |

**结论（修正版，取代早先「35 例今天就能跑」的误判）**：

- 黑盒候选 = B(9) + D(28) = **37 例**，这是新前端验收分母的来源。但**按运行前提分层**：能离线自跑的黑盒 mock 用例是 **0**——B 类 9 例全要真实部署 + env 矩阵，D 类 28 例全要 mock LLM 环境 + `real/launch.mjs` 编排；「同一 mock `/api` 今天就能跑」只对 A 类成立，而 A 类耦合旧 store，新前端一个都跑不了。**新前端侧今天能直接跑的黑盒用例 = 0，尺子的可用性押在 Issue F2 的 runner（接入 launch 编排 + B 类部署矩阵脚本化）上。**
- A 类 34 例恰是行为最精细的流式滚动/生命周期/组合装配用例，且是唯一离线可跑的一批——机器判尺子在最需要的地方（流式/滚动）最薄：F2 须先为流式与滚动补**黑盒**机器断言（不依赖旧 store 钩子），再开工 F5 聊天面。
- **防意外 skip**：B/D 类大量 `test.skip(!env)` 与页内条件跳过，且存在弱断言——`real/session.spec.mjs:31` 控件缺失时条件点击静默通过。验收跑必须出「应执行数 vs 实际执行数」对账（72 − 显式 skip 逐条列理由），配 `--forbid-only`，对不上即红。
- 截图：多数用例已带 `page.screenshot` 输出但无像素 diff 基线——**补基线机制、不补像素断言**（must-know ①：判语义，不判像素）。

## 四、第三方重依赖在 Rust/Wasm 生态的对应物

依赖清单来自 `ui/package.json`（版本行号列于表内）。旧迁移包已定过政策：**第三方 JS 组件允许保留、须入依赖库存；第一方业务逻辑必须 Rust 化，wasm 自动生成的 JS 绑定允许**（`PilotDeck-Rust/docs/rust-migration-package/decisions.md:11`、`inventory/dependencies.csv:4-7`）。

| 依赖 | 用在哪 | Rust/Wasm 对应物 | 判断 |
|---|---|---|---|
| `@xterm/xterm` + fit/webgl/web-links/clipboard（`package.json:71-75`） | `components/shell/hooks/useShellTerminal.ts:3-4`（Terminal/FitAddon 类型） | **无成熟 DOM 内等价物**。Rust 侧有 `alacritty_terminal`/`vte` 解析器，但 canvas 渲染 + IME + 无障碍层要自建 | 保留为 JS 岛 |
| CodeMirror 6 全家桶（`package.json:36-47, 57`） | `components/code-editor/`（65 文件 12,347 行封装） | **无**。`yew-codemirror` 一类也是反向包 JS | 保留为 JS 岛 |
| Univer 表格 14 包（`package.json:58-70`） | `code-editor/view/subcomponents/SpreadsheetInteractivePreview.tsx` | **无** | 保留为 JS 岛 |
| `pdfjs-dist`（`package.json:102`） | `code-editor/view/subcomponents/PdfDocumentPreview.tsx` | 原生有 pdfium，**wasm+DOM 场景无等价物** | 保留为 JS 岛 |
| `docx-preview`(85)、`exceljs`(86)、`@aiden0z/pptx-renderer`(35) | `DocxBuiltinPreview.tsx` 等 | **无** | 保留为 JS 岛 |
| `react-markdown`(109) + `remark-gfm/math` + `rehype-katex/raw` + `katex`(96) + `streamdown`(117) + `react-syntax-highlighter`(111) | `chat/view/subcomponents/Markdown.tsx`（聊天渲染核心） | **有**：`pulldown-cmark`/`comrak`（Markdown→HTML，wasm 可用）、`syntect`（高亮，wasm 可用）、`katex` crate（公式） | 可 Rust 化，但渲染管线要重搭 + 对拍（GFM/公式/原始 HTML 注入的边界行为） |
| `fuse.js`(89) | 前端模糊搜索 | **有**：`nucleo`/`fuzzy-matcher` | 可 Rust 化 |
| `i18next`(92-93) | `i18n/config.js`，zh/en 双语 | 无标准件；JSON 文案可直接复用，运行时换 Rust 查表 | 可 Rust 化（文案资产零改） |
| `react-router-dom`(110) | `App.tsx:2`（单通配路由） | **有**：Leptos/Dioxus/Yew 均带路由 | 随框架换 |
| `radix-ui`(103)、`lucide-react`(97)、`cytoscape`(84)、`html2canvas`(91) | 无障碍原语、图标、关系图、导出截图 | 图标可搬 SVG 资产；cytoscape/html2canvas **无等价物**（使用面窄，个案处理） | 岛或个案 |
| Tailwind 3 + typography（`package.json:158` 附近 devDeps） | 全局样式 | Tailwind 产物是纯 CSS，与框架无关 | **CSS 原样保留** |
| Electron 42（`apps/desktop/package.json:37`） | 桌面壳 | Wry/Tao（旧计划 DESK-01 候选） | 本阶段不动（见 decision (d)） |

**判断**：无对应物的重依赖集中在「终端 / 编辑器 / Office / PDF」四个 Isles——正是旧迁移包 UI-00 风险 R05 点名的四件（`inventory/risks.csv:6`）。除这四件与 markdown 管线外，其余依赖要么是工具（有 Rust 对应物）、要么是资产（CSS/图标/文案可直接搬）。

## 删掉的结论（起草时写不出 `路径:行号`，已删）

- ~~「Leptos / Dioxus / Yew 三框架在 IME 与高频流式更新下的实测对比」~~：属探针产出（计划 004 Issue F1），不是本轮读码能给的；只保留旧包的定性风险（`risks.csv:6`：synthetic 键盘事件不算真实 IME 证明）。
- ~~「新前端包体积 / 首屏耗时预估」~~：没有可引用的实测基线；留给探针。
- ~~「WS 入站 25 种消息的完整接收点清单」~~：能给出消息类型集合（§二.2），但「每种消息被哪个组件哪一行处理」尚未逐条回源核完，列不出可信行号——已转为计划 004 Issue F2 的产出物，不在本文虚写。
