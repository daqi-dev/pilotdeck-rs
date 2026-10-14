# F2：双前端对拍、流式黑盒与基线 72 例 runner

旧入口是固定基线 `ui/` 的完整静态 Vite bundle，新入口仍是 `ui-rs/probe-leptos` 探针。两入口代理到一个 mock backend，顺序运行，固定 127.0.0.1:28376（mock）/28377（old）/28378（new）。不启动基线 dev server、gateway 或真实 `ui/server`。

本 runner 有三个独立套件：`skeleton`（3 例登录/HTTP/WS）、`streaming-scroll`（4 例聊天黑盒）、`baseline`（直接复用基线 15 spec，循环展开 72 例）。新增用例不冲抵 72 分母。`streaming-scroll` 可以单独作为 F5 前置校准；`skeleton` 保留第一 PR 的完整基线缺口闸门，仍总 exit 1。**本 PR 只做离线构建与自检；旧侧黑盒通过和 #61 真实报告验收仍须协调者运行后确认。**

## 第二 PR：流式 / 滚动 / 搜索定位

测试在完整 `/session/s` 页面运行：HTTP 历史 seed + 主 `/ws` 推送，观察 DOM 文本、`scrollTop`、尺寸和 viewport。STREAM-002 最终用 textContent 验证两轮内容仍在 DOM，innerText 同时保留供渲染可见性对照。没有旧 store / `window.streamFixture` / React / Wasm 内部状态钩子。`__f2Observation` 仅是测试自己保存的 DOM 采样；API 控制 mock 开始/结束，不经浏览器替业务注入消息。

| 用例 | 机器判据 |
|---|---|
| F2-STREAM-001 | 固定 25Hz × 60s、1500 delta；实际发送率 ≥20Hz，每帧恰好送往一个已 watch 的连接；最终 DOM 序号完整且顺序一致、无重复；有超过 20 个中间内容状态；rAF 采样无 >500ms 停顿、底部不得连续 >500ms 偏离 ≥3px，结束距底 <3px |
| F2-STREAM-002 | 真实 wheel 上滚后，每次 rAF 的 scrollTop 与阅读起点差 <2px，覆盖 200 delta 和 stream_end/complete/历史刷新；点击 Back to latest 后距底 <3px，再启动一次流验证持续跟随 |
| F2-STREAM-SEARCH ×2 | 搜索普通文本及 Markdown 隐藏 URL，显示 `1 / 1` 且下一命中按钮可用；唯一活动高亮 / 正确 href 的链接在 DOM 中可见（`toBeVisible`，不等同进入 viewport）；wheel 阅读后，200 delta 及结束期间位置差 <2px，不能反复居中；Escape 关闭搜索 |

wire 复用消息表的字段形状：`{kind:'stream_delta', id, sessionId, provider, timestamp, runId, content}`；先 `status/started`，后 `stream_end`、`complete`，不是伪造 `type:'stream_delta'`。完成前保存 mock 历史，供旧壳自动刷新。每个测试 reset；测试结束及 runner finally 取消计时器，断线移除订阅。

第 2 轮修复：mock 必须提供 `/api/modules/runtime`，其 `modules` 与冻结构建的 `generatedFrontendProfile` 一致，且 `runtime.gatewayState='ready'`。旧壳在此接口 404 时会保持 `assembly=null`（`ui/src/composition/runtime.ts:77,177`），因此会话标题可以显示而聊天 surface 不挂载。`/session/:sessionId` 路由（`ui/src/components/app-shell/AppShellV2.tsx:81`）和 `[data-chat-search-surface]`（`ui/src/components/chat-v2/MessagesPaneV2.tsx:1267`）均存在。输出目录中的 `mock-runtime.json` 是 `real/*` fixture 的编排文件，不能替代这个 HTTP 接口。离线自检直接执行基线纯 reader/validator，验证响应及 404/错配负例；不等同于旧侧 4/4 浏览器通过。

第 3 轮依据（冻结 commit `cc7c4e6`，以下 `ui/` 相对只读 `resources/PilotDeck/`）：

