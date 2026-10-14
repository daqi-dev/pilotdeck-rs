# exp007 · F1 双候选前端探针

> 2026-10-14；基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，只读。开始 04:13（Asia/Shanghai）。只做探针，不迁生产页面；不提交，不改 board/changelog。Rust 工程独立放在 `ui-rs/`，不进入 `src/` workspace。

## 结论与放行状态

**暂选 Leptos 0.8.17，AI 代批，待人复核。F1 尚未完整验收，不能据此放行 F3 生产迁移。** Dioxus 0.7.9 也通过本次 DOM 降级层，不是失败候选；混合方案没有触发“两候选全败”仲裁。

暂选依据：相同 payload、发布 profile、wasm-bindgen 流水线下，Leptos wasm 和 JS 更小；两者均能构建纯静态资产，均能经显式导出的 `TerminalBridge` 驱动真实 xterm。DOM 层 Leptos 延迟较小，但单轮、非浏览器绘制数据不用于宣称框架性能普遍更优，也不把此次 CSR 桥接当成 Leptos SSR islands 官方路径的证明。

**验证边界**：系统 Chrome 155.0.8059.40 使用专用 worktree profile 启动后 SIGABRT，日志出现 EPERM。没有下载 Playwright 浏览器，没有使用用户 Chrome profile，没有提升权限或换 UI 控制路径绕过 sandbox。按任务授权降级为 **两份真实 CSR wasm + jsdom 29.1.1 + 真 xterm 5.5.0 + 本地 HTTP/WS**。这能验证 DOM 事件分发、接收/DOM 更新帧数、节点/缓冲/DOM focus 的生命周期；不能验证浏览器绘制、可感卡顿、真实滚动、OS IME 候选窗、BFCache、真实整页跨壳恢复。三命门中流式的“不卡顿/滚动不跳”仍缺证据，不把缺测写成通过。

## 探针实现与同负载方法

- 两个 `src/main.rs` 各实现登录触发、列表渲染、composer 的 framework 原生 composition/input/key handlers、stream signal 更新、条件挂卸岛。没有把某候选的渲染绕到普通 JS DOM 写入。
- 共用 `ui-rs/shared/core.rs`：HTTP 登录/Bearer、token 刷新、storage、主 WS、30s ping、1s→30s 退避、`watch-session`、帧序号计数、**wasm-bindgen 导出的 `TerminalBridge`** 及对 JS owner 的类型化 imports。WS closures 随连接 owner 清理，不 `forget()` 堆积重连回调。
- `island.js` 是 xterm 适配件：保留同一个 Terminal 与 DOM root，卸载时移到 DocumentFragment，重挂归还节点和焦点；mock `/shell` init/input/output 适配留在岛边缘，没有生产 PTY。断线 clear，不自动重连；探针按钮手动重进。
- 请求按回源契约使用 `/api/auth/login`、`/api/projects`、**`/api/projects/probe/sessions?limit=5&offset=0`**。计划的 `/api/sessions` 是概括，基线 `ui/src/utils/api.js:231` 的列表 API 是 project-scoped。主 `/ws?token=`，shell `/shell?token=`；全部服务只绑定 `127.0.0.1`，端口限定 **28370–28375**（`PROBE_PORT` 可在此范围内覆盖）。
- IME：**synthetic composition only，未测 OS IME**。DOM 层用 `new CompositionEvent`；浏览器层经 `page.evaluate` 派发 `new CompositionEvent(..., {bubbles:true})`，并核验 constructor。两候选的 `data-composing` 必须由 handler 从 false → true → false；浏览器等待标记及两个 rAF 后再测 Enter。每条 keydown 经 `data-keydowns` 确认 handler 确实触发；组合期间、独立 isComposing / keyCode229 / Shift+Enter 路径均不得发送，普通 Enter 才发送一次。输入草稿保存、已有 storage 的启动恢复另测。
- 流式：序号 1…1500，每帧一行中文增量，nominal **25Hz**，服务器总持续 ≥60s 且实测平均 ≥20Hz。选择 25Hz 是给 Node timer 漂移留余量：初版 nominal 20Hz/1200 帧实际不足 20Hz，故没有使用那轮结果当验收数据。两候选最终运行按顺序，负载相同。
- 接收计数是 Rust 同步入站处理；第 2 轮另暴露 **`data-seq = Frame.seq`**，MutationObserver 以 `data-received` 变化辨识一次更新，并同时记录真实 `data-seq` 与接收计数。两者均须 1…1500，trace 还须逐项匹配服务器已发序列、缺序计数为 0。首轮 trace 的 seq 实为接收计数，不能独立证明真实帧序号连续。浏览器滚动断言先要求 `scrollHeight > clientHeight > 0` 且实际 `scrollTop > 0`，再检查阅读期间位置不跳。**DOM observer 不是浏览器 paint 帧。** jsdom 的 canvas measureText 仅给 xterm 初始化所需的固定几何桩，不把该桩当滚动/尺寸/绘制实测。
- jsdom 29 的 WS 使用 Node/undici。DOM fallback 给 global Event 的 instanceof 同时接受 jsdom Event 与 Node Event，并保留 Node constructor/brand，避免 generic input 被 Dioxus 丢弃。这是测试环境适配，不是候选生产补丁；前期因此失败的 Dioxus quick 结果未当选型失败。

