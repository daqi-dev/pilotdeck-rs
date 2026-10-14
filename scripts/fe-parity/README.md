# F2 第一 PR：双前端对拍骨架

旧入口是基线 `ui/` 的完整 Vite bundle，新入口是人已定选的 `ui-rs/probe-leptos` 探针占位。两入口代理到**一个**mock backend，旧/新顺序跑同一组3例登录/项目请求/主WS黑盒骨架用例。定位器差异集中在测试适配函数；没有像素比较。截图只有旧侧，落本次输出的 `baseline/`，Playwright自动截图/视频/trace关闭。

**本骨架不是72例或37例验收。** 每次实时 `--list --forbid-only` 展开基线 `ui/e2e/**/*.spec.mjs`，与 exp007 的72个注册身份核对；新骨架用例单独列出，绝不冲抵基线分母。默认双跑即使3/3两侧都PASS，完整对账仍为72应执行/0实际执行，逐项NOT_RUN并exit 1。后续PR接入原有用例与部署后才可能转绿。

## 依赖与源码构建

从worktree根运行。`BASELINE_UI`可指向只读基线ui目录；默认向祖先搜索ui/或resources/PilotDeck/ui。本PR复用基线**现有**node_modules中的Vite、ws、Playwright、xterm；未装新JS包。缺依赖时先由协调者准备有lockfile的worktree源码副本，采用 `npm ci --ignore-scripts --prefix <目录>`，不能写只读基线、伪造lockfile或下载Playwright浏览器。

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
# 由协调者在可listen环境跑，检查真实静态/HTTP代理/WS upgrade，两入口同mock：
node scripts/fe-parity/run.mjs --network-self-check
# 协调者浏览器双跑：本机已装Chrome，persistent context且每例worktree临时profile。
node scripts/fe-parity/run.mjs --suite skeleton --frontends old,new
# 当前预期：skeleton.md有3行PASS/FAIL；baseline-parity.md有72行NOT_RUN；总exit 1。
# 没有浏览器报告时的严格负向对账（预期exit 1）：
node scripts/fe-parity/run.mjs --reconcile
# 后续完整基线报告对账：每个JSON须携带config.forbidOnly=true，无重试/重复项目结果。
node scripts/fe-parity/run.mjs --reconcile --old-report <old.json> --new-report <new.json>
```

输出默认 `_tmp/fe-parity/run-<随机后缀>/`；可用 `--output _tmp/fe-parity/<本次目录>` 指定，拒绝worktree外路径。含实时注册原件、各侧Playwright JSON/日志、逐例表和result.json。profile单独留 `_tmp/fe-parity/profiles/`。指定的输出目录若已存在立即拒绝（EEXIST），不覆盖/清理人的证据；重跑请用新输出目录。

self-check只验证注册、进程内mock handlers、对账正负例和静态资产存在/路径保护。输出明确72/0 NOT_RUN，但 `PASS_SELF_CHECK_ONLY`/exit 0仅表示这个独立规则检查通过。network-self-check额外检查两侧真实HTTP/WS代理，不启动浏览器，也不计基线执行数。浏览器/对账模式的任何缺口则exit 1。

固定端口 mock=28376、old=28377、new=28378（28379保留）。所有监听写死127.0.0.1，不读取PORT/HOST，不启动真实PilotDeck/gateway，不碰3001/18789/用户PILOT_HOME。资源finally关闭；中断仅向本runner创建的Playwright进程组发信号，不全局kill。

## 对账规则

分母来自本次展开注册数，历史72用于漂移警报而非取代动态清点。逐身份核对完整结果：应执行=注册数−**有显式skip description的实际skipped**；实际执行=passed/failed/timedOut的唯一尝试。每条SKIP打印原因；无理由skip、NOT_RUN、重复/未知身份、interrupted、xfail、behavioral FAIL、report errors、注册漂移、数量不符全部FAIL。报告必须声明forbidOnly；每侧独立核对，两个报告同时漏例也不能绿。

## 下一PR

- 流式/滚动/搜索定位的黑盒机器断言（F5开工闸门）。
- A类34例逐条处置与替代分母，改冻结用例按另issue+评审流程。
- B类9例部署/profile矩阵脚本化，接入D类28例mock LLM编排；完整37候选双跑并对账72。

WS施工图见 [ws-message-map](../../docs/research/007_frontend/ws-message-map.md)。#42正式Chrome复跑见exp007 browser.mjs；本runner不会代替其IME/流式/岛门。
