# U7 settings 写与重载金样（#1318）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU 78.2
（生成器断言 `process.version === 'v22.22.0'` 与 `Intl.Collator` 解析为 `en-US`，
须 `LANG=en_US.UTF-8`）。覆盖 `apply_flag_settings` / `update_settings` /
`reload_output_styles` / `reload_config` / `reload_extensions` 的参数校验序、
能力门、WS 错误帧投影（`{code,message}`，无 code 的原始抛错 →
`gateway_request_failed`）、事件流与会话 / registry / store 后置状态。

生成器加载真实三层栈：真实 `SessionRouter`（真实 beginTurn/close 与
`asAgentHandle` 会话，key `f` 的 dispose 以真实 rejection 走 close-fault 格）、
真实 `InProcessGateway`（仅 interactionCoordinator 在原型链上包一层记录
close-session 原因串，#1215 模式）、真实 `GatewayWsConnection`（假文本传输）、
真实 `ProjectRuntimeRegistry.prototype` 的 `applyFlagSettingsForSdk` /
`reloadOutputStylesForSdk`（真 Map 状态；resolve / pluginRuntime 为桩）、真实
`GatewayRuntimeRefreshBundle`（录音协作者；configStore delegate 始终指向当前场景
树的真实 `createPilotConfigStoreSync` store）、真实 `updatePilotLocalSettings`
写盘（每场景树一套 scratch home/project，行间按需复用，如 `t1` 被
update-merges / rewrite-same-value / clear-value / noop 四行共用）。`updateSettings`
闭包体为 `createLocalGateway.ts:1384-1404` 逐字副本（createLocalGateway 本身
要起整个 CLI，离线不可运行）；`createLocalGateway.ts` 计入 sourceSha256，
闭包任何编辑都会改变 oracle 的 provenance。

`oracle.json` 全由生成器输出，不手写期望：54 个 case 行（每行 `{id, method,
params, setup, events, response, post}`）+ `assemblyCases.capability`（裸组装的
能力门答案）+ `setupKeys`。金样的方法范围与产品自有方法集不是一回事：金样只覆盖
生成器 `generate.mts` 实际驱动的 TS 侧方法（`assemblyCases.capability` 同范围），
产品方法集的唯一来源是 `src/pilotdeck-gateway/src/runtime/settings/gateway.rs`
的 `METHODS`；`METHODS` 里不在金样中的方法（如 `resolve_settings`）由 #1320 的
native 路由覆盖，本文件不重复计数。update 系行写真实 YAML（`agent` flow map
按 yaml 包规范空格形重排），事件含 `store-event:` / `log:` / `sdk-config-change:`
/ `broadcast:config_changed:` / `mark-*-dirty:` / `registry-reload`。无时钟 /
PID / 路径进 oracle（scratch 路径不录制；changedPaths 与事件均为内容函数）。
金样在本机 macOS 与 cb8（Linux）各以同基线重生成，逐字节一致。

复现（从仓库根，输出目录必须为新目录）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/settings-gateway/generate.mts /tmp/settings-golden-1
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/settings-gateway/generate.mts /tmp/settings-golden-2
cmp /tmp/settings-golden-1/oracle.json /tmp/settings-golden-2/oracle.json
cmp /tmp/settings-golden-1/oracle.json \
  src/pilotdeck-gateway/tests/fixtures/settings-gateway/oracle.json
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::settings
```

Rust 重放（`runtime/settings/gateway_tests.rs`）逐行执行同一 setup 词表，经
`SettingsGateway::dispatch` 重建 WS 帧，比对 response / events / post 三格
（JSON 结构等价：对象键序经 serde 归一，不钉键序字节）；
`#[ignore]` 的 `settings_gateway_fixture_regenerates_identically_from_ts` 为
重生成守卫（`PDRS_BASELINE` 下实跑）。

负控（独占本 worktree，不与任何编译或写入并发；七个变异的目标文件在本分支
新增，宿主提交后由 `mutating_sources` 在 HEAD 字节上执行；基线 ref 先解析成
完整 SHA，记录与字节校验用同一对象；沙箱验证轮用
`PDRS_NEGATIVE_BASELINE=<git stash create 的悬空提交>` 钉住评审树，实际基线
SHA 与来源写进每条 summary 记录、`baseline.json` 与 `baseline-replay.log`
首行，非 HEAD 基线显式打 `NON-HEAD BASELINE` 且即使全杀也以专属码 `42`
退出、stdout 与 `baseline.json` 标 `NON-HOST EVIDENCE` / `hostEvidence:
false`（宿主口径只认 HEAD，杀失败仍优先 `1`）；该逃生口的冷/暖 scanner target 两态统一见
上级 [`fixtures/README.md`](../README.md)「共享输入扫描器」节，#2305 条 5 同步）：

