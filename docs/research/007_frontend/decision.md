# 取舍卡 · 浏览器端 `ui/src` 要不要改成 Rust、怎么改

> 2026-10-14。**一页纸。** 依据见 `gap.md` 与 `must-know.md`（同目录）。
> 依据背景：人 2026-10-14 夜原话「如果做完了，还可以把前端也一并重构……整体进入优化阶段」，顺序 C→A→B→**前端**→优化；`.42cog/intent.md:24` 注明「『前端重构』AI 理解为改成 Rust，**待人复核**」。
> 同夜授权扩大（`CLAUDE.md` §五）：重构过程中的判断 AI 可代做，**每处记「AI 代拍，待人复核」**。本卡拍板栏照此填写。

## 这次不可妥协的几条

见 `must-know.md`：① 用户可见行为一致 ② 对拍能机器判 ③ 能渐进替换、不必一次切换。

## 我选择什么（草稿 → AI 已代拍，待人复核）

> **混合：Rust/Wasm 做壳、路由、状态与全部第一方业务 UI（框架初选 Leptos，探针同负载实测 Leptos/Dioxus 后定，不锁死）；xterm.js、CodeMirror、Univer、pdf.js 等「无对应物四件」保留为 JS 岛，经类型化桥接；按页面渐进替换，聊天面放最后。共存靠「双入口托管 + 双向导航拦截」，跨壳丢连接态登记为已接受差异。桌面端 Electron 本阶段不动。**

### (a) 要不要真的改成 Rust——三选项对比

| | 改成 Rust/Wasm（Leptos/Dioxus/Yew） | 保留 React、只换后端 | **混合：壳与状态 Rust、重组件留 JS 岛** |
|---|---|---|---|
| 与意向书「改成 Rust」的关系 | 完全符合 | 不符——「前端重构」名存实亡，只是阶段 A/B | 主体符合：第一方业务逻辑（≈12 万行里的绝大多数）Rust 化，例外只有无对应物的第三方件 |
| 风险 | 重写 xterm/CodeMirror/Univer：无成熟对应物（gap §四）、IME 是旧包点名的最高风险（`risks.csv:6`） | 最低 | 中：岛桥接与双技术栈共存期的工程纪律 |
| 对拍可判性 | 最难：像素级行为都要重造 | 现成（前端没变） | 可行：黑盒候选 37 例可复用（9 例要真实部署矩阵、28 例要 mock LLM 环境编排，gap §三），岛内行为不变 |
| 旧决策的连续性 | — | — | 与旧迁移包政策完全一致：第三方 JS 允许保留、第一方必须 Rust（`decisions.md:11`） |

**AI 代拍：混合（待人复核）。** 理由：① 意向书要求 Rust 且待人复核的是「改成 Rust」这个方向本身——混合是**保住方向**的同时不押注在「用 Rust 重写终端模拟器」这种无对应物、旧包评估两年都没人敢做的事上；② React 状态层有个自证的痛点——`WebSocketContext.tsx:21-28` 专门写了 `subscribe` 直通来绕开 React 状态批处理。这条证据**只证明一件事：高频 `stream_delta` 需要逐帧分发、不能被状态批处理合并**，因此重写时这是必须过的一关（对拍重点）；它推不出「细粒度响应式框架天然不掉帧」——任何框架处理不当都会丢，须实测（F1）；③ 旧迁移包 P4 默认就是「Rust/Wasm UI + 待评估桌面」（`roadmap.md:11`），本卡等于接回那条线。

