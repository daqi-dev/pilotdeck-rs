---
type: plan
number: "004"
date: 2026-10-14
title: 前端重构：浏览器端 ui/src → Rust（Leptos 壳 + JS 岛）——探针先行、按页面渐进
tags: [frontend, leptos, wasm, islands, e2e-parity]
status: planned
related: [research/007_frontend, research/002_parity-boundary, research/003_prior-attempt, plan/001_phase-c-agent-loop-sidecar]
---

# 前端重构计划（浏览器端 `ui/src` → Rust）

## Context

授权与依据一句话：人 2026-10-14 夜定顺序 C→A→B→**前端**→优化并授权 AI 代做全部判断（`CLAUDE.md` §五「授权扩大」）；`.42cog/intent.md:24` 把「前端重构」理解为改成 Rust（待人复核）。调研 007 的 AI 代拍结论：**混合**——Leptos 壳 + 无对应物四件（xterm/CodeMirror/Univer/pdf.js）留 JS 岛，**按页面渐进**，e2e 黑盒 35 例当主尺子，Electron 不动（`docs/research/007_frontend/decision.md`）。

本计划与阶段 A/B **可并行**（intent.md:24：各阶段替换的进程不同；前端是静态资产，A/B 换后端进程不影响本计划——但共享 `ui/server` 的对拍环境编排时要注意端口与资源上限）。

## 现状（已确认）

| 项 | 状态 | 证据 |
|---|---|---|
| 规模 | `ui/src` 841 文件、代码 136,940 行（css 15,504 行另计）；聊天面 ≈4.8 万行、设置 17,384、composition 25,697 | 调研 007 gap §一 |
| 接口面 | HTTP 走 `utils/api.js:36-76`（40+ `/api` 前缀、动态拼接）；主 WS `/ws` 无类型 JSON（`WebSocketContext.tsx:41-46`）；终端 WS `/shell`（`shell/utils/socket.ts:4-13`） | gap §二 |
| e2e | 15 spec 60 例 + 10 smoke：黑盒可直接复用 35 例；fixture 耦合 24 例；Electron 3 例；API 级 1 例 | gap §三 |
| 重依赖 | xterm/CodeMirror/Univer/pdf.js/office 系在 Rust/Wasm 无成熟对应物；markdown 管线可 Rust 化（pulldown-cmark/syntect/katex crate） | gap §四 |
| 旧迁移包 | P4 曾默认 Rust/Wasm UI + Wry/Tao，UI-00 探针哲学（真实 IME/焦点/PTY 先证明）；政策：第三方 JS 可留、第一方必须 Rust | `PilotDeck-Rust/docs/rust-migration-package/`（只读） |
| 桌面端 | Electron 只 `loadURL` 到 `ui/server` 端口，对前端实现透明 | `apps/desktop/src/main.ts:622` |

## 目标

终点：浏览器端**第一方业务逻辑 Rust 化**（Leptos），React 构建退役（xterm 等四个 JS 岛及其直连依赖除外）；黑盒 e2e 对拍全绿（35 例主尺子 + A 类移植/改写后的用例）；Electron 桌面在「新前端 + 现有壳」下 3 个 smoke 照常通过。**怎么算数：对拍**（must-know ②），不以「代码搬了多少」计。

## 执行步骤

> 拆条原则同计划 001：**探针先行的闸门结构**——F1 不出结论，F3 起一页都不许动。F1/F2 可与阶段 A/B 并行开工，但**每条 issue 一个 worktree、同受资源上限约束**（≤5 worktree、`_build`+`_tmp` ≤30 GB、会话 ≤2h，`CLAUDE.md` §五）。issue 编号 GitHub 开了再补。

### Issue F1 · 探针：Leptos 最小页面跑通同一组 e2e（第一条，≤1 天；超时停下交人）

**选「登录 + 会话列表」当探针页的理由**：登录是最小完整面（`/api/auth/*` + token 进 localStorage）；会话列表同时压到 HTTP（`/api/projects`、`/api/sessions`）与主 WS（`watch-session`/`projects_updated`），且黑盒用例对 `/` 路由已有覆盖（`real-auth-composition.spec.mjs:91`）。

**S1 选型 spike（半天盒）**
- `src/`（或独立 `ui-rs/` crate，位置在 S1 里定并记入本计划补丁）起 Leptos CSR 工程：`trunk` 构建，产物为纯静态资产。
- 对照跑 Dioxus 的 hello 级样例各一次，把构建链、wasm 体积、dev 热替换记录进实验文档——**不比 UI 框架性能，只证明构建链可用**。

