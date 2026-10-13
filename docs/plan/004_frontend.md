---
type: plan
number: "004"
date: 2026-10-14
title: 前端重构：浏览器端 ui/src → Rust（Leptos/Dioxus 双候选实测后定壳 + JS 岛）——探针先行、按页面渐进、双壳托管共存
tags: [frontend, leptos, wasm, islands, e2e-parity]
status: planned
related: [research/007_frontend, research/002_parity-boundary, research/003_prior-attempt, plan/001_phase-c-agent-loop-sidecar]
---

# 前端重构计划（浏览器端 `ui/src` → Rust）

## Context

授权与依据一句话：人 2026-10-14 夜定顺序 C→A→B→**前端**→优化并授权 AI 代做全部判断（`CLAUDE.md` §五「授权扩大」）；`.42cog/intent.md:24` 把「前端重构」理解为改成 Rust（待人复核）。调研 007 的 AI 代拍结论：**混合**——Rust 壳（初选 Leptos，F1 同负载实测 Leptos/Dioxus 后定，不锁死）+ 无对应物四件（xterm/CodeMirror/Univer/pdf.js）留 JS 岛，**按页面渐进**，共存靠**双入口托管 + 双向导航拦截**（跨壳丢连接态入 `accepted-diffs.md`），e2e 黑盒候选 37 例当主尺子分母（B 9 例要部署矩阵、D 28 例要 mock LLM 编排，均非零环境可跑），Electron 不动（`docs/research/007_frontend/decision.md`）。

本计划与阶段 A/B **可并行**（intent.md:24：各阶段替换的进程不同；前端是静态资产，A/B 换后端进程不影响本计划——但共享 `ui/server` 的对拍环境编排时要注意端口与资源上限）。

## 现状（已确认）

| 项 | 状态 | 证据 |
|---|---|---|
| 规模 | `ui/src` 841 文件、代码 136,940 行（css 15,504 行另计）；聊天面 ≈4.8 万行、设置 17,384、composition 25,697 | 调研 007 gap §一 |
| 接口面 | HTTP 走 `utils/api.js:36-76`（40+ `/api` 前缀、动态拼接）；主 WS `/ws` 无类型 JSON（`WebSocketContext.tsx:41-46`）；终端 WS `/shell`（`shell/utils/socket.ts:4-13`） | gap §二 |
| e2e | 15 spec 61 `test()`（grep 行首 60 + `model-selection.spec.mjs:108` 同行 1 处）+ 10 smoke，**循环展开后注册 72 例**：黑盒候选 37（B 9 例 env 门控真实部署 + D 28 例 mock LLM 编排）；A 类 34 例离线可跑但耦合旧 store；API 级 1；Electron smoke 3 | gap §三 |
| 重依赖 | xterm/CodeMirror/Univer/pdf.js/office 系在 Rust/Wasm 无成熟对应物；markdown 管线可 Rust 化（pulldown-cmark/syntect/katex crate） | gap §四 |
| 旧迁移包 | P4 曾默认 Rust/Wasm UI + Wry/Tao，UI-00 探针哲学（真实 IME/焦点/PTY 先证明）；政策：第三方 JS 可留、第一方必须 Rust | `PilotDeck-Rust/docs/rust-migration-package/`（只读） |
| 桌面端 | Electron 只 `loadURL` 到 `ui/server` 端口，对前端实现透明 | `apps/desktop/src/main.ts:622` |

## 目标

终点：浏览器端**第一方业务逻辑 Rust 化**（框架 F1 定），React 构建退役（xterm 等四个 JS 岛及其直连依赖除外）；黑盒 e2e 对拍全绿（B 9 例部署矩阵 + D 28 例 mock LLM 编排 = 37 例主尺子分母，另加 A 类移植/改写后并入的用例）；Electron 桌面在「新前端 + 现有壳」下 3 个 smoke 照常通过。**怎么算数：对拍**（must-know ②），不以「代码搬了多少」计。

## 执行步骤

> 拆条原则同计划 001：**探针先行的闸门结构**——F1 只出「探针结论」（框架选型按实测定、共存机制两选项定夺、差异清单对账），不出「方向结论」；F3 起一页都不许动。F1/F2 可与阶段 A/B 并行开工，但**每条 issue 一个 worktree、同受资源上限约束**（≤5 worktree、`_build`+`_tmp` ≤30 GB、会话 ≤2h，`CLAUDE.md` §五）。issue 编号 GitHub 开了再补。