**框架 AI 代拍：初选 Leptos 作探针起点，不锁死，F1 同负载实测两候选后定（待人复核）。** 初选理由（弱，仅为探针排序，不是结论）：① Leptos 与 Dioxus 都以 CSR 模式可用（产物纯静态、客户端全量渲染），满足「静态资产 + 同端口托管」的硬前提；② Leptos 文档与社区活跃度在三候选中靠前，遇到 IME 一类问题更可能搜到已知解。**先前「内建 islands 架构、壳挂 JS 岛是官方路径」的论证不成立，已撤回**：Leptos 的 islands 是 **SSR** 场景（服务端渲染页面 + 选择性水合），我们的场景是 **CSR SPA 里由 Rust 壳反向挂 JS 岛**——这不是任何框架的官方路径，岛桥接（挂载/卸载/重挂时岛内 DOM 保留、事件与输入焦点不丢）无论选谁都要自己写、都要实测。同理，「信号天然不掉帧」「社区公认最优」均无依据，撤回。**F1 探针同一套负载（S2–S3 探针页 + xterm 岛）Leptos 与 Dioxus 各跑一遍**，判据相同（中文 IME、≥20Hz 流式 60s、岛生命周期），按实测结果定稿；Yew 的回调式模型与 React hooks 迁移距离最远，仍不选。

### (b) 怎么渐进——按路由 / 按组件岛 / 整体

**AI 代拍：按页面（路由/Tab 粒度）切 + 页内组件岛 + 双入口托管与双向导航拦截（待人复核）。** 理由与顺序：

1. 应用是单通配路由（`App.tsx:50`），`AppShellV2.tsx:79-84` 按 URL 分辨页面——「按路由」的实质是**按页面**：`/settings/*`、`/cron`、`/skills`、`/sop`、`/knowledge` 等都是整页切换。**但「跨页导航是整页加载」在旧 SPA 内不成立**（`AppShellV2.tsx:5, 73, 387` 全是客户端 `navigate`，gap §一），且 `ui/server/index.js:3471` 对全部 HTML 路由单一通配回旧 `dist/index.html`——静态按前缀选 bundle 只在整页加载时生效。因此共存机制必须是两件事，缺一不可：
   - **双入口托管**：选项 A——`ui/server` 的 `app.get('*')` 加路径前缀分支（几行改动，单独评审、可单独回退）；选项 B——前置反代按前缀分流，`ui/server` 零改动。两选项在 F1 双壳实验里实测后定（探针期用选项 B 也行，生产期倾向 A，避免多一个常驻组件）。
   - **双向导航拦截**：旧壳到已迁前缀改硬跳，新壳跳未迁路径时硬跳回旧 bundle。拦截点**下移到路由/历史层**（包住导航原语或监听 history，一处生效），**不逐处包 `navigate` 调用**——「改动点集中在 `navigate` 调用处」的早先判断不符源：全 src `navigate(` 59 处、8 文件用 `useNavigate`，且 `ModelConfigurationErrorScreen.tsx:40` 的 `<Link to="/settings/models">` 组件式跳转绕过 `navigate`，浏览器前进/后退也不经过任何调用点，逐处包必漏；F1 实测验证 `navigate`/`Link`/`NavLink`/前进后退四类入口都被拦住。
   - **跨壳硬跳的已知差异**（登记 `accepted-diffs.md`）：整页加载丢 SPA 内存态——主 WS `/ws` 断开**自动重连**（1s 起、×2、封顶 30s，`WebSocketContext.tsx:48-50, 172-186`）；终端 `/shell` 断开**不自动重连**、`onclose` 只清屏，须手动重进终端（`useShellConnection.ts:174-183`，`shell/utils/socket.ts:4-13` 无重连逻辑——早先「两条通道都按退避重连」的表述不符源，已改）；流式中途的增量文本靠 `watch-session` 重挂但不保证逐字连续；滚动位置与前进后退变成整页加载（慢但行为可用）。**composer 草稿不在差异之列**——已落 localStorage（`chatStorage.ts:6-9`），跨壳保留是契约（见下第 5 条），丢了是 bug。同源 localStorage/sessionStorage 不受影响。