## 首轮 S1/S3 DOM 实测（历史证据）

| 指标 | Leptos | Dioxus |
|---|---|---|
| wasm 启动至探针 DOM（ms；单轮） | 53.29 | 50.00 |
| compositionstart/end + Enter guard（synthetic only，未测 OS IME） | PASS_DOM_ONLY / 组合中 0 次，结束后 1 次 | PASS_DOM_ONLY / 组合中 0 次，结束后 1 次 |
| 服务器注入持续（s） | 61.539 | 61.583 |
| 实际平均注入频率（Hz） | 24.375 | 24.357 |
| 接收 / DOM 更新 / 缺序 | 1500 / 1500 / 0 | 1500 / 1500 / 0 |
| DOM 延迟 p50（ms） | 1.581 | 1.642 |
| DOM 延迟 p95（ms） | 2.183 | 1.984 |
| DOM 延迟 p99（ms） | 2.330 | 2.434 |
| DOM 延迟 max（ms） | 2.888 | 4.701 |
| DOM 更新最大间隔（ms） | 45.367 | 46.874 |
| xterm 挂卸 / 身份 / 缓冲 / DOM focus / 输入 | 10 次 / PASS_DOM_ONLY | 10 次 / PASS_DOM_ONLY |
| 主 WS 重连耗时（ms） | 1018.67 | 1013.32 |
| 实际 ping 次数 | 2 | 2 |
| shell 断开 / 手动重进 | 2.2s 无自重连 / 手动新连接 PASS | 2.2s 无自重连 / 手动新连接 PASS |
| 浏览器绘制、滚动、原始 37 黑盒双跑 | NOT_RUN / Chrome 启动阻塞 | NOT_RUN / Chrome 启动阻塞 |


每个候选的 [原始结果](evidence/leptos.json)、[Dioxus 原始结果](evidence/dioxus.json) 均含 wasm SHA256；分别与 [构建清单](evidence/build.json) 一致。[Leptos trace](evidence/leptos-trace.json) / [Dioxus trace](evidence/dioxus-trace.json) 各有 1500 条 DOM 更新；首轮 seq 是 `data-received` 接收计数，连续性只另受 missing 检查，不能当作真实帧序号对账证据。这里保留原件与原始数字；第 2 轮验证另记。此表对应沙箱 DOM 运行，协调者首轮 Chrome 记录的效力见文末。

岛十次循环每次断言 root/input 对象身份相同、detach 时不在文档、remount 时在文档、DOM activeElement 是原 terminal textarea、缓冲仍有 BUFFER_SENTINEL；最终 DOM Enter 事件产生一个 xterm onData 并经 `/shell` echo。**这里的 DOM focus 通过不等于跨文档/OS 焦点验证。**

## 构建链、资产与环境

