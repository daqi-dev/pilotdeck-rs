# A 类 34 例处置清单（F2 · Refs #58）

> **本清单的分母变更须报人复核，未复核前不作为计分依据。**
> 状态：AI 起草（Claude，2026-10-18），**待人复核**。复核前，现行主尺子仍是黑盒候选 37 例（B 9 + D 28，`docs/plan/004_frontend.md:32`），A 类不进任何分母（`docs/plan/004_frontend.md:154` 拍板 c）。

基线：`resources/PilotDeck` @ `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（`scripts/fe-parity/skip-allowlist.json:2`）。下文「文件:行」都相对 `ui/e2e/`，行号是 `test(` 注册所在行（与 JSON reporter 的 `spec.line` 口径一致）；「Lnn」指同一文件中断言或驱动所在的行。

## 一、34 是怎么数出来的

身份正本：`docs/experiments/exp007-frontend-probe/evidence/e2e-inventory.json`（`registered: 72`），runner 用 `checkInventory` 逐身份比对（`scripts/fe-parity/accounting.mjs:50-61`）。72 例按文件归类：

| 类 | 文件 | 注册数 | 身份来源 |
|---|---|---|---|
| A | `streaming-lifecycle` 9 · `streaming-scroll` 5 · `streaming-search` 8 · `model-selection` 11 · `composition` 1 | **34** | inventory 中除 B/C/D 外的全部身份 |
| B | `formal-composition` 4 · `formal-route-matrix` 1 · `real-auth-composition` 3 · `real-auth-native` 1 | 9 | `skip-allowlist.json`（`accounting.mjs:346-347` 断言 9） |
| C | `history-fork` 1 | 1 | — |
| D | `real/*` | 28 | `pending-orchestration.json`（`accounting.mjs:24` 断言 28） |

34 + 9 + 1 + 28 = 72，与 `docs/research/007_frontend/gap.md:94` 逐文件分解（9 + 5 + 8 + 11 + 1）一致，**没有差额**。runner 里没有单独的 A 类身份文件：A 类是由 72 减去 B、C、D 推出来的。pr2 落地 A 类用例时应补一份 A 身份清单，并对 34 做断言，与 B、D 的做法保持一致。

## 二、判据（按顺序判，命中即停）

1. **放弃**：被测对象不属于产品，或断言的内容不是用户可观察的行为。具体包括：只有设置构建开关才会打包的原型页；直接 `import` 旧源码模块来驱动；按内部 CSS 变量名或像素级颜色断言（`docs/plan/004_frontend.md:145` 已把 pixel-perfect 视觉还原列为范围外）。放弃时必须写清：该行为是否已由其他分母覆盖。
2. **改写黑盒**：断言能改写成 DOM 文本、角色、几何、`scrollTop`、网络请求、localStorage 上的观察，而驱动能改成「用户输入 + runner 的 mock HTTP/WS 注入」。旧 store 调用（`window.streamLifecycle.store.*`、`window.streamFixture.*`）一律换成 mock 后端推帧或 mock 接口返回；fixture 专用的输出元素（`data-testid="selection"` 等）一律换成产品 UI 上能看到的东西。
3. **移植测试钩子**：只有当驱动无法经由用户输入或 mock 后端到达时，才由新前端提供等价的页面侧钩子。这条是**兜底**，本轮没有命中；pr2 的样板如果证明某类驱动（例如 subagent 帧）的 mock 扩展成本失控，该类可以降级为移植。**移植和改写都计入分母**，因此降级不改变下文的分母。

**定位契约（不算钩子，新前端须保留）**：已有的 F2 黑盒套件就用这些 DOM 定位点（`scripts/fe-parity/tests/streaming-scroll.spec.mjs:7, 46, 59`），改写沿用同一口径：`[data-chat-search-surface]`、`[data-message-key]`、`[data-stream-scroll-viewport]`、region「Live thinking content」、按钮「Back to latest」「Thought process」（含 `aria-expanded`）、`mark[aria-current="true"]`、searchbox，以及 `data-testid="assistant-message-actions"` 和 `assistant-message-model`。

**行分层契约（同属定位契约）**：已折叠的完成轮里，轨迹宿主必须是独立的顶层 `[data-message-key]` 行（顶层 = 父链上没有别的 keyed 祖先）；该轮可见的顶层 keyed 行只能是 user、轨迹宿主、answer 三种，按此顺序排列；轨迹展开后的子行（thinking、tool、compact 等）可以带 `[data-message-key]`，但必须嵌在宿主行内，不得平铺到顶层。依据（旧侧，基线同上）：`ui/src/components/chat-v2/processGrouping.ts:961-969` 折叠时只推出 user 项、一个合成的 `turn-trace-*` 宿主项和 answer 项，其余项进入宿主的 `turnTrace.items`（:944-956）；`ui/src/components/chat-v2/MessagesPaneV2.tsx:1096` 每项一个 `MeasuredMessageItem`，其根 div 带 `data-message-key`（:288-293），展开的子行渲染在宿主 item 之内（:1110-1125）。未折叠的轮（进行中、异常、无最终回答，:936-939）不受此条约束。#1 改写按此契约数顶层 keyed 行（`scripts/fe-parity/tests/a-streaming-lifecycle.spec.mjs` 的 `ROWS`）。

**改写时一律去掉的内部断言**：
- `store.getMessages().map(kind)`（lifecycle L31-32、L42）→ 改为按 DOM 行顺序断言；
- `detail.isLoading`（lifecycle L201）→ 改为等 mock 路由放行；
- DOM 节点同一性（lifecycle L195、L199、L203）→ 只保留它带来的可观察后果，即 `aria-expanded` 和阅读位置（L202、L204）；
- `[data-virtualized-messages]`（scroll L85）；
- `[data-chat-search-render-pending]`（search L61、L64、L69）；
- `.process-live-status`（scroll L57）→ 改为按文案计数；
- `mark.chat-history-search-highlight-active`（scroll L72）→ 改为 `mark[aria-current]`；
- fixture 专用的 `/api/test-submit`（model L25-28）→ 改为在 mock 侧捕获 WS 发出的聊天命令帧；
- fixture 计数器 `settings-opened`、`commands-loaded`、`model-requests`、`command-messages`（model L119、L127、L130、L132、L140）→ 依次改为：设置界面可见、命令菜单项、WS 聊天帧计数为 0、聊天区出现帮助文案。

## 三、逐例处置（34）

### streaming-lifecycle.spec.mjs（9）

| # | 文件:行 | 标题 | 处置 | 依据 |
|---|---|---|---|---|
| 1 | streaming-lifecycle:10 | history refresh preserves thinking and compaction order and a single image query | 改写黑盒 | L29、L35-39、L43-44 都是 DOM 断言（文本计数、`img` 计数、思考块位于回答之上）。驱动 L19-28、L30、L41 改为由 mock 推送 user 回显、thinking、compact_boundary、answer 帧，并在完成后让 history 接口换一份内容来触发刷新。L32、L42 的 kind 顺序改为 DOM 行顺序。需要扩展 mock 的 thinking/compact 帧。计划 F1 S4 建议用此例做样板（004:59） |
| 2 | streaming-lifecycle:48 | short thinking ignores upward wheels, then global resume restores the inner tail | 改写黑盒 | L53-75 全部是 `scrollTop`、gap 几何和「Back to latest」按钮，滚动由真实 wheel 触发（L56、L62）。`streamFixture.think` 改为推送 thinking 增量帧 |
| 3 | streaming-lifecycle:78 | upward wheels over short thinking yield to the scrollable conversation | 改写黑盒 | L83-91 都是外层滚动几何，驱动同 #2 |
| 4 | streaming-lifecycle:95 | stream search waits for the visible match | 改写黑盒 | L108、L113、L116-117 断言 `mark[aria-current]` 和 `scrollTop`。`set(messages)` 改为文本增量帧，现有 mock 已支持（`scripts/fe-parity/stream.mjs:9`）。现有 F2-STREAM-SEARCH 没覆盖「先搜、后到」这种情况 |
| 5 | streaming-lifecycle:95 | stream search waits for the visible match and respects cancellation | 改写黑盒 | 同 #4，加上 L110-111 的真实 wheel 取消和 L114 的 `top === 0` |
| 6 | streaming-lifecycle:122 | historical sessions open at the bottom with auto follow disabled and preserve revisits | 改写黑盒 | L125、L132-139、L146-151 是几何断言和 `[data-message-key]` 锚点偏移。`auto=false` 对应用户设置「自动滚动到底部」（`ui/src/components/settings/view/general/ChatInputSection.tsx:68-70`），可以经设置界面关闭。`append`、`setSession` 改为 mock 推送新消息加侧栏切会话。需要 mock 提供两个各 240 条的会话 |
| 7 | streaming-lifecycle:155 | subagent drains completion text while following the bottom | 改写黑盒 | L158-159、L169 是 `[data-stream-scroll-viewport]` 几何。驱动 `updateSubagentDetailStreaming`（fixture `streaming-lifecycle.jsx:86-91`）改为 mock 推送 subagent 帧并打开子代理详情。需要扩展 mock 的 subagent 帧，这一项成本最高，可能降级为移植 |
| 8 | streaming-lifecycle:155 | subagent drains completion text while preserving the reader | 改写黑盒 | 同 #7，加上 L161-163 的 wheel 和 L168 的阅读位置不动 |
| 9 | streaming-lifecycle:173 | completion snapshot refresh preserves the mounted thinking viewport and its reading state | 改写黑盒 | L179-184 已经用 `page.route` 卡住快照请求，本身就是黑盒驱动。L189-193、L202、L204 是可观察断言。L199、L203（节点同一性）和 L201（`detail.isLoading`）按第二节去掉。thinking 增量要走 mock 推帧 |

### streaming-scroll.spec.mjs（5）

| # | 文件:行 | 标题 | 处置 | 依据 |
|---|---|---|---|---|
| 10 | streaming-scroll:14 | small upward gestures win over text streaming and explicit resume follows again | 改写黑盒 | L17-27 都是几何和按钮断言。F2-STREAM-002 已经覆盖「阅读不跳、显式恢复后继续跟随」（`tests/streaming-scroll.spec.mjs:109-132`），差别只在 L19 的小手势 −30（F2 用的是 −120）。可以给 F2 的 `readUp` 加参数来承接，但计数只算本身份 |
| 11 | streaming-scroll:30 | thinking retains its viewport, expansion and reading position through tools and completion | 改写黑盒 | L38、L43、L45-50 是 thinking 区几何和 `aria-expanded`。驱动 think/tool/finishTool/complete（fixture `streaming-scroll.jsx:38-51`）改为 thinking 加 `tool_use`/`tool_result` 帧 |
| 12 | streaming-scroll:54 | web fetch has one status in plan mode and empty results finish it; parallel calls remain distinct | 改写黑盒 | L58、L60、L62、L64 按文案「Fetching web content...」计数，属于用户可见的状态。L57 的 `.process-live-status` 去掉。plan 模式经 composer 运行模式切换进入，web_fetch 工具帧由 mock 推送 |
| 13 | streaming-scroll:67 | stream updates do not repeatedly recenter an existing search match | 改写黑盒 | L72 改为 `mark[aria-current]`，L79 是阅读位置。F2-STREAM-SEARCH 的 lighthouse 例已经做了「搜到 → 上翻 → 流式不回中」（`tests/streaming-scroll.spec.mjs:134-184`），可以直接承接，计数只算本身份 |
| 14 | streaming-scroll:82 | prepending virtualized history while streaming preserves the visible message anchor | 改写黑盒 | L89、L97-104 是 `[data-message-key]` 锚点几何，L85 的虚拟化属性去掉。`prepend` 改为上翻触发分页加载更早消息（需要 mock 的 `/api/sessions/s/messages` 支持分页）并同时推文本增量帧 |

### streaming-search.spec.mjs（8）

| # | 文件:行 | 标题 | 处置 | 依据 |
|---|---|---|---|---|
| 15 | streaming-search:21 | search locates a phrase spanning text and a link | 改写黑盒 | L27-28、L31、L37 是 link 可见、mark 计数和阅读位置。驱动 `set(messages)` 改为 mock history 返回 30 条加目标消息，这是纯 history，现有 mock 改一下返回内容就够，成本最低 |
| 16 | streaming-search:21 | search locates a hidden Markdown URL | 改写黑盒 | 同 #15。与 F2-STREAM-SEARCH 的 `f2-hidden-reference`（`tests/streaming-scroll.spec.mjs:159-161`）近似，但本例是静态 history，后续是「来新消息不回中」（L33-37） |
| 17 | streaming-search:21 | search locates a virtualized distant result without a highlight | 改写黑盒 | 同 #15，history 加大到 200 条 |
| 18 | streaming-search:47 | search fallback waits for the rendered tail while the stream remains open | 改写黑盒 | 保留 L63、L70-73 的 `scrollTop`、link 和 mark 断言，`page.clock` 属于测试侧，可以留用。L61、L64、L69 的 pending 属性按第二节去掉。`set` 改为文本增量帧 |
| 19 | streaming-search:47 | search fallback waits for the rendered tail after the backend completes | 改写黑盒 | 同 #18，尾部改为在完成帧里一次到齐 |
| 20 | streaming-search:47 | search fallback waits for the rendered tail with user cancellation | 改写黑盒 | 同 #19，加上 L66 的 wheel 取消和 L72 的 `top === 0`。L66 用的是 `dispatchEvent('wheel')`，改写时改成真实 `mouse.wheel` |
| 21 | streaming-search:47 | search fallback waits for the rendered tail before its first animation frame | 改写黑盒 | 同 #19，起始 assistant 内容为空（L53）。对应「流刚开始、首个增量到达前已在搜索」这一协议上自然存在的状态，不是内部实现 |
| 22 | streaming-search:77 | subagent search retains message-level fallback for hidden link URLs | 改写黑盒 | L84、L87-88 是 link、`scrollTop` 和 mark 断言。驱动同 #7，依赖 subagent 帧，可能降级为移植 |

### model-selection.spec.mjs（11）

fixture 把 Composer 和 hook 单独挂载（`fixtures/model-selection.jsx:23-66`），用 `/api/test-submit` 代替 WS 发送（L25-28）。改写时统一换成：真实应用 + mock 的 `/api/models`、`/api/sessions/model`；用 WS 帧捕获代替 `submitted`；用 composer 模型选择器上的可见标签代替 `data-testid="selection"`；用侧栏切换项目代替 fixture 按钮「General/Project」。

| # | 文件:行 | 标题 | 处置 | 依据 |
|---|---|---|---|---|
| 23 | model-selection:37 | general and project defaults match configuration and sending is blocked while loading | 改写黑盒 | L39 Send 按钮 disabled 可见。L41、L43、L47 的默认选择改为读选择器标签。L45-46 改为 WS 帧里的 `options.modelSelection`。L18 卡住 catalog 的做法在 mock 路由里照搬 另承接 #32/#33 残留的「Escape 关闭模型菜单」断言（菜单项计数为 0），见 #32。 |
| 24 | model-selection:50 | manual selection survives sending, completion and reload | 改写黑盒 | L57 的请求序列是网络观察，原样保留。L60 改为 WS 帧。L63-64 的 reload 改为真实路由 `/session/<id>` 重载 |
| 25 | model-selection:67 | explicit Auto stays selected without a composer execution banner | 改写黑盒 | L74 改为 WS 帧，L75 的横幅不存在是可见断言 |
| 26 | model-selection:79 | unavailable configured models block sending and the picker still allows recovery | 改写黑盒 | L82、L84、L86 分别是 disabled、`role=alert` 文案、enabled，全部可见 |
| 27 | model-selection:89 | new conversations and projects inherit the last accepted send, while unsent choices stay local | 改写黑盒 | L96、L105 是网络观察。L100-104 的继承语义改为「新建会话 / 切项目后看选择器标签」 |
| 28 | model-selection:108 | settings/help commands work with model ready=false, via click and keyboard | 改写黑盒 | L126、L131 是 `/api/commands/execute` 的网络观察。L127 改为设置界面出现，L130 改为帮助文案出现在聊天区，L132、L140 改为「没有 WS 聊天帧」。ready=false 对应 catalog 里没有可用模型 |
| 29 | model-selection:108 | settings/help commands work with model ready=true, via click and keyboard | 改写黑盒 | 同 #28，不含 L133-141 那段 |
| 30 | model-selection:145 | accepted model choices synchronize across tabs while existing sessions retain their model | 改写黑盒 | 同一个 context 开两个页面属于测试侧。L161 的 localStorage 键 `composer-model-last-sent` 是双壳共享存储契约，保留断言；其余改为读选择器标签 |
| 31 | model-selection:169 | response model appears before time only with the response hover actions | 改写黑盒 | 回答帧携带 model，由 mock 推送。L177、L183-184 是文案，L179、L181 的 opacity 是计算样式，L182 是 DOM 顺序。依赖定位契约 `assistant-message-actions`/`assistant-message-model`（第二节），不需要钩子 |
| 32 | model-selection:194 | the mint model menu keeps white surfaces and follows the active accent | **放弃** | L197-201 直接 `import('/src/lib/appearanceRuntime.ts')` 来驱动。L204 断言固定 `rgb(255, 255, 255)`，L205-216 拿内部 CSS 变量名 `--pd-accent-*` 换算颜色，L218-220 断言的图标只是 fixture 拼进来的（`model-selection.jsx:55-58`）。这些都是视觉令牌层面的实现，pixel-perfect 已在范围外（004:145）。主题切换的功能面如果要对拍，由 F3 设置页另写黑盒，不从本例承接。本例另有两条产品可观察的行为：L202 点开模型菜单、L222-223 按 Escape 关闭菜单（菜单项计数为 0）。前者已由 #23-31 的驱动覆盖，后者在 A/B/D 现有身份里都没有断言，所以**并入 #23 的改写，补一条 Escape 关闭断言**，不随本例放弃。**人复核重点** |
| 33 | model-selection:194 | the blue model menu keeps white surfaces and follows the active accent | **放弃** | 同 #32（视觉令牌部分放弃；Escape 关闭菜单的行为并入 #23） |

### composition.spec.mjs（1）

| # | 文件:行 | 标题 | 处置 | 依据 |
|---|---|---|---|---|
| 34 | composition:9 | profile assembly, replacement, interactions and layout | **放弃** | L12 打开的 `/composition.html` 是「Composition Prototype」（`ui/composition.html:6, 10` → `src/composition/prototype/main.tsx`），只有设置 `PILOTDECK_INCLUDE_COMPOSITION_PROTOTYPE=true` 才会进构建输入（`ui/vite.config.js:82-87`），默认产品里没有这一页。真实 profile 装配、路由与替换已经由 B 类 `formal-composition`（4）、`formal-route-matrix`（1）、`real-auth-composition`（3）在主尺子里覆盖 |

**合计**：改写黑盒 31 · 移植测试钩子 0 · 放弃 3（#32、#33、#34），共 34。

## 四、替代分母（待人复核）

- **替代分母 = 37 + 移植 0 + 改写 31 = 68**（B 9 + D 28 + A 改写 31）。之后 31 例中如果有降级为移植的，分母不变。
- 防 skip 对账的注册分母仍是 72（`docs/plan/004_frontend.md:78`），不受影响。A 改写后的用例是新身份：每例标题里带源身份 `[A:<file>:<title>]`，以 (file, title) 为键，与身份正本 `e2e-inventory.json` 的口径相同。不用行号，因为循环展开的多个实例共用一个 `test(` 注册行：#4/#5、#7/#8、#15-17、#18-21、#28/#29、#32/#33 共 13 例落在 6 个行号上，按行号编号会撞号。两边按这个键一一映射，pr2 落地时对 34 个 (file, title) 做唯一性断言。3 例放弃的身份在对账报告里列为「放弃」并附本表理由，**不得静默消失**。C 类 1 例（history-fork）属于后端验收，不进前端分母。
- **哪些计入哪一期的主尺子**：
  - **F3 设置页**：A 类不贡献。F3 继续用 B（`real-auth-native` 等）和 D（`real/settings`）。#28、#29 虽然会打开设置，但它们测的是 composer 的命令通路，归 F5。
  - **F5 聊天面**：31 例全部计入，这与 `docs/plan/004_frontend.md:102`（composer 等以移植后的 A 类为尺）和 :104 的验收条件一致。其中 #1-22 是流式、滚动、搜索，#23-31 是 composer 的模型选择与命令。
  - **F6 收尾**：68 例全绿，再加防 skip 对账零缺口。
  - 已有的 F2-STREAM-* 套件（2 协议 × 4 = 8 例）仍是 F5 开工闸门（004:79、:103），**不并入 68**，以免重复计数。#10、#13、#16 与它重叠的部分只按 A 身份计一次。
- **pr2 的 mock 扩展前置**（按成本递增）：
  1. 纯 history 内容（#15-17）；
  2. 文本增量，已有（#4、#5、#10、#13、#18-21）；
  3. 模型、命令接口与 WS 帧捕获（#23-31）；
  4. thinking、compact、tool 帧（#1-3、#9、#11、#12）；
  5. 分页 history（#14）、双会话（#6）；
  6. subagent 帧（#7、#8、#22，可能降级为移植）。

  样板建议选 #15、#5（只用现有 mock）加 #1（计划 F1 S4 指定，用来量出需要扩帧时一例的成本）。