2. **顺序**：探针页（登录 + 会话列表）→ **设置页**（`components/settings/` 17,384 行，表单型、无重岛，风险最低）→ 业务页组（cron/skills/files/git/tasks，`main-content-v2/`）→ **聊天面最后**（≈4.8 万行，行为最细、fixture 用例最多）→ 壳/侧栏（`app-shell/`）收尾退役 React 构建。聊天面迁完前，跨壳跳转避不开流式会话中途切壳的场景，F1 必须实测一次。
3. 页内机制是组件岛：Rust 页面里挂 xterm/CodeMirror 岛（探针就要挂一个真终端岛验证桥）。
4. **放弃**「整体一次性切换」（违反 must-know ③）与「按组件逐个换」（137k 行组件两两桥接的复杂度爆炸，且旧包教训是父任务收不了口——调研 003）。
5. 双壳共存期的共享状态只有 localStorage/sessionStorage 键（`auth-token`：`api.js:29`；`pilotdeck-active-project`：`api.js:45`；**composer 草稿键 `draft_input_<projectName>:<sessionId|'new'>`：`chatStorage.ts:6-9`**——旧壳本就有恢复/写入/离页刷新（`useChatComposerState.ts:316-321, 1621-1629, 1643-1644`），**草稿跨壳必须保留，丢草稿是 bug 不是差异**；主题与语言键由探针盘点），键名与语义对新前端是契约，不许改。**内存态（WS 连接、流中文本）不跨壳共享**，是已登记的已接受差异。

### (c) 对拍装置：61 个 spec 用例（循环展开后注册 72 例）+ 10 个 smoke（见 gap §三）能不能直接当验收

**AI 代拍：分四类、按运行前提分层，不能一刀切（待人复核）。**

- **黑盒候选 = B(9) + D(28) = 37 例，是主尺子分母的来源**——但按运行前提分两层，**没有一类是「今天零环境就能跑」的**：
  - **B 类 9 例要真实部署 + env 矩阵**：`formal-composition`（4，`FORMAL_COMPOSITION_URL` + `FORMAL_REPLACEMENT` 换 profile）、`formal-route-matrix`（1）、`real-auth-composition`（3，profile 三选一、单轮只执行 1/3）、`real-auth-native`（1，注册真实账号）——全部 `test.skip(!env)` 门控（gap §三有逐条行号），**不是 mock 用例**。
  - **D 类 28 例要 mock LLM 环境 + `real/launch.mjs` 三进程编排**（`launch.mjs:17-19` 强制 env；vite 管线写死，新前端接入靠 F2 runner 改造）。跑起来后对任何被测前端一视同仁。
  - 因此「B 类已带 mock、35 例今天就能跑」的早先表述**失实，修正为**：能离线自跑的恰是 A 类 34 例（唯一带 mock/无后端的一批），但它们耦合旧 store，新前端跑不了——**新前端侧今天能直接跑的黑盒用例 = 0**，尺子的可用性押在 F2（runner 接入编排 + B 类部署矩阵脚本化）。
- **A 类 34 例（展开口径，早先记 24 漏了循环展开）不能直接用**：驱动的是旧 store 的 window API（`streaming-lifecycle.spec.mjs:19-28`）。两条路：新前端暴露同等测试钩子（保留对拍能力）；或改写为黑盒用例（更长期，逐条改）。**在改写/移植完成前，这 34 例不计入新前端验收分母**，缺口由 Issue F2 补齐后回收；其中流式与滚动是 A 类测得最细的行为，F2 须先补这两块的**黑盒**机器断言，聊天面（F5）才开工。
- **防意外 skip 是验收的一部分**：B/D 大量 `test.skip(!env)` 且有弱断言（`real/session.spec.mjs:31` 控件缺失静默通过），验收跑必须出「应执行数 vs 实际执行数」对账（72 − 显式 skip 逐条列理由）并 `--forbid-only`，对不上即红。
- **与前端无关（1 例）**：`history-fork.spec.mjs` 是 API 级，归后端。
- **桌面专用（3 例）**：Electron smoke 只在桌面验收时跑；另 7 个 smoke 同 A 类处理。
- 截图：多数用例已带 `page.screenshot` 输出但无像素 diff 基线——**补基线机制、不补像素断言**（must-know ①：判语义，不判像素）。