固定直接依赖：Leptos `=0.8.17`，Dioxus `=0.7.9`，wasm-bindgen `=0.2.129` / futures `=0.4.79` / web-sys 与 js-sys `=0.3.106`，gloo-net `=0.6.0`、timers `=0.3.0`、serde `=1.0.228`、serde_json `=1.0.150`；两个 Cargo.lock **已在当前 HEAD 被 Git 跟踪**（第 2 轮核验 `git ls-files --error-unmatch`；并非待 add 的未跟踪文件），构建统一使用 `--locked`。Dioxus 顶层是 0.7.9，锁文件的 core/html/web 等兼容子包有 0.7.10，不能误称整个依赖图只有 0.7.9。Rust `1.99.0`，node `22.22.0`。完整版本见 [环境记录](evidence/environment.json)。

所有 cargo 都用本 worktree `_build/cargo-home`。用 `cargo install --root <worktree>/_build/tools --version … --locked` 安装：Trunk 0.21.14、dx / dioxus-cli 0.7.9（disable-telemetry）、wasm-bindgen-cli 0.2.129。Trunk release 安装有 num-traits build-script SIGKILL 两次，改 **debug 安装**成功（工具本身未优化不影响 cargo release probe 产物）；没有将该安装错误归因于 Leptos。

原生 bundler 均实际成功：`trunk build --release --locked` 与 `dx build --web --release --locked`。Trunk 按 `/probe/leptos/` 生成入口，dx 配 `/probe/dioxus/`；二者产物都只需静态托管，不需要 Rust web server。报告的公平包体比较统一使用 cargo + 同一个 wasm-bindgen `--target web`，包含生成 JS 与岛 snippets；没有把 dx 与 Trunk 的不同默认 bundling 方式混在一个公平体积数字里。

| 指标 | Leptos | Dioxus |
|---|---|---|
| wasm 原始字节 | 449,336 | 718,068 |
| wasm gzip 字节 | 111,122 | 210,103 |
| 生成 JS + snippets 字节 | 47,472 | 97,111 |
| 生成 JS + snippets gzip 字节 | 8,949 | 17,828 |
| 本次 common cargo 增量编译（s；非 cold） | 3.780 | 2.776 |
| wasm-bindgen（s） | 0.034 | 0.039 |
| 原生 bundler（s；缓存状态不作公平速度排名） | 0.444 | 3.540 |


相同 profile：opt-level=z、LTO、1 codegen unit、panic=abort、strip=false；不做 wasm-opt。wasm 仍有 name 信息，后续不能直接把本表当“极限优化体积”。统一管线输出 `_build/dist-{leptos,dioxus}/`，HTML/JS/wasm 都在该目录，第三方 vendor 由 mock server 从只读 node_modules 托管。

共同第三方 xterm.js：289,441 B，gzip 67,245 B；CSS 5,559 B，gzip 2,071 B，MIT（[库存](evidence/xterm.json)）。它的成本对两候选相同，未计入各自 Rust/生成 JS 差值；无测试库/jsdom 打进发布资产。

构建链踩坑：

1. Trunk 0.21.14 在 build 子命令把继承的 `NO_COLOR=1` 当 bool 解析而拒绝；runner 仅移除该变量，未改全局环境。
2. dx 在 strip=true 下调用 Rust 1.99 的 rust-objcopy，该工具在此 host 缺相对 `libLLVM.dylib`；改 dyld 搜索目录后又发现提前 strip 会破坏 wasm-bindgen 所需 sections。因此两个候选统一 **strip=false**，由 bundler/wasm-bindgen 处理。没有改 ~/.rustup 或安装目录。
3. dx 的工具缓存显式设 `_build/dx-home`，session cache `_build/dx-session`，避免写用户 ~/.dx。配置 resource.dev 必须显式存在；已修正。
4. Leptos CSR 中动态 view/on_cleanup 闭包有 Send/Sync 要求，JS owner 用 SendWrapper<Rc<…>>（只在当前浏览器线程）；Dioxus props 使用 JS owner identity 的 PartialEq。两者都不允许 renderer 接管岛内部 DOM。

Dev 热替换：**未测端到端热替换/状态保留，也未拿 watch 日志冒充 HMR 成功**。Chrome 的共同启动阻塞使该项无法完成。可在可启动 Chrome 的环境用 Trunk serve 的 `--address 127.0.0.1 --port 28371` / dx serve 的 `--addr 127.0.0.1 --port 28372 --open false` 补跑；dx 当前为 minimal features，devtools/HMR 配置应单独记录，不能把启用调试特征后的体积与上表混比。