**S2 探针页实现**
- 登录页：表单 → `/api/auth/login` → token 写 localStorage 键 `auth-token`（键名是双壳契约，`api.js:29`）。
- 会话列表页：拉 `/api/projects` 与 `/api/sessions`，主 WS 连 `/ws?token=`（`WebSocketContext.tsx:41-46` 的 URL 形态）、30s ping、1s→30s 指数退避重连（`:48-50`）。
- **在同一探针页挂一个真 xterm.js 岛**（接 mock 的 `/shell` WS 或本地 echo），岛桥接用 `wasm-bindgen` 导出的类型化接口——探针必须包含岛，否则 (a) 的核心假设没被测到。

**S3 两个命门实测（不过就触发回退）**
- **中文 IME**：composer 输入框拼音组合中按回车不得发送（旧包 `contracts/ui.md:2` 的硬要求；`risks.csv:6`：synthetic 键盘事件不算证明，必须真 IME 或 `compositionstart/end` 级验证）。
- **高频流式**：以 ≥20Hz 注入 `stream_delta` 类消息 60s，页面无可感卡顿、滚动位置不跳（对应 A 类用例盯的行为）。
- 任一失败：先查 Leptos 已知 issue；无解 → 换 Dioxus 重跑 S2–S3（decision a′ 的回退条件）；再失败 → 停，交人仲裁 (a)。

**S4 跑同一组 e2e**
- 新前端与旧前端各起一份（同 mock `/api`，`page.route` 由用例自带），跑黑盒子集中与探针页相关的用例；A 类挑 1 条（建议 `streaming-lifecycle.spec.mjs` 的 history refresh 例）**试移植**为黑盒版当样板，量出移植一条的真实成本。
- 双壳共享键清单（localStorage/sessionStorage 全量盘点）写进实验文档。

**Issue F1 怎么算做完**
- [ ] 实验文档 `docs/experiments/exp006-frontend-probe/readme.md`：IME/流式两个命门的实测数据（截图或 trace）、Leptos vs Dioxus 构建链记录、A 类移植 1 条的成本
- [ ] 探针页在黑盒 e2e 子集上与旧前端同判（或差异逐条有解释）
- [ ] 拍板栏 a′ 复核完成（人或晨报追认）
- [ ] CI `check` 绿（探针代码进 `src/` workspace，照常 fmt/clippy/test）

### Issue F2 · 对拍装置：同一组用例双跑 + 尺子补齐

- **双前端 runner**：`scripts/fe-parity/run.mjs`——同一 mock 后端（或 `real/launch.mjs` 编排），新旧前端各跑一遍同一用例集，输出逐例 PASS/FAIL 对照表与截图基线目录（只存基线，不做像素断言）。
- **WS 入站「类型→接收点」表**：~25 种消息（服务端起点 `ui/server/index.js:2373`）逐条落到前端处理组件（回源给行号），这张表是聊天面（F5）的施工图。
- **A 类 24 例处置清单**：每条标「移植测试钩子 / 改写黑盒 / 放弃（写理由）」，替代计分母报人复核。
- **怎么算做完**：runner 一键出双跑对照表；消息表与处置清单落 `docs/research/007_frontend/`（追加）；黑盒 35 例双跑结果留档。

### Issue F3 · 设置页（第一个生产页面）

- 范围：`/settings/*` 全部（`components/settings/` 17,384 行 + `AppShellV2.tsx:83-84` 的 settings 路由）。
- 理由：表单型、无重岛、黑盒用例现成（`real-auth-native.spec.mjs` 整例打 `/settings/module/agent-route`；smoke 里 settings 系 4 个）。
- 双壳共存：静态托管按 `/settings` 前缀选新 bundle，其余路径走旧 bundle；跨壳跳转即整页加载。
- **怎么算做完**：settings 相关黑盒用例在新前端全绿；旧前端该路由下线；跨壳共享键行为验证（登录态互通）。

### Issue F4 · 业务页组（cron / skills / files / git / tasks / 仪表盘）

- 范围：`main-content-v2/` 各页（9,921 行）+ `git-panel/`、`file-tree/` 等。file-tree/git 若有编辑器与预览入口，接岛。
- **怎么算做完**：同 F3 判法，逐页黑盒用例绿；新增该页缺的黑盒用例（对拍缺口先补尺子再迁页面）。

### Issue F5 · 聊天面（最大、最后）

- 范围：`main-content/` + `chat/` + `chat-v2/` ≈4.8 万行；markdown 管线 Rust 化（pulldown-cmark + syntect + katex crate），GFM/原始 HTML/公式的边界行为按 F2 的消息表逐条对拍。
- composer/权限/elicitation/排队输入等高频交互以移植后的 A 类用例为尺。
- **怎么算做完**：A 类移植用例 + 黑盒用例全绿；`real/*` 26 例在新前端全绿。

