---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R13
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R12`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R12 同法，承接关系由文件名与正文表达）。
登记 #2068 实现单（分支 `issue-2068`，基线 `d15ab7719` = #2720 合入后的 main，
未提交、以 final.patch 交付）落地后 session_dir 族其余 `projectKey` 读点的三处
答案格变化——R2:33 前置、R6:41 / R7:78 登记、R8:28 评审 r1 补记的「邻接旧形状」
之一（`config.rs` `resolve_project`）一并同改：

- 差异 ①（非串 `projectKey` 静默回落）：修前 `resolve_project`
  （`session_dir.rs:49` 旧形状，`config.rs:157` 同形拷贝）用
  `as_string().and_then(|s| s.to_utf8().ok()).unwrap_or_else(→ 配置项目)`，
  数字 / 布尔 / 数组 / 对象键**静默列出 / 改写已配置项目**；TS 侧所有消费链
  （`ProjectRuntimeRegistry.resolve` 与 `dialog.projects.resolveProjectKey`）的
  第一语句都是 Node `path.resolve`，非串键先答裸
  `TypeError [ERR_INVALID_ARG_TYPE]`（过 WS 变 `gateway_request_failed`，
  R9 已实测的既有分层事实）。修后两处 `resolve_project` 共享
  `methods/mod.rs:204` `resolve_project_key`：nullish → 回落；字符串 → lossy
  真键；非串 → `gateway_request_failed` + Node `paths[0]` 报文逐字
  （`type number (7)` / `type boolean (true)` / `an instance of Array` /
  `an instance of Object`，数字走共享 `js_value::number_string` 保持 JS
  Number#toString 拼法）。`serves_project` 检查不动：真键未服务仍
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
  sessionKey 里）。修后 `project_prefix_units`（`session_dir.rs:93`）按
  truthiness 分流（nullish / `""` / `false` / `0`/`NaN` → 无前缀），其余经共享
  `js_value::to_string_units_checked`（完整 JS `String()` 语义：ToPrimitive
  抛错面、数组 join、`[object Object]`、深度上限）渲染，**UTF-16 拼装**进
  sessionKey（`session_new` `:213-237`，channelKey 的 UTF-8 门保留为注册
  gate A7.8）。

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
四条。关键行：

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

修后：`methods/mod.rs:188` `path_argument_type_error`（Node
`ERR_INVALID_ARG_TYPE` 报文逐字）+ `:204` `resolve_project_key`（nullish →
`fallback.to_string_lossy()`；`String` → `from_utf16_lossy` 真键，#2083 同法；
`Number`/`Bool`/`Array`/`Object` → `gateway_request_failed` + 报文逐字，
null/undefined 的 `params` 先走 `input_property` 的
`Cannot read properties of …` TypeError）。`session_dir.rs:49` 与
`config.rs:157` 的 `resolve_project` 均改调它，`serves_project` 检查与
`CAPABILITY_UNAVAILABLE` 隔离报文原样。`session_dir.rs:93`
`project_prefix_units`：nullish / 空串 / `false` / `0`/`NaN` → 无前缀；
其余（含空白、BOM-only、孤立代理项串、非串真值）经
`js_value::to_string_units_checked` 渲染（ToPrimitive 抛错面以
`{name}: {message}` 的 `gateway_request_failed` 传播），UTF-16 拼进
`session_new`（`:213-237`）的 `channelKey:s_<uuid>` 键——孤立代理项以原始
UTF-16 单元存活进 sessionKey，与 TS 模板串一致（probe :6）。

分层口径不变（#2083 已登记的层叠）：**真实入口**（`requests.rs:219-230`
准入）对「显式存在且非本配置项目拼写」的 projectKey 仍先答隔离拒绝——包括
非串键（admission 不认 `String(7)` 这类拼写），也互不冲突：本件定形的是
准入之下的**守卫层**形状，两层的非串答案不同（入口隔离拒绝 / 守卫
`paths[0]` TypeError），与 #1604 的 `close_project_sessions` 同一先例（其
TypeError 测试也走 `session_dir_request` 直调）。非串键的守卫层报文在
`session_config_request` 直调下同理可见（`config.rs` 用例）。

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
#2562 任务书已明示「P3 记录在案不在本单」）与 `services.rs` submit 门的
`to_utf8` 错误格（#2562 已定形的 nullish 读 + 自有域错误
`projectKey must be a valid path string.`，非 `path.resolve` TypeError——
submit 链 TS 侧语义未实测对拍，收口另单）；`rename`/`tag` 的 `value` 字段
ToString 简化（number/bool only，非 projectKey 读点，#1457 fix-r2 已登记
族）；数字渲染的 JS Number#toString 边角（`1e21` 等）走共享
`js_value::number_string`，本件不重复登记。不放宽判分。

## 机器判据

行号为工作树（基线 `d15ab7719` + final.patch）上的行号：

- 用例（`session_dir.rs` tests 模块新增四个）：
  `list_sessions_non_string_project_key_answers_the_path_resolve_type_error`
  （`:1805`——number/boolean/array 三格断 `gateway_request_failed` + Node
  报文逐字）、`list_sessions_string_project_keys_are_real_keys_not_the_fallback`
  （`:1839`——空白 / BOM-only / 孤立代理项三格断 `CAPABILITY_UNAVAILABLE` +
  隔离报文，null 对照格回落配置项目）、
  `new_session_project_key_prefix_follows_ts_truthiness`（`:1887`——truthy
  四格前缀逐字（含 BOM），孤立代理项以原始 UTF-16 单元断言，falsy 四格
  null/`""`/`0`/`false` 无前缀）；`config.rs` tests 模块新增一个：
  `session_model_set_project_key_read_follows_the_ts_chain`（`:3351`——
  数字 7 断 `gateway_request_failed` + 报文逐字，孤立代理项断
  `CAPABILITY_UNAVAILABLE` + 隔离报文）。
- 绿跑（worker 树，HEAD `d15ab7719` + 未提交改动）：
  `cargo test -p pilotdeck-gateway --lib -- native_process::methods::config::tests
  native_process::methods::session_dir::tests` 基线 64 passed → 修后
  **68 passed; 0 failed**（`after-fix-sessiondir-config.out`）；全 lib 套件
  `5346 passed / 4 failed`——四例全为环境性（EADDRINUSE 18973/18990 被他
  worktree 占用 + 满并发时序敏感例），孤立复跑均过（`full-lib-tests.out` /
  `rerun-env-failures.out`）。fmt --check 与 clippy --all-targets -D warnings
  无输出（交付前自查）。
- 负控（`tests/fixtures/native-config/negative_controls.py` 新增七行 + 六
  pin）：差异 ①② 的 `project-key-nonstring-folded` / `project-key-utf8-folded`
  （METHODS_MOD——共享读的 Number 臂折回 fallback / String 臂折回
  `to_utf8().ok()` 链，绑 session_dir 两用例）与 `config-project-key-
  nonstring-folded` / `config-project-key-utf8-folded`（同码点绑 config
  用例，#2564 一码点多绑先例）；差异 ③ 的 `new-session-prefix-trim-gate`
  （issue 正文原假设的 trim 门形状）、`new-session-prefix-number-dropped`
  （Number 臂恒丢前缀）、`new-session-prefix-surrogate-stripped`（渲染
  lossy 化，孤立代理项变 U+FFFD）。`--check` → `#1457 77 mutation anchors
  OK; #2065 66 literal anchors OK`（70+7 行 / 60+6 pin）；
  `test_literal_anchors.py` 11 用例全绿。七行在沙箱逐行验证：apply →
  `cargo test <bound test> -- --exact` → 真 `negative_suite.classify` 判
  KILLED → 还原 → 字节复核（脚本 `negctl-verify-2068.py`，逐行输出与
  per-mutant cargo 日志在 `negctl-2068-samples/`）；**正式整表 runner 因
  「工作树 ≠ HEAD 且沙箱不提交」不可跑**（`mutating_sources` 拒脏树），
  由协调者提交后宿主跑，与 #2562 同批注记。
- 账本闸门：`diffs/` 只追加（本件为新增文件，R2 及更早字节未动），
  `check_frozen.py` 的 `check_diffs` 规则要求本件 frontmatter `original`
  写基础 ID（已写 `-001`，非 `-R12`）。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-10，#2068 实现单落地后
AI 代登记。三处差异均为对齐性修复（非串 / 非 UTF-8 键不再静默回落配置项目、
前缀按 TS truthiness 渲染），不新增豁免、不改判分语义、不改 #2564/#2272
已钉的 nullish 回落格与 #2083 已登记的入口层叠。人复核若否决（如裁定守卫层
非串答案应与入口层统一为隔离拒绝），循账本规则出 `-R14` 更正。
