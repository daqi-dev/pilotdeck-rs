# F2：双前端对拍、流式黑盒与基线 72 例 runner

旧入口是固定基线 `ui/` 的完整静态 Vite bundle，新入口仍是 `ui-rs/probe-leptos` 探针。`skeleton` / `streaming-scroll` / `baseline` 两入口代理到一个 mock backend，顺序运行，固定 127.0.0.1:28376（mock）/28377（old）/28378（new）。这三个套件不启动基线 dev server、gateway 或真实 `ui/server`。`real`（D 类）与 `blackbox-deploy`（B 类）使用隔离的真实 runtime 和本地 mock LLM，见文末；B 以旧侧为主，#927 起新侧 native-auth 配对入口开放（见文末），StaffDeck 依赖缺失保持 BLOCKED。

原三个独立套件：`skeleton`（3 例登录/HTTP/WS）、`streaming-scroll`（legacy / timeline 各 4 例，共 8 例聊天黑盒）、`baseline`（直接复用基线 15 spec，循环展开 72 例）。新增用例不冲抵 72 分母。`streaming-scroll` 可以单独作为 F5 前置校准；`skeleton` 保留第一 PR 的完整基线缺口闸门，仍总 exit 1。**本 PR 只做离线构建与自检；旧侧黑盒通过和 #61 真实报告验收仍须协调者运行后确认。**

## 第二 PR：流式 / 滚动 / 搜索定位

测试在完整 `/session/s` 页面运行：HTTP 历史 seed + 主 `/ws` 推送，观察 DOM 文本、`scrollTop`、尺寸和 viewport。STREAM-002 同时要求 textContent 的 0000–0399 全部保留，以及 innerText 恰好含 0200–0399；两者都须有序且各一次。没有旧 store / `window.streamFixture` / React / Wasm 内部状态钩子。`__f2Observation` 仅是测试自己保存的 DOM 采样；API 控制 mock 开始/结束，不经浏览器替业务注入消息。

| 用例 | 机器判据 |
|---|---|
| F2-STREAM-001 | 固定 25Hz × 60s、1500 delta；实际发送率 ≥20Hz，每帧恰好送往一个已 watch 的连接；最终 DOM 序号完整且顺序一致、无重复；有超过 20 个中间内容状态；rAF 采样无 >500ms 停顿、底部不得连续 >500ms 偏离 ≥3px，结束距底 <3px |
| F2-STREAM-002 | 真实 wheel 上滚后，每次 rAF 的 scrollTop 与阅读起点差 <2px，覆盖 200 delta 和 stream_end/complete/历史刷新；点击 Back to latest 后距底 <3px，再启动一次流验证持续跟随；textContent=0000–0399，innerText=0200–0399，各一次且有序 |
| F2-STREAM-SEARCH ×2 | 搜索普通文本及 Markdown 隐藏 URL，显示 `1 / 1` 且下一命中按钮可用；搜索前目标在视口外，搜索后唯一活动高亮 / 正确 href 的链接必须 `toBeInViewport`；距底 >50px 在 `readUp` 之前检查；wheel 阅读后，200 delta 及结束期间位置差 <2px，不能反复居中；Escape 关闭搜索 |

wire 有两条独立线路，所有四项用例都运行两次，标题带 `[legacy]` / `[timeline]`，报告按 8 个不同身份对账。legacy 使用 `{kind:'stream_delta', id, sessionId, provider, timestamp, runId, content}`。timeline 额外使用稳定 `blockId`、`timeline{version:1,turnId,id,order,revision,offset}`；offset 是此前文本的 UTF-16 长度，revision 单调增长。结束发送带 `streamBoundary{turnId,through,revision}` 的 stream_end、由 assistant_block 投影的 `text/role=assistant/isFinal=true/streamState=closed` 完整块，再 complete。timeline 每轮（包括首轮）都有带 runId/turnId 的独立 user 锚点；HTTP 历史保留同一块坐标及 turnId/isFinal，避免刷新时回退 legacy、重复回答或把无锚点的首轮排到第二轮后面。完成前保存 mock 历史；每例 reset，结束及 runner finally 取消计时器，断线移除订阅。原生形状与边界限制见第 8 轮。

第 2 轮修复：mock 必须提供 `/api/modules/runtime`，其 `modules` 与冻结构建的 `generatedFrontendProfile` 一致，且 `runtime.gatewayState='ready'`。旧壳在此接口 404 时会保持 `assembly=null`（`ui/src/composition/runtime.ts:77,177`），因此会话标题可以显示而聊天 surface 不挂载。`/session/:sessionId` 路由（`ui/src/components/app-shell/AppShellV2.tsx:81`）和 `[data-chat-search-surface]`（`ui/src/components/chat-v2/MessagesPaneV2.tsx:1267`）均存在。输出目录中的 `mock-runtime.json` 是 `real/*` fixture 的编排文件，不能替代这个 HTTP 接口。离线自检直接执行基线纯 reader/validator，验证响应及 404/错配负例；不等同于旧侧 4/4 浏览器通过。

第 3 轮依据（冻结 commit `cc7c4e6`，以下 `ui/` 相对只读 `resources/PilotDeck/`）：

- **002 的 400/200 是序号数量，不是滚动像素。** `coordinator-stream-old2/old.json` 指向旧测试第 87 行；第一轮 549 个 rAF 样本的 scrollTop 全为 5490。旧夹具连续两次只发 assistant，没有新 user 消息。`ui/src/components/chat-v2/processGrouping.ts:142` 按 user 划分回合，`:944` 把非最后回答收入 trace；`MessagesPaneV2.tsx:1115` 仅在展开时挂载 trace。因此修复第二次 start 的消息边界：先保存并通过原 WS 通道发送唯一 user text，再发送 status/delta/完成消息。保留最终 400 个 DOM 序号的原断言；不加长种子历史、不改 1280×900 视口。离线自检执行原版 `foldCompletedTurns`，旧夹具复现 200，修复后为 400。
- **撤回第 3 轮的搜索降级依据。** old2 的完整壳截图只能证明当时的自建夹具未定位，不能证明基线没有该能力。更对口的 `ui/e2e/streaming-search.spec.mjs:21–27` 明确要求隐藏 Markdown URL `toBeInViewport`；同一旧产物的 `coordinator-baseline-old2/old-{staffdeck,minimal,replacement}.json` 三个 profile 均 passed。此前引用 `streaming-scroll.spec.mjs` 的 `toBeVisible` 来降低定位断言不成立；issue #70 已关闭为不成立。第 8 轮恢复严格定位并修正搜索前置状态，详见下文。

SEARCH 的 `search-dom-evidence` 在定位成功或失败时均记录搜索前后几何、virtualized 标记及 DOM 阅读锚点；测试不调用 scrollIntoView 或改 CSS。阅读 <2px、底部 <3px、持续失跟随/采样停顿 >500ms 判失败、25Hz×60s 和 1500 增量口径保持。

### 第 5 轮：跨轮连续编号，撤掉临时探针

根据协调者对 old4 附件的结论，本轮修复夹具与累计序号断言的契约：`createStream` 用跨轮 `nextIndex` 生成 marker，仅 `reset` 清零。两次 reading 分别发 `F2FRAME0000END`–`0199END`、`F2FRAME0200END`–`0399END`；每轮 `count/sent` 仍为 200，状态增加 `startIndex` 便于核对。第 3 轮的新 user 回合边界保留。

STREAM-002 在第 5 轮最终使用与首轮相同的 `textErrors(innerText, 400)`（第 7 轮改为 textContent，见下文），要求 0000–0399 每个恰好一次且顺序一致，阅读和恢复跟随判据保持。此前实现只检查正则命中次数，并不检查唯一性；本轮把“400 个不同序号、不丢不重”的既定语义落实到共享校验器。离线自检真实生成两轮，核对 WS 内容和完成历史一致、reset 归零；另构造仍有 400 个 token、却只有 200 个不同序号的重叠负例，必须被同一个最终校验器拒绝。

第 4 轮针对 content-visibility 的取证路线按协调者结论结束，本轮不改旧前端 CSS 或渲染方式。移除临时 `sentBoundaries/historyResponses` 日志、textContent/逐行 CSS/虚拟化属性/资源 URL 探针；保留 `stream-dom-evidence`，以及精简后的 `resume-final-evidence`（注入状态、最终 innerText、总数/不同序号数、滚动几何）。旧附件原件不改写。第 4 轮的探索记录保留在 `_tmp/RESULT.md`，第 5 轮补充实际证据边界。

### 第 6 轮：验证 HTTP 历史，保留原判定

old5 的最终附件证明 innerText 只有 0200–0399，不能据此说第一轮已从 DOM 删除。现有 mock 已在每轮 complete 前 append 完整回答；历史接口返回累计 transcript。old5 记录了 STREAM-002 的初载和两次历史刷新请求，但未保存响应体。old4 原始取证还明确记录第二轮历史有两条 200-marker assistant、最终 textContent=400/innerText=200、两条消息行的 content-visibility=auto。因此“未持久化第一轮”不符合已读代码/历史证据；离屏内容未计入 innerText 是有证据支持的解释，old5 单次运行仍缺响应体及完整 DOM 对照，详见 `_tmp/RESULT.md` 第 6 轮。

新增独立离线自检通过真实 `createMock.handleHTTP` 启动两轮、读取历史，验证 complete 发出当刻及完成后均保留全部已完成回复、完整文本与 WS delta 一致、第二个 user 回合边界存在、累计 0000–0399 有序无重复。删除/清空第一轮回复必须 FAIL；reset 清会话历史。注入虚拟时钟只用于离线自检，无 listen/Chrome。

old6 的 `result.json.mock.historyResponses` 保存每次真实历史响应的深拷贝及 requestIndex/url/runId/status；后续 reset 不覆盖。`resume-final-evidence` 在原断言后补充 textContent 和消息行 key/marker/CSS 值，只供诊断。第 6 轮的最终 PASS 仍由 `textErrors(innerText, 400)` 和原滚动阈值决定，不以 HTTP 或 textContent 代替 DOM 判定。本轮没有更改持久化行为，没有宣称浏览器已修复。

### 第 7 轮：以 textContent 验证 DOM 保留，双读数取证

先解码 old6 原始 `resume-final-evidence`：innerText 仅 0200–0399（200 token / 200 不同序号），textContent 为 0000–0399（400 token / 400 不同序号）；两条 assistant 消息行各有 200 marker，computed `contentVisibility='auto'`，收尾 scrollTop=17758、scrollHeight=18404、clientHeight=646，距底 0。提取摘要保存在 `_tmp/round7-old6-evidence.json`，原报告不改写。

冻结基线 commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的回源位置（以下路径相对只读 `resources/PilotDeck/`）：`ui/src/components/chat-v2/MessagesPaneV2.tsx:290` 给消息行设置 `.chat-message`，`ui/src/index.css:824` 设置 `content-visibility: auto`，`:825` 设置 `contain-intrinsic-size: auto 120px`。离屏且被跳过渲染的内容可能不贡献祖先的 innerText；textContent 读取 DOM 后代文本，仍包含这些节点。因此“每个 delta 都在 DOM 里”的最终判据改为 **`textErrors(textContent, 400)`：0000–0399 每个恰好一次且顺序一致**。只读到后半轮 200 个、缺任意序号、重复或乱序均 FAIL；不以 HTTP 历史替代 DOM 断言。

`resume-final-evidence` 在同一次 DOM evaluate 中采集完整 innerText/textContent，各自记录总 token 数及不同序号数，并记录首轮段落数量、每段 marker、`inDOM`、段落自身 computed `contentVisibility`、所属消息行 key 和 computed `contentVisibility`。段落自身可能为 `visible`，祖先消息行的 `auto` 才是跳过渲染的设置；两者分别保存，避免误判继承关系。附件另保留逐行 marker 和滚动几何，断言成功或失败均采集。`text` / `uniqueMarkerCount` 保留为旧版 innerText 字段的兼容别名。

新增独立离线自检通过同一个最终校验器验证完整 textContent 正例；分别删除 0000、0137、0199、0200、0399，或删除整个首轮，只剩 0200–0399，均必须 FAIL。既有重号/乱序负例继续执行。这些是校验器输入自检，浏览器取证仍由 old7 实跑提供。阅读 <2px、恢复后高度增长和底部 <3px、全程 rAF 与首轮 innerText 判据保持。

old7 随后得到旧侧 4/4 PASS，但当时搜索仅要求 DOM 可见，innerText 仅取证，且只有 legacy 线路。这个历史结果不能放行第 8 轮恢复、补强后的 8 项测试；原始报告保留。

### 第 8 轮：恢复搜索定位、双文本判定、双协议线路

本轮重新解码三份 baseline-old2 原件和 old7 的 `resume-final-evidence`：隐藏 URL 搜索三个 profile 均 passed；old7 的 innerText 严格等于 0200–0399，textContent 严格等于 0000–0399，距底 0。摘要在 `_tmp/round8-prior-evidence.json`。

搜索夹具对照（冻结基线 `cc7c4e6`）：

| 项目 | 基线 transcript fixture | 完整 `/session/s` 夹具 |
|---|---|---|
| 容器 | `streaming-lifecycle.jsx:137–150` 的 100vh flex 容器直接挂 MessagesPane | 完整 shell + ChatInterfaceV2；两者实际滚动元素均为 `[data-chat-search-surface]` |
| 尺寸 | runner 浏览器 1280×900，fixture 无完整壳标题/输入区 | 同为 1280×900；old7 聊天 clientHeight=646；本轮不改 viewport/CSS |
| 行窗口与 CSS | 隐藏 URL 用例为 31 行，另有 201 行的虚拟化专例 | seed=48 行，目标由 Answer 10 移到 Answer 21，后留两轮；目标未折叠；未达到行数阈值 160，短种子也未达到估高 20,000；两者共用 content-visibility:auto |
| 目标位置 | 30 条历史之后追加 target，后续新回答紧随目标 | 对齐尾部目标，保留两轮尾部供搜索后、wheel 前距底 >50px；避免中段阅读时新流式行远离渲染区域 |
| 初始滚动/跟随 | `window.streamLifecycle.set` 更新消息，初始在顶部；没有传 onPauseScroll，也没有挂 useScrollFollow | `open` 明确等待到底部；ChatInterfaceV2:911 传 onPauseScroll，搜索 reveal 会暂停跟随并捕获阅读锚点 |
| 搜索操作 | 点击容器 → Meta+f → fill | 保留同样操作；搜索前增加真实 Home，等待顶部与 Back to latest，先断言目标在视口外 |

