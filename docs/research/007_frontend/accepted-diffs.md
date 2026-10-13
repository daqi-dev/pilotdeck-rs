# 已接受差异 · 前端双壳共存期

> 2026-10-14 起。格式沿用计划 001（`docs/plan/001_phase-c-agent-loop-sidecar.md`）：每条写场景、差异形状、类别、依据、谁批的。
> 范围：旧 React 壳与新 Rust 壳在同一产品内共存期间（计划 004 F3–F6），**跨壳整页加载**必然带来、且决定接受的行为差异。全部条目在 F1 双壳实验（计划 004 Issue F1 S5）实测复核后转正；实测与描述不符的条目回炉，不静默保留。
> 判定依据：`.42cog/intent.md` 不做什么表第一行（崩溃/自相矛盾/安全问题才修）——跨壳丢内存态不属三类，登记为已接受差异。
> 变更（2026-10-14 第 2 轮评审）：**撤原 D3「composer 草稿跨壳丢失」**——回源不符：草稿并非纯内存态，旧壳本就落 localStorage（键 `draft_input_<projectName>:<sessionId|'new'>`，`chat/utils/chatStorage.ts:6-9`；恢复 `useChatComposerState.ts:316-321`、节流写入 `:1621-1629`、`pagehide`/`beforeunload` 离页刷新 `:1643-1644`），移入下方不变式作共享契约；撤后编号暂缺 D3，随后续拆分提交顺延补齐。
> 变更（2026-10-14 第 2 轮评审）：**原 D1 拆分**——两条通道重连行为不同源：主 WS `/ws` 自动退避重连（`WebSocketContext.tsx:48-50, 172-186`），终端 `/shell` 的 `onclose` 只清屏不重连（`useShellConnection.ts:174-183`；`shell/utils/socket.ts:4-13`）。现 D1=`/ws` 自动重连、D2=`/shell` 不自动重连需手动重进，原 D2（流式）顺延为 D3，D4/D5 编号不变，D1–D5 保持连续。

| # | 场景 | 差异形状 | 类别 | 依据 | 批准 |
|---|---|---|---|---|---|
| D1 | 任意跨壳跳转（旧→新或新→旧） | 整页加载替代 SPA 客户端导航：主 WS `/ws` 断开并**自动重连**（1s 起、×2、封顶 30s：`WebSocketContext.tsx:48-50`，`onclose` 排重连 `:172-186`）；断口期间的离线队列只缓存 `check-session-status`（`WebSocketContext.tsx:53-75`），其余出站消息丢失 | 有意差异（共存机制代价） | gap §一「跨页导航是 SPA 客户端跳转」；decision (b)1 | AI 代拍，待人复核 |
| D2 | 跨壳时终端页开着 | 终端 WS `/shell` 断开**不自动重连**：旧实现 `onclose` 只清屏（`shell/hooks/useShellConnection.ts:174-183`；`shell/utils/socket.ts:4-13` 无重连逻辑），跨壳回来须手动重进终端才能恢复会话 | 有意差异（共存机制代价） | gap §二.3；decision (b)1 | AI 代拍，待人复核 |
| D3 | 流式会话进行中跨壳（尤其聊天面迁移后） | 中途增量文本不保证逐字连续：跨壳回来靠 `watch-session` 重挂会话状态，已完成的整段在、断口处的部分帧不可复现 | 有意差异 | `/ws` 无类型 JSON、流式帧不重放（gap §二.2） | AI 代拍，待人复核 |
| D4 | 前进/后退跨过壳边界 | 回退/前进触发整页加载（慢于 SPA 内切换），页面状态由 URL + storage 重建而非内存保留 | 有意差异 | 双入口托管按前缀选 bundle（gap §一） | AI 代拍，待人复核 |
| D5 | 跨壳后的滚动位置与焦点 | 跨壳前进/后退不恢复滚动位置与焦点（浏览器整页加载对 SPA 路由的恢复语义本就有限） | 有意差异 | 同 D4 | AI 代拍，待人复核 |

**不变式（不是差异，是契约）**：同源下 localStorage/sessionStorage 键跨壳共享，键名与语义不许改（`auth-token`：`api.js:29`；`pilotdeck-active-project`：`api.js:45`；**composer 草稿键 `draft_input_<projectName>:<sessionId|'new'>`：`chat/utils/chatStorage.ts:6-9`**；主题/语言键 F1 盘点后补入 decision (b)5）。跨壳后登录态与 composer 未发送草稿必须保持——草稿在旧壳本就落 localStorage（恢复 `useChatComposerState.ts:316-321`、节流写入 `:1621-1629`、`pagehide`/`beforeunload` 离页刷新 `:1643-1644`），丢失不是已接受差异、是回归 bug；这是 F3/F5 每页验收项。

