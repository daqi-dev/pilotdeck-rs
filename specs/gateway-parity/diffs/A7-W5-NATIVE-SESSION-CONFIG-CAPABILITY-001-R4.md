---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R4
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正 + 收窄：-R3 登记「7 方法已接真实能力」，其中 `set_permission_mode`、`set_session_thinking`、`set_output_style` 的写入落在进程级静态表里，而 native 进程没有任何读方——`assembly.rs` 创建会话时仍写死 `permissionMode: "default"`，`SdkSessionRegistry` 只在测试里构造。三个方法因此只写不读，`applied: true` 是假阳性。本更正把这三个方法改为「保留 TS 门序与报文，校验通过后如实回 `CAPABILITY_UNAVAILABLE`」，并删掉那张无人读的静态表；不放宽判分。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0。真实栈探针 `_tmp/20261106-issue-1457/ts-probe.mts`（原始输出 `ts-probe.out` / `ts-probe.err`、矩阵 `segment-1.md`）：`createLocalGateway.ts:1371-1382` 的 `setPermissionMode`、`:1403-1409` 的 `setSessionThinking` / `setOutputStyle` 回调把选择写进 per-gateway `registry`（`sessionOverrides`、`sdkThinkingOverrides`、`sdkSessionConfigs.outputStyle`），下一次 `createSession` / `query` 读回并生效；`InProcessGateway.ts:2321-2329`、`:2364-2372`、`:2379-2385` 是这些回调前的线序门与 busy 门。写与读在 TS 侧同属一个 registry 实例。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/config.rs`（fix-r1 后）：

- `set_permission_mode`：mode 白名单门（`Unsupported permission mode: <String(mode)>` 无句点）→ raw-key busy 门 → sessionKey 门 → `CAPABILITY_UNAVAILABLE`「Native gateway method set_permission_mode is not configured.」。
- `set_session_thinking`：sessionKey → busy → `validate_thinking`（8 mode 白名单、budgetTokens 非负整数）→ 同上不可用；不再落表、不再 `owner.close`。
- `set_output_style`：sessionKey → busy → name 三态门（非串 `input.name.trim is not a function`、缺字段 `Cannot read properties of undefined (reading 'trim')`、空白 `name must be non-empty or null.`）→ 插件样式校验（`Unknown output style: <name>` 精确 UTF-16）→ 同上不可用；不再落表、不再 `owner.close` / `close_session`。
- `output_styles_list`：仍枚举项目插件样式（name/description/plugin/source），不再回显 `selected`（无人能写该选择）。
- 进程级 `SESSION_CONFIG` 静态表删除。不可用报文由 `session_config_unavailable(method)` 单点生成，与既有 `CAPABILITY_UNAVAILABLE` 报文形状一致。

依据：native 进程未组装 session registry（`runtime/sdk_session/registry.rs` 无生产构造点），`native_process/assembly.rs` 的会话 config 写死 `permissionMode: "default"`；把选择写进本地表不会有任何读方，属功能缺失而非收窄。

## 范围

三个选择器方法与一个列表方法的 `selected` 回显。`session_model_set` / `session_model_clear`（转录条目 + saved/effective）与 `seed_read_state`（owner 维护栅栏 + 工具检查点）仍是真实接线，不在本条范围；能力清单 7 项保留（方法已注册、按 TS 报文校验与报错），不因本条回退。真正接线（native 侧组装 session registry 并把覆盖读进下一回合）转独立跟进单。

## 机器判据

`cargo test -p pilotdeck-gateway --lib native_process::methods::config`：`set_permission_mode_validates_then_reports_unavailable`（每种 TS mode 都得到 `CAPABILITY_UNAVAILABLE` + 精确报文，未知/缺失/非串 mode 仍按 TS 报文拒绝）、`set_session_thinking_validates_then_reports_unavailable`（mode 对象与显式 null 都如实不可用，非法 thinking 仍按 `INVALID_THINKING_CONFIG` 拒绝）、`output_styles_list_reports_project_styles`（插件样式仍枚举，`selected` 缺席）、`set_output_style_validates_names_and_key`（三类 name 门与未知样式仍在）。负控 `src/pilotdeck-gateway/tests/fixtures/native-config/negative_controls.py` 同步重锚：stub-revert 与报文变异按新报文断言判死，逐条 KILLED，锚点漂移报错退出。

## 批准记录

未批准，非已接受差异。2026-11-06 #1457 fix-r1 由 AI 代登记：四条独立评审来源同时指认「`SESSION_CONFIG` 只写不读、`applied: true` 假阳性」，按问题分级当轮修正；真实接线转独立跟进单，待人复核。