```sh
CARGO_TARGET_DIR=… python3 \
  src/pilotdeck-gateway/tests/fixtures/settings-gateway/negative_controls.py \
  _tmp/20261026-issue-1318/negative-controls <可选变异名子集>
```

每次仅临时改一处产品实现，要求被点名测试以自己的 `#1318` panic 消息失败。
变异与判杀标记的唯一来源是 `negative_controls.py`：`mutations` 表每行给出变异名、
目标文件、锚点原文、变异体与判杀测试，`MARKERS` 表给出变异名到 panic 标记的映射；
`case <id>` 形式的标记另由脚本启动断言钉到真实金样行。脚本启动时对两张表做双向
覆盖、标记唯一性与分母 / 名册校验，所以本文件不复制清单，也不按插入顺序指代变异
（要指某一个变异就写它的名字）。

两种标记形态的强度不同，别混用：

- **单断言标记**（如 `#1318-r1 update-drop: …`、`#1318-r2 extensions-truthiness: …`）——
  绑定到那一条断言。要求单行、非 `assertion` / `assertion \`left == right\`` 前缀、
  非裸 `"<case>":`、非 `: step` 结尾、非 watchdog/timeout 前缀；判据实现只有一份
  （`tests/fixtures/negative_scope.py`，`negative_panic` 与 `_negkit` 共用）。
- **行级标记**（`case <id>`）—— 绑定到金样的一行（一个场景），不是该行里的某一条断言。
  它比单断言弱，但 oracle 回放本身按行比对 response/events/post 三格，行级绑定是该测试
  能给的最强形式；启动期还会核 `<id>` 真的在 `oracle.json` 里。要更强就把标记换成该行
  实际触发的那条 panic 消息。

标记还不能取自判杀条件自身的固定 token：判定同时要求 `'#1318' in evidence`，所以
`#1318` / `#13` 这类是 `'#1318'` 子串的标记会让标记检查恒真，启动期直接拒绝。

`assert` 在 `python3 -O` / `PYTHONOPTIMIZE=1` 下会被整条剥离，本脚本的启动守卫因此全部
写成普通 `if … : refuse(…)` / `raise`，一个都不依赖 `assert`；`negative_controls.py` 自测
（`settings-gateway/test_negative_controls.py`）在 `-O` 下逐个复算这些拒绝路径。
编译错误、信号退出、锚点过期（出现次数 ≠ 1）都不算杀死，直接报错退出。
每轮 finally 恢复原始源码并核 SHA-256；输出每条补丁、原始日志、失败摘要，
结束后应重跑正常测试和 fmt/clippy。

退出码与证据（宿主按此分诊；`complete` 说明这一轮是否跑完，`selectedTotal` / `selected`
说明跑的是不是全量——子集运行同样 `complete: true`）：

| 退出码 | 含义 |
|---|---|
| `0` | 本次选中的变异全部 KILLED（`summary.json` 此时 `complete: true`） |
| `1` | 开跑过但没得到全杀结论。三种形态：① 基线重放不通过——`baseline.json` + `baseline-replay.log`，**没有** `summary.json`；② 有变异 SURVIVED / INVALID——`summary.json` 在，`complete: true`；③ 跑动中被打断或收尾恢复校验失败——`summary.json` 在，`complete: false`（残卷） |
| `2` | 从未真正开跑，**本轮没有写过任何证据**：变异名非法、输出目录已存在、缺 `CARGO_TARGET_DIR`、基线无法解析、`oracle.json` 读不了或结构不对、树不干净 / 领锁超时（`mutating_sources` 进入失败，`.cargo-slots.sh` 等待超过 30 分钟同理）、marker 不指向真实 oracle 行、某个 marker 值为空 / 不指向单一断言 / 与另一个变异共用 / 取自判杀条件自身的固定 token（`#1318`）、表被清空 / 增删行（静态分母 `EXPECTED_TOTAL = 7` 与冻结名册 `EXPECTED_NAMES`）、`MARKERS` 与变异表不互相覆盖 |
| `42` | 全杀但基线不是 HEAD，非宿主证据 |

`2` 的判据是「第一次写盘之前就返回」：`mutating_sources` 的进入失败（树不干净、领锁超时）
与 oracle 读取都在第一次变异之前，所以它们是`2`而不是`1`——`1` 表示**确实跑过**。唯一的例外是
「输出目录已存在」：它可能紧挨着**别的运行**留下的文件（见下）。同理，基线重放**期间**被打断
属于「跑过」，此时只有 `baseline-replay.log`（末尾写明 `interrupted before the baseline replay
produced output`，或就是被打断那一刻的日志），`baseline.json` / `summary.json` 都不在。

