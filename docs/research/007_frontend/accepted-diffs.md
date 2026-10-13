# 已接受差异 · 前端双壳共存期

> 2026-10-14 起。格式沿用计划 001（`docs/plan/001_phase-c-agent-loop-sidecar.md`）：每条写场景、差异形状、类别、依据、谁批的。
> 范围：旧 React 壳与新 Rust 壳在同一产品内共存期间（计划 004 F3–F6），**跨壳整页加载**必然带来、且决定接受的行为差异。全部条目在 F1 双壳实验（计划 004 Issue F1 S5）实测复核后转正；实测与描述不符的条目回炉，不静默保留。
> 判定依据：`.42cog/intent.md` 不做什么表第一行（崩溃/自相矛盾/安全问题才修）——跨壳丢内存态不属三类，登记为已接受差异。
> 变更（2026-10-14 第 2 轮评审）：**撤原 D3「composer 草稿跨壳丢失」**——回源不符：草稿并非纯内存态，旧壳本就落 localStorage（键 `draft_input_<projectName>:<sessionId|'new'>`，`chat/utils/chatStorage.ts:6-9`；恢复 `useChatComposerState.ts:316-321`、节流写入 `:1621-1629`、`pagehide`/`beforeunload` 离页刷新 `:1643-1644`），移入下方不变式作共享契约；撤后编号暂缺 D3，随后续拆分提交顺延补齐。

| # | 场景 | 差异形状 | 类别 | 依据 | 批准 |
|---|---|---|---|---|---|
| D1 | 任意跨壳跳转（旧→新或新→旧） | 整页加载替代 SPA 客户端导航：主 WS `/ws` 与终端 `/shell` 连接断开并按既有重连逻辑（1s→30s 退避，`WebSocketContext.tsx:48-50`）重建；断口期间的离线队列只缓存 `check-session-status`（`WebSocketContext.tsx:53-75`），其余出站消息丢失 | 有意差异（共存机制代价） | gap §一「跨页导航是 SPA 客户端跳转」；decision (b)1 | AI 代拍，待人复核 |
| D2 | 流式会话进行中跨壳（尤其聊天面迁移后） | 中途增量文本不保证逐字连续：跨壳回来靠 `watch-session` 重挂会话状态，已完成的整段在、断口处的部分帧不可复现 | 有意差异 | `/ws` 无类型 JSON、流式帧不重放（gap §二.2） | AI 代拍，待人复核 |
| D4 | 前进/后退跨过壳边界 | 回退/前进触发整页加载（慢于 SPA 内切换），页面状态由 URL + storage 重建而非内存保留 | 有意差异 | 双入口托管按前缀选 bundle（gap §一） | AI 代拍，待人复核 |
| D5 | 跨壳后的滚动位置与焦点 | 跨壳前进/后退不恢复滚动位置与焦点（浏览器整页加载对 SPA 路由的恢复语义本就有限） | 有意差异 | 同 D4 | AI 代拍，待人复核 |

**不变式（不是差异，是契约）**：同源下 localStorage/sessionStorage 键跨壳共享，键名与语义不许改（`auth-token`：`api.js:29`；`pilotdeck-active-project`：`api.js:45`；**composer 草稿键 `draft_input_<projectName>:<sessionId|'new'>`：`chat/utils/chatStorage.ts:6-9`**；主题/语言键 F1 盘点后补入 decision (b)5）。跨壳后登录态与 composer 未发送草稿必须保持——草稿在旧壳本就落 localStorage（恢复 `useChatComposerState.ts:316-321`、节流写入 `:1621-1629`、`pagehide`/`beforeunload` 离页刷新 `:1643-1644`），丢失不是已接受差异、是回归 bug；这是 F3/F5 每页验收项。