夹具已知的不等价点是**从完整壳底部跟随状态直接切入搜索**；`MessagesPaneV2.tsx:1196–1200`、`useChatHistorySearch.ts:102–119,181–195` 与 `useScrollFollow.ts:99–111,293–308` 显示搜索 reveal、暂停、锚点恢复之间存在额外状态变化，transcript 没有这条路径。本轮用真实 Home 输入建立与 transcript 一致的顶部阅读起点，再验证搜索自身完成视口外→视口内的移动。Home 不触碰目标；种子仍为 24 轮，命中位置移到 Answer 21，容器和 CSS 不改。原来的 Answer 10 使搜索成功后新流式行距离阅读区域很远，content-visibility 可以跳过该行；基线却把目标放在尾部。本轮同时修正这项场景差异，保留原来的流式 innerText/rAF 判据，不靠改断言掩盖离屏采样问题。`distance>50` 已恢复在搜索定位后、`readUp` 前检查。**精确到浏览器帧的锚点竞争尚未在本沙箱复现，不能写成已证根因；此修复的浏览器有效性仍须 old8 确认，也不声称覆盖从底部直接搜索的状态切换。**

STREAM-002 保留 `textErrors(textContent,400)`，增加 `textErrors(innerText,200,200)`，共用有起始序号参数的严格校验器。只有 textContent 正确而第二轮未渲染、innerText 为空、漏号、重号、乱序、出现第一轮序号均 FAIL。old7 附件离线回放满足两条判据；这不替代 timeline 的新浏览器实跑。

`stream-contract-self-check.mjs` 执行冻结源声明，对比 `src/agent/stream/TurnTimeline.ts:18–42,66–79` → `src/gateway/client/GatewayAgentEventProjector.ts:41–44,98–112` → `ui/server/pilotdeck-bridge.js:1206–1280` 的 delta/最终块，以及 `ui/server/routes/messages.js:187–218` 的持久化投影。每个 live delta 还进入冻结的 `SessionTimeline`，在最终快照之前核对完整前缀，防止最后一帧掩盖增量丢失；缺 timeline/blockId、错 offset/revision/id/turnId 的变体必须失败。两种线路均经真实 mock HTTP handler 验证两轮历史、complete 当刻和 reset（legacy total=51，timeline 因首轮也有 user 锚点 total=52）；冻结 mergeTimeline 另验证跨轮刷新顺序，删除首轮锚点必须失败；六个基线 side/profile 注册仍各 72，黑盒每端 8 个唯一身份。

边界回源限制：冻结 Gateway `mapModelEvent`（约 :530–543）不投影 message_end，不能声称当前原生链路实际发出了 assistant_stream_end。mock 使用 TurnTimeline 的真实 message_end 边界坐标，覆盖 bridge 已支持的 assistant_stream_end → stream_end 契约；自检明确断言该原生 projector 输出为空，并单独验证边界的 bridge 形状。timeline 的 delta 与 assistant_block 最终块则确实由原生 projector 逐帧对比，前端 `useChatRealtimeHandlers.ts:745–750` 会将它们送入 applyTimelineMessage。

协调者 old8 命令（无需重建旧 Vite 产物；output 必须尚不存在）：

```sh
(
  set -eu
  mkdir /tmp/pdrs-parity.lock || exit 1
  trap 'rmdir /tmp/pdrs-parity.lock' EXIT
  node scripts/fe-parity/run.mjs --suite streaming-scroll --frontends old \
    --output _tmp/fe-parity/coordinator-stream-old8
)
```

放行要求 **8/8 PASS，legacy 与 timeline 各 4/4，0 skip，exit 0 / PASS_SINGLE_FRONTEND_ONLY**。本轮离线自检不代替 old8；任一搜索仍不能进入视口或第二轮 innerText 不满足范围，保留 FAIL 和附件继续定位，不再降级断言。

序号证明**增量内容不丢失**，不声称每个网络包都对应单独一次浏览器绘制。底部允许一个布局/跟随周期的瞬时差，连续失跟随由全程采样捕获；阅读位置不允许先跳再回。`stream-dom-evidence` JSON attachment 保留发送时间、订阅计数、逐帧 DOM 指标及最终文本。截图仍不做像素比较。

新探针没有聊天 surface，测试产生真实 `FAIL`（断言信息 `NOT_IMPLEMENTED: probe has no conversation scroll/search surface`），不 skip、不 xfail。单旧侧 8/8 通过时总状态 `PASS_SINGLE_FRONTEND_ONLY`；双侧只有全部通过才 `PASS_SUITE_ONLY`。基线仍标 72/0 NOT_RUN。

## 第二 PR：完整基线报告（#61）

`baseline.config.mjs` 直接指向只读基线 `ui/e2e/**/*.spec.mjs`，无基线 config import、webServer、globalSetup 或 `real/launch.mjs`。旧构建额外包含 `composition.html`、model-selection/streaming-lifecycle/streaming-scroll fixture HTML，所以 A 类原用例可从静态服务加载；新入口不提供旧 fixture。浏览器仅 `channel: chrome`；基线 Playwright 默认临时 profile 使用本次输出下的 `browser-tmp`，黑盒使用 `_tmp/fe-parity/profiles/` 的独立 persistent profile。不会下载浏览器。

`--profiles staffdeck,minimal,replacement` **分别生成完整 72 例原始 JSON**，不是单报告中重复 projects，也不从一份报告复制。`execution-env.mjs` 清理继承的 FORMAL/REAL/PILOTDECK/REPLACEMENT/FE/PLAYWRIGHT 环境变量，所有 spec 直接读取的 URL 固定到被测侧的 2837x 入口；runtime/evidence 路径固定到本次 worktree 输出。模型标题变量不继承，以免注册漂移。每轮执行前对实际 config 再 `--list --forbid-only`，逐身份核对原 72。

- profile 只设置 `REAL_COMPOSITION_PROFILE` 和 `FORMAL_REPLACEMENT` 等原 spec 门控。**这些是 mock 执行 profile，不是三套真实部署**。本 mock 不实现 Knowledge/SOP/设置/真实 LLM；相应用例会失败。API 级 history-fork 已有非空历史，fork 接口仍明确 404/FAIL。原 spec 内的 `page.route` fixture 保持原样，不改冻结来源。
- `FORMAL_ROUTE_PROFILE` 保持 unset，保留历史 `formal undefined routes ...` 身份和原文 skip；真实 route 矩阵需要后续注册口径评审，不能本轮自动改名。
- 原基线 serial 组首例失败后，Playwright 可能把后续例标无理由 skip。D 类的精确身份归为 `NOT_RUN_PENDING_ORCHESTRATION`，逐条保留原始 skip annotations 和源码依据，仍总 FAIL；B 类继续原身份/理由规则，无理由级联 skip 仍非法。原 spec 已知条件点击弱断言也仍存在，完整报告不是行为全覆盖证明。
- 每份 JSON 含实际 config metadata：frontend/baseURL/profile/backend=`f2-mock`。独立验证后才合并；任一 profile FAIL 不被另一个 PASS 覆盖。两侧合并 skip 身份不等同样 FAIL。

预期全量报告当前**总 exit 1**，原因可包含未实现 API/页面、serial 级联 skip，以及新探针缺 fixture。#61 验收应检查真实报告的 72 个身份与每个 skip 的判定，不能只看退出码。不得据此宣称 37 个真实部署/LLM 用例已通过。

## 依赖与源码构建

从worktree根运行。`BASELINE_UI`可指向只读基线ui目录；默认向祖先搜索ui/或resources/PilotDeck/ui。runner 在用例发现、self-check、报告对账和服务启动之前，要求该目录为 Git 仓库的实际 `ui/`，HEAD 精确等于清单中的 `baselineCommit`（`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`）；不匹配、无法读取 Git、tracked 源码相对 HEAD 有改动、e2e 下有未跟踪源码均拒绝。校验结果写入 `result.json.baseline`。不能用父仓库 HEAD 为随意复制的 ui 背书。复用基线**现有**node_modules中的Vite、ws、Playwright、xterm；未装新JS包。node_modules 完整性（#61.2）：`node_modules/.pnpm/lock.yaml`（pnpm 安装时写入的锁文件副本）必须与 `pnpm-lock.yaml` 逐字节一致；`.modules.yaml` 须为 `nodeLinker: isolated`、`virtualStoreDir: .pnpm`、整数 `virtualStoreDirMaxLength`。ui 与根两个 manifest 的**全部**直接依赖（dependencies + devDependencies，真实基线共 145 个，含 runner 经 `requireUI`、`--import tsx` 与被求值冻结源码加载的包）逐一按锁文件对应 importer 绑定：store 依赖须 realpath 到按 pnpm 编码（含 peer 嵌套与超长名截断 + SHA-256 前缀）推得的目录且 package.json 版本吻合，`link:` 须指向锁定的本地目录，`file:` 以 store 目录绑定；未声明的 `@xterm/*` 也拒绝。self-check 断言 scripts/fe-parity 下所有 `requireUI(...)`/`requireUI.resolve(...)` 目标都在受检集合内（playwright-core 在 ui 下不可解析，调用方须容忍其缺失，单列为 UNBOUND）。结果写入 `result.json.baseline.nodeModules`。边界：不校验包内文件内容哈希与传递依赖的链接，安装后篡改包内文件仍检测不到。缺依赖时由协调者准备有 lockfile 的源码副本，用 `pnpm install --frozen-lockfile --ignore-scripts`（产生 isolated `.pnpm` 布局与 `node_modules/.pnpm/lock.yaml`）重建依赖；npm/hoisted 布局会被上面的 node_modules 门拒绝，不能用。不能写只读基线、伪造 lockfile 或下载 Playwright 浏览器。

旧构建不调用会生成源码的npm prebuild；使用已经生成的基线composition源码，Vite configLoader=runner，构建/缓存只写本worktree。输出包含原版业务模块，未使用exp007的孤立React测试页；mock明确构建OSS模式并启用本地认证（PILOTDECK_DISABLE_LOCAL_AUTH=0 / VITE_IS_PLATFORM=false），不继承平台.env。

```sh
node scripts/fe-parity/build-old.mjs
# 需要本机已有wasm32 target；不要rustup下载预编译target/工具。
# 若本worktree尚无匹配工具，用cargo源码构建；只下载crates源码。
CARGO_HOME="$PWD/_build/cargo-home" \
CARGO_TARGET_DIR="$PWD/_build/tools-target" CARGO_BUILD_JOBS=2 \
cargo install wasm-bindgen-cli --version 0.2.129 --locked --root "$PWD/_build/tools"
python3 scripts/fe-parity/build-new.py
```

old=`_build/fe-parity/old`，new=`_build/dist-leptos`（与exp007共同产物形态）。Python构建固定worktree CARGO_HOME/target，检查wasm-bindgen 0.2.129与Leptos lockfile相符，只构建Leptos。

## 验证与运行

```sh
# 可在禁止loopback listen的worker沙箱跑，绝不监听/启动Chrome：
node scripts/fe-parity/run.mjs --self-check
# 只检查规则/注册、不要求构建产物（不代表资产或浏览器已验收）：
node scripts/fe-parity/run.mjs --self-check --rules-only
# 已构建旧侧时可单独校准其资产：
node scripts/fe-parity/run.mjs --self-check --frontends old
# 由协调者在可listen环境跑，检查真实静态/HTTP代理/WS upgrade，两入口同mock：
node scripts/fe-parity/run.mjs --network-self-check
# 协调者先校准旧侧聊天，再观察新侧真实失败：
node scripts/fe-parity/run.mjs --suite streaming-scroll --frontends old
node scripts/fe-parity/run.mjs --suite streaming-scroll --frontends old,new
# A 类改写样板（a-identities.json 绑定 (file, title)；独立账，不并入现行分母 37；
# `--suite a-class --reconcile` 直接拒绝，A 改写结果出现在基线/real 报告里一律 FAIL）：
node scripts/fe-parity/run.mjs --suite a-class --frontends old
# 基线完整三 profile / 两侧原始报告：
node scripts/fe-parity/run.mjs --suite baseline --profiles staffdeck,minimal,replacement --frontends old,new
# 可先仅旧侧生成三份报告：
node scripts/fe-parity/run.mjs --suite baseline --profiles staffdeck,minimal,replacement --frontends old
# 协调者浏览器双跑：本机已装Chrome，persistent context且每例worktree临时profile。
node scripts/fe-parity/run.mjs --suite skeleton --frontends old,new
# 当前预期：skeleton.md有3行PASS/FAIL；baseline-parity.md有72行NOT_RUN；总exit 1。
# 没有浏览器报告时的严格负向对账（预期exit 1）：
node scripts/fe-parity/run.mjs --reconcile
# 后续完整基线报告对账：须为不同文件，声明 config.forbidOnly 与 frontend/baseURL 来源，无重试/重复项目结果。
node scripts/fe-parity/run.mjs --reconcile --old-report <old.json> --new-report <new.json>
# 合并多个 profile 的完整报告，再核对两侧最终 skip 集合：
node scripts/fe-parity/run.mjs --reconcile \
  --old-report <old-staffdeck.json> --old-report <old-minimal.json> --old-report <old-replacement.json> \
  --new-report <new-staffdeck.json> --new-report <new-minimal.json> --new-report <new-replacement.json>
```

输出默认 `_tmp/fe-parity/run-<随机后缀>/`；可用 `--output _tmp/fe-parity/<本次目录>` 指定，拒绝worktree外路径。含实时注册原件、各侧Playwright JSON/日志、逐例表和result.json。黑盒 persistent profile 留 `_tmp/fe-parity/profiles/`；基线自动清理的临时 profile 只创建在本次 `browser-tmp/`。指定的输出目录若已存在立即拒绝（EEXIST），不覆盖/清理人的证据；重跑请用新输出目录。