### Issue F1 · 探针：双候选跑通同一探针页与同一组 e2e（第一条，≤1 天；超时停下交人）

**选「登录 + 会话列表」当探针页的理由**：登录是最小完整面（`/api/auth/*` + token 进 localStorage）；会话列表同时压到 HTTP（`/api/projects`、`/api/sessions`）与主 WS（`watch-session`/`projects_updated`），且黑盒用例对 `/` 路由已有覆盖（`real-auth-composition.spec.mjs:91`）。

**S1 选型不锁死：双候选同负载实测**
- `src/`（或独立 `ui-rs/` crate，位置在 S1 里定并记入本计划补丁）起 Leptos CSR 工程与 Dioxus CSR 工程各一：`trunk`/`dx` 构建，产物均为纯静态资产；构建链、wasm 体积、dev 热替换记录进实验文档。
- **同一探针代码（S2–S3）两候选各实现一遍**（探针页小，重复成本可控；共享的岛桥接与 mock 装置复用），判据完全相同，按实测结果定稿——初选 Leptos 仅为探针排序，不是结论。早先「Leptos islands 官方路径」「信号天然不掉帧」的依据不成立（islands 属 SSR 场景，我们的是 CSR SPA 反向挂 JS 岛），已撤回，见 decision (a′)。

**S2 探针页实现**
- 登录页：表单 → `/api/auth/login` → token 写 localStorage 键 `auth-token`（键名是双壳契约，`api.js:29`）。
- 会话列表页：拉 `/api/projects` 与 `/api/sessions`，主 WS 连 `/ws?token=`（`WebSocketContext.tsx:41-46` 的 URL 形态）、30s ping、1s→30s 指数退避重连（`:48-50`）。
- **在同一探针页挂一个真 xterm.js 岛**（接 mock 的 `/shell` WS 或本地 echo），岛桥接用 `wasm-bindgen` 导出的类型化接口——探针必须包含岛，否则 (a) 的核心假设没被测到。

**S3 三个命门实测（两候选同判据；不过就触发回退）**
- **中文 IME**：composer 输入框拼音组合中按回车不得发送（旧包 `contracts/ui.md:2` 的硬要求；`risks.csv:6`：synthetic 键盘事件不算证明，必须真 IME 或 `compositionstart/end` 级验证）。
- **高频流式**：以 ≥20Hz 注入 `stream_delta` 类消息 60s，页面无可感卡顿、滚动位置不跳、中间帧不丢（逐帧计数对账，不许「看起来流畅」）——旧代码 `WebSocketContext.tsx:21-28` 证明这关必须过，但推不出哪个框架能过，只能实测。
- **JS 岛生命周期**：xterm 岛挂载/卸载/重挂（页面内来回切换触发）后，岛内 DOM 保留、事件连通、输入焦点不丢、终端缓冲不重置。
- 两候选全败：停，交人仲裁 (a)（混合方案核心假设被推翻）；单候选败：选过关者，无需再议。

**S4 跑同一组 e2e**
- 新前端与旧前端各起一份，跑与探针页相关的用例并双跑对照：D 类 `real/session.spec.mjs`（会话列表/侧栏/模型选择，走 `real/launch.mjs` 编排 + mock LLM env）；B 类 `real-auth-*` 按其 env 门控起最小部署（`REAL_COMPOSITION_URL` 等，S5 的双入口环境可复用）。**注意黑盒用例没有一类零环境可跑**（gap §三）——本步的环境成本就是后续 F2 runner 要脚本化的东西，逐项记录。
- A 类挑 1 条（建议 `streaming-lifecycle.spec.mjs` 的 history refresh 例）**试移植**为黑盒版当样板，量出移植一条的真实成本。
- 双壳共享键清单（localStorage/sessionStorage 全量盘点）写进实验文档。