## S4 · 双跑与分母（明确缺测）

基线 Playwright 1.60.0 `--list` + `forbidOnly` 实际注册 **72**，无收集错误；[逐例清单](evidence/e2e-inventory.json) 保留标题、文件与 expectedStatus。正式黑盒候选仍是 B 9 + D 28 = **37**，本次 **0/37 真实浏览器执行**。没有用 env skip / 条件点击做出虚绿。

| 层 | 旧前端 | Leptos | Dioxus | 解释 |
|---|---|---|---|---|
| 原 B 9 例 | NOT_RUN | NOT_RUN | NOT_RUN | Chrome 启动受阻；另缺 formal/real-auth 的 staffdeck/minimal/replacement 真部署矩阵。登录+列表探针不提供设置/知识等生产页面 |
| 原 D 28 例（含 real/session 8） | NOT_RUN | NOT_RUN | NOT_RUN | Chrome 受阻；`real/launch.mjs` 要 LLM_CENTER_URL/API_KEY 与三进程，未起真实 gateway/LLM；探针只用 mock |
| S2 三组可比较断言：登录/token/认证请求、projects_updated、主 WS 重连 | PASS（原模块 DOM fixture） | PASS_DOM_ONLY | PASS_DOM_ONLY | 原 LoginForm/AuthProvider/WebSocketProvider/api 在原路径编译并直接运行，没有重写旧鉴权/WS。**该 fixture 不是完整旧 App**，也不是正式 37 用例 |
| S3 composition/流式/岛三组 | 未给旧壳拼一个假的同功能 composer | PASS_DOM_ONLY（绘制/滚动未测） | 同左 | 两 Rust 同判；不计入原黑盒分母 |
| A history refresh 样板 | 部分通过，第四阶段 FAIL | NOT_RUN | NOT_RUN | 见下；未为让探针通过而迁聊天逻辑 |

原模块对照见 [baseline-dom.json](evidence/baseline-dom.json)；列表的 mock 同时覆盖 auth status/user、onboarding、projects/session、主 WS。基线源码不改，只用新写的 fixture wrapper 引原模块。

A 类移植尝试：[history-refresh.mjs](../../../scripts/exp007/history-refresh.mjs)。读取原 `streaming-lifecycle.spec.mjs` 的 history refresh 例，改为点现有 `?manual-reconcile` fixture 的可见回放按钮 + 一份图片/一次提问/思考在回答之前的 DOM 断言，不再调用 window.store。前 3 阶段通过；第 4 阶段“完成历史”切换 assistantWorking=false，DOM 只剩提问/回答，思考内容不再渲染，严格断言失败（[结果与失败 DOM](evidence/history-refresh.json)）。原用例一直保持 working=true，故这个按钮式驱动不是一比一等价移植：**样板是 PARTIAL，不判基线回归，不把一次失败变成放弃该尺子**。约 8 分钟含读源/编写/打包/两轮试跑；单次约 6s。F2 应拆开“refresh while running”与“completion refresh”两种触发，再在原/新真实页面双跑；包体/DOM 几何也需浏览器验证。

## S5 · B 前置反代与历史边界

只完成 **B（Caddy）**；**A 真 ui/server 前缀 patch 未搭、未测**，不把模拟 Express catchall 或纸面 patch 计为 A 实测。B 是本轮降级交付，共存最终方案留 F3/F2 环境补测。没有改真实 `ui/server` / baseline 源码；没有起用户 PilotDeck 进程或碰 3001/18789。

Caddy 2.10.2 mac/arm64 的官方 release archive 已对官方 SHA512 清单校验（SHA256 cc9ad20742ea7bfee5dd1d435d42ab7fcf8592294f9ec43bf08fd21cbe448bc4）。配置：[Caddyfile](../../../scripts/exp007/Caddyfile)，admin/auto_https off，grace_period=2s，入口 127.0.0.1:28375；新 `/probe`、shared/vendor → 28370 的 Rust 静态/mock 服务；其余 → 28374 的真实旧 Vite index；API/ws/shell → 28370。旧 Vite 缓存写本 worktree，源码保持只读。

