---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R3
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正 + 关单（#1457 实现单落地）：原条目与 -R1 的核心陈述「7 方法 stub `CAPABILITY_UNAVAILABLE`、`describe_server` 不广告」随 #1457 失效——7 方法已接真实能力，capabilities 按 oracle `contract.capabilities` 逐字补齐 7 项。同时作废 -R2 的 ① 号陈述：真实栈 setPermissionMode **确有** requireSessionKey 门（createLocalGateway.ts:1375），-R2 ①「set_permission_mode 在 TS 侧不校验 sessionKey」来自注入假回调的探针（probe1 的 `hasActiveTurn` 为注入件，收到原始值即返回场景标志），不代表真实链路。并补记 -R2 ③ 的遗漏：真实 set/clear 链路 `reserveTranscriptWrite`（IPG.ts:2191-2209 包裹）与 bundle 内 `hasActiveTurn` 门**两者都在**，-R2 ③ 只登记了前者并把后者读成「没有」。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0。真实栈探针（`createLocalGateway({projectRoot, pilotHome, builtinSkillsRoot}).gateway`，非注入假回调；探针与完整输出存 `_tmp/20261106-issue-1457/ts-probe.mts` / `ts-probe.out`，矩阵见 `segment-1.md`）：

- setPermissionMode：缺 sessionKey → `INVALID_SESSION_KEY "sessionKey is required."`（createLocalGateway.ts:1375）；非法 mode → `INVALID_PERMISSION_MODE "Unsupported permission mode: admin"`（无句点）。
- sessionModelSet/Clear：set 成功返回 `{projectKey, sessionKey, saved, effective:{source:"session"}}`；transcript 追加 `session_metadata` 条目，`turnId:"model-selection"` / `"model-selection-clear"`，`metadata:{modelSelection, updatedAt}`；busy 报文 "Cannot change the model during an active turn."。
- oracle `contract.capabilities` 7 项逐字（顺序即广告顺序）：`session_model_set`、`session_model_clear`、`set_permission_mode`、`set_session_thinking`、`output_styles_list`、`set_output_style`、`seed_read_state`。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/config.rs`（#1457 后）：

- session_model_set / session_model_clear：`replacement.reserve_transcript_write` 包裹（IPG.ts:2193,2203 的对应物，所有出口释放）→ resolveProjectKey → hasActiveTurn 门 → selection 形状门 → validate_selection（model_catalog 目录校验：auto/router、provider/model、speed/reasoning capability）→ normalize → sessionKey 门（要求门序与实测一致：形状门先于 sessionKey 门）→ `record_model_selection`（AsyncTranscriptAdapter 写 `model-selection(-clear)` 条目）→ `owner.close` → set 返回 saved+effective，clear 返回 undefined（线上 null）。
- set_permission_mode：mode 白名单门（PERMISSION_MODES，`Unsupported permission mode: <String(mode)>` 无句点）→ raw-key busy 门 → sessionKey 门（R3 修正后的真实门序）→ `SESSION_CONFIG.overrides`（SessionConfigOverrides spread 语义）落 permissionMode。
- set_session_thinking：sessionKey → busy → validate_thinking（8 mode 白名单、budgetTokens 非负整数）→ `SESSION_CONFIG.thinking` 存/删 + `owner.close`。
- output_styles_list：resolveProjectKey → 插件样式枚举（ProjectRuntimeRegistry lease → PluginResources.runtime → refresh → create_view().list_output_styles()）→ styles 数组（name/description/plugin/source）+ selected 回显。
- set_output_style：sessionKey → busy → name 门（保留 -R2 ② 的三态：非串 `input.name.trim is not a function`、缺字段 `Cannot read properties of undefined (reading 'trim')`、空白 `name must be non-empty or null.`）→ 插件样式校验（`Unknown output style: <name>` 精确 UTF-16）→ `SESSION_CONFIG.output_styles` 存/删 + `owner.close` + `interactions.close_session("sdk_output_style_changed")`。
- seed_read_state：sessionKey → path → mtime（非负整数毫秒）→ busy → `sdkSessionConfig` 门（native 无该回调，答 unavailable）→ `owner.begin_maintenance` 栅栏 → get_or_create → `handle.seed_read_state`（经 `seed_agent_read_state` 走 read_file 的 resolve_workspace_path/stat/读文/`readFileState["<abs>::text::1::all::"]` 写入，即 TS seedReadState.ts 的逐行对应物）。
- `SESSION_CONFIG`：进程级静态存储（overrides/thinking/output_styles 三张表），对应 TS createLocalGateway 的 per-gateway `sessionOverrides` / `sdkThinkingOverrides` / `sdkSessionConfigs.outputStyle`。
- `requests.rs::describe_server` capabilities 在 `session_model_get` 后按 oracle 顺序逐字插入 7 项。

## 范围

同原条目 7 方法 + 能力清单 + 对应测试与负控夹具（`tests/fixtures/native-config/negative_controls.py` 重锚到新 config.rs）。-R2 ② 的非串参数 TypeError 形状已随本单对齐（`require_text` 走 TS `input.X?.trim()` 的裸 TypeError 报文）；-R2 ③ 的 busy 语义由 reserve + hasActiveTurn 双门对齐；-R2 ④ 的 lone-surrogate 放行经 `js_trim` 保持。-R2 前置于 #1604 的项（session_dir projectKey、tools.rs `required_trim` 迁移）不受本条影响。不涉及 proxy 分发、session runtime 生命周期与其他方法族；不放宽判分。

## 机器判据

`cargo test -p pilotdeck-gateway --lib native_process::methods::config`（10 项）：set/clear 的 transcript 副作用（`session_metadata` 条目 + modelSelection/mode 字段）、门序表（selection 形状先于 sessionKey、auto 无 router、未知 provider、mtime 边界、path/file 错误码、TypeError 形状、busy 报文）、样式往返（插件样式 description/plugin/source + selected 回显 + null 清除）、seed 应用/陈旧/越界。`management_tests.rs` 四处 capabilities 期望数组随能力清单同步。负控 `negative_controls.py` 26 条变异（7 条 stub-revert 各绑一个方法正测 + 门/报文/transcript/回显/seed 接线变异 + 2 条 session_dir 跨族保留）须全部 KILLED；锚点漂移报错退出。

## 批准记录

未批准，非已接受差异。2026-11-06，#1457 实现单落地后 AI 代登记关单。原条目 / -R1 登记的「未实现」由本单闭合；-R2 ① 依本条作废（其证据基础为注入假回调探针）；-R2 ③ 补记双门事实；-R2 ②④ 已对齐。人复核以持久决策指针登记。