### (d) 桌面端 `apps/desktop`（Electron）怎么办

**AI 代拍：本阶段一行不动（待人复核）。** 理由：Electron 壳只是 `loadURL` 到 `ui/server` 端口（`main.ts:622`），新前端是 wasm 静态产物、仍由同一端口托管，对桌面端透明；4,925 行桌面代码对 TS 后端的硬依赖（`main.ts:250-258, 921, 930, 941`）是**阶段 B（`ui/server` Rust 化）与桌面 Rust 化（旧计划 DESK-01，Wry/Tao）**的既有问题，与本阶段解耦。桌面端在本阶段的验收 = 3 个 Electron smoke 在「桌面壳 + 新前端」组合下照常通过（计划 004 Issue F6）。Wry/Tao 替换单开 issue，不做进本阶段。

## 我放弃什么

> 放弃「保留 React 只换后端」（与意向书方向不符）；放弃在浏览器里用 Rust 重写终端/编辑器/Office/PDF 渲染（无对应物，旧包两年没敢做）；放弃「整体一次性切换」与「按组件逐个换」；放弃像素级截图断言（只做基线与语义断言）；本阶段不动 Electron、不动 `ui/server` 的**业务与协议代码**（阶段 B）——但共存所需的**双入口托管**要么给 `ui/server` 的 `app.get('*')` 加前缀分支（几行、单独评审），要么前置反代分流，「一行不动」的早先断言撤回；不追上游。

## 依据来自哪一类

- [x] 自有积累 —— 调研 002（边界与协议）、调研 003（旧迁移教训：父任务收不了口、探针先行）
- [x] 他人积累 —— PilotDeck 基线逐条实测（gap §一–§四全部带行号）；旧迁移包只读（`decisions.md:11`、`roadmap.md:11`、`contracts/ui.md`、`risks.csv:6`、`tasks.yaml` UI-00）
- [x] 自己跑的 —— F1 已有 DOM 与协调者 Chrome r3 记录，真实 OS IME 后续由人亲测后定 Leptos（见本卡末尾）；正式37例黑盒双跑仍未完成。

## 两把尺子

**复用分级**

| 来源 | 权限 | 这次用到哪一步 |
|---|---|---|
| PilotDeck `ui/src`（AGPL，被移植对象） | 逻辑可移植，衍生同为 AGPL | 行为参照与文案/样式资产直接复用 |
| 旧迁移包（用户自有，只读） | 只读借鉴 | 政策与风险清单（第三方保留、IME 风险、UI-00 探针哲学）不搬代码 |
| xterm/CodeMirror/Univer/pdf.js 等第三方 | 按各自许可证，入依赖库存 | 原样保留为 JS 岛 |

**满意与最优 · 停止线**：四个问题（a–d）各有结论、每个结论有三选项对比与回退条件、e2e 复用面有实测数字——**到此为止**。框架真伪与 IME 交给探针，不再读材料。

## 支持它的最强依据

`vite build` 产物是纯静态资产（`ui/package.json:22` `build` 脚本），Leptos/Dioxus 的 CSR 产物同为静态资产——双入口托管落在 `ui/server` 既有托管层的最小改动内（`app.get('*')` 前缀分支）或一个前置反代，机制有明确落点、量级已探明（F1 双壳实验实测验收）；黑盒候选 37 例的用例文本与 `real/launch.mjs` 编排现成，新前端的接入点集中在 runner 与部署 env，不需要改用例本身。

## 反对它的最强依据