self-check验证六份 side/profile 配置注册、流式/滚动正负控制、进程内mock handlers、单端与双端对账正负例、报告文件身份/来源、WS表逐链接字面量或显式分支标注，以及静态资产存在/路径保护。输出明确72/0 NOT_RUN，但 `PASS_SELF_CHECK_ONLY`/exit 0仅表示这个独立规则检查通过。network-self-check额外检查两侧真实HTTP/WS代理，不启动浏览器，也不计基线执行数。所选套件的任何失败或非法 skip 则 exit 1；streaming-scroll 的基线 72 未执行单独列账。

固定端口 mock=28376、old=28377、new=28378（28379保留）。所有监听写死127.0.0.1，不读取PORT/HOST，不启动真实PilotDeck/gateway，不碰3001/18789/用户PILOT_HOME。资源finally关闭；中断仅向本runner创建的Playwright进程组发信号，不全局kill。

## 对账规则

分母来自本次展开注册数，历史72用于漂移警报而非取代动态清点。逐身份核对完整结果：应执行=注册数−**身份与理由均获准的实际skipped**；实际执行=passed/failed/timedOut的唯一尝试。每条SKIP打印原因；无理由或身份/理由清单外skip、NOT_RUN、重复/未知身份、interrupted、xfail、behavioral FAIL、report errors、注册漂移、数量不符全部FAIL。报告必须声明 `config.forbidOnly=true`；每侧独立核对后再联合核对，两个报告同时漏例也不能绿。

**D 类待编排单独列账**：[pending-orchestration.json](pending-orchestration.json) 固定 28 个精确身份，仅提供诊断分类，与 B 类 skip 清单不相交。只有收到这些身份真实的 `results[0].status='skipped'` 才归为 `NOT_RUN_PENDING_ORCHESTRATION`；已执行的 PASS/FAIL 保留，缺结果仍是普通 NOT_RUN，未知 `real/*` 身份不自动归类。原始 `real/*.spec.mjs` 没有 `test.skip` 调用，因此这次 serial 级联的“基线 skip 原文”明确记为**无**（`baselineSkipDescription: null`），`rawSkipAnnotations: []`，不编造理由；另附注册、serial 配置、`real/launch.mjs:17` 的编排依赖行号与可观察到的前序失败身份。意外出现 skip annotation 时原样保留并另报源码漂移错误。

`pendingOrchestration` 计数不扣减分母、应执行数或最低执行数，也不增加执行数和合法 skip 数。三 profile 合并按身份去重统计待编排数；任一原件 pending 即保留 FAIL，即使另一个 profile 已执行。`baseline-parity.md` 另列每端/每原件的完整身份、原始 annotations、基线 skip 原文及回源；`result.json` 的逐 profile rows/observations 保留结构化证据，离线对账的 `reportInputs` 给出原件顺序。D 类接上 mock LLM 编排前，不得据此宣称 B 9 + D 28 的 37 例目标已完成。

**历史全量 `baseline` 套件的身份许可来源唯一固定为 [skip-allowlist.json](skip-allowlist.json)**：按 `caseId = JSON.stringify([file, 完整展开 title])` 精确匹配，只收 exp007 B 类的9例真实部署/env门控用例。每项的 `source` 给出固定基线 `cc7c4e6` 的用例注册、门控与环境依赖路径:行号，`rationale` 逐条说明原因；不按 `formal-*` / `real-auth-*` 文件名或标题前缀放行。`formal-route-matrix` 保留历史清点未设 profile 时的完整标题 `formal undefined routes follow the generated composition`；不把具体 profile 标题自动归一化成已获准 skip 身份。B 部署使用文末独立的严格 cell 对账：保留具体标题、项目名和源位置，不调用历史 skip 许可来扣减应执行数。

**实际执行数必须 ≥ 注册数 − 本次注册中命中身份清单的数量**：当前72−9=63。三例 skeleton 与清单无交集，因此最低执行数仍是3。清单内用例允许正常执行，不要求skip；全执行时应执行72，9例合法skip时应执行63。缺失报告/NOT_RUN不算显式skip，仍需72例。身份清单不由报告、环境变量或CLI扩展；任何增删/更名都必须提交清单diff与逐条基线源码依据，经评审后才能生效。

skip 必须只有一个 `type: "skip"` annotation，其字符串 description **逐字等于该身份 `skipReasons[].description` 中的一项**；理由命中不能替代身份许可，也不能借用另一个已获准身份的理由。每条理由的 `source` 回指基线 `test.skip` 的路径:行号。以下全部是冻结源码中的字符串字面量，无理由模板；formal-route 的 `${profile}` 是**标题**模板，不是理由模板。`beforeEach` 的 URL 门控先执行，缺环境时中断后续 profile 检查；文件级 skip 同样阻止进入测试体，所以这些原文理由是备选值，不拼接。其他 annotation 类型不影响匹配，多条 skip annotation、空白变体、改一个字或旧的人造理由 `deployment profile not applicable` 均 FAIL。

| 绑定身份（缩写，完整身份见 JSON） | 原文 description 与基线来源 |
|---|---|
| formal Knowledge / SOP lifecycle / SOP+Skills（三例分别绑定） | `Set FORMAL_COMPOSITION_URL to run against a formal Web+StaffDeck deployment.`（`ui/e2e/formal-composition.spec.mjs:6`）；`Replacement profile only runs its conformance workflow.`（`:8`） |
| formal replacement Knowledge | 同文件 `:6` 的 URL 理由；`Replacement conformance requires FORMAL_REPLACEMENT=1.`（`:9`） |
| formal undefined routes | `Set FORMAL_ROUTE_PROFILE and FORMAL_COMPOSITION_URL for a formal route-matrix run.`（`ui/e2e/formal-route-matrix.spec.mjs:5`） |
| authenticated StaffDeck | `Set REAL_COMPOSITION_URL and REAL_COMPOSITION_PROFILE for an authenticated composition run.`（`ui/e2e/real-auth-composition.spec.mjs:7`）；`StaffDeck acceptance only.`（`:53`） |
| authenticated minimal | 同文件 `:7` 的 URL/profile 理由；`Minimal-profile acceptance only.`（`:135`） |
| authenticated replacement | 同文件 `:7` 的 URL/profile 理由；`Replacement-profile acceptance only.`（`:151`） |
| native Settings | `Set REAL_NATIVE_URL for the local authenticated native-profile acceptance run.`（`ui/e2e/real-auth-native.spec.mjs:4`） |

缺环境触发的**基线原文** skip 允许对账通过，但只代表未执行该用例，结果仍显式列 SKIP；不能据此宣称该部署行为已验收。清单与理由变更须提交源码依据并评审。

两侧 **合并后 SKIP 身份集合（file + 完整展开 title）必须完全相等**；skip 数量相等但身份不同也 FAIL。D 类待编排不进入获准 skip 集合，本身即导致 FAIL。两侧72例全SKIP、单侧72例全SKIP、两侧同一清单外身份使用原文理由SKIP均 FAIL。self-check 对16条身份/理由绑定逐条验证原文正例、改单字反例、清单外身份与跨身份借用理由反例；另检查 D 类28个身份逐条回源、无扣减、原文保留、跨 profile 执行不掩盖 pending，以及实际 PASS/FAIL 不被重分类。

**多 profile 合并规则已实现**：重复 `--old-report` / `--new-report`，每份必须是同一注册清单的**完整报告**（当前72例）；每份内仍只允许每个身份一个结果和一次尝试，不接受分片、重试、重复项目。先逐份验证注册身份、理由、状态、forbidOnly 与来源，再按身份合并：任一 profile 执行就计一次执行；所有 profile 都合法 skip 才扣除一次。跨 profile 多次执行不增加分母或执行数。任一报告的失败、超时、xfail、interrupted、缺例、非法 skip 或 report errors 都保留为 FAIL，其他 profile 的 PASS 不能掩盖。`result.json` 保存各份 `profiles` 账目及各身份 `observations`，两侧在合并后比较 skip 集合。每份原件（含同侧）必须不同，别名/硬链接重复输入也拒绝。

合并不会重命名身份：`FORMAL_ROUTE_PROFILE` 非空会把历史 `formal undefined ...` 标题改为具体 profile，仍触发注册漂移/未知身份拒绝。合并支持当前固定身份的互斥用例；正式 route 矩阵的注册口径与部署编排仍按下一 PR 的独立评审处理，不能为了合并自动归一化标题。

报告来源由执行时的 Playwright config 写入，`browser.config.mjs` 与 `baseline.config.mjs` 已自动设置；接入完整基线套件时也必须在实际使用的 config 中设置：

```js
metadata: { frontend: 'old', baseURL: 'http://127.0.0.1:28377' }
// new 对应 { frontend: 'new', baseURL: 'http://127.0.0.1:28378' }
```

JSON reporter 将其保存在 `config.metadata.frontend` / `config.metadata.baseURL`。`--reconcile` 要求它们与所标端及固定入口精确一致，缺失/互换/端口不符均 FAIL；不会按命令行标签替报告补写元数据。`--old-report` 和 `--new-report` 必须为不同文件，同路径、相对/绝对别名、符号链接和硬链接复用同一原件均 FAIL；复制 old 报告到另一个文件也会因 new 来源不符而 FAIL。此校验依赖报告如实保留执行配置，不构成对手工篡改 JSON 的密码学认证。

WS 映射 self-check 检查全部45条消息（35主通道＋4历史兼容＋6其他通道/本地事件）的每个行号链接：标签/固定commit目标一致，目标行含声明的字符串字面量；无字面量的共用分支须显式标注分支代码和另一个可验证的字面量链接。历史 `stream_delta` 错指 Thinking 注释的反例必须 FAIL。

## 留待协调者与后续 PR

- 协调者真实 Chrome 跑旧侧流式双协议 8 例并保存全量 72 例多 profile 报告；未跑前 F5 闸门与 #61 待验收项不勾选。
- A类34例逐条处置与替代分母，改冻结用例按另issue+评审流程。
- B 类 StaffDeck 4 例仍 BLOCKED，待锁定外部模块源码与既有运行时/种子编排；D 类已有 real 编排。B/D 的完整 37 候选双跑与 72 全量验收仍待后续。

WS施工图见 [ws-message-map](../../docs/research/007_frontend/ws-message-map.md)。#42正式Chrome复跑见exp007 browser.mjs；本runner不会代替其IME/流式/岛门。


## D 类真实 runtime + mock LLM（issue #58）

`node scripts/fe-parity/run.mjs --suite real --frontends old` 默认 `--real-mode all`，先 ready 24 例、再 onboarding 4 例。`--real-mode ready` / `onboarding` 可单独诊断，输出 `PASS_PARTIAL_REAL_MODE_ONLY` 时仍明确 D 分母 28 和未请求身份 NOT_RUN，不能据此宣称 D 28 已验收。真实启动目前只支持 old；new 并没有等价的原生 UI/runtime。另支持不启动服务的 `--suite real --reconcile --frontends old,new`，每侧必须提供 ready/onboarding 两份独立原件，配置元数据必须如实声明该端，不能把 old 报告记作 new。

`real.config.mjs` 的 ready 使用自有 `real-tests/` 包装入口 import 只读基线 spec，onboarding 直接加载只读基线；注册按相对 `ui/e2e` 的完整身份及源行号核验 24+4。JSON reporter 的 spec.file 按其 config.rootDir 还原真实源路径，再相对基线 e2e 对账，不重命名注册。没有 import 基线的 Playwright config / webServer / globalSetup，也没有执行或修改 `launch.mjs`。保留基线 viewport=1440×900、timeout=120s、expect=15s、单 worker 和 serial；固定 forbidOnly/retries=0/repeatEach=1。只用本机 `channel: chrome`，关闭 video（避免另需下载 ffmpeg），保留失败 trace/screenshot，浏览器临时 profile 在输出的 browser-tmp 内。

等价启动工作由 `real-launch.mjs` 完成：新建独占目录，写同 schema 的 provider/agent/webui 配置与带 E2E-FILE-SEED 的 README.md/sample.ts、e2e-seed SKILL.md、runtime.json；auth.db / PILOT_HOME / workspace 全在该目录内。JSON 写入 pilotdeck.yaml（合法 YAML）。复用只读 native `createRuntimeSupervisor`，其 server 和 gateway 命令不变；仅用 spawnImpl 将 client 换成 `real-vite.mjs`，Vite 仍加载只读基线配置/源码，但使用 configLoader=runner、worktree cache、strictPort，避免写基线缓存或另找端口。`real-setup.mjs` 用真实 Create Account / Sign in 流程取 storageState，显式等待 auth-token；不伪造登录。原 globalSetup 未指定 Chrome channel，因此不能原样调用。

所有运行数据在本 worktree `_tmp/fe-parity/real-run/run-<随机>/ready|onboarding/`，旧目录不删、不覆盖。启动前通过只连接端口的探测检查 **全部七个**固定端口空闲：ready=3010/5180/18798，onboarding=3011/5181/18799，LLM=28379。占用、权限不足或探测超时均 FAIL；不杀占用者。每次 spawn 前再查本 mode 端口；显式 HOST/PROXY_HOST/GATEWAY_HOST=127.0.0.1。自有 runtime 的预加载保护也拒绝其他 TCP bind 和外部连接，阻止 server/Vite 自动换端口及用户 proxy/env 泄漏。launcher detached 建立一个自有进程组；supervisor 的子进程不 detached；清理只信号本 runner spawn 的进程组，TERM 后最多 5s KILL。不会连接、监听或杀 3001/18789，不碰 ~/.pilotdeck。

固定 `REAL_TEST_KEY='fe-parity-mock-key'` 在 env.mjs 标明是公开测试值，realExecutionEnv 设置 E2E_LLM_CENTER_URL=127.0.0.1:28379。实际 spec 不根据三个 chat 标题切 provider，默认三个都走 OpenAI；Anthropic/Google 配置可显示但不据此声称协议已调用。mock 只实现 OpenAI `/v1/chat/completions`（兼容 `/chat/completions`）的 SSE / 非流式及 `/v1/models` 列表。确定性脚本：最新 user 的 E2E-CHAT marker、metadata.purpose=session_title_generation 的 JSON 标题、连接文字 1、基线随机 solid PNG 的确切颜色。tools 可以出现在请求里，响应不生成 tool_calls。未知请求/脚本显式拒绝并使实跑总体 FAIL。