「输出目录已存在」是唯一会带着**别的运行**留下的文件的 `2`：`runId` 是「基线前缀 + 随机
nonce」，本轮没有 nonce 可对，所以目录里任何文件都不是本轮的，别拿它当结论。那些文件
可能属于更早的一轮，也可能是**并发**另一轮正在写（路径撞车）；本轮一个字都没写。反过来，
一次 `2` 拒绝会留下空目录，用同一路径重跑仍会撞「目录已存在」——那时目录里没有文件，
换新目录即可。其余 `2` 都在第一次写盘之前返回，目录是空的。

被信号直接杀死（SIGINT 130 / SIGTERM 143）不在本表：`summary.json` 停在最后一次原子发布的
状态，`complete` 是 `false` 就说明这一轮没跑完。`SIGKILL`（137）同理，连收尾都来不及做。

`summary.json` 是对象：`runId`（与 `baseline.json` 同代）、`registeredTotal`、
`selectedTotal`、`complete`、`counts`、`results`；跑动中每轮都会原子重写一次且
`complete: false`，轮询读者必须看到 `complete: true` 才算一份完整结论。

`complete: true` 只在 `mutating_sources` 块正常退出**之后**才发布，也就是只在它做完
权威的恢复 + 逐字节校验之后。这条顺序是硬约束：在块内先发绿再「撤回」的写法（fix-r3–r5）
被两家评审推翻——撤回本身可能被第二次信号打断，绿结论就留在脏树上。现在的形态下，
块返回之前盘上最新的记录永远是 `complete: false`，没有可撤回的东西。
代价是相反的窗口：树锁释放到发布之间还有几个系统调用，另一个运行可能已开始变异；
两种顺序不可兼得，这里选「宁晚不假」。因此这条不变式**只覆盖本轮**：进程退出时，
「目录里是 `complete: true`」意味着**本轮**在锁内做的恢复 + 逐字节校验已经通过
（`mutating_sources` 保证），不表示「此刻树上没有任何变异体」——锁释放后并发另一轮
可能正在变异，那是那一轮自己的责任。运行期间按目录轮询的读者应以「进程已退出」为前提，
退出码仍是本脚本的第一判据。
失败收尾只做一件事：把残卷标成 `complete: false`；连这次改写都失败（例如磁盘满）时，
它会删掉 `summary.json` 并在 stderr 说明。
`baseline.json` 记同一 `runId`、基线 SHA 与来源、`registeredTotal` /
`selectedTotal` / `selected`（子集运行不再与全量同形）。
所有证据文件（`summary.json` / `baseline.json` / `baseline-replay.log` / 逐变异 `.patch` / `.log`）
一律 `.tmp` + `rename` 原子发布。

native 侧已接线（#1320）：`native_process/methods/settings.rs` 的家族表
（`METHODS` 直接 re-export 本组件的常量）与 `Requests::settings_request`、
`native_process/services.rs` 的 `native_settings` 组合都路由进本组件的 `dispatch`，
金样回放见 `native_process/settings_tests.rs`；仍未接的是 `dispatch.rs` 的
`LOCAL_METHODS` 一侧。registry 的 `applyFlagSettingsForSdk` /
`reloadOutputStylesForSdk` 与 `updatePilotLocalSettings` 生产写盘件仍为镜像端口
（`MirrorWriter` 保持同一 allowlist / YAML 编辑语义），随接线单评估替换。
已知未收敛差异：
`reload_extensions` 的 `projectKey` 分支按原值 JS 真值选择（fix-r2，
`0` / `false` / `NaN` / `""` 与 TS 同走全量分支；实测
`_tmp/20261026-issue-1318/fix-r2/probe-extensions-truthiness.out`，fix-r3
又补测 `[""]` / `[null]` 两格：
`_tmp/20261026-issue-1318/fix-r3/probe-extensions-truthiness-r3.out`）；真值
非字符串 `projectKey` / `changedPaths` 元素仍被 `String(value)` 强转而 TS
原值透传，`[]` / `[""]` / `[null]`（真值但 `String()` 为空串的数组格族）
仍走全量分支而 TS 走项目 no-op（金样 54 行均为字符串，不触发；
`changedPaths:[1]` / `projectKey:5` 两格随 #1320 接线补入），登记于
`specs/gateway-parity/diffs/U7-EXTENSIONS-RELOAD-STRING-COERCION-001.md`，
前置于 #1320。