**S5 双壳同源共存实验（新增，不过则 F3 起的共存方案回炉）**
- **双入口托管两选项各搭一遍**：A——本地给 `ui/server` 的 `app.get('*')`（`index.js:3471`）打前缀分支的最小 patch（几行，实验用，不提交主线）；B——前置反代（caddy/nginx 任一）按前缀分流，`ui/server` 零改动。同一源下新 bundle 挂 `/probe` 前缀、其余走旧 bundle，量出两选项的改动面与运维面，记进实验文档供 F3 定夺。
- **双向导航拦截**：旧壳点入 `/probe` 前缀页须硬跳——拦截**下移到路由/历史层**（包住导航原语或监听 history，一处覆盖三类入口：`navigate` 调用、`<Link>`/`<NavLink>` 组件跳转（如 `auth/view/ModelConfigurationErrorScreen.tsx:40` 的 `<Link to="/settings/models">`，绕过 `navigate`）、浏览器前进/后退；不在各调用点逐处包 `window.location.assign`，逐处包必漏）；实验里逐类入口验证都被拦住；新壳跳未迁路径硬跳回旧 bundle。
- **跨壳行为实测**：① 登录态跨壳保持（`auth-token` 等 storage 键）；② 前进/后退跨壳边界（整页加载、状态由 URL+storage 重建）；③ 流式中途跨壳再返回（`watch-session` 重挂，断口部分帧不可复现）；④ 重连行为按通道分断言——主 `/ws` 断开自动重连（1s→30s 退避），终端 `/shell` 断开**不**自动重连、须手动重进（旧实现 `onclose` 只清屏：`useShellConnection.ts:174-183`）；⑤ composer 草稿跨壳**保留**（`draft_input_<projectName>:<sessionId|'new'>` 键是共享契约，`chatStorage.ts:6-9`——旧壳本就有 localStorage 恢复/写入/离页刷新：`useChatComposerState.ts:316-321, 1621-1629, 1643-1644`；丢草稿是 bug 不是差异）。③–⑤ 按 `accepted-diffs.md` D1–D5 逐条对账，实测与描述不符的条目回炉改写，不静默保留。

**Issue F1 怎么算做完**
- [ ] 实验文档 `docs/experiments/exp006-frontend-probe/readme.md`：两候选 IME/流式/岛生命周期三命门的同负载实测数据（截图或 trace）与选型结论、双入口托管两选项对比、路由/历史层拦截的入口覆盖验证（`navigate`/`Link`/`NavLink`/前进后退逐类过一遍）、A 类移植 1 条的成本
- [ ] `accepted-diffs.md` D1–D5 逐条实测对账完成（对不上的条目已改写或移除）
- [ ] 探针页在黑盒 e2e 子集上与旧前端同判（或差异逐条有解释）
- [ ] 拍板栏 a′ 复核完成（人或晨报追认）
- [ ] CI `check` 绿（探针代码进 `src/` workspace，照常 fmt/clippy/test）

### Issue F2 · 对拍装置：同一组用例双跑 + 尺子补齐

- **双前端 runner**：`scripts/fe-parity/run.mjs`——同一 mock 后端（或 `real/launch.mjs` 编排，vite 管线写死处 `launch.mjs:13` 参数化以接入新前端），新旧前端各跑一遍同一用例集，输出逐例 PASS/FAIL 对照表与截图基线目录（只存基线，不做像素断言）。
- **B 类部署矩阵脚本化**：9 例 env 门控用例（`formal-*`、`real-auth-*`）的部署与 env 编排（含 profile 三选一矩阵：staffdeck/minimal/replacement）进 runner——不脚本化，「37 例黑盒候选」就只是纸面分母。
- **防 skip 对账**：每次验收跑输出「应执行数 vs 实际执行数」（以展开后 72 例注册数为分母），显式 skip 逐条列理由，配 `--forbid-only`；对不上即红。弱断言补强：`real/session.spec.mjs:31` 一类「控件缺失静默通过」的条件点击改为必现断言（改用例走另开 issue、写明理由、过评审——must-know ②）。
- **流式与滚动的黑盒机器断言**：A 类 34 例里测得最细的正是这两块，黑盒面目前为零——F2 须先补（≥20Hz 流式注入、滚动位置不跳、搜索定位），这是 F5 聊天面开工的前置条件，不是可选项。
- **WS 入站「类型→接收点」表**：~25 种消息（服务端起点 `ui/server/index.js:2373`）逐条落到前端处理组件（回源给行号），这张表是聊天面（F5）的施工图。
- **A 类 34 例处置清单**：每条标「移植测试钩子 / 改写黑盒 / 放弃（写理由）」，替代计分母报人复核。
- **怎么算做完**：runner 一键出双跑对照表（B/D 两层环境就绪）+ 对账报告零缺口；消息表、流式/滚动黑盒断言与处置清单落 `docs/research/007_frontend/`（追加）；黑盒候选 37 例双跑结果留档。

### Issue F3 · 设置页（第一个生产页面）