- **002 的 400/200 是序号数量，不是滚动像素。** `coordinator-stream-old2/old.json` 指向旧测试第 87 行；第一轮 549 个 rAF 样本的 scrollTop 全为 5490。旧夹具连续两次只发 assistant，没有新 user 消息。`ui/src/components/chat-v2/processGrouping.ts:142` 按 user 划分回合，`:944` 把非最后回答收入 trace；`MessagesPaneV2.tsx:1115` 仅在展开时挂载 trace。因此修复第二次 start 的消息边界：先保存并通过原 WS 通道发送唯一 user text，再发送 status/delta/完成消息。保留最终 400 个 DOM 序号的原断言；不加长种子历史、不改 1280×900 视口。离线自检执行原版 `foldCompletedTurns`，旧夹具复现 200，修复后为 400。
- **两条 SEARCH 的自动滚入 viewport 在完整旧壳实跑中未完成，搜索验收改为已观察到的命中行为。** 两张 old2 截图显示 `1 / 1`，但仍在 Question 22/23；错误快照分别给出活动 mark 和 href 链接，viewport ratio 都为 0。种子 Answer 10 是普通最终回答，未放在折叠 trace 内。原基线 `ui/e2e/streaming-scroll.spec.mjs:67` 的同类测试在 `:72` 只要求活动高亮 `toBeVisible()`，随后 wheel 并检查位置差 <2px（`:73–79`）。本套件据此改为命中计数、唯一活动高亮/正确链接与 DOM 可见性；不把 `toBeVisible` 描述成 viewport 定位成功。URL 用例还验证搜索计数，避免原本就存在的链接单独造成假通过。
- **源码存在定位意图，不声称“旧版没有搜索滚动代码”。** `ui/src/components/chat-v2/useChatHistorySearch.ts:158–161,181–195` 对新命中请求 reveal；`chatHistorySearchUtils.ts:421` 返回活动 mark 或消息行（隐藏 URL 没有文字 mark），`:425–439` 请求容器居中滚动。`MessagesPaneV2.tsx:1196–1199` 同时暂停跟随/展开 trace；`ui/src/components/chat/hooks/useScrollFollow.ts:99–105,293–308` 保存并恢复阅读锚点。现有证据没有记录 scrollTo 调用及后续 DOM 位移时序，**不能把平滑滚动/锚点恢复竞争写成已证根因**。可确定的是该场景的完整旧壳未完成自动定位，原自建 `toBeInViewport` 超出了实跑行为和原 E2E 判据。本轮校准命中与阅读稳定性，自动定位缺口明确保留，不计为已验收能力。

SEARCH 新增 `search-dom-evidence` attachment，记录命中目标和聊天容器的 rect/scrollTop/scrollHeight/clientHeight；不通过测试代码调用 scrollIntoView 或改 CSS 把目标搬进视口。SEARCH 标题由 `locates` 改为 `finds`，只影响本套件的独立 4 例，冻结 72 身份不变。阅读 <2px、底部 <3px、持续失跟随/采样停顿 >500ms 判失败、25Hz×60s 和 1500 增量口径均未改变。

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

`stream.mjs` 由 Node runner → mock 直接 import，不属于旧 Vite bundle；新启动 runner 即加载本轮代码，**无需重建旧产物**。协调者从 worktree 根目录持锁运行，output 必须尚不存在：

```sh
(
  set -eu
  mkdir /tmp/pdrs-parity.lock || exit 1
  trap 'rmdir /tmp/pdrs-parity.lock' EXIT
  node scripts/fe-parity/run.mjs --suite streaming-scroll --frontends old \
    --output _tmp/fe-parity/coordinator-stream-old7
)
```

旧侧放行门仍为 4/4 PASS、0 skip、`PASS_SINGLE_FRONTEND_ONLY`；离线自检不替代 old7 浏览器结果，历史 FAIL 必须保留。此为最后一次协调者复测：若 textContent 也仅有 200 个不同序号，原样交回 FAIL 和附件，由协调者挂起此条，不再追加试改。

序号证明**增量内容不丢失**，不声称每个网络包都对应单独一次浏览器绘制。底部允许一个布局/跟随周期的瞬时差，连续失跟随由全程采样捕获；阅读位置不允许先跳再回。`stream-dom-evidence` JSON attachment 保留发送时间、订阅计数、逐帧 DOM 指标及最终文本。截图仍不做像素比较。

新探针没有聊天 surface，测试产生真实 `FAIL`（断言信息 `NOT_IMPLEMENTED: probe has no conversation scroll/search surface`），不 skip、不 xfail。单旧侧 4/4 通过时总状态 `PASS_SINGLE_FRONTEND_ONLY`；双侧只有全部通过才 `PASS_SUITE_ONLY`。基线仍标 72/0 NOT_RUN。

## 第二 PR：完整基线报告（#61）

`baseline.config.mjs` 直接指向只读基线 `ui/e2e/**/*.spec.mjs`，无基线 config import、webServer、globalSetup 或 `real/launch.mjs`。旧构建额外包含 `composition.html`、model-selection/streaming-lifecycle/streaming-scroll fixture HTML，所以 A 类原用例可从静态服务加载；新入口不提供旧 fixture。浏览器仅 `channel: chrome`；基线 Playwright 默认临时 profile 使用本次输出下的 `browser-tmp`，黑盒使用 `_tmp/fe-parity/profiles/` 的独立 persistent profile。不会下载浏览器。

