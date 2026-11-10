---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R13
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R12`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R12 同法，承接关系由文件名与正文表达）。
登记 #2068 实现单（分支 `claude/issue-2068-session-dir-projectkey`，基线
`d15ab7719` = #2720 合入后的 main；r1 交付已提交为 `e32966b5b`，fix-r1 以
final.patch 增量交付）落地后 session_dir 族其余 `projectKey` 读点的三处
答案格变化——R2:33 前置、R6:41 / R7:78 登记、R8:28 评审 r1 补记的「邻接旧形状」
之一（`config.rs` `resolve_project`）一并同改：

- 差异 ①（非串 `projectKey` 静默回落）：修前 `resolve_project`
  （`session_dir.rs:49` 旧形状，`config.rs:157` 同形拷贝）用
  `as_string().and_then(|s| s.to_utf8().ok()).unwrap_or_else(→ 配置项目)`，
  数字 / 布尔 / 数组 / 对象键**静默列出 / 改写已配置项目**；TS 侧所有消费链
  （`ProjectRuntimeRegistry.resolve` 与 `dialog.projects.resolveProjectKey`）的
  第一语句都是 Node `path.resolve`，非串键先答裸
  `TypeError [ERR_INVALID_ARG_TYPE]`。**fix-r1 勘误（评审 P1）**：该 TypeError
  自带 `code`，过 WS 保留 `ERR_INVALID_ARG_TYPE`（`GatewayWsConnection.ts:223`
  `hasStructuredErrorCode`；冻结 oracle `session-model/cases.json` 的
  `project-number` 即 TS WS 原件）——R9 登记的 `gateway_request_failed` 只覆盖
  无 `code` 的裸 TypeError，不适用此处，r1 交付曾误用该分层。修后两处
  `resolve_project` 共享 `methods/mod.rs` `resolve_project_key`：nullish →
  回落；字符串 → lossy 真键；非串 → `ERR_INVALID_ARG_TYPE` + Node `paths[0]`
  报文，且报文/拼写改由共享 `metadata::argument_received` +
  `invalid_arg_type_message` 构造（与 `session_model.rs` `path_argument` /
  `skills/mod.rs` `path_arg` / `management.rs` `validate_command_project` 等
  既有 `paths[0]` 端口同源，fix-r1 删除 r1 手写 Number/Bool/Array/Object 四
  臂）：`-0` 走 `dispatch::error_number`（`type number (-0)`，与
  `js_value::number_string` 的 `"0"` 分家）、对象 constructor 检视变体
  （`[Object]` / `an instance of null` / `an instance of ` 等，探针
  `probe-2068-r2.out` 实测同形）、constructor 检视自身抛错的 `Throws` 臂是
  无 `code` 的裸 TypeError/RangeError——这一类过 WS 才是
  `gateway_request_failed` + **裸** `error.message`（与 `management.rs` 的
  Throws 映射同形）。`serves_project` 检查不动：真键未服务仍
  `CAPABILITY_UNAVAILABLE` + 隔离报文。
- 差异 ②（非 UTF-8 串静默回落）：修前孤立代理项串被 `to_utf8().ok()` 折到
  配置项目（被 serving）；TS 把它当**真键**（`path.resolve` 收任意串）。
  修后 lossy 转换取真键待遇（#2083 对准入层同法），未服务即拒。
- 差异 ③（`session_new` 前缀门的形状）：issue 正文两处假设经实测**均证伪**——
  (a) 正文怀疑「BOM-only 拼出 `project=eff:` 前缀是错」：TS truthiness 正是
  这么拼（probe :5），修前 Rust 的 `is_empty` 过滤同形，**不是差异**；
  (b) 正文怀疑缺 `js_trim`：TS 该读点（`InProcessGateway.ts:2029`）是纯
  truthiness，连 trim 都没有。真差异是**非串真值键**（数字 7 / true）与
  **非 UTF-8 键**：修前 `as_string` 把它们静默丢前缀；TS 用 JS `String(value)`
  渲染进模板串（probe :3-:6：`project=7:` / 孤立代理项原样保留在
  sessionKey 里）。修后 `project_prefix_units`（`session_dir.rs`）按
  truthiness 分流（nullish / `""` / `false` / `0`/`NaN` → 无前缀），其余经共享
  `js_value::to_string_units_checked`（完整 JS `String()` 语义：ToPrimitive
  抛错面、数组 join、`[object Object]`、深度上限）渲染，**UTF-16 拼装**进
  sessionKey（`session_new`，channelKey 的 UTF-8 门保留为注册
  gate A7.8）。**fix-r1 勘误（评审 P2）**：ToPrimitive 抛错面过 WS 是
  `gateway_request_failed` + **裸** `error.message`（`GatewayWsConnection.ts:252`
  只取 `error.message`，无 `name:` 前缀；探针 `probe-2068-r2.out` 的
  `prefix-throwing-tostring` 格、#1604 q3/q7 探针、`runs.rs` `template_text`
  同口径）——r1 交付曾写成 `{name}: {message}` 前缀传播，已改。

issue 正文两处勘误随单登记：#2720 合入后行号漂移（`:48`→`:49`、`:191-197`→
`:192-198`，形状未变）；「`record_session_status` 共用 `resolve_project`」
不成立——`session_record_status` 直接读 `options.project`（TS
`recordAgentStatusMessage` 根本不读 projectKey），该法不在本单范围、零改动。

## TS 依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（`resources/PilotDeck`，
只读），Node v22.22.0 / ICU 78.2。真实 IPG 方法体 + #1455 式桩装配探针
`_tmp/20261110-issue-2068/probe/probe-2068.mjs`，原始输出同目录
`probe-2068.out`（24 条记录）：`newSession` 直调真实 `InProcessGateway`
（无 router 依赖）；`listSessions` 用真实 `SessionRouter` + 真实
`createGatewaySessionCatalogConsumer`，`resolveStorage` 一行镜像
`ProjectRuntimeRegistry.ts:1542` 的 `resolve(projectKey ?? fallbackProjectRoot)`；
delete/rename/tag 的前置用真实 `createDialogProjectRegistry.resolveProjectKey`
（`projectRegistry.ts:41-58`，起点 `resolve()`）；外加裸 `path.resolve` 对照
四条。fix-r1 补充探针 `probe-2068-r2.mjs` / `probe-2068-r2.out`（11 条记录）：
裸 `path.resolve` 的 `-0` / 对象 / constructor 检视变体格 + 前缀读点的
`String({toString: null})` 抛错面，每格都过 `GatewayWsConnection.ts:223-255`
的边界序列化判定（`error.code` 是 string → 原样保留；否则
`gateway_request_failed` + 裸 `error.message`）。关键行：

- `newSession`（`InProcessGateway.ts:2029` truthiness）：数字 7 →
  `probe-ch:project=7:s_…`（:3）；空白 `" "` → `project= :`（:4）；BOM-only →
  `project=eff:`（:5）；孤立代理项 → 前缀原样保留 `\ud800`（:6）；null / `""` →
  无前缀（:7-:8）。
- `listSessions`（`?? fallback` → `path.resolve`）：数字 7 → 裸
  `TypeError [ERR_INVALID_ARG_TYPE]: The "paths[0]" argument must be of type
  string. Received type number (7)`（:10）；空白 / BOM-only / 孤立代理项 /
  空串 → 都是**真键**：解析为项目根继续列出（:11-:13、:15）；null → 回退
  fallback（:14）；limit/cursor 透传对照证明装配是活的（:17）。
- delete/rename/tag 的前置 `resolveProjectKey`：数字 7 → 同一 `paths[0]`
  TypeError（:18）；空白 / BOM / 孤立代理项 / 空串 → `PROJECT_NOT_FOUND` /
  `Unknown projectKey: <原文>`（真键，未注册即拒，:19-:21、:23）；null → 回退
  （:22）。
- 裸 `path.resolve` 报文形状对照（:25-:31）：`type number (7)`（null 直调为
  `Received null`，但链路上 null 已被 `??` 拦下，不达 `resolve`）。

## Rust 行为

修前（基线 `d15ab7719`）：`resolve_project`（`session_dir.rs:49`，list :124 /
delete :235 / rename·tag :330 共用）与 `config.rs:157` 的同形拷贝
（session_model_set / session_model_clear / output_styles_list 共用）都是
`as_string + to_utf8().ok() + unwrap_or_else(→ 配置项目)`——非串键与非 UTF-8
串键两类都**静默回落已配置项目**（后者被 `serves_project` 放行）；
`session_new`（`:192-198` 旧形状）的 `project=` 前缀对非串真值键与非 UTF-8 键
静默丢弃。#1604 只收敛了 `close_project_sessions` 一处（该法走
`require_text` 家族的 `?.trim()` 门），其余读点保持旧形状——即本件差异 ①②③
的修前形态，#2564/#2272 已钉的 nullish 回落格（null / 缺键）修前修后同形。

修后（fix-r1 形状）：`methods/mod.rs` `resolve_project_key`（nullish →
`fallback.to_string_lossy()`；`String` → `from_utf16_lossy` 真键，#2083 同法；
非串统一走共享 `metadata::argument_received`：`Text` 臂 →
`ERR_INVALID_ARG_TYPE` + `invalid_arg_type_message("paths[0]", …)`，
`Throws` 臂 → `RequestError::new(裸 message)` 即边界形状
`gateway_request_failed` + 裸报文；null/undefined 的 `params` 先走
`input_property` 的 `Cannot read properties of …` TypeError；r1 的手写
`path_argument_type_error` 四臂已删）。`session_dir.rs` 与 `config.rs` 的
`resolve_project` 均改调它，`serves_project` 检查与
`CAPABILITY_UNAVAILABLE` 隔离报文原样。`session_dir.rs`
`project_prefix_units`：nullish / 空串 / `false` / `0`/`NaN` → 无前缀；
其余（含空白、BOM-only、孤立代理项串、非串真值）经
`js_value::to_string_units_checked` 渲染（ToPrimitive 抛错面以**裸**
`error.message` 的 `gateway_request_failed` 传播，fix-r1 改正 r1 的
`{name}: {message}` 前缀形状），UTF-16 拼进 `session_new` 的
`channelKey:s_<uuid>` 键——孤立代理项以原始
UTF-16 单元存活进 sessionKey，与 TS 模板串一致（probe :6）。

分层口径不变（#2083 已登记的层叠）：**真实入口**（`requests.rs:219-230`
准入）对「显式存在且非本配置项目拼写」的 projectKey 仍先答隔离拒绝——包括
非串键（admission 不认 `String(7)` 这类拼写），也互不冲突：本件定形的是
准入之下的**守卫层**形状，两层的非串答案不同（入口隔离拒绝 / 守卫
`paths[0]` TypeError），与 #1604 的 `close_project_sessions` 同一先例（其
TypeError 测试也走 `session_dir_request` 直调）。非串键的守卫层报文在
`session_config_request` 直调下同理可见（`config.rs` 用例）。fix-r1 补注
（评审 concurrency #2）：③ 的孤立代理项前缀（及空白 / BOM-only / 空串等一
切非配置项目拼写的显式字符串键）同样被入口准入先拒——`newSession` 的
非常规 projectKey 前缀**生产不可达**（真实入口下只成形于 nullish 回落与
配置项目拼写本身两途），前缀全形状的判定只发生在 `session_dir_request`
直调用例里，与上述 #1604 先例同一性质；入口拒答形状已由 #2090
`blank_key_gate_order` 钉格（`gateway_request_failed` + 隔离报文）。

## 范围

覆盖：`list_sessions` / `delete_session` / `rename_session` / `tag_session`
（经 `session_dir.rs` `resolve_project`）与 config 族三法（经 `config.rs`
`resolve_project`）的非串与非 UTF-8 `projectKey` 答案格，`new_session` 前缀
的 truthiness 全形状，及其机器判据（下节）。不覆盖：`record_session_status`
（issue 正文称其共用 `resolve_project`，实测不成立——直接读
`options.project`，TS 侧无 projectKey 读点，零改动）；`close_project_sessions`
（#1604 已收敛的 `require_text` 家族，本件零变化）；入口层准入（非串显式键
先答隔离拒绝，#2083/#2090 已登记的层叠，本件零变化）；R8:28 评审 r1 补记的
其余邻接旧形状——`management.rs` `admit_management_project` /
`snapshot_restore` 的 `targetProjectKey` 门（`as_string + to_utf8` 形状保留，
#2562 任务书已明示「P3 记录在案不在本单」）；fix-r1 起三处邻接读点已开
**#2882**（先探针后修）：`services.rs` submit 门（#2562 已定形的 nullish 读 +
自有域错误 `projectKey must be a valid path string.`，非 `path.resolve`
TypeError——submit 链 TS 侧语义未实测对拍）、`module_host.rs`
`public_skills_create_input` 的 `projectKey ?? runtime projectKey` 映射、
`config.rs` `seed_read_state` 的 `as_string`-only 读——本单探针的裸
`path.resolve(7)` 结论（`probe-2068.out` :25，`ERR_INVALID_ARG_TYPE` +
报文逐字）可作 #2882 的起始证据；`rename`/`tag` 的 `value` 字段
ToString 简化（number/bool only，非 projectKey 读点，#1457 fix-r2 已登记
族）；数字渲染的 JS Number#toString 边角（`1e21` 等）走共享
`js_value::number_string`，本件不重复登记。不放宽判分。

## 机器判据

行号为工作树（HEAD `e32966b5b` + fix-r1 final.patch）上的行号：

- 用例（`session_dir.rs` tests 模块共五个——r1 四个 + fix-r1 一个）：
  `list_sessions_non_string_project_key_answers_the_path_resolve_type_error`
  （`:1815`——fix-r1 扩格：number / `-0` / boolean / array / object 与
  constructor 检视变体（`[Object]` / `an instance of null` /
  `an instance of `）八格断 `ERR_INVALID_ARG_TYPE` + Node 报文逐字，另
  两格（`{"constructor":5}` / `constructor.name` 不可转换）断 description
  phase 抛错的 `gateway_request_failed` + 裸报文）、
  `list_sessions_string_project_keys_are_real_keys_not_the_fallback`
  （`:1899`——空白 / BOM-only / 孤立代理项三格断 `CAPABILITY_UNAVAILABLE` +
  隔离报文，null 对照格回落配置项目）、
  `new_session_project_key_prefix_follows_ts_truthiness`（`:1947`——truthy
  四格前缀逐字（含 BOM），孤立代理项以原始 UTF-16 单元断言，falsy 五格
  null/`""`/`0`/`NaN`/`false` 无前缀，fix-r1 补 NaN 格）、
  `new_session_throwing_project_key_prefix_answers_the_boundary_typeerror`
  （`:2035`——fix-r1 新增：`{"toString": null}` 键断 `gateway_request_failed`
  + **裸** `Cannot convert object to primitive value`）；`config.rs` tests
  模块一个：`session_model_set_project_key_read_follows_the_ts_chain`
  （`:3355`——数字 7 断 `ERR_INVALID_ARG_TYPE` + 报文逐字，孤立代理项断
  `CAPABILITY_UNAVAILABLE` + 隔离报文）。
- 绿跑（worker 树，HEAD `e32966b5b` + 未提交改动）：
  `cargo test -p pilotdeck-gateway --lib -- native_process::methods::config::tests
  native_process::methods::session_dir::tests` r1 后 68 passed → fix-r1 后
  **69 passed; 0 failed**（`fix-r2-after-sessiondir-config.out`）；全 lib 套件
  与 fmt / clippy 见本轮 RESULT（fix-r2- 前缀原始输出）。
- 负控（`tests/fixtures/native-config/negative_controls.py`：r1 新增七行 +
  六 pin，fix-r1 增改后 #2068 族共十行）：差异 ①② 的
  `project-key-nonstring-folded` / `project-key-utf8-folded`（METHODS_MOD
  ——共享读的非串臂整体折回 fallback / String 臂折回 `to_utf8().ok()` 链，
  绑 session_dir 两用例）与 `config-project-key-nonstring-folded` /
  `config-project-key-utf8-folded`（同码点绑 config 用例，#2564 一码点多绑
  先例）；fix-r1 新增 `project-key-object-folded`（Object 臂单独折回——
  评审 Object 臂覆盖缺口）与 `project-key-code-reverted`（code 字面量回退
  `gateway_request_failed`，绑 ERR_INVALID_ARG_TYPE code 断言格——评审
  P1 的直接负控）；差异 ③ 的 `new-session-prefix-trim-gate`（issue 正文
  原假设的 trim 门形状）、`new-session-prefix-number-dropped`（Number 臂
  恒丢前缀）、`new-session-prefix-surrogate-stripped`（渲染 lossy 化，
  孤立代理项变 U+FFFD）；fix-r1 新增 `new-session-prefix-throwing-prefixed`
  （裸 message 回退 `{name}: {message}` 前缀形状，绑 throwing-object 用例）
  并同步 `nonstring` 两行与 `surrogate-stripped` 行的变异锚到共享读新形状。
  `--check` → `#1457 80 mutation anchors OK; #2065 67 literal anchors OK`
  （77+3 行 / 66+1 pin，r1 的 `boundary TypeError code` pin 更名
  `Node TypeError code`）；`test_literal_anchors.py` 11 用例全绿。十行在
  沙箱逐行验证：apply → `cargo test <bound test> -- --exact` → 真
  `negative_suite.classify` 判 KILLED → 还原 → 字节复核（脚本
  `negctl-verify-2068-r2.py`，逐行输出 `fix-r2-negctl-verify.out`，per-mutant
  cargo 日志在 `negctl-2068-r2-samples/`）：**10/10 全 KILLED，
  failures=0 of 10**；**正式整表 runner 因「工作树 ≠ HEAD 且沙箱不提交」
  不可跑**（`mutating_sources` 拒脏树），由协调者提交后宿主跑，与 #2562
  同批注记。