[coexist.json](evidence/coexist.json) 验证 `/`、`/settings`、`/probex` 返回旧入口；两个 `/probe/{candidate}/` 返回新入口且 wasm Content-Type/内容可取；代理登录 token 正常、主/终端 WS upgrade 正常、终端 CADDY_ECHO 来回。Caddy 在独立进程组中起，finally 只 SIGTERM 自己的组；mock 与 Vite 在 runner 内 finally 同时 close（5s 清理窗口），随后仅退出本 runner，避免 Vite/esbuild idle handles 悬留；最后一次 close 均完成。两次旧 runner 跨 exec 单元直接 SIGTERM 被 sandbox 拒绝，已通过原 exec session Ctrl-C 退出（130）；所有实验端口收尾无监听。额外运维面：一个配置 + 一个常驻前置代理、部署时要同时管理旧 upstream 与新静态服务；生产静态资产可由 Caddy file_server 直接供，但本次没有把那个变体算为验证过。

导航钩子：[boundary.js](../../../ui-rs/shared/boundary.js)。在 router 初始化之前封装 history.pushState/replaceState，并监听 popstate；加载壳身份固定，前缀严格只匹配 `/probe` / `/probe/`，`/probex` 不误切；跨壳 push 用 assign，replace/遍历用 replace，避免后退产生重复 history entry。新壳到未迁 URL 反向同判。实验注入真实 React Router BrowserRouter/useNavigate/Link/NavLink，三类入口均记录了硬跳目标；jsdom 原生 back/forward traversal 均记录 replace，见 [navigation.json](evidence/navigation.json)。**硬跳函数以 recorder 替代：只证明拦截调用，未证明浏览器 document reload 成功。**

两候选已有 shared auth/draft 的 bootstrap 恢复各通过：[Leptos](evidence/leptos-restore.json) / [Dioxus](evidence/dioxus-restore.json)。同文档 history 不改变 storage 通过；**不把给 jsdom seed storage 写成真实跨文档 storage 共享**。完整静态键盘点见 [storage-keys.md](storage-keys.md) 与 [逐调用点 AST 结果](evidence/storage.json)：769 文件、197 调用点（生产 125），动态表达式保留，不假定静态分析能穷举 runtime/第三方隐含键。

## D1–D5 对账