- 范围：`/settings/*` 全部（`components/settings/` 17,384 行 + `AppShellV2.tsx:83-84` 的 settings 路由）。
- 理由：表单型、无重岛、黑盒用例现成（`real-auth-native.spec.mjs` 整例打 `/settings/module/agent-route`——注意它是 env 门控的真实部署用例（`REAL_NATIVE_URL`），F2 的部署矩阵就绪后才跑得起来；smoke 里 settings 系 4 个）。
- **双壳共存落地（机制由 F1 S5 实测后在此定稿，两件事缺一不可）**：
  - **双入口托管**：选项 A——`ui/server` 的 `app.get('*')`（`index.js:3471`）加 `/settings` 前缀分支返回新 bundle 的 index（几行、单独评审、可回退）；选项 B——前置反代按前缀分流（`ui/server` 零改动、多一个常驻组件）。静态分流只在整页加载时生效。
  - **双向导航拦截**：旧壳是 SPA 客户端导航（`AppShellV2.tsx:387` `navigate(SETTINGS_PATH)` 等），不拦截则旧 SPA 点进设置页永远还是旧 bundle——拦截在路由/历史层做（覆盖 `navigate`、`<Link>`/`<NavLink>`、前进/后退三类入口，机制与实测见 F1 S5；`AppShellV2.tsx:387` 只是入口之一），旧壳到已迁前缀改硬跳，新壳跳未迁路径硬跳回旧 bundle。
  - 跨壳硬跳丢内存态（WS/流中文本/滚动）按 `accepted-diffs.md` D1–D5 执行与对账；composer 草稿键跨壳保留是验收项（契约，见 accepted-diffs 不变式），丢草稿按 bug 处理。
- **怎么算做完**：settings 相关黑盒用例在新前端全绿（B 类走部署矩阵）；旧 bundle 对 `/settings` 前缀停返回；跨壳共享键行为验证（登录态互通——这是契约不是差异，坏了是 bug）。

### Issue F4 · 业务页组（cron / skills / files / git / tasks / 仪表盘）

- 范围：`main-content-v2/` 各页（9,921 行）+ `git-panel/`、`file-tree/` 等。file-tree/git 若有编辑器与预览入口，接岛。
- **怎么算做完**：同 F3 判法，逐页黑盒用例绿；新增该页缺的黑盒用例（对拍缺口先补尺子再迁页面）。

### Issue F5 · 聊天面（最大、最后）

- 范围：`main-content/` + `chat/` + `chat-v2/` ≈4.8 万行；markdown 管线 Rust 化（pulldown-cmark + syntect + katex crate），GFM/原始 HTML/公式的边界行为按 F2 的消息表逐条对拍。
- composer/权限/elicitation/排队输入等高频交互以移植后的 A 类用例为尺。
- **前置**：F2 的流式/滚动黑盒断言已就绪（这是 F5 的开工闸门）；聊天面迁入后跨壳跳转避不开流式中途切壳，D1–D5 差异在本期复核一遍。
- **怎么算做完**：A 类移植用例 + 黑盒用例全绿；`real/*` 28 例在新前端全绿。

### Issue F6 · 壳与收尾

- `app-shell/`（侧栏/导航/组合装配）Rust 化；`composition/`（25,697 行）随壳迁移——它是 Profile 装配系统，是「native/staffdeck」双形态的开关（`formal-route-matrix.spec.mjs` 整例测它）。
- 旧 React 构建退役：四个 JS 岛及其资产保留，其余 `ui/src` 归档（不删，铁律 4）。
- **桌面验证**：3 个 Electron smoke 在「Electron 壳 + 新前端」下通过（`main.ts:622` 不改一行）。
- **怎么算做完**：黑盒候选 37 例（B 部署矩阵 + D mock LLM 编排）+ 移植用例全绿，防 skip 对账零缺口；Electron smoke 过；双壳托管层与导航拦截拆除。

## 验收（端到端怎么测）

```bash
# Issue F2 之后可用（示意，实际以 runner --help 为准）
node scripts/fe-parity/run.mjs --suite blackbox-deploy --frontends old,new   # B 类 9 例（部署矩阵 + env 编排）
node scripts/fe-parity/run.mjs --suite blackbox-real   --frontends old,new   # D 类 28 例（real/launch 编排 + mock LLM env）
node scripts/fe-parity/run.mjs --suite ported         --frontends old,new   # A 类移植用例
node scripts/fe-parity/run.mjs --suite streaming-scroll --frontends old,new # F2 新补的流式/滚动黑盒断言
node scripts/fe-parity/run.mjs --reconcile                                # 防 skip 对账：应执行 vs 实际执行（分母 72）
# 桌面（Issue F6）
node e2e/appearance.smoke.mjs && node e2e/desktop-chrome.smoke.mjs && node e2e/desktop-layout-regression.smoke.mjs
```