主尺子对新前端**今天 0 例可直接跑**（A 类 34 例耦合旧 store，B 类 9 例要部署矩阵，D 类 28 例要 mock LLM 编排）——尺子最薄的地方恰是行为最细的流式/滚动，Issue F2 若补不齐，「行为一致」的成色就要打折；共存机制（导航拦截 + 双入口托管）会动旧壳的 `navigate` 调用点与托管层，跨壳硬跳丢内存态（WS、流中文本）已登记为已接受差异但**未实测**（草稿属 storage 契约、跨壳须保留，同样待实测）；聊天面（≈4.8 万行）既是最大工作量又是风险最高点，若探针在 IME/stream_delta 上两候选都失败，混合方案的核心假设（Rust 壳能扛住高频流式 UI）被推翻，整个 (a) 要重拍。

## 仍未补齐的缺口

① Leptos 与 Dioxus 在真实中文 IME + 高频 `stream_delta` + JS 岛生命周期下的同负载实测（F1）；② 双壳同源共存实验——双入口托管两选项、双向导航拦截改动点数量、跨壳登录态/前进后退/流式中途切壳/草稿保留（F1）；③ 跨壳共享的全部 localStorage/sessionStorage 键清单（F1/F2）；④ WS 入站 ~25 种消息的「类型→接收点」表（F2）；⑤ A 类 34 例的移植/改写清单（F2）；⑥ B 类部署矩阵脚本化与防 skip 对账（F2）；⑦ 流式与滚动的黑盒机器断言（F2，F5 开工前置）。

## ⚠️ 拍板的地方

> 授权依据：`CLAUDE.md` §五「授权扩大」（2026-10-14 夜人原话）。以下每处均**待人复核**，复核记录追加于本表。

| # | 需要拍板的 | 选项 | AI 代拍（待人复核） | 复核 |
|---|---|---|---|---|
| a | 改不改 Rust | 全 Rust · 保留 React · 混合 | **混合**：壳/状态/业务 UI 用 Rust，无对应物四件（xterm/CodeMirror/Univer/pdf.js）留 JS 岛 | 待人 |
| a′ | 框架选型 | Leptos · Dioxus · Yew | **初选 Leptos 作探针起点，不锁死**：F1 同一探针负载实测 Leptos/Dioxus（IME、高频流式、岛生命周期），按结果定；islands/信号两类早先依据已撤回（SSR 场景不适用、推不出不掉帧） | 待人 |
| b | 渐进方式 | 按路由/页面 · 按组件岛 · 整体 | **按页面**（探针→设置→业务页→聊天→壳），页内组件岛；共存靠**双入口托管 + 双向导航拦截**（托管选项 A 改 `app.get('*')` 前缀分支 / 选项 B 前置反代，F1 实测定）；跨壳丢连接态入 accepted-diffs | 待人 |
| c | e2e 当验收 | 直接全用 · 只用黑盒 · 全部重写 | **黑盒候选 37 例当主尺子分母**（B 9 例要部署矩阵 + D 28 例要 mock LLM 编排，均非「今天零环境可跑」，可用性押 F2 runner）；A 类 34 例移植/改写前不计分母；防 skip 对账 + 流式/滚动黑盒断言补齐后才验收；截图只做基线 | 待人 |
| d | Electron | 不动 · 同步换 Wry/Tao · 退役桌面 | **不动**；桌面验证 = 3 个 Electron smoke 在新前端下照常过；Wry/Tao 另开 issue | 待人 |

## F1 exp007 复核追加（2026-10-14）

**a′ 暂选 Leptos 0.8.17（AI 代批，待人复核）**：两候选真实 CSR wasm 在 DOM 降级层均通过 composition 合成事件（**synthetic composition only，未测 OS IME**）、≥20Hz/≥60s 接收与 DOM 更新计数、真 xterm 十次挂卸保留 DOM/缓冲/焦点/输入。Leptos 在相同发布 profile / wasm-bindgen 管线下 gzip wasm 更小（111,122 B vs Dioxus 0.7.9 的 210,103 B），第 2 轮插桩后 gzip wasm 为 113,050 B vs 210,521 B，仍为 Leptos 较小；其暂选依据是本探针的体积、构建可用性与其他已测项，**不是 islands SSR 论证，也不是“信号天然不卡”**。详情与 trace 见 [exp007](../../experiments/exp007-frontend-probe/readme.md)。