`--profiles staffdeck,minimal,replacement` **分别生成完整 72 例原始 JSON**，不是单报告中重复 projects，也不从一份报告复制。`execution-env.mjs` 清理继承的 FORMAL/REAL/PILOTDECK/REPLACEMENT/FE/PLAYWRIGHT 环境变量，所有 spec 直接读取的 URL 固定到被测侧的 2837x 入口；runtime/evidence 路径固定到本次 worktree 输出。模型标题变量不继承，以免注册漂移。每轮执行前对实际 config 再 `--list --forbid-only`，逐身份核对原 72。

- profile 只设置 `REAL_COMPOSITION_PROFILE` 和 `FORMAL_REPLACEMENT` 等原 spec 门控。**这些是 mock 执行 profile，不是三套真实部署**。本 mock 不实现 Knowledge/SOP/设置/真实 LLM；相应用例会失败。API 级 history-fork 已有非空历史，fork 接口仍明确 404/FAIL。原 spec 内的 `page.route` fixture 保持原样，不改冻结来源。
- `FORMAL_ROUTE_PROFILE` 保持 unset，保留历史 `formal undefined routes ...` 身份和原文 skip；真实 route 矩阵需要后续注册口径评审，不能本轮自动改名。
- 原基线 serial 组首例失败后，Playwright 可能把后续例标无理由 skip。D 类的精确身份归为 `NOT_RUN_PENDING_ORCHESTRATION`，逐条保留原始 skip annotations 和源码依据，仍总 FAIL；B 类继续原身份/理由规则，无理由级联 skip 仍非法。原 spec 已知条件点击弱断言也仍存在，完整报告不是行为全覆盖证明。
- 每份 JSON 含实际 config metadata：frontend/baseURL/profile/backend=`f2-mock`。独立验证后才合并；任一 profile FAIL 不被另一个 PASS 覆盖。两侧合并 skip 身份不等同样 FAIL。

预期全量报告当前**总 exit 1**，原因可包含未实现 API/页面、serial 级联 skip，以及新探针缺 fixture。#61 验收应检查真实报告的 72 个身份与每个 skip 的判定，不能只看退出码。不得据此宣称 37 个真实部署/LLM 用例已通过。

## 依赖与源码构建

从worktree根运行。`BASELINE_UI`可指向只读基线ui目录；默认向祖先搜索ui/或resources/PilotDeck/ui。runner 在用例发现、self-check、报告对账和服务启动之前，要求该目录为 Git 仓库的实际 `ui/`，HEAD 精确等于清单中的 `baselineCommit`（`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`）；不匹配、无法读取 Git、tracked 源码相对 HEAD 有改动、e2e 下有未跟踪源码均拒绝。校验结果写入 `result.json.baseline`。不能用父仓库 HEAD 为随意复制的 ui 背书。复用基线**现有**node_modules中的Vite、ws、Playwright、xterm；未装新JS包。#61 的 node_modules 完整性 P2 残余信任仍未解决。缺依赖时先由协调者准备有lockfile的worktree源码副本，采用 `npm ci --ignore-scripts --prefix <目录>`，不能写只读基线、伪造lockfile或下载Playwright浏览器。

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

**身份许可来源唯一固定为 [skip-allowlist.json](skip-allowlist.json)**：按 `caseId = JSON.stringify([file, 完整展开 title])` 精确匹配，只收 exp007 B 类的9例真实部署/env门控用例。每项的 `source` 给出固定基线 `cc7c4e6` 的用例注册、门控与环境依赖路径:行号，`rationale` 逐条说明原因；不按 `formal-*` / `real-auth-*` 文件名或标题前缀放行。`formal-route-matrix` 保留历史清点未设 profile 时的完整标题 `formal undefined routes follow the generated composition`；将来接入部署矩阵改变展开身份须同步评审注册口径与清单，不能自动归一化成已获准身份。

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

- 协调者真实 Chrome 跑旧侧流式 4 例并保存全量 72 例多 profile 报告；未跑前 F5 闸门与 #61 待验收项不勾选。
- A类34例逐条处置与替代分母，改冻结用例按另issue+评审流程。
- B类9例部署/profile矩阵脚本化，接入D类28例mock LLM编排；完整37候选双跑并对账72。

WS施工图见 [ws-message-map](../../docs/research/007_frontend/ws-message-map.md)。#42正式Chrome复跑见exp007 browser.mjs；本runner不会代替其IME/流式/岛门。