- 账本闸门：`diffs/` 只追加（本件为新增文件，R2 及更早字节未动），
  `check_frozen.py` 的 `check_diffs` 规则要求本件 frontmatter `original`
  写基础 ID（已写 `-001`，非 `-R12`）；fix-r1 轮 `check_frozen.py --base
  origin/main` PASS（30 entries）。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-10，#2068 实现单落地后
AI 代登记。三处差异均为对齐性修复（非串 / 非 UTF-8 键不再静默回落配置项目、
前缀按 TS truthiness 渲染），不新增豁免、不改判分语义、不改 #2564/#2272
已钉的 nullish 回落格与 #2083 已登记的入口层叠。人复核若否决（如裁定守卫层
非串答案应与入口层统一为隔离拒绝），循账本规则出 `-R14` 更正。2026-11-11
fix-r1（r1 评审 P1/P2 簇）同轮更正本件：非串键 code 误写
`gateway_request_failed`（实为过 WS 保留的 `ERR_INVALID_ARG_TYPE`）、
ToString 抛错面误写 `{name}: {message}` 前缀（实为裸 `error.message`）、
相应机器判据/负控同步，并在「分层口径不变」补 ③ 的生产不可达注记、
「范围」把三处邻接读点点名 #2882——差异本体（nullish 回落 + 真键 + TS
truthiness）自 r1 未变。