**第 2 轮撤回协调者首轮「Dioxus IME FAIL → 选定 Leptos」结论**：Playwright `dispatchEvent` 派发了普通 Event，Dioxus 的 CompositionEvent handler 未触发，失败属于探针 bug；首轮 Chrome 原始记录保留在 exp007。修正后用真 CompositionEvent、handler 标记与 rAF 等待；isComposing/keyCode229 各自断言。流式另以 `data-seq = Frame.seq` 做真实帧序号对账，首轮 observer 的 seq 实为接收计数，不能独立证明真实序号连续。协调者后来已补跑修正后的 Chrome 门：两候选均因草稿严格等值断言失败，诊断副本均4/4通过，详见 exp007「协调者补跑 r2」。#42 已修正正式断言；r3 已正式复跑 PASS：协调者用原样 browser.mjs 在本机 Chrome 复跑 Leptos、Dioxus，两候选均4/4 PASS_BROWSER_MACHINE_CHECKS，原件与 bytes/SHA256 见 [r3 manifest](../../experiments/exp007-frontend-probe/evidence/r3/manifest.json)。r2 诊断结果保留为历史记录，当前正式验收以 r3 为准。 本轮原版网络 DOM gates 还因 sandbox listen EPERM 未进入页面；两候选实际 wasm 的七组 DOM gates 通过另行明示的进程内 HTTP/WS 替代传输（1500 帧、≥60s、真实 seq 对账），不算本轮真实网络验证。

F1 worker sandbox 当时无法启动系统 Chrome；协调者 r3 已正式复跑 PASS，两候选各4/4通过，覆盖合成 composition 守卫、登录/storage/列表、xterm 生命周期与真实流式/帧序号/滚动机器检查。浏览器跨壳状态与正式黑盒双跑尚未验证，**F1 不标完成，不凭暂选放行 F3 生产页面迁移**。两候选没有被证明全败；无需按“两败”条款重拍混合方向，r3 的正式 Chrome 证据已补齐；余下缺口仍是浏览器跨壳状态与正式黑盒双跑。

b 的探针结论：Caddy 2.10.2 前置反代 B 已用真实旧 Vite 入口 + 两 Rust 静态入口与 HTTP/WS 做实验，导航原语与 history traversal 拦截在 DOM 级覆盖。A 未搭真实 `ui/server` patch 环境；生产共存方式暂不定稿。D1–D5 以 [accepted-diffs 的 F1 追加表](accepted-diffs.md#f1-exp007-对账与修订2026-10-14追加当前有效口径) 为当前口径，均未以完整跨壳浏览器实测转正。

## a′ 定选（2026-10-14 上午，人亲测后）

**定 Leptos 0.8.17。** 人用真实系统拼音输入法亲测两个探针（`scripts/exp007/serve-ime.mjs`）：组合期间回车上屏不发送、上屏后回车发送，**Leptos 与 Dioxus 均正常**。已测维度（真实 IME、合成 composition、xterm 生命周期、≥20Hz 流式与真实帧序号、滚动）两者无差别，唯一实测差异是同管线 gzip wasm 体积（Leptos 113,030 B vs Dioxus 210,530 B）。按人定 3C「无反例即定 Leptos」执行。

- 推翻的话：F2 起的 runner 与页面都按 Leptos 写；换 Dioxus 需重做 F2 的前端侧适配（探针 `ui-rs/probe-dioxus` 保留可用）。
- 原则（一次拍板 → 一条原则）：自动化测不到的维度由人亲测后才锁选型；锁定后探针保留，作为回退路径的证据。