### Issue F6 · 壳与收尾

- `app-shell/`（侧栏/导航/组合装配）Rust 化；`composition/`（25,697 行）随壳迁移——它是 Profile 装配系统，是「native/staffdeck」双形态的开关（`formal-route-matrix.spec.mjs` 整例测它）。
- 旧 React 构建退役：四个 JS 岛及其资产保留，其余 `ui/src` 归档（不删，铁律 4）。
- **桌面验证**：3 个 Electron smoke 在「Electron 壳 + 新前端」下通过（`main.ts:622` 不改一行）。
- **怎么算做完**：黑盒 35 例 + 移植用例全绿；Electron smoke 过；双壳托管层拆除。

## 验收（端到端怎么测）

```bash
# Issue F2 之后可用（示意，实际以 runner --help 为准）
node scripts/fe-parity/run.mjs --suite blackbox --frontends old,new      # 黑盒 35 例双跑对照表
node scripts/fe-parity/run.mjs --suite ported   --frontends old,new      # A 类移植用例
node scripts/fe-parity/run.mjs --suite real     --frontend new           # real/* 26 例（需 mock LLM env）
# 桌面（Issue F6）
node e2e/appearance.smoke.mjs && node e2e/desktop-chrome.smoke.mjs && node e2e/desktop-layout-regression.smoke.mjs
```

最后一道是人：真人用新前端配 Rust 后端完整跑一个真实会话（含中文输入、终端、文件编辑），这一步不交给 AI。

## 风险与权衡

| # | 风险 | 应对 |
|---|---|---|
| R1 | Leptos IME/高频流式不过关 | F1 限 1 天、双候选对照、回退 Dioxus 的路径写死（decision a′）；再失败交人 |
| R2 | A 类 24 例移植成本失控（旧 store API 深） | F1 先量 1 条的成本再承诺；F2 出处置清单报人复核分母 |
| R3 | 聊天面体量（≈4.8 万行）重蹈旧迁移父任务收不了口 | 放最后；只在前四条 issue 全部合并后开工；内部再按 F2 消息表分片 |
| R4 | 双壳共存期状态漂移（localStorage 键语义变了） | 键清单是契约，F1 盘点、每条 issue 验证跨壳登录态 |
| R5 | 与阶段 A/B 并行时抢资源（全量对拍同刻只许 1 个） | 对拍跑前 `mkdir /tmp/pdrs-parity.lock` 式加锁；worktree ≤5、磁盘 ≤30 GB |
| R6 | 误碰用户在用的 `~/.pilotdeck` 与 PilotDeck 进程 | 一律临时 `PILOT_HOME`/`TMPDIR`、高位端口，绝不 kill 用户进程（计划 001 R6 同款） |
| R7 | e2e mock 与真实 `ui/server` 行为有偏差（多数用例 mock 了 `/api`） | 主尺子黑盒 + `real/*` 26 例真端到端双保险；差异记「已接受差异」 |

## 范围外

`apps/desktop` 换 Wry/Tao（旧计划 DESK-01，另开 issue）；`ui/server` 的 Rust 化（阶段 B）；性能优化（整体「优化」阶段）；追上游 `cc7c4e6` 之后的提交；pixel-perfect 视觉还原。

## AI 代拍（待人复核；依据 `CLAUDE.md` §五授权扩大）

| # | 需要拍板的 | AI 代拍 | 复核 |
|---|---|---|---|
| a | 改法 | 混合：Leptos 壳 + 四件 JS 岛（`research/007_frontend/decision.md` (a)） | 待人 |
| a′ | 框架 | Leptos，IME/流式失败回退 Dioxus | 待人 |
| b | 渐进 | 按页面：探针→设置→业务页→聊天→壳 | 待人 |
| c | 验收尺子 | 黑盒 35 例直接用；A 类移植/改写前不计分母 | 待人 |
| d | Electron | 本阶段不动，只做 smoke 验证 | 待人 |
| e | 探针页选型 | 登录 + 会话列表 + 一个 xterm 岛（F1 理由如上） | 待人 |

## 相关

`docs/research/007_frontend/`（本计划的依据与全量证据）· `docs/research/002_parity-boundary/`（接口边界）· `docs/research/003_prior-attempt/`（旧迁移教训）· `docs/plan/001_phase-c-agent-loop-sidecar.md`（结构范本）· `PilotDeck-Rust/docs/rust-migration-package/`（旧 P4 计划，只读）