onboarding 是空 providers，无 gateway。其 OpenAI UI 预设 URL 不读 E2E 环境变量，`real-network.mjs` 仅在自有 runtime 中将 `https://api.openai.com` 的 `/v1/models`、`/models`、`/v1/chat/completions`、`/chat/completions` fetch 改为本地同路径，保持 method/body/headers，写 network.jsonl。其他外部连接阻断留证，更新检查可显示错误；没有外部下载。该注入只替换 LLM 网络目的地，不改 /api 业务响应、DOM、spec 或 provider/config catalog。浏览器用 DNS 隔离参数阻止外部请求。

每次实跑保留 `old-ready.json` / `old-onboarding.json` 原始 reporter、runtime/stdout/stderr 日志、注册 JSON、llm-requests.jsonl（所有请求/脚本/响应/逐帧 SSE；credential 字段只标 fixed-test-key）、network.jsonl（重定向/阻断）、test-results、real-parity.md 和 result.json。`reconcileReal` 对每个原件先用既有 reconcile 核对固定分区及真实配置 provenance，再组装互斥的 D 28 身份；不能把 24/4 当重复 profile 合并，否则会错误报缺例。完整模式注册分母 28、合法 skip=0；真实 passed/failed/timedOut 按既有规则记 PASS/FAIL。第 4 轮起，只有冻结源码和已匹配失效 FAIL 机械推出的 STALE_CASCADE 扣减 expected/minimumExecuted（不计执行/通过/行为覆盖）；其余 serial skipped 仍为 NOT_RUN_PENDING_ORCHESTRATION / FAIL。72 全量规则和 B 九身份理由许可未改，其他 44 例明确 NOT_RUN。

离线：`node --test scripts/fe-parity/mock-llm.test.mjs`（14 组正负控制；冻结 native OpenAI parser 验证流式/非流式，冻结 PNG 编码验证 4 色，stub 网络保护，无 listen/Chrome）；`run.mjs --self-check` 同时核对两个 real 配置的实际注册。默认 self-check 仍要求 old/new 资产齐备；只有 old 资产的现场使用 `--self-check --frontends old`；无资产的规则验证使用既有 `--rules-only`。real 浏览器 preflight 不要求 mock-UI 静态 bundle，因为运行只读 Vite dev 源码，而规则/协议/注册检查全部执行。

协调者在沙箱外运行（output 必须尚不存在；无需再提供真实 API key）：

```sh
(
  set -eu
  mkdir /tmp/pdrs-parity.lock || exit 1
  trap 'rmdir /tmp/pdrs-parity.lock' EXIT
  node scripts/fe-parity/run.mjs --suite real --real-mode all --frontends old \
    --output _tmp/fe-parity/coordinator-real-old5
)
```

完整实跑 D 注册分母仍为 28、合法 skip=0。第 5 轮当前规则要求四条清单失效满足 MATCHED_FAIL 的旧侧条件，11 条 serial 级联机械推出，其余 13 条全 PASS、0 待编排；应执行/最低执行为 17（ready 13 + onboarding 4）。原始 FAIL 保留，旧侧校准门只证明单侧；详见末尾第 5 轮及 `_tmp/RESULT.md`。


第 2 轮修复针对协调者 old1 的 16/28 执行结果。ready 不再注入显式 gateway TOKEN：原生 gateway 生成 `PILOT_HOME/server-token`，bridge 的 TOKEN_PATH 显式指向同一文件。就绪 gate 要求该文件非空且原生 bridge 已报告 connected；账号 setup 随后读取真实 `/api/projects`，保存 `old-ready.projects.json` 并验证 General 的 workspaceCwd。`WORKSPACES_ROOT` 清理后显式设为本 mode 的 workspace；配置 databasePath 与 env 的 runDir/auth.db 一致，避免规范化配置默认值把它改到 home/auth.db。General 预建目录不替代 RPC 成功；仍使用原始项目/session 状态。

onboarding 自动发现列表会在未填 key 时发无 Authorization 的 OpenAI GET models。mock 仅为这个只读端点接受完全缺失认证头，completion 仍要求固定测试 Bearer，错误 key/Basic/其他协议头继续拒绝。LLM 日志新增 authHeaders 的 absent/fixed-test-key/invalid 三态，不落 credential 原文。网络日志新增每个预加载进程的 entrypoint/executable/ppid。runtime PATH 使用新目录 bin/node（链接本机 Node）及系统路径，避开操作人全局 TaskMaster CLI 的自动更新探测；额外设置 TASKMASTER_SKIP_AUTO_UPDATE=1、npm offline、自有 cache，不下载依赖。

runner 的独立 `real-tcp-audit.mjs` 用 `ps` 递归取本进程树和自有 process group，再以 `lsof -nP -a -p <PIDs> -iTCP -F pftnT` 观察全部 TCP 状态和监听，含 Chrome。实跑前、每 mode 测试前后、runtime 清理后及总清理后均强制采样，期间每轮结束 250ms 后再次采样；原始结果、进程树和解析的 TCP 目标写 `tcp-connections.jsonl`。观察到目标 3001/18789（或本树监听它们）立即 FAIL，TERM 自有组并在 5s 后 KILL，停止整个 real run；观察工具/解析/证据写入失败也 FAIL。没有在这些端口主动连接或清理占用者。轮询会漏掉采样间完整生灭的极短连接，因此它是 net.connect 钩子的独立补充，不等于完整连接历史或 OS 防火墙。离线自检只注入 ps/lsof 文本，不执行这个观察器。

old1 根因、每项证据/源码行号、launch env/目录差异、独立基线失败及 old2 重跑检查点见 `_tmp/triage-r2.md`；第 2 轮验证与交接追加到 `_tmp/RESULT.md`。基线及 28 执行下限未改变，old2 仍需保留真实失败，不能预报 28 PASS。


## 第 3 轮：项目注册和基线失效对账（历史；失效级联口径见第 4 轮）

old3 的 workspace/e2e-project 有种子，但 home/projects 中没有注册。冻结 launch.mjs:21-23 只创建 workspace/home，:77-80 只写种子，没有 `.cwd`。原生 src/web/server/listProjects.ts:28、:45、:201-210 以 PILOT_HOME/projects 为目录源，读取有效 `.cwd` 才恢复绝对路径；ui/server/projects.js:131-152、:169-212 会读取并补齐同一注册。real-launch 现在调用冻结 `createProjectId`，写入本轮 home/projects/<id>/.cwd，runtime.json 保存 projectPath/projectId/registrationPath；不写会话。setup 通过真实 /api/projects 验证该项目恰好出现一次且 files 未显式禁用（常规项目不下发 capabilities，UI 默认启用），保留 RPC 原件。

注册仍不足以选项目：AppShellV2.tsx:232-256 与 appShellSelection.ts:32-35 在 `/` 默认选 General；MainAreaV2.tsx:123-134 对 General 关闭项目 Files。仅 Files 的包装入口在冻结 beforeEach/appReady 后通过原生侧栏 `e2e-project` 按钮选择项目，并检查真实 `/p/<id>` URL，写入 real-project-precondition 附件。原 frozen 文件测试、其他三个 ready serial spec 和 onboarding 不改；不拦截请求、不注入 DOM/业务 state、不单例重跑、不绕过 serial。

`baseline-stale.json` 只列三个已证明的精确身份：SES-SIDEBAR-003、SET-ABOUT-001（设置页返回文案已替换）、SKL-CREATE-001（字面竖线选择器）。FIL-TREE 不入表。每条含 defectType、证据路径/行号/原文/文件 SHA-256、可复现 proofScript、失败 assertion 与 spec callsite（文件、行、列）、matcher、完整 locator。清单由代码中的已评审 canonical JSON SHA-256 绑定；改单字/身份借用/增删/证据行号漂移均 FAIL。源码证据也逐行、逐文件 hash 验证，import 或自检即失败，不能通过单独改 JSON 扩大豁免。

离线证明 `node scripts/fe-parity/baseline-stale-proof.mjs` 执行冻结 fixtures.clickButton 声明，渲染冻结 SettingsSidebar 和 Skills Header（真实 react-dom/server；只替代翻译/图标等依赖），读取 en/zh-CN 原翻译，并检查全 ui/src 可执行源码中没有 backToApp 引用。证明的是选择器/文案失效，不是浏览器实跑。设置返回按钮实际使用 backToProjects，仍接 onClose；AppShellV2.tsx:389-396 根据 selectedSession / selectedProject 返回 session / project / `/`。git 历史 f4afc71f3226912d8de6c8bf0a87a926271256d2 也明确把 backToApp 换成 backToProjects。

失效用例照常执行，原始和账目行始终保留真实 FAIL；单侧运行仍 FAIL。双侧只有**全部原件**都在绑定 helper 断言位置、各自 spec 调用位置、matcher 和 locator FAIL 才记 `MATCHED_FAIL` 一致。两侧同在错误位置失败、单侧/两侧 PASS、SKIP、缺失、重复/重试、借用身份、其他业务失败、serial pending、provenance/report errors 均不允许被此规则覆盖。72 多 profile 路径与 D 28 分区路径共用这条规则。侧账目仍显示 FAIL，双侧总状态只表示对账一致，不改用例 PASS/FAIL。

报告新增 baselineStale 独立清单与 behavioralCoverage；D 最多 25 个有效行为守护身份，失效三条覆盖记 0。执行分母/应执行/最低执行仍 28。serial 后继六个 Session、两个 Skills 仍不算执行，不能因已知前序失效自动放行。old4 在 Files 全部成功且无新问题时可能恢复四个 Files 执行，但不预报其通过，更不预报 D 28 放行。

双侧 D 原件离线对账（每端的 JSON metadata 需为该端、原 mode、suite=real-D、backend=native-with-mock-llm，baseURL 固定为 ready 5180 / onboarding 5181；各端分时运行）示例：

```sh
node scripts/fe-parity/run.mjs --suite real --reconcile --frontends old,new \
  --old-report <old-ready.json> --old-report <old-onboarding.json> \
  --new-report <new-ready.json> --new-report <new-onboarding.json> \
  --output _tmp/fe-parity/coordinator-real-pair1
```

成功只报 PASS_REAL_PARITY_ONLY（D_28_PAIR_ONLY），不表示全量 72 或真实运行的网络/TCP 审计验收。此命令不启动服务；读取原件并验证不同文件/别名和固定来源。当前没有 new 实跑结果，不能据此宣称双侧通过。第 3 轮源码调查、old3 只读复核、离线验证和 old4 完整命令见 `_tmp/RESULT.md`。


第 3 轮保留协调者 lsof exit 1 + 空 stderr 的可用 stdout 修复与原单测，并补上 `!killed && !signal`：execFile 超时后若子进程处理 SIGTERM 并 exit(1)，可能同时报告 code=1 / killed=true / signal=null / 空 stderr，这份部分观察不能接受。已用无 listener 的短进程复现 Node 属性组合，新增独立负控制；正常 exit 1 的非空/空 stdout 路径继续有效。详见 `_tmp/lsof-exit-timeout-proof-r3.json` 与 RESULT 第 3 轮审阅。


## 第 4 轮：Files explorer 展开与失效级联（历史；Files 改判见第 5 轮）

old4 的 Files trace 中，顶部 Files 模式已 pressed，但右侧 `navigation[aria-label="Workbench panels"]` 中 Files 为 `aria-pressed=false`，Smart Chat 为 true；全部 56 条 /api 记录里没有文件树请求。冻结 `MainContent.tsx:540,564-568` 默认/进入 Files 时折叠 explorer，`:1088-1135` 只在 explorerVisible 时挂载 FilesV2；冻结 clickButton 的 `.first()` 点到顶部模式按钮，不展开右侧 explorer。因此需要额外真实用户交互，不是空种子/权限/根节点问题，FIL-TREE 不增加失效登记。

`files-precondition.mjs` 在 Files 包装入口的项目选择之后点顶部 Files，再精确定位 Workbench panels 内的 Files 按钮并展开；等待真实 GET `/api/projects/<id>/files`，记录 status/body/按钮状态到 `real-files-explorer-precondition`。该树来自 `useFileTreeData -> api.getFiles -> ui/server/index.js:1944 -> extractProjectDirectory(.cwd) -> getFileTree`；gateway `project_files_list` 的 `/api/projects/files?projectKey=...` 是另一条接口。保持冻结原断言、身份、serial、视口及服务链，原件不改写。

real 的 `STALE_CASCADE` 由 `serial-topology.mjs` 读取冻结五个 spec 的 AST，核对完整文件 hash、serial configure 范围和注册行号，结合固定 28 身份的实际 --list 核验，从失效前驱之后的源码注册顺序推出；没有新增级联身份 JSON 清单。只有前驱在登记 assertion/callsite/matcher/locator 真实 FAIL、expectedStatus=passed，后继原始 skipped 且 skip annotation 为空，才归入级联。失效 PASS、其他断言 FAIL、缺失/重复/重试、其他 serial 组、失效之前、手工状态/skip 理由均不能取得这个排除；缺结果仍 NOT_RUN，非失效的 Files 失败后继仍待编排。

注册分母 28 保留。当前三条失效的 serial 后继机械得出 Session 6 + Settings 0 + Skills 2 = 8；这些行不计执行、不计 PASS、不计行为覆盖，不记合法 skip。满足旧侧校准条件时实际执行/应执行/最低执行均为 20，17 PASS + 3 原始 FAIL，失效覆盖 0，级联覆盖 0，行为守护 17/17；报告单列失效 FAIL 条件满足数、STALE_CASCADE 数、待编排数。old4 离线重账为 17/20 执行、3 失效条件满足、8 级联、3 待编排，旧侧门仍 FAIL。

`oldGate` 是单侧校准门：完整 ready/onboarding 来源正确、三条失效条件满足、级联规则有效、其余全执行 PASS、待编排 0、无其他报告/编排/LLM 错误才 PASS。行仍保留 FAIL，断言 account.status 仍 FAIL；完整 old runner 依 oldGate 选择 PASS_SINGLE_FRONTEND_ONLY 或 FAIL。部分 mode 无法满足完整旧侧门，仍只报局部结果。正常 Playwright exit 1 只在原始 reporter 含 failed/timedOut 且无信号时作为行为失败处理；exit 2/信号/退出码与原始结果不一致仍编排 FAIL，实际退出码仍写入 executions，不改原始 reporter。双侧必须三条 MATCHED_FAIL 全部成立，并重新验证两边级联推导及身份集合完全一致，才可能 PASS_REAL_PARITY_ONLY；失效 PASS 却保留级联、手工标非后继、单侧级联集合不同均 FAIL。