当前有效口径以 [accepted-diffs F1 追加表](../../research/007_frontend/accepted-diffs.md#f1-exp007-对账与修订2026-10-14追加当前有效口径) 为准：

- D1：约 1s 重连、watch 重挂、实际 ping 支持；跨文档 bootstrap 与同文档断线 retry 分开。失败握手完整 backoff 梯阶/离线队列未测。
- D2：shell 2.2s 没有自重连、手动重进新建连接；真实跨壳终端页触发与真实 PTY 未测。
- D3：[断口演示](evidence/stream-gap.json) 两次真实网络连接共收到 64/75 帧，缺 21–31，watch 两次；它是 mock 不重放的演示，完整历史/真实后端不算验证。
- D4：入口分流与 router/history 拦截通过，浏览器 reload/BFCache/状态恢复未测。
- D5：撤回“跨壳前进/后退必不恢复”的无条件口径，改成不承诺原内存滚动/焦点恢复；history traversal 允许浏览器自动恢复，具体待测。[scrollRestoration 文档](https://developer.mozilla.org/en-US/docs/Web/API/History/scrollRestoration) 说明 auto 可以恢复滚动，不能靠整页加载推断必丢。文档核查不是本次实测。

五条均未伪装成完整跨壳实测转正。登录态与 composer 草稿继续是共享不变式，丢草稿是 bug。主题/语言的真实键是 themeMode/theme 与 userLanguage。

## 复现与验证

在 worktree 根，脚本不会写 resources 或用户 profile。`BASELINE_UI` 可覆盖只读基线 ui 路径（须有既有 node_modules）；默认从 worktree 向祖先目录寻找 `resources/PilotDeck/ui`，不硬编码机器绝对路径。第 2 轮只使用既有工具与缓存，**不下载预编译二进制**；已有 Caddy 的来源记录属于首轮历史，不运行其下载脚本。

```sh
python3 scripts/exp007/build.py --bundlers
# 严格顺序：mock 的 28370 不得与 coexist runner 同时抢用。
node scripts/exp007/gates.mjs leptos
node scripts/exp007/gates.mjs dioxus
node scripts/exp007/baseline.mjs
node scripts/exp007/baseline-dom.mjs
node scripts/exp007/navigation.mjs
node scripts/exp007/coexist.mjs
node scripts/exp007/stream-gap.mjs
node scripts/exp007/restore.mjs leptos
node scripts/exp007/restore.mjs dioxus
node scripts/exp007/history-refresh.mjs  # 预期目前 FAIL，不隐藏失败
node scripts/exp007/storage.mjs
# 有能力启动系统 Chrome 的环境补跑；persistent profile 在 worktree 内。
node scripts/exp007/browser.mjs leptos
node scripts/exp007/browser.mjs dioxus
python3 scripts/exp007/check.py
```

基线清点专用 config 只能 `--list`，见 `list-tests.config.mjs` 注释；不得把该清点 config 当作浏览器 runner 绕开专用 profile。browser.mjs 只能补机器断言/绘制间隔，OS IME 与可感顺滑仍建议人亲手复核。无需新下载浏览器。

本地 CI 同配置 `src/` fmt / clippy --all-targets -D warnings / test --all **全通过，696 tests passed / 0 failed / 0 ignored**；独立探针 wasm-target clippy -D warnings、fmt check、JS 语法检查通过。冻结尺子校验通过，没有改 specs/parity。命令与输出见 [CI 摘要](evidence/ci.json) 和 [验证输出](evidence/validation.log)。没有推送，未声称远端 GitHub CI 已运行。

收尾资源：本 worktree `_build` 约 9.5 GB；主仓全体 `_build` 约 16 GB、`_tmp` 约 3 GB，合计约 19 GB，未超 30 GB。日志与 canonical JSON/trace 留档；构建二进制与 baseline bundle 只在忽略的 `_build`，不入库。

降级/待补：浏览器 paint/scroll 与真实跨文档 storage/BFCache；37 黑盒部署/LLM 双跑；A 真 server prefix patch；dev HMR；history refresh 的一比一黑盒驱动与新聊天面覆盖。交接写 `_tmp/RESULT.md`，协调者提交与决定后续环境。

参考只用于构建/协议方法：Leptos [CSR 部署](https://book.leptos.dev/deployment/csr.html)、Dioxus [0.7 工程设置](https://dioxuslabs.com/learn/0.7/tutorial/new_app/)、Caddy [reverse_proxy](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy) / [global options](https://caddyserver.com/docs/caddyfile/options)。选型数据来自本次实测，不来自框架宣传。

## 第 2 轮本地验证（传输层替代已明示）

本轮沙箱除无法启动 Chrome 外，还拒绝 `listen(127.0.0.1:28370)`（EPERM）。两候选原版网络 DOM gates 都尝试运行，均在服务启动时受阻，未进入候选检查：[Leptos 阻塞记录](evidence/r2/leptos-network-blocked.json)、[Dioxus 阻塞记录](evidence/r2/dioxus-network-blocked.json)。未把环境阻塞算作候选失败。Chrome 门未在本轮沙箱执行，留给协调者。

为验证本轮修正，另在 `_tmp/r2-offline/runner.mjs` 使用真实 CSR wasm、原 mock 的 HTTP/WS handlers、真实 xterm 与 jsdom，把底层监听/连接替换为进程内传输。原版 `gates.mjs` 保留 localhost HTTP/WS 路径。**下表 PASS 仅为 `PASS_DOM_IN_PROCESS_TRANSPORT_ONLY`，不是本轮网络、Chrome、绘制、几何或 OS IME 证据**；替代传输的延迟不作选型依据。

| 第 2 轮检查 | Leptos | Dioxus |
|---|---|---|
| 七组 DOM gates | 全部 PASS（进程内传输） | 全部 PASS（进程内传输） |
| composition handler 标记 / keydown 次数 | false → true → false / 5 | false → true → false / 5 |
| composition 范围 | synthetic composition only，未测 OS IME | 同左 |
| 接收 / DOM 更新 / 真实 Frame.seq | 1500 / 1500 / 连续 1…1500 | 1500 / 1500 / 连续 1…1500 |
| mock 定时负载持续 / 实际频率 | 61.440 s / 24.414 Hz | 61.497 s / 24.391 Hz |
| xterm DOM/缓冲/焦点/输入 | 十次重挂 PASS | 十次重挂 PASS |
| 序号负向对照（隔离 missing 检查） | 序号偏移 +1000 被真实 seq 断言拒绝，exit 1 | 同左，exit 1 |
| wasm gzip（共同管线，含新增插桩） | 113,050 B | 210,521 B |
| Chrome / 滚动几何 | NOT_RUN，协调者补跑 | 同左 |

[Leptos 原始结果](evidence/r2/leptos-dom-offline.json)、[Dioxus 原始结果](evidence/r2/dioxus-dom-offline.json) 与各自 [trace](evidence/r2/leptos-dom-offline-trace.json) / [trace](evidence/r2/dioxus-dom-offline-trace.json) 的 wasm SHA256 和 [本轮 build.json](evidence/r2/build.json) 一致。序号负向对照 [Leptos](evidence/r2/leptos-negative-seq.json) / [Dioxus](evidence/r2/dioxus-negative-seq.json) 为预期 FAIL；该对照在临时 harness 内故意跳过 missing 检查，以单独确认真实 seq 断言的效力，正式 gates 没有跳过。

共同 release 构建与 Trunk/dx `--locked` 构建均通过；两探针 wasm clippy `-D warnings` / fmt 通过；[src/ CI](evidence/r2/ci.json) 的 fmt、clippy、test 全通过（696 passed / 0 failed / 0 ignored）；[16 个 JS 模块语法检查](evidence/r2/syntax.json) 通过；[岛消息边界](evidence/r2/island-boundary.json) 验证 8 个坏 JSON/形状输入不抛错且之后合法 output 仍写入。端口范围正向 6 项、拒绝 11 项与 BASELINE_UI 覆盖检查通过。完整交接、命令及边界见 `_tmp/RESULT-r2.md`；[本轮证据 manifest](evidence/r2/manifest.json) 与首轮 manifest 独立，首轮原件哈希未改。

## 协调者补跑：首轮 Chrome 结论撤回（2026-10-14，第 2 轮修订）

协调者首轮在沙箱外使用本机已装 Chrome（Playwright `channel: chrome`，headless，worktree 专用 profile）各跑一次。原始记录保留，不修改：

| 候选 | 首轮记录（历史原件） | 当前效力 |
|---|---|---|
| Leptos 0.8.17 | PASS_BROWSER_MACHINE_CHECKS | 探针已修正，旧 PASS 不能替代第 2 轮 composition/真实帧序号/滚动溢出断言；synthetic only，未测 OS IME |
| Dioxus 0.7.9 | composition 期间 Enter 发送，FAIL；后续项未执行 | compositionstart handler 未收到正确事件类型，不能归因于 Dioxus 的 IME 缺陷；后续项仍缺测 |

原件：[Leptos 协调者首轮记录](evidence/browser-leptos-coordinator.json)、[Dioxus 协调者首轮记录](evidence/browser-dioxus-coordinator.json)。

**首轮「Dioxus IME FAIL → 选定 Leptos」结论因探针 bug 作废，明确撤回。** 协调者已核实 Playwright `locator.dispatchEvent('compositionstart')` 在真实 Chrome 构造的是普通 `Event`，而 Dioxus 按 `CompositionEvent` 类型过滤，故 handler 未触发。没有 handler 插桩和等待也让首轮探针缺少时序隔离。这份证据不支持淘汰 Dioxus，也不能把 Leptos 合成事件 PASS 外推为 OS IME 安全。

当前口径与 [decision.md 的 F1 追加](../../research/007_frontend/decision.md#f1-exp007-复核追加2026-10-14) 统一：**暂选 Leptos（AI 代批，待人复核），依据退回同发布 profile / 同 wasm-bindgen 管线下的包体、构建可用性与其他已测项**。首轮 gzip wasm 为 111,122 B vs 210,103 B；第 2 轮插桩后为 113,050 B vs 210,521 B，仍仅支持本探针体积差异。Dioxus 保留为可用候选。F1 尚未完整验收，不能据此放行 F3。

修正后的浏览器门由协调者在可启动系统 Chrome 的环境**顺序**运行，无需下载浏览器：

```sh
node scripts/exp007/browser.mjs leptos
node scripts/exp007/browser.mjs dioxus
```

范围始终是 **synthetic composition only，未测 OS IME**。真实 OS 输入法、可感顺滑、37 例黑盒双跑、跨壳 storage/BFCache、A 托管 patch 与 HMR 仍需补测。第 2 轮沙箱未运行 Chrome；本地可执行检查与额外网络限制见 `_tmp/RESULT-r2.md`。

## 协调者补跑 r2：真实 Chrome（2026-10-14 06:40）

按第 2 轮修正后的 `browser.mjs`（真 `CompositionEvent`、isComposing/keyCode229/Shift 路径、data-seq、滚动前置断言），本机 Chrome、worktree 临时 profile：

| 运行 | Leptos | Dioxus |
|---|---|---|
| 原样 `browser.mjs` | FAIL @ `browser.mjs:55` | FAIL @ `browser.mjs:55` |
| 诊断副本（仅把 `:55` 草稿断言改为记录值，未入库） | **PASS_BROWSER_MACHINE_CHECKS**（4/4） | **PASS_BROWSER_MACHINE_CHECKS**（4/4） |

`:55` 的失败两候选完全相同：组合期间的真实 Enter 两边都**未发送**（`sent=0`，`:54` 通过），但合成 composition 不会像真实 IME 那样吞掉 Enter 的默认动作，浏览器照常插入换行（草稿 `'你好\n'`）。这是探针断言与合成事件的口径问题，不是候选差异：真实 IME 下 Enter 由输入法消费。两家第2轮评审确认 #42 修法：正式断言改为「sent=0 且草稿以 `你好` 开头」，并用该完整草稿作为后续合成键守卫的保持基值；候选不补 preventDefault，保持基线 IME Enter 直接 return 的行为。正式脚本需在协调者环境复跑。

**结论**：在已测范围内两候选的 IME 守卫、xterm 生命周期、≥20Hz 流式与真实帧序号、滚动均无差别；选型依据只剩体积（gzip wasm Leptos 111,122 B vs Dioxus 210,103 B）与构建可用性。**暂选 Leptos（AI 代拍 a′，待人复核）**。原件 `evidence/r2/browser-*-coordinator-{raw,diag}.json`。

## F2 前置 #42 修正与证据归档（2026-10-14）

正式 browser.mjs 已保留 sent=0，并将 composition 内 Enter 的草稿断言改为前缀；后续 isComposing/keyCode229/Shift 必须保持 Enter 后完整草稿，不清理真实换行。候选源码未改。截图固定写 worktree `_tmp/exp007/screenshots/`，不入证据库、不做像素断言。

r2 的4份协调者 raw/diag 原件 SHA256 已追加 [manifest](evidence/r2/manifest.json)，原件未改。诊断 diff 见 [patch](evidence/r2/browser-coordinator-diagnostic.patch) 与 [归档说明](evidence/r2/browser-coordinator-diagnostic.md)：原临时脚本已缺失，归档为明确标识的可复现重建版，不伪称原版逐字归档。正式 runner 不依赖诊断副本。

正式浏览器验证由协调者依次执行 `node scripts/exp007/browser.mjs leptos`、`node scripts/exp007/browser.mjs dioxus`，预期4/4机器检查PASS；本轮 F2 worker 不监听、不运行 Chrome。当前 Leptos 定选以 decision.md 最后「a′ 定选」的人亲测结论为准，历史暂选记录保留其时点效力。

## #42 修正后正式复跑（2026-10-14，协调者沙箱外，本机 Chrome）
原样 `scripts/exp007/browser.mjs`（断言改为「sent=0 且草稿以『你好』开头」，候选不加 preventDefault）：**Leptos、Dioxus 均 PASS_BROWSER_MACHINE_CHECKS（4/4）**。原件 `evidence/r3/browser-*-coordinator.json`。