## F1 exp007 对账与修订（2026-10-14，追加；当前有效口径）

依据：[exp007 实验报告](../../experiments/exp007-frontend-probe/readme.md)。本次系统 Chrome 在 sandbox 中启动即 SIGABRT，并出现 EPERM；按任务授权降级为真实 wasm + jsdom DOM 事件测试、真实 xterm 与本地 HTTP/WS。**上表是原假设记录，不能当作已经过完整跨壳浏览器实测的批准表；以下状态与限定覆盖上表的无条件描述。** 不把缺测当作候选失败，也不把 mock 的设计性质当作真实后端已证明的行为。AI 代批，待人复核。

| # | exp007 实测 | 当前口径 / 后续条件 | 状态 |
|---|---|---|---|
| D1 | 两 Rust 探针主 `/ws` 断开后约 1s 自动重连并重发 `watch-session`；30s ping 实际观测。基线原 `WebSocketProvider` 在 DOM fixture 上也约 1s 自动重连。1/2/4/8/16/30/30s 的公式已核对，失败握手完整梯阶未计时 | **区分同文档断线退避与跨文档重新启动连接**：整页加载的新壳重新初始化 `/ws`；旧页面的重试计时器/内存离线队列不跨文档继承。原描述不能推成“跨壳后必等 1s 再连”。只队列 `check-session-status` 属后续生产协议验证，本探针没有实现或验证队列 | 部分支持；跨文档与队列仍待验证，未转正 |
| D2 | 两候选 `/shell` 关闭后观测 2.2s 无自动新连接；点击探针“手动重进终端”后有新的 init/连接。页内挂卸岛保持缓冲是 S3；WS 关闭后 clear 是另一条路径 | 保留“同文档终端断线不自动重连，手动重进可恢复”的假设。跨壳离页/返回是否只有重新进入真实终端页才建连，待浏览器 + 真终端页测试；不将页内岛保留缓冲推成跨文档保留 | 部分支持，未转正 |
| D3 | mock 25Hz 流中关闭旧连接、留 480ms 断口，再建新连接：75 帧中缺 21–31 共 11 帧，两个 `watch-session` 已观测 | 只证明探针/本 mock 没有主动补断口流帧。**“完成整段还在”尚未验证**：探针不迁移生产聊天/历史合并。需真实后端完成快照与生产新聊天面复测，不以这次 mock 为后端帧重放证明 | 部分支持；历史快照未验证，未转正 |
| D4 | 原 React Router 的 navigate / Link / NavLink 均命中 history 层拦截；jsdom 真 back/forward traversal 命中 replace 型硬跳请求；Caddy 按目标 URL 返回两种真实静态入口 | 只证明入口选择与硬跳调用。浏览器真正整页加载、BFCache、返回后的 URL + storage 恢复尚未跑通，不将 spy 记录写成浏览器加载成功 | 部分支持，未转正 |
| D5 | 页内岛重挂焦点通过，跨壳浏览器滚动/焦点没有可用实测 | **撤销上表“跨壳前进/后退不恢复”的无条件断言。修订为：普通新文档重建不承诺恢复原内存中的滚动/焦点；历史遍历的实际结果待 Chrome/BFCache 矩阵验证，允许浏览器自动恢复。** 原因：当前无布局验证，浏览器 `scrollRestoration=auto` 可恢复滚动（[MDN](https://developer.mozilla.org/en-US/docs/Web/API/History/scrollRestoration)），不能靠“整页加载”推出必不恢复。该文档说明不是本次实测 | 原无条件口径撤回；修订假设待测，未转正 |

不变式对账：两 Rust 壳从已有 `auth-token` / `draft_input_probe:s` 启动时，登录请求带 Bearer、composer 恢复“跨壳尚未发送草稿”；`pilotdeck-active-project` 保持。输入事件写草稿通过。同 origin 的 history 遍历不改变 storage 的 DOM 级实验通过，**浏览器旧↔新整页导航仍待补**。主题 `themeMode` / `theme`、语言 `userLanguage` 已回源，完整静态调用点及动态键表达式见 [storage 盘点](../../experiments/exp007-frontend-probe/storage-keys.md)。草稿丢失仍是回归 bug，不变成已接受差异。