失效清单仍为原三条；本轮只修改 note 并同步已评审 canonical digest，身份、证据、断言/调用点未扩展。72 全量和 mock 多 profile 的待编排/skip 原规则保持；real 的级联排除不借给 mock 测试。离线完整自检包括三组失效/级联正负控制、14 组 mock/native 编排测试，以及真实 --list 的 24+4 注册。old5 命令见上面的运行块；本轮不代跑，不起服务，不提交。详细原件与根因/TCP 复核见 `_tmp/RESULT.md` 第 4 轮及 `_tmp/round4-old4-evidence/`。


## 第 5 轮：FIL-TREE 基线失效改判

撤除 `files-precondition.mjs` 及 Files 包装入口的项目选择/附件：冻结 spec 仅通过普通 import 注册，原 beforeEach、serial、断言、视口不变。原生 `.cwd` 注册、种子与账号 setup 的 `/api/projects` 校验保留。冻结 global setup 只保存登录状态；appReady 每例 goto('/')；默认路由选 General。项目可见不等于自动选中，额外侧栏 click 属于补交互，因此也撤除。

离线证明执行冻结 chooseDefaultProject、fixtures.clickButton 和 JSX/状态 AST 片段，用真实 react-dom/server 渲染 en/zh-CN 按钮。General 下工作台 Files 不挂载，宽选择器实际命中 composer 的 Add files or skills / 添加文件或技能并打开上下文菜单，无法显示种子树。普通项目的对照场景中，`.first()` 先命中顶部 Files 模式按钮，进入 Files 再折叠 explorer，FilesV2 不挂载；再点顶部按钮返回 chat，复现 old5 前置与原 spec 的冲突。反向控制展开 explorer 时可挂载。这个脚本不启动浏览器/服务，不把片段渲染当整页实跑。

`baseline-stale.json` 新增 FIL-TREE-001，四条清单的 canonical SHA-256 为 `80b8fa5c1624133e630a00ef717343cf4bc9d648b1044d1c047d1c56565f5d38`。Files 精确失败为 assertion/callsite 同一 `real/files.spec.mjs:9:38`、matcher `toContainText`、locator `locator('body')`。直接 spec 断言的 callsite 取原 stack 首帧，helper 断言继续保留不同文件调用点；错误行列、locator、PASS/skip、重试与来源均拒绝。失败仍真实执行，覆盖记 0。

历史证明：`a5f1d1128bf716ea54f12042cd244bb503f74405`（2026-09-04）把 explorer 初值 false 改 true，并新增进入 Files 时折叠的 effect；`bed6177be...`（2026-09-05）引入顶部 Files ↔ chat 切换与 General 的 Files 隐藏；files.spec 最后修改 `e816fca4fc7e46feb1c550bcd600f4bc194d1be3`（2026-09-29），在 UI 提交之后。因此证据是冻结 spec 与当时已有 UI 契约不符，不能声称 spec 写在 UI 改动之前。有界 git log/blame/show、离线脚本结果及 old5 只读摘要见 `_tmp/round5-old5-evidence/`。

serial AST 自动推出 Files 后三例 FIL-TREE-002 / FIL-EDIT-001 / FIL-PREVIEW-001；加上原 Session 6、Skills 2，共 11 STALE_CASCADE。期望总账为注册 28、执行/应执行/最低执行 17 = 13 PASS + 4 原始 FAIL、合法 skip 0、待编排 0、行为守护 13/13；ready 13 执行 = 9 PASS + 4 FAIL，onboarding 4 PASS。四条旧侧 MATCHED_FAIL 候选仍 UNPAIRED；两侧真实原件同位 FAIL 且级联集合相同才可确认配对。old6 命令把 output 设为 `_tmp/fe-parity/coordinator-real-old6`；唯一锁、原件保存、bridge/LLM/TCP 全审计继续由原 runner 执行。本轮不代跑、不起服务、不提交。

## 第 6 轮：real 的 OS HOME 隔离与本机应用审计

real 的 ready/onboarding 各有独立 `R=real-run/run-*/<mode>`：`HOME=USERPROFILE=R/home-os`，与 `PILOT_HOME=R/home` 分开；XDG config/data/cache 位于 home-os 内，`TMPDIR/TMP/TEMP=R/tmp`。目录在 --list 之前创建并检查 canonical ancestor，禁止 symlink 逃逸。real 入口的 inventory 子进程也使用 output/preflight 下的隔离环境。server、gateway、Vite、Playwright CLI/worker/Chrome 继承相同隔离 HOME；Playwright 自行创建临时 profile，仍使用已安装 Chrome。每模式的 `old-<mode>.isolation.json` 只记录隔离路径和断言结果。

强制 `PILOTDECK_SKIP_BROWSER_OPEN=1`；启动前用 `os.userInfo().homedir` 核验 HOME 不等于操作者真实 home，并核验全部路径与 opt-out。real launcher、supervisor spawn 与 Node preload 再次断言。进程审计改用完整 `ps -ww ... args=`：看到 open、xdg-open、osascript 或非 Playwright 的 `/Applications/` 应用，先触发自有进程组 TERM / 5 秒后 KILL，再写 FAIL 证据，不等待 lsof。仅允许具备自有 Playwright CLI 祖先、`--remote-debugging-pipe`、本模式 tmp 内 `playwright_chromiumdev_profile-*` 的 Chrome；其固定 Chrome bundle helper 还须有该浏览器祖先。GoogleUpdater、server 自启 Chrome、LibreOffice 不获放行。审计观察到即失败；定期进程快照不构成全历史 OS 沙箱证明。

冻结基线的 `ui/server`、`src`、`packages` 全检索有 113 个匹配行，逐项去向与源码 hash 见 `_tmp/round6-isolation-evidence/home-isolation-audit.md`。old1–old6 的影响分级及 old7 完整命令见 `_tmp/RESULT.md` 第 6 轮。本轮不启动服务/浏览器，不提交；不能将旧版 TCP PASS 当作 HOME 隔离或本机应用安全 PASS。若 old7 观察到 soffice/GoogleUpdater，应按此门失败并保留证据。

## 第 7 轮：按进程区分 HOME，限制 updater / soffice

本轮替代第 6 轮的“Chrome 也继承隔离 HOME”及一律禁止 updater / soffice 两项策略，其余服务隔离、原断言、记账和失败关闭保持。old7 原件仍为 FAIL：`tcp-connections.jsonl:98` 捕获 Chrome bundle 的 `Helpers/GoogleUpdater.app/.../GoogleUpdater --install`；不把它重算为获准的已有用户 updater。

- server / gateway / Vite / setup / Playwright CLI 和 worker：`HOME=USERPROFILE=R/home-os`、隔离 XDG、`TMPDIR/TMP/TEMP=R/tmp`、`PILOT_HOME=R/home`、`PILOTDECK_SKIP_BROWSER_OPEN=1`。mock 现在是独立 Node 子进程，使用 `real-run/run-*/mock` 的同类隔离环境和网络 preload；通过 IPC 回传最终原统计，不改 HTTP 协议/响应/拒绝记账。launcher、supervisor spawn、preload、Vite、setup 入口继续或补充隔离断言。
- Playwright 启动的本机 Chrome 及其子进程：仅 `launchOptions.env` 中 `HOME/USERPROFILE=os.userInfo().homedir`，移除服务侧 XDG 路径；TMP 仍在运行目录，NODE_OPTIONS 清空。globalSetup 和各测试 browser 共用同一 launchOptions。临时 `--user-data-dir` 仍由 Playwright 创建，CLI 本身不使用真实 HOME。
- `real-browser.mjs` 经 `@playwright/test → playwright → playwright-core` 解析**实际依赖**，读取默认 Chromium switches，四个指定开关仅补缺项。当前 Playwright 1.60.0 已默认提供 `--disable-component-update`、`--disable-background-networking`、`--no-first-run`、`--no-default-browser-check`，本轮移除旧配置重复的 background 参数。无法解析默认列表即 FAIL；每模式 isolation.json 保存依赖版本/源码 SHA、默认与追加列表、浏览器 HOME/TMP。
- Chrome 放行仍要求自有 Playwright CLI 祖先、remote-debugging-pipe、当前 tmp **直接子目录**中的 `playwright_chromiumdev_profile-*`，并核对真实 HOME 与四个开关各一次。固定 bundle helper 必须有核验 Chrome 祖先、实际 HOME 正确；以前已观察的 helper 可凭 PID+原命令+本次 HOME 复核处理 reparent。
- GoogleUpdater 仅放行操作者真实 `~/Library/Application Support/Google/GoogleUpdater/<version>/GoogleUpdater`，且**当前**祖先链有已核验 Chrome/helper，HOME 为真实 home，无安装/注册/卸载或其他部署模式开关。拒绝 Helpers updater、不同 home、路径穿越、只有 Playwright worker/相同 group、任何 `--install`；已装 updater 不因安装路径正确而单独获准。
- soffice / soffice.bin 仅放行自有 native supervisor 下、命令精确匹配冻结 server/index.js 的 server 后代，并核对 server 与 soffice 本次 HOME 都为该运行隔离目录；仅有 supervisor/gateway/CLI 祖先不够。open / xdg-open / osascript 一律 FAIL。

实际 HOME 来自 `ps -ww -E -p <目标 PID> -o pid=,ppid=,pgid=,args=`，与首份 args 快照的 PID/PPID/PGID/完整 argv 匹配后仅抽取 HOME，证据只写 HOME、不写完整环境。结构性违规先 fatal，HOME 不符则在 lsof 前 fatal；环境不可读、缺失/歧义、命令变化或目标在两份快照间消失均 FAIL。本 worker 沙箱禁止 ps；本轮只有注入快照正负验证，old8 需在协调者侧执行真实观察。此审计仍是定时采样，不是全进程生命周期监控。