**没有一类黑盒用例零环境可跑**（gap §三）：B 要真实部署矩阵、D 要 mock LLM 三进程编排——「今天就能跑」的承诺不成立，验收前 F2 必须把两层环境脚本化并在对账报告里体现。

最后一道是人：真人用新前端配 Rust 后端完整跑一个真实会话（含中文输入、终端、文件编辑），这一步不交给 AI。

## 风险与权衡

| # | 风险 | 应对 |
|---|---|---|
| R1 | 两候选 IME/高频流式/岛生命周期均不过关 | F1 限 1 天、双候选同负载实测、判据写死（选型不锁死，decision a′）；全败即停交人，不硬扛 |
| R2 | A 类 34 例移植成本失控（旧 store API 深） | F1 先量 1 条的成本再承诺；F2 出处置清单报人复核分母 |
| R3 | 聊天面体量（≈4.8 万行）重蹈旧迁移父任务收不了口 | 放最后；只在前四条 issue 全部合并后开工；内部再按 F2 消息表分片；流式/滚动黑盒断言先行（F2） |
| R4 | 双壳共存期状态漂移（storage 键语义变了）与跨壳丢连接态 | 键清单是契约（含 composer 草稿键 `draft_input_*`），F1 盘点、每条 issue 验证跨壳登录态与草稿保留；内存态丢失按 `accepted-diffs.md` D1–D5 对账，F1 S5 实测复核 |
| R5 | 与阶段 A/B 并行时抢资源（全量对拍同刻只许 1 个） | 对拍跑前 `mkdir /tmp/pdrs-parity.lock` 式加锁；worktree ≤5、磁盘 ≤30 GB |
| R6 | 误碰用户在用的 `~/.pilotdeck` 与 PilotDeck 进程 | 一律临时 `PILOT_HOME`/`TMPDIR`、高位端口，绝不 kill 用户进程（计划 001 R6 同款） |
| R7 | e2e 与真实 `ui/server` 行为有偏差；B 类部署矩阵搭不起来；意外 skip 让主尺子虚绿 | B+D 双层验收（黑盒部署矩阵 + 真端到端）；F2 对账报告「应执行 vs 实际执行」+ `--forbid-only`；差异记「已接受差异」 |
| R8 | 双入口托管改动引发旧产品回归（选项 A 动了 `app.get('*')`） | patch 限前缀分支几行、单独评审可回退；F1 S5 双选项实测后定；选项 B（反代）作零改动备胎 |

## 范围外

`apps/desktop` 换 Wry/Tao（旧计划 DESK-01，另开 issue）；`ui/server` 的 Rust 化（阶段 B）；性能优化（整体「优化」阶段）；追上游 `cc7c4e6` 之后的提交；pixel-perfect 视觉还原。

## AI 代拍（待人复核；依据 `CLAUDE.md` §五授权扩大）

| # | 需要拍板的 | AI 代拍 | 复核 |
|---|---|---|---|
| a | 改法 | 混合：Rust 壳 + 四件 JS 岛（`research/007_frontend/decision.md` (a)） | 待人 |
| a′ | 框架 | 初选 Leptos 不锁死：F1 双候选同负载实测（IME/流式/岛生命周期）按结果定；两败交人 | 待人 |
| b | 渐进 | 按页面：探针→设置→业务页→聊天→壳；共存 = 双入口托管（A 改 `app.get('*')` / B 前置反代）+ 双向导航拦截，F1 S5 实测后定稿；跨壳丢连接态入 accepted-diffs | 待人 |
| c | 验收尺子 | 黑盒候选 37 例当分母（B 9 要部署矩阵 + D 28 要 mock LLM 编排，非零环境可跑，F2 脚本化）；A 类 34 例移植/改写前不计分母；防 skip 对账 + 流式/滚动黑盒断言补齐后再验收 | 待人 |
| d | Electron | 本阶段不动，只做 smoke 验证 | 待人 |
| e | 探针页选型 | 登录 + 会话列表 + 一个 xterm 岛 + 双壳同源共存实验（F1 理由如上） | 待人 |

## 相关

`docs/research/007_frontend/`（本计划的依据与全量证据）· `docs/research/002_parity-boundary/`（接口边界）· `docs/research/003_prior-attempt/`（旧迁移教训）· `docs/plan/001_phase-c-agent-loop-sidecar.md`（结构范本）· `PilotDeck-Rust/docs/rust-migration-package/`（旧 P4 计划，只读）
