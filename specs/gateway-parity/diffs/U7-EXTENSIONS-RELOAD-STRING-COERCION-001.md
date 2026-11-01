---
id: U7-EXTENSIONS-RELOAD-STRING-COERCION-001
---

## 类别

已知未收敛差异（U7 settings 网关 `reload_extensions` 入参非字符串原值的强制转字符串）。

## 基线依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0 / ICU 78.2，非作者评审
（#1318 fix-r1 评审 P2-1）以只读探针实测真实
`src/cli/GatewayRuntimeRefreshBundle.ts` 的 `reloadExtensions`：

- `changedPaths: [1, true, null]` → 结果与广播 `{"reloaded":true,"changedPaths":[1,true,null]}`
  （原值透传，`null` 保持 `null` 而非空串）；
- `projectKey: 5` → TS 以数值 5 调 `invalidate(5)` / `markProjectDirty(5)`
  （字符串键表上等价 no-op），日志走 `changedPaths.join(", ")` 自身的字符串化。

原始输出见 `_tmp/20261026-issue-1318/fix-r1/review.md` P2-1 引文（评审探针，
本沙箱未复跑；金样 `tests/fixtures/settings-gateway/oracle.json` 的 54 行均为
字符串 `projectKey` / `changedPaths`，不覆盖非字符串格）。

## Rust 行为

`src/pilotdeck-gateway/src/runtime/settings/gateway.rs` 的 `extensions_reload`
（`String(value)` 强转）：`changedPaths: [1, true, null]` → 响应 / 广播 / registry
副作用三处均为 `["1", "true", ""]`；`projectKey: 5` → 转成 `"5"`，会真的 retire
名字为 `5` 的项目运行时并置 dirty。日志文本两侧一致（`join` 后同为字符串化）。
类型根源：`runtime/refresh.rs:189-192` 的 `ExtensionsReload { project_key:
Option<JsString>, changed_paths: Option<Vec<JsString>> }`。

## 范围

仅 `reload_extensions` 的非字符串 `projectKey` / `changedPaths` 元素（合法 JSON
帧可达）。金样的 54 行均为字符串，不触发本差异；其余四方法与字符串路径不受影响。
修复需要 `ExtensionsReload` / `ConfigChangeNotice` / `ReloadOutcome` 携带 `JsValue`
并级联 refresh 车道端口（`RefreshRegistry::invalidate`、`RefreshRouter::
mark_project_dirty`）与 session 通知消费者，跨接线边界，归 #1320 接线单评估
（前置于 #1320）。

## 机器判据

当前无金样覆盖（该差异即评审所指「无测试、无差异登记」）。收敛时机：#1320 接线时
`generate.mts` 补 `changedPaths: [1]` / `projectKey: 5` 两行金样（Node v22.22.0
真实 TS 生成），`ExtensionsReload` 改携带 `JsValue` 后由
`settings_gateway_replays_the_real_ts_oracle` 逐格比对钉死，本条目随后按更正
流程注销。

## 批准记录

未批准，非已接受差异，待人。#1318 fix-r1 登记于评审 P2-1（按评审建议的「类型
改动留到接线单」路线）；不改判分口径，不改金样。