server 的 LibreOffice `--version` 探测来自冻结 `ui/server/services/officePreview.js:235–240`，没有 env override 或独立 UserInstallation；它继承隔离 HOME。只读核对本机 `/Applications/LibreOffice.app/Contents/Resources/bootstraprc` 为 `UserInstallation=$SYSUSERCONFIG/LibreOffice/4`，版本 26.8，buildid `bce0998afefdbc355585ca324285661a2170ba77`。该版本 [HOME/configDir 源码](https://github.com/LibreOffice/core/blob/bce0998afefdbc355585ca324285661a2170ba77/sal/osl/unx/security.cxx#L315) 先用存在的 HOME，macOS configDir 在它下面加 `Library/Application Support`；[bootstrap 解析](https://github.com/LibreOffice/core/blob/bce0998afefdbc355585ca324285661a2170ba77/sal/rtl/bootstrap.cxx#L502) 将 SYSUSERCONFIG 取为 configDir。因此默认 profile 为 `R/home-os/Library/Application Support/LibreOffice/4/user`，处于运行目录中。service env 清除 UserInstallation/SYSUSERCONFIG/SYSUSERHOME/URE_BOOTSTRAP 的继承覆盖；HOME 预建以避免源码回退到账户目录。本轮未运行 soffice，不声称 --version 实际创建过 profile；真正转换的临时 `-env:UserInstallation` 仍按原源码处理。

完整第 7 轮证据、21 组离线单测、自检与 old8 命令见 `_tmp/RESULT.md` 及 `_tmp/round7-process-home-evidence/`。不启动服务/Chrome，不提交；不据此宣称 old8 或双侧 real 已通过。

## 第 8 轮：运行 bin 的真实 git 与 Xcode license 精确例外

`real-git.mjs` 在 ready/onboarding 的 `--list` 前按 `/opt/homebrew/bin/git`、`/usr/local/bin/git` 顺序发现真实可执行文件，解析 realpath，拒绝 `/usr/bin/git` 与 `/Applications/Xcode.app/` 下的路径；已存在但不安全的候选立即 FAIL，不跳过违规安装。两处都缺失、不可执行、版本探测失败或输出非 `git version ...` 均失败关闭，没有系统 git 回退。只建立 `R/bin/node` 和 `R/bin/git` 两个链接，PATH 保持 `R/bin:/usr/bin:/bin:/usr/sbin:/sbin`，不增加整个 Homebrew bin。每模式 `isolation.json.git` 保存候选、realpath、version、探测 argv、链接与 PATH；runner 启动前和 launcher 入口复核链接。继承的 `GIT_*` 环境覆盖被清除，避免配置路径、工作树与 exec-path 逃离隔离环境。

审计仅允许完整命令 `/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild -license check`：无额外参数、无 shell 包装，当前祖先链必须含本轮 runtime 下精确识别的 server 或 git/xcrun，核验来源进程与检查进程的 HOME 均为该 mode 的 home-os。记录 `readOnlySystemChecks` 后保留 PASS；同组、runner/Playwright/未知祖先或 orphan 不能获准。其他 xcodebuild 命令以及 xcrun 命令仍 FAIL；本轮没有增加任何 xcrun argv 放行项。路径及三元素 argv 用既有 `ps -ww args=` 完整文本匹配；沿用 ps 扁平文本、定时采样的验证限制，未声称内核级 argv 或全生命周期监控。

old8 原件仍 FAIL，不追溯重算。冻结 git 调用回源见 `_tmp/round8-git-isolation-evidence/git-source-audit.md` 与 `git-callsites.json`：seed 项目操作指向 R/workspace；用户全局配置位于 R/home-os、插件位于 R/home-os/.pilotdeck/plugins；telemetry 启动 rev-parse 和 `/status` 则只读基线仓库。因此 **HOME 隔离不能证明所有 git 都只操作运行工作区**。更新服务在 real supervisor 的 dev 模式中先返回 development，再到任何 git；任意 API 请求的路径也不因 HOME 自动受限。当前 worker 两个指定 Git 安装位置都不存在，已验证 FAIL_CLOSED、未执行 shim/版本探测；协调者须在指定位置已有真实 Git 后才可执行 old9。

第 8 轮未起服务/Chrome、未提交。指定三类负例、完整离线单测与自检、old8 SHA 复核、old9 命令见 `_tmp/RESULT.md` 第 8 轮及 `_tmp/round8-git-isolation-evidence/`。

## 第 9 轮：hardened 应用环境不可读时的替代证据

本轮替代第 7 轮“任何进程 ps -E 环境不可读均 FAIL”的条款。直接 spawn 的普通 Node（launcher、mock、Playwright CLI/--list）及以同一 Node 可执行文件运行的 server、gateway、Vite、setup/worker 后代全部必须读到 HOME，并匹配各自 ready/onboarding/mock 的隔离 home-os；协调者 runner 自身不属于这些服务。运行根的 HOME 由 spawn 元数据传给审计，不从应用的 argv 推测。Node 环境不可读、HOME 错误、运行根不明均在 lsof 前 fatal。Xcode license/git 的 HOME 条款也没有不可读例外。

Chrome、固定 bundle Helpers/crashpad、已有用户 GoogleUpdater 与 soffice 仅在第二次快照 PID/PPID/PGID 和完整 argv 与首份完全相同，且 `ps -E` 只返回 argv 时，记为 `environment: "unreadable"`。环境可读则继续核对 HOME；可见环境中 HOME 缺失/为空/重复、argv 改变、进程消失、ps 命令执行失败均 FAIL。

- Chrome 不可读：仍需当前自有 Playwright 祖先、临时 `--user-data-dir`、remote-debugging-pipe 与四个开关各一次；记录 Playwright 祖先 PID 和 profile。Helpers 不可读：当前祖先链必须回到此次核验的 Chrome/profile；只有以前缓存的 orphan 不能放行。环境可读的 reparented helper 保留第 7 轮的缓存复核规则。
- soffice 不可读：当前祖先链必须含本 runtime 下精确 server argv，且 server 的 HOME 已实际读到并核对为该 mode 的 home-os。记录 server PID、继承的 HOME 及源码位置。冻结 `officePreview.js` 的版本探测（235–240）和转换（411–415）都不覆盖 env，离线执行原 probe 函数并检查两处 execFile options 作为继承依据。
- GoogleUpdater 不可读：保留现有用户目录/版本路径、规范路径、禁止安装/注册等参数和当前核验 Chrome/helper 祖先规则，记录路径与祖先 PID。

`tcp-connections.jsonl.environmentEvidence` 对可读进程只保存 HOME 和身份匹配来源，对不可读的获准应用保存替代证据；不记录完整环境或凭空填入应用的实际 HOME。采样仍有时间窗口，替代证据不构成读取了应用实际环境的证明。old10 的错误只定位到通用解析器，没有保存失败 PID，故 hardened 解释仍待现场确认。

本沙箱对普通 Node 子进程执行 `ps -E -o args= -p <pid>` 返回 EPERM；Chrome 未启动，无法完成现场对照。协调者可对已存在的 Chrome PID 运行 `node _tmp/round9-environment-evidence/probe-ps-environment.mjs <ChromePID>`：脚本仅启动一个空闲普通 Node，比较两种 ps 输出形态并立即清理 Node，只打印脱敏摘要。

协调者新增的 `/Library/Developer/CommandLineTools/usr/bin/git` 第三候选及测试期望已审阅保留；当前候选顺序是 Homebrew、/usr/local、CLT，三者全部缺失仍失败关闭。CLT 候选也执行同样的 realpath、可执行性、版本和 runtime 链接复核。本机 file 显示其为 Mach-O universal executable，未执行系统 Git shim。本轮完整验证和 old11 命令见 `_tmp/RESULT.md` 第 9 轮。

## 第 10 轮：已安装用户 GoogleUpdater 的精确 wake 例外

按协调者自决、待现场复核的裁定，替代第 7/9 轮 updater 的路径/参数/祖先范围：仅允许操作者真实 `~/Library/Application Support/Google/GoogleUpdater/<版本>/GoogleUpdater.app/Contents/MacOS/GoogleUpdater`，实际 realpath 必须与此规范路径相同；完整 ps argv 文本精确为 `path --wake` 或 `path --wake-all`。缺失/不可解析的 realpath、旧短路径、bundle Helpers updater、安装/注册/卸载及任何其他参数组合均 FAIL。

直接父进程必须是**当前快照**核验通过的 Playwright Chrome 主进程，或同一路径、按本规则已放行的 `--wake-all` updater。先核验快照内主 Chrome，再迭代核验 updater 父链，不依赖历史缓存和 ps 排列；helper、其他中间进程、只有同组或 Playwright worker 都不能授权。可读环境继续核对真实 HOME；不可读继续执行第 9 轮身份/完整 argv 双快照绑定，并记录 canonical path、两元素 argv、直接 parentPid、chromeAncestorPid。updater 的独立 PGID 继续进入审计树、TCP 观察和失败清理组。

本轮无服务/listen/Chrome 实跑、未提交。34 组离线单测、全量 self-check、old11 只读核验与 **old12** 命令见 `_tmp/RESULT.md` 第 10 轮。仍使用 ps 扁平 argv 和定期采样，未提供内核 NUL 分隔 argv 或全生命周期证明；old11 原 FAIL 不追溯改判。

## 第 11 轮：Chrome 系列按当前祖先链核验，不做 HOME 环境核对

按协调者「自决·待复核」裁定（AI 代批，待人复核），本节替代第 7/9 轮 Chrome、固定 bundle Helpers/crashpad 的环境及缓存放行条款。R12 的启动环境继续为操作者真实 HOME + Playwright 临时 profile；审计从 `environmentTargets` 移除这三类进程，不调用它们的 `ps -E`，也不以 `processHOME` 或完整 argv 跨快照不变作为身份前提。

每次采样都从当前快照重新核验主 Chrome：固定 bundle 主 executable、自有 Playwright 祖先、当前运行 tmp 直接子目录的唯一 `playwright_chromiumdev_profile-*`、remote-debugging-pipe 和四个必需开关各一次。Helpers/crashpad 必须在同一当前快照沿祖先链回到已核验主 Chrome，绑定该主进程的 profile；helper 若显式带 `--user-data-dir`，必须唯一且与主进程一致。允许 helper 进程标题改变。两阶段均使用当前祖先链，历史 PID/argv 缓存、只有同组或 reparented orphan 不再授权任何 Chrome 系列进程。主进程无临时 profile、缺必需开关或 helper 断链继续 FAIL。

`tcp-connections.jsonl.browserEvidence` 独立保存当前 Chrome/helper 的 PID、主 Chrome PID 与 profile，标记 `environment: "not-checked"`；它不声称读到了实际 HOME。`environmentEvidence` 保留 Node（必须可读）、soffice、GoogleUpdater（可读核对，不可读沿用精确双快照及替代证据），第 8 轮 git/Xcode license HOME 规则继续执行。TCP 观察和自有组清理范围保持不变。

协调者新增的 launchd 收养 `--wake-all`（PPID 1）例外及三条断言已审阅保留：规范已安装用户 updater 路径、realpath、精确两元素 argv、HOME/不可读证据门仍须成立；同路径 wake 子进程可获准，PPID 1 的裸 `--wake` 或附加参数仍 FAIL。此例外只处理已经进入自有审计树的 updater。

第 11 轮离线验证、old13 只读快照回放与 **old14** 命令见 `_tmp/RESULT.md` 第 11 轮。采样/ps 扁平 argv 的验证边界继续保留；old13 原 FAIL 不追溯改判。

## 第 12 轮：参数不可读的 `(comm)` 行也执行禁止规则

每次采样对 `(name)` 行补查 `ps -ww -p <pid> -o pid=,ppid=,pgid=,stat=,comm=`，绑定当前 PID/PPID/PGID，核对绝对路径及 realpath。原始 command 保留，`processes[].executableObservation` 记录来源、路径、realpath、stat 与 resolved/exited/zombie/unavailable 状态。路径、realpath 的 basename 和括号内名称都过 open/xdg-open/osascript、xcodebuild/xcrun、updater、soffice 及 `/Applications/` 规则；不可读 argv 不能证明精确 license-check 或 updater wake 参数，因而不能获得这两项例外。

Chrome 子树的 `(name)` 行必须沿当前祖先链回到已验证的 Playwright 主 Chrome，且 executable 与 realpath 都在固定 Chrome bundle 的 `Contents/` 内（含 Helpers/crashpad/Frameworks）。bundle 外的系统程序也 FAIL；祖先断链、路径边界伪装、realpath 指向 bundle 外均 FAIL。既有 updater 例外仍要求完整精确 argv、路径、HOME 与父链。Chrome 系列仍不做 HOME 核对。

路径不可取得时，只有目标 ps 明确为空（已退出）、stat 为 Z，或当前 `<defunct>` 标记可记录后放行；仍存活、查询报错/超时、身份改变或记录异常均 FAIL。realpath 失败会再查当前状态，只有确认退出/僵尸才能放行。路径证据逐次重查，不缓存授权。soffice 放行新增固定 `/Applications/LibreOffice.app/Contents/MacOS/soffice` 与相同 realpath 约束，并继续要求当前被测服务祖先及隔离 HOME。

指定六项对照、40 组离线单测、完整 self-check、old14 原件摘要及 **old15** 命令见 `_tmp/RESULT.md` 第 12 轮。old14 没有保存 comm/stat 路径证据，历史快照的注入控制不证明其实际可执行路径，也不重算原结果。

## 第 13 轮：Node 环境读取的启动 / 退出 / argv 竞态

old15 在 PID 45129（Playwright 1.60 test worker，PPID/PGID 35658）此前已经实际读到隔离 HOME 后，报 `ps -E did not expose environment with unchanged argv`。原件只读，失败采样没有保存原始 ps -E，不能据此确定是 exec、退出还是改标题。

Node 环境目标首次只有 argv、缺行或与首份 argv 不匹配时，允许**初读之外最多 3 次重采**，每个 PID 从首次失败起使用单调时钟 **500ms 总预算**；重采间隔 50ms，每次重新取目标 `ps args` 与 `ps -E args`，每条都绑定原 PID/PPID/PGID。允许最新 argv 替换旧 argv 后重新配对，HOME 仍必须实际可读、唯一且等于所属隔离根。重采子命令按剩余预算超时并以 SIGKILL 终止；身份改变、查询失败/超时、可见环境 HOME 缺失/重复/为空或错误 HOME 仍 FAIL。3 次仍不可读或预算耗尽 FAIL。soffice/updater/git/Xcode 的既有门保持。

重采的目标 ps 或 ps -E 明确为空时，记录 `environmentEvidence[].environment: "exited-before-environment"` 及 PID/PPID/PGID、重采次数和来源；该进程本次跳过 HOME 校验与 lsof。重采成功记录 `retries`、`argvRebound` 和最新双快照绑定来源。新 argv 仍经过本次禁止规则，不缓存上次 HOME，不保存完整环境。

回源本机 Playwright **1.60.0** 的 worker/common/runner 与 playwright-core JS 未找到 `process.title` 赋值；[workerProcessEntry.ts](https://github.com/microsoft/playwright/blob/v1.60.0/packages/playwright/src/worker/workerProcessEntry.ts) 调用 [startProcessRunner](https://github.com/microsoft/playwright/blob/v1.60.0/packages/playwright/src/common/process.ts)，初始化仅保存内部 `processName`，用于 profiling；[workerHost.ts](https://github.com/microsoft/playwright/blob/v1.60.0/packages/playwright/src/runner/workerHost.ts) 传入 `worker-${workerIndex}`。这不是设置 OS 进程标题的证据。本轮仍支持同身份的标题变化后重新绑定，不把改标题认作已证实的 old15 根因。

四项指定控制、额外边界、全量离线自检及 **old16** 命令见 `_tmp/RESULT.md` 第 13 轮。本轮保留第 12 轮未提交改动，不代跑 real、不起服务、不提交；old15 原 FAIL 不改判，定期 ps 采样仍不证明全生命周期。

## 第 14 轮：R14 updater 按身份放行，不再要求父进程

R14 替代第 7/9/10/11/12 轮的 updater 父链条件：操作者已安装的规范 `~/Library/Application Support/Google/GoogleUpdater/<版本>/GoogleUpdater.app/Contents/MacOS/GoogleUpdater`、与 argv 路径相同的 realpath，以及恰为 `path --wake` 或 `path --wake-all` 的完整 argv 即可按身份放行。非 Chrome 父进程、无 Playwright 祖先、wake 父进程和 launchd 收养均不再构成 updater 的拒绝理由。现有可读 HOME 核对与不可读环境的双快照绑定继续执行；路径不规范、realpath 越界或版本目录与 argv 不同、额外参数、安装/注册/卸载等仍 FAIL。父进程自身继续按原有规则审计，updater 独立 PGID 仍纳入 TCP 和 fatal 清理。

协调者的 inactiveProcess 修复保留：当前 executableObservation.stat 含 E（exiting）或 Z 的行视为不活跃。old17 的 `(Google Chrome He)` 为 `?E`；old16/old18 的 wake-all 由 `(Google Chrome)` 且 status exited 的父进程启动，无法再提供主 Chrome 祖先证明。四组旧测试已按 R14 更新；集成测试用 finally 清理定时器，`node --test --test-timeout=8000` 的完整 **44/44** 单测及 **32/32** real 全量离线自检通过。详细原件行号、哈希及 **old19** 命令见 `_tmp/RESULT.md` 第 14 轮。本轮不代跑 real、不起服务、不提交，old16–old18 原 FAIL 保留。

## B 类部署矩阵（issue #58 第 4 项）

`--suite blackbox-deploy --frontends old` 是旧前端的隔离部署入口；每格自动 **起 → 等原生 bridge/token 和模块 health → 跑 unchanged spec → 收自有进程组**。`--profiles` 默认 `staffdeck,minimal,replacement`，可选其中的非重复子集；native 两格始终作为辅助格纳入。#927 起新侧配对入口开放：`--frontends new` / `old,new` 只把 native-auth 一格接到 #927 共存部署上如实记录（探针无设置页 → 原件 FAIL），其余格记 `NOT_RUN_NEW_SIDE`。**判分语义（如实）**：新侧永远不能把 B 变绿，但新侧 native-auth 的诚实 FAIL 会把 B 总状态记 FAIL（F3+ 迁移完成前这是预期形态，不是回归）；`--reconcile` 仍拒绝，不会把探针算成设置页实现。

基线实际支持四种部署：StaffDeck（`products/pilotdeck-staffdeck-sop/profiles/formal-host.yaml`）、minimal（`formal-minimal.yaml`）、replacement（`replacement-knowledge.yaml`）、native（`formal-native.yaml`）。native 必须单列：`real-auth-native` 测 agent-route 设置，而 minimal 的 `frontend.businessModules: {}`（`formal-minimal.yaml:22–23`）没有这个设置模块。route-matrix 在 `ui/e2e/formal-route-matrix.spec.mjs:24–37` 检查 SOP/Knowledge 不渲染，只适用于 native/minimal/replacement，不能在启用这些页面的 StaffDeck 上假跑。

完整清单如下。路径相对只读 `resources/PilotDeck/` @ `cc7c4e6`；同一注册的 desktop/mobile 是执行实例，不新增源码用例身份。

| 源码注册（完整标题） | 所需 env 与门控源码 | 部署与其他前提 |
|---|---|---|
| `ui/e2e/formal-composition.spec.mjs:13` — formal Knowledge query and citation resolution | `FORMAL_COMPOSITION_URL` 非空（`:6`）；`FORMAL_REPLACEMENT=0`（`:7–9`） | StaffDeck，真实 Knowledge query/citation 和预置知识数据；BLOCKED |
| 同文件 `:32` — formal SOP lifecycle resumes through the real StaffDeck runtime | 同上 | StaffDeck，真实 `operator_approval`、handoff/resume；模型可用确定性 OpenAI tool call，但缺真实 SOP runtime；BLOCKED |
| 同文件 `:52` — formal SOP and Skills routes follow the active composition | 同上 | StaffDeck 同格的 SOP/Skills 页面；完整部署依赖未就绪，BLOCKED |
| 同文件 `:59` — formal replacement Knowledge switches frontend and backend implementation | `FORMAL_COMPOSITION_URL` 非空（`:6`）；`FORMAL_REPLACEMENT=1`（`:7–9`） | replacement，基线自己的 `fixtures/replacement-knowledge-runtime.mjs` 独立进程 |
| `ui/e2e/formal-route-matrix.spec.mjs:7` — formal `${profile}` routes follow the generated composition | `FORMAL_ROUTE_PROFILE` 与 `FORMAL_COMPOSITION_URL` 均非空（`:3,5`）；本编排设 `FORMAL_ROUTE_AUTH=1`（`:11`） | 分别 native/minimal/replacement；真实本地 register/login + complete-onboarding（`:12–20`） |
| `ui/e2e/real-auth-composition.spec.mjs:52` — authenticated StaffDeck profile persists settings and queries the real Knowledge runtime | `REAL_COMPOSITION_URL`、`REAL_COMPOSITION_PROFILE` 均非空（`:4–7`）；profile 必须 staffdeck（`:53`） | 本地 `evidence-staffdeck` 账号（`:10–19`）、两个预置 KB、alternate SOP、supervised restart（`:22–33,55–59,77–83,117–130`）；BLOCKED |
| 同文件 `:134` — authenticated minimal profile keeps disabled module routes and requests inert | 同文件 `:4–7`；profile 必须 minimal（`:135`） | 本地 `evidence-minimal` 账号；Knowledge/SOP 均 disabled |
| 同文件 `:150` — authenticated replacement profile sends saved result limits to its replacement runtime | 同文件 `:4–7`；profile 必须 replacement（`:151`）；`REPLACEMENT_KNOWLEDGE_EVIDENCE_PATH` 必须存在（`:169–179`） | 本地 `evidence-replacement` 账号；证据文件必须来自独立 replacement runtime，不合成响应或日志 |
| `ui/e2e/real-auth-native.spec.mjs:6` — native Settings reads, saves, reloads, and authenticates runtime/config calls | `REAL_NATIVE_URL` 非空（`:4`） | native；真实本地 `browser-evidence` 注册/登录、onboarding 完成（`:7–18`） |

formal 配置原有两个项目完整保留（`ui/e2e/formal-composition.config.mjs:15–18`）：desktop 1440×1000、mobile 390×844 + `isMobile/hasTouch`。real-auth 沿用其单项目默认 1280×720。B 沿用原配置默认 30s test / 5s expect、trace=on；单 worker、零 retry/repeat、forbid-only。仅将浏览器指定为本机 Chrome，并关闭需要另下载 ffmpeg 的 video。不会加载基线 webServer/globalSetup；B 原配置本身没有这两项，auth composition 的 onboarding 前提通过 `deploy-setup.mjs` 真 register/complete-onboarding API 准备，账号与原 spec 一致，不制造 JWT。native 与 route specs 自己完成注册和 onboarding。

| 部署格 | 实际注册项目实例 | 应执行实例 | 计划的 profile 不适用实例 | 前提状态 |
|---|---:|---:|---:|---|
| staffdeck-formal | 8 | 6 | 2 | BLOCKED |
| replacement-formal | 8 | 2 | 6 | 已编排，待协调者运行 |
| staffdeck-auth | 3 | 1 | 2 | BLOCKED |
| minimal-auth | 3 | 1 | 2 | 已编排，待协调者运行 |
| replacement-auth | 3 | 1 | 2 | 已编排，待协调者运行 |
| native-auth | 1 | 1 | 0 | 已编排，待协调者运行 |
| native-routes | 2 | 2 | 0 | 已编排，待协调者运行 |
| minimal-routes | 2 | 2 | 0 | 已编排，待协调者运行 |
| replacement-routes | 2 | 2 | 0 | 已编排，待协调者运行 |
| 合计 | 32 | **18** | 14 | 9 个源码用例身份；4 个身份 / 7 次实例 BLOCKED |

**防 skip**：先列全局 72 身份（保留历史 undefined route title），再按 `deploy-matrix.mjs` 逐格精确核对原 spec 的文件、具体完整标题、line/column 与 project。只允许**不适用项目实例**以源码对应的唯一 profile 理由实际 skipped，记 `PROFILE_EXCLUDED`；缺 URL/env 理由不获准。应执行实例出现任意 skipped（包括 serial 无 annotation）、缺失、重复、retry、interrupted、xfail、FAIL、来源不符即红。某格 PASS 不掩盖另一格的 skip/失败。`accounting.mjs` 的 `reconcileDeploy*` 输出每格 expected/executed、project 观察原件与源码身份总表；route 的 native/minimal/replacement 标题保留在 observations 中。源码汇总仅按已核对的冻结注册位置关联历史行，**不授权 skip、不改全局 72 注册、原 skip allowlist 或基线 spec**。

完整矩阵的应执行为 9 个身份 / 18 次实例。StaffDeck BLOCKED 保留应执行数与分母；如果其他格实跑全部成功，最多是 **5/9 个身份、11/18 次实例**，完整矩阵仍 FAIL/exit 1。只请求部分 profile 时输出 `PASS_PARTIAL_B_PROFILES_ONLY`（需所有请求实例真的执行通过且无待人工确认身份；第 4 轮 replacement formal 带旧侧不确定标记，整体继续 FAIL），全局仍列 72 与 A/C/D NOT_RUN，不宣称 B 9 或黑盒 37 已验收。

所有 URL 门控由 `deployExecutionEnv` 设置为 **http://127.0.0.1:28411**，配置与 gateway 也固定本仓端口：server=28410、Vite=28411、gateway=28412、replacement Knowledge=28413、mock LLM=28379。逐格顺序复用端口，运行目录不复用。`assertDeployExecutionEnv` 在 config/launcher/Vite/setup 入口拒绝缺 env、3001/18789、任何其他端口/外部 URL、localhost、路径变体、错误 profile 与越界 runtime/config/evidence 路径。仅 connect 探测固定高位端口空闲，权限/超时/占用即 FAIL，绝不杀占用者。

runtime 目录为本 worktree `_tmp/fe-parity/deploy-run/run-*/<cell>/`。复用 D 的 HOME/XDG/TMP/PILOT_HOME 隔离、真实 Git bin 发现与断言、原生 project/.cwd seed、native supervisor 及 bridge/token readiness。新目录独占创建；设置 `PILOTDECK_SKIP_BROWSER_OPEN=1`、`SKIP_BROWSER_OPEN=1`，每格单独 auth.db/workspace/cache/home-os。profile 只重写历史机器路径、model/provider、host/端口与本地模块 endpoint；原模块选择与设置保留。Vite `deploy-vite.mjs` 使用基线纯 generator 在内存提供生成模块，以原 generated 模块的解析位置处理相对 imports，不写 baseline generated/cache/dist，也不拷贝基线源。replacement 直接启动基线原件。mock-llm 已覆盖连接探测，并按基线 local-openai-mock 的 operator 模式补齐广告为 submit_step_result 的 handoff/completed 工具调用、会话标题和流式参数分块；冻结 native OpenAI parser 正反验证通过。模型脚本不代替真实 SOP/Knowledge API；未知请求拒绝并让整体 FAIL。

**全程审计**复用 `withRealAudit` / `real-tcp-audit.mjs`：从部署注册检查之前到 mock/runtime/Chrome 最后清理之后。服务 Node HOME 必须真实可读且匹配隔离根；Chrome 按 R12 使用本机 HOME + 临时 `--user-data-dir`，driver HOME 仍隔离。原 opener、soffice、Chrome 当前祖先、R14 updater 身份及 Git/Xcode 规则不放宽。B 在 D 的 3001/18789 拒绝门上额外要求所有观测 TCP 端点为 127.0.0.1，且连接至少一端为固定 B 端口（服务 accept 的另一端为临时客户端端口）；通配 listener、外部目标、其他本机端口均 fatal。Node preload 同时拒绝越界 bind/connect；过程日志与连接证据分别写 `network.jsonl`、`tcp-connections.jsonl`。只有 after-cleanup 也成功才记录 `deploy.audit.status=PASS`。ps/lsof 属定时采样，离线注入不能证明真实运行的全生命周期或实跑 PASS。

**StaffDeck BLOCKED 的具体缺项**：PilotDeck 基线仅含接入 profile/协议和 replacement fixture，真实模块来自独立锁定 StaffDeck checkout。回源 `tests/composition/formal-approval-fixture.ts:79–83` 要求 `STAFFDECK_SOP_ROOT` 与匹配 Python；`products/pilotdeck-staffdeck-sop/conformance/run_staffdeck_knowledge_lifecycle.py:28,40–43,47–63,115–131` 使用外部 `app.module_knowledge_app` 和预置种子。基线默认路径 `/Users/a1/.../StaffDeck-portable-sop` 不在本机；本机另有 StaffDeck 目录/venv，但缺这两个所需模块入口，不等价于锁定 runtime。还需真实预置 Knowledge 的 `kb_preset_data_001` / `kb_preset_sales_001` / `kchunk_preset_sales_001`；SOP 原件 `sops/operator-approval.yaml` 已同时包含 `operator_approval` 与 `operator_approval_alternate`，需由隔离真实模块加载，不能误报为缺少定义文件。现入口固定报告 BLOCKED，**不会**接受任意外部服务 URL 或假 Knowledge/SOP 作为替代。人提供锁定源码、已有依赖与种子后还需补其受审计的隔离启动编排；本轮未下载、安装、借用活跃服务或伪造业务结果。这是尚未完成的前提，不是永久降低验收分母。

协调者整块旧侧命令（output 必须新建；当前预期因 StaffDeck 为 BLOCKED 最终 exit 1，保留其余格原始结果）：

```sh
(
  set -eu
  cd /Users/da/ws/pilotdeck-rs/_build/wt-f2e
  test ! -e _tmp/fe-parity/coordinator-b-old4
  mkdir /tmp/pdrs-parity.lock || exit 1
  trap 'rmdir /tmp/pdrs-parity.lock' EXIT
  node scripts/fe-parity/run.mjs --suite blackbox-deploy \
    --profiles staffdeck,minimal,replacement --frontends old \
    --output _tmp/fe-parity/coordinator-b-old4
)
```

独立离线检查（无 listen、Chrome 或真实 ps/lsof）：

```sh
node scripts/fe-parity/deploy-self-check.mjs
node scripts/fe-parity/run.mjs --suite blackbox-deploy --self-check --rules-only --frontends old
node --test --test-timeout=8000 scripts/fe-parity/mock-llm.test.mjs
```

新增检查用真实 `--list` JSON 的身份/位置/项目/metadata 派生结果负控，执行结果为注入值，不会伪装成真实浏览器报告。保留原 full self-check、orchestration-self-check、baseline-stale-self-check；B 的新检查也由 full self-check 调用。输出含各格 `*.inventory.json`、原 `old-<cell>.json`、stdout/stderr/runtime/isolation、test-results 的 trace/截图、replacement 实际调用日志、逐格及逐例 `deploy-parity.md` 和统一 `result.json`。缺失模块/原始报告及审计失败不会改成绿。

第 2 轮补齐 runner 实际调用的 replacement health 路径检查：fetch 注入返回 Node 22 原生 `new Response(...)`，其 `ok` / `status` 为属性、body 为真实 `ReadableStream`。HTTP 503、implementationId 不匹配/缺失、坏 JSON 和超时均拒绝；health 失败三格仍为 FAIL/零执行。Playwright setup 的 `APIResponse.ok()` / `status()` 保留方法调用。第 1 轮 `coordinator-b-old1/` 与 `_tmp/coordinator-b-old1.log` 保留，重跑用上面的新目录。


第 3 轮根据保留的 `coordinator-b-old2` 原件定位 replacement formal/auth 的 3 次真实 FAIL。三份 trace 的 `POST /api/modules/knowledge/query` 都返回 HTTP 501 / `MODULE_IDENTITY_UNAVAILABLE`；截图和 error-context 显示同一错误，三个独立 fixture 的实际 `replacement-calls.jsonl` 均为空。`network.jsonl` 确认运行的是冻结 `fixtures/replacement-knowledge-runtime.mjs`；该日志只有 runtime-process 观察，不能单凭没有 fetch 行判断请求。原 352 次 TCP/process 采样含 after-cleanup 均 PASS；LLM 日志 0 条。完整可复核证据见 `_tmp/round3-old2-forensics.json`、`_tmp/round3-old2-accounting.json`、`_tmp/round3-baseline-history.diff`；old2 日志/输出及原 runtime 的 25938 个文件 hash 见 `_tmp/round3-old2-preserved.json`，原件未改。

判定为 **baseline-stale**：冻结 replacement profile 本身没有 tenantId/actorUserId 或 StaffDeck copy binding。`468b4ba9` 在这两个 unchanged replacement spec/fixture 发布后为 Knowledge query/citation 加入身份必需门；`666dace9` 又无条件加入 `verifyStaffDeckKnowledgeBinding`。只补两个身份值仍被 `COPY_UNAVAILABLE` 拒绝；独立 fixture 只有 health 与 module_call，没有真实 StaffDeck `/api/auth/me` 与目录 API。这不是端点、启动顺序或 fixture 文案缺失。不能通过伪造 StaffDeck identity/directory 回包使其通过；本轮未改编排配置、基线 spec/fixture、业务回包或超时。module health 只证明进程身份/存活，不能证明宿主认可领域调用。

R8 清单新增精确 B 身份 `formal-composition.spec.mjs` 的 replacement 例及 `real-auth-composition.spec.mjs` 的 replacement 例，分别绑定 `64:131` 与 `164:131`、matcher `toBeVisible` 和完整 exact locator。证据含源文件 hash/行/原文、上述完整 commit ID 和独立离线 proof。清单现共 6 身份，canonical SHA-256 为 `b855a582a47ffa3cc6bc8865cf6a7ad36f7a4ab4749e15c3805c38ef2b3c85c6`。`replacement-stale-proof.mjs` 通过 AST 执行冻结 query/citation handler 及真实 copy 验证函数：原 profile 501、仅补身份 501，均在上游调用前拒绝；另执行未编辑 fixture handler，确认其原查询回包确实含 exact 文案。这些都是 **OFFLINE_FROZEN_SOURCE_ONLY**，不写 runtime evidence，不监听、不启动浏览器，不充当真实行为回包。

B 原始行/分区/总体继续 **FAIL**，floor 仍 9 身份 / 18 实例，old2 实际 5 身份 / 11 实例执行（3 身份 / 8 实例 PASS、2 身份 / 3 实例 FAIL，4 身份 / 7 实例 BLOCKED）。B 报告只记录精确 FAIL 的 `matchedFailCondition`，旧侧条件满足仍 `pairedConsistency=UNPAIRED`，不宣称双侧 MATCHED_FAIL；未标记身份两侧都有有效原件且同位 FAIL 才能确认配对；第 4 轮 formal 例外见下文。原 profile 排除规则、端口审计与行为错误诊断不放宽。失效的两个身份行为覆盖记 0；其余 7 身份含 4 个 BLOCKED，当前通过 3。D 仍要求其原有 4 个精确 stale 身份，B 身份不能替代 D 条件，D 原 17 执行 / 11 cascade / 13 PASS 的校准口径保持。

新增离线负控拒绝错误断言行/列、重复 locator、别种错误、PASS 冒充匹配 FAIL、xfail、skip、retry、错误 provenance 和 report error；确切 FAIL 的账目也必须红。此次没有编排修复，所以无需制造“修复旧编排”的 mutation。本轮所有检查及命令详见 `_tmp/RESULT.md` 第 3 轮。第 3 轮曾交接 `coordinator-b-old3`；当前按上面的 `coordinator-b-old4` 新目录重跑仍预期 replacement 两格 FAIL、StaffDeck 两格 BLOCKED；该命令用于验证 unchanged spec 与 R8 记录，不能作为 replacement 修复 PASS 的承诺。


## B 第 4 轮：formal 旧侧时序竞争需人工确认（#114 P1）

独立只读复核 old2/old3 的 formal 四份 trace 和 auth 两份 trace：old3 desktop fill 为 **3143.777–3160.368ms**；第二轮 runtime 请求起点 **3164.148/3164.264ms**，完成估计 **3170.254/3170.752ms**。`3164.381ms` 的 after-fill 快照已移除模块页、显示 runtime 验证占位页；Search 在 **3190.193ms** 解析为 disabled，随后不断 not enabled，整例 timedOut 停在 `formal-composition.spec.mjs:63:67`，没有 query 请求。**3164ms 是请求起点，不是回包时刻。** old3 mobile 第二轮完成估计 **48379.657/48381.764ms**，早于 fill **48389.010–48399.423ms**；其 query **48448.622ms** 收到 501，最后在 `:64:131` failed。old2 desktop/mobile 第二轮也先于 fill，二者均在登记断言 failed；auth old2/old3 都在自身 `:164:131` failed / HTTP 501。

源码链（均相对只读冻结 `resources/PilotDeck/`）：`ui/src/components/app-shell/AppShellV2.tsx:165–166` 的项目/加载状态变化发出 runtime-changed；`ui/src/composition/runtime.ts:209,213` 清空 runtime，`:178` 返回空 assembly，`:205` 回包后恢复；`AppShellV2.tsx:85` 失去 modulePage，`MainAreaV2.tsx:429–440` 模块页条件挂载与项目 key 使页面重挂载。`fixture-knowledge-search.tsx:9` 初始化 query 为空，`:25` 以 `disabled={!query.trim()}` 禁用 Search；spec `:62–64` 无 refresh 完成的前置等待。快照和日志直接证明页面失去搜索输入、重现 disabled Search；项目事件触发/重挂载链由冻结源码支持，trace 未直接录 React hook 调用。

`baseline-stale.json` 仅给精确 formal 身份登记 `oldSideNondeterministic=true` 和 `oldSideNondeterminism`，附身份、四份 ZIP/报告路径与 SHA-256、trace member、精确时间、观测状态和源码行/哈希。更新固定清单 digest；标记缺证据、来源漂移、证据借给别的身份均拒绝。常规 preflight 核验固定清单和冻结源码；历史 ZIP 的独立复核命令是 `python3 scripts/fe-parity/old-side-timing-proof.py --verify`，不要求未来 live runner 携带历史 `_tmp` ZIP。

`failureSignature` 和 `matchesStaleFailure` 语义原样：timedOut 不生成签名。`compareStaleRows` 对标记身份始终输出 **NEEDS_HUMAN_CONFIRMATION**、计数 `needsHumanConfirmation`，不加入 matchingIds，也不消费失败诊断；即使双侧恰好都在登记断言 failed，整体仍 FAIL。原始 PASS/FAIL/timeout、provenance/retry/report errors 均保留；没有人工确认的自动开绿入口。未标记身份继续原 MATCHED_FAIL/FAIL 规则，D 的 4 条、17 执行 / 11 cascade / 13 PASS 校准不变。B 单侧 JSON/console/Markdown 也显示待确认及证据，exact `matchedFailCondition` 只说明当前原件是否满足旧侧断言条件，不代表自动配对；formal mobile 条件满足、desktop 不满足是正常结果。

离线自检增加缺证据、借身份、exact failed/timeout/PASS/skip 配对仍红和未标记身份通过正控。old2/old3 原件重新对账均 **FAIL，5/9 身份、11/18 实例，1 个待确认身份**。完整复核、验证记录、更正后的第 3 轮结论及 `coordinator-b-old4` 命令见 `_tmp/RESULT.md`「第 4 轮」。本轮不提交、不改 state/、不启动需要 listen 的部分；P2 esbuild/短命 node env 证据由协调者另记。


## #927 F3 前置：真实部署双壳共存验证包

`node scripts/fe-parity/run.mjs --suite coexist --frontends old` 落地计划 004 F3 选项 A 的最小可回退双入口托管，并实测双向导航 × 共享键 × 回退开关。选 A 而非 B（exp007 前置反代）的判断：B 已有 mock 级证据且明确不能当 F1/S5 放行依据，本包要补的正是「真实 A patch 零证据」这一格；A 的补丁约 20 行、单一 env 开关（关 = 字节等价回旧行为）、独立 diff 只打进部署副本。

部署副本 `_build/fe-parity/coexist-deploy/pilotdeck/`：rsync 只读基线（排除 .git/node_modules/dist/work/evidence，node_modules 符号链接回基线只读使用）；`ui/dist` 来自 `build-old.mjs` 信任产物（基线里来历不明的旧 dist 不用）；`ui/new-shell` 由 `build-new.py` 探针产物 + `ui-rs/shared/boundary.js` + xterm 资产组装，引用统一重写到 `/probe/leptos` 前缀；A 补丁打在副本 `ui/server/index.js`（`/probe/leptos` 静态挂载 + `app.get('*')` 顶部 `PILOTDECK_NEW_SHELL=1` 时的 `/settings` 严格段边界分支），原件与补丁后文件留档 `a-patch.original.js` / `a-patch.patched.js`。旧 bundle 的 `dist/index.html` 注入一行 boundary-boot（先于 React router 的 module script）。

runtime 为基线自己的 `createRuntimeSupervisor` **start-built** 模式（server + 冻结 TS gateway，无 Vite；SPA 由 ui/server 静态 + catchall 提供，A 补丁才会被真实命中）。端口 28420（server，即 origin）/28421（gateway）/28422（预留 vite），mock LLM 28379，全部 127.0.0.1。隔离 HOME/XDG/TMP/PILOT_HOME 与 D 同款；`coexist-launch.mjs` 在 spawn 边界断言 `assertCoexistExecutionEnv`。

三阶段各为独立部署（同一副本、各自 runDir/auth.db）：**valid**（配置 ready）跑 S1/N1/N3/N3b/N4/N5/C1/C2a/C2b/C3/D1；**invalid**（agent.model 指向未声明 provider，旧壳渲染 ModelConfigurationErrorScreen）跑 N2（唯一 `<Link to="/settings/models">` 入口）；**rollback**（开关 unset 重启）跑 R1/R2（`/settings` 回旧 bundle、`.pilotdeck-settings-app` 真渲染）。导航判据用 `window.__coexistMarker`：跨壳必须整页加载（marker 消失），SPA 路由则保留。D3（流式断口帧）不在本包冒充验证，待 F5；跨壳视觉闪烁等仅人眼项在 RESULT 单列「待人工验收」。

跨壳 history 遍历（N3/N3b，#927 发现）：**N3 是原验收格，照原判据执行**——旧壳侧出发的 goForward 进入 `/settings` 必须落新 bundle；当前实测被冻结旧壳固有行为阻断（见下），该格如实记 FAIL（不改成「特征化通过」），总状态 FAIL，修复或接受缺口为 F3 决策项（决策卡 `decision-n3.md`：改 no-store / 去启动 push / 接受不可前进入新壳）。根因（N3b 特征化为断言，变更即红）：后退入旧壳在本装置必整页重载——**本装置 Chrome 由 Playwright 以 `--disable-back-forward-cache` 启动，BFCache 整体关闭**（`tcp-connections.jsonl` 进程 argv 实测）；「冻结 TS server 对 shell 文档发 no-store 会使真实用户浏览器不命中 BFCache」为读码推断（HTTP 标准语义），未在本装置对照实测。重载启动时 `AppShellV2.tsx:207-223` 的 URL 同步 effect 调 `handleProjectSelect → navigate('/')`（pushState），把跨壳前进项清掉并隔离已迁前缀。两项均在冻结 TS 侧，非本包 A patch/boundary 缺陷。

共享键契约：C1 登录态互通（探针以同一 `auth-token` 挂载，其 `/api/projects/probe/sessions` 请求 Authorization 头 === 旧壳 token，返回旧壳无需重登）；C2 草稿跨壳保留（旧壳 `draft_input_<project>:<sessionId|'new'>` 与探针 `draft_input_probe:s` 双向，丢即 FAIL）；C3 跨跳转共享键不变——快照同时采 **localStorage 与 sessionStorage**（活动项目键 `pilotdeck-active-project` 在 sessionStorage，`AppShellV2.tsx:163` 写入），且先断言该键实际存在再比较（快照函数单源于 `coexist-shared-keys.mjs`，浏览器与离线 oracle 跑同一份字节）。已知回退形态如实记录：rollback 下旧壳 sidebar navigate 仍被 client boundary 硬跳一次，由旧 bundle 在 `/settings` 整页加载接住。

计分三态（fix-r1，P0）：`coexistAccounting.status` ∈ PASS / FAIL / ERROR——`ERROR` = 证据无效（任一阶段 `report.errors` 非空、注册漂移即 executed 与登记多重集不等、fatal 编排错误），`FAIL` = 行为失败或身份缺失，`PASS` = 14 例全绿且无错误；执行错误永不折算成 PASS，run.mjs 透传三态。并发：装配（`rm -rf` 部署副本）前先 `assertCoexistPortsFree`——第二个入口必须 fail-closed 退出，而不是毁掉第一个入口正在服务的副本；跨进程互斥沿用协调者 `/tmp/pdrs-parity.lock` 惯例。

离线正控/负控：`node --test scripts/fe-parity/coexist-boundary.test.mjs scripts/fe-parity/coexist-patch.test.mjs scripts/fe-parity/coexist-accounting.test.mjs scripts/fe-parity/coexist-self-check.test.mjs`（无 listen/Chrome）。`coexist-patch.test.mjs` 用 `executeOptionAPatchBranch` **真实执行**部署模板（不再镜像谓词）；`coexist-accounting.test.mjs` 合成 Playwright 报告验证三态；`coexist-self-check.test.mjs` 核对 COEXIST_CASES↔spec 注册、C3 快照覆盖、S1/C3 接线与 `assertCoexistExecutionEnv` 拒绝用例。浏览器级负控按本仓负控规则放 `src/pilotdeck-gateway/tests/fixtures/fe-coexist/negative_controls.py`（声明式 mutations 表，19 变异：boundary 10 + 部署模板 3 + 计分/目录 3 + C3 快照/接线 3；新增文件提交前不在 HEAD，`mutating_sources` 拒跑时由协调者宿主执行）。B 新侧配对：`--suite blackbox-deploy --frontends new`（或 `old,new`）只跑 native-auth 格（冻结 `real-auth-native.spec.mjs` 原样对共存部署），其余格 `NOT_RUN_NEW_SIDE` 列理由；新侧不能把 B 变绿，但其诚实 FAIL 会使 B 总状态 FAIL。

```sh
node scripts/fe-parity/run.mjs --suite coexist --frontends old    # 当前预期：N3 原验收格 FAIL → 总状态 FAIL（F3 决策项）
node scripts/fe-parity/run.mjs --suite blackbox-deploy --frontends new   # 新侧配对（native-auth only，诚实 FAIL）
node --test scripts/fe-parity/coexist-boundary.test.mjs scripts/fe-parity/coexist-patch.test.mjs \
  scripts/fe-parity/coexist-accounting.test.mjs scripts/fe-parity/coexist-self-check.test.mjs
```
