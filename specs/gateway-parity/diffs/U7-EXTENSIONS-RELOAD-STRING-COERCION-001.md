---
id: U7-EXTENSIONS-RELOAD-STRING-COERCION-001
---

## 类别

已知未收敛差异（U7 settings 网关 `reload_extensions` 入参非字符串原值的强制转字符串）。

## 基线依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0 / ICU 78.2。
#1318 fix-r1 评审 P2-1 以只读探针实测真实
`src/cli/GatewayRuntimeRefreshBundle.ts` 的 `reloadExtensions`（引文存
`_tmp/20261026-issue-1318/fix-r1/review.md`）；fix-r2 又以
`_tmp/20261026-issue-1318/fix-r2/probe-extensions-truthiness.mts` 在同基线
直接调真实 bundle 逐格复测（原始输出 `probe-extensions-truthiness.out`）；
fix-r3 以 `_tmp/20261026-issue-1318/fix-r3/probe-extensions-truthiness-r3.mts`
同法补测两格（原始输出 `probe-extensions-truthiness-r3.out`，同基线同
Node）：

- `projectKey` 缺省 / `null` / `0` / `false` / `NaN` / `""` → 全量分支
  （`invalidate()` + `markAllDirty`，日志 "for all runtimes"）；
- `projectKey: []` / `[""]` / `[null]`（JS 真值、`String()` 为 `""` 的数组
  格族）→ 项目分支 no-op（原值透传，`invalidate([""])` /
  `markProjectDirty([null])` 等在字符串键表上不命中；日志 "for
  project : "，三格事件流逐行同形）；
- `projectKey: {}` / `5` → 项目分支，原值透传；
- `changedPaths: [1, true, null]` → 结果 / 广播均原值透传（`null` 保持
  `null` 而非空串），日志 `join(", ")` 为 `1, true, `。

金样 `tests/fixtures/settings-gateway/oracle.json` 的 54 行均为字符串
`projectKey` / `changedPaths`，不覆盖非字符串格。

## Rust 行为

`src/pilotdeck-gateway/src/runtime/settings/gateway.rs` 的 `extensions_reload`：
分支按 `projectKey` **原值 JS truthiness** 选择（fix-r2：`0` / `false` /
`NaN` / `""` 与 nullish 同走全量分支，与 TS 一致；此前先 `String()` 强转再
判空导致 0/false/NaN 翻转到项目分支，按评审修复）；存活下来的真值非字符串
`projectKey` 与非字符串 `changedPaths` 元素仍 `String(value)` 强转（本条目
主体）：`changedPaths: [1, true, null]` → 响应 / 广播 / registry 副作用三处
均为 `["1", "true", ""]`；`projectKey: 5` → 转成 `"5"`，会真的 retire 名字
为 `5` 的项目运行时并置 dirty。`projectKey: []` / `[""]` / `[null]` →
真值但 `String()` 强转为 `""`（fix-r3 补登后两格），被
`runtime/refresh.rs:320` 的空键过滤送回全量分支（TS 走项目 no-op 分支，
见「范围」）。`{}` → `"[object Object]"`，分支与日志文本两侧一致。类型根源：
`runtime/refresh.rs:189-192` 的 `ExtensionsReload { project_key:
Option<JsString>, changed_paths: Option<Vec<JsString>> }`。

## 范围

仅 `reload_extensions` 的非字符串 `projectKey` / `changedPaths` 元素（合法 JSON
帧可达；`NaN` 仅组件内构造可达，同格登记）。金样的 54 行均为字符串，不触发
本差异；其余四方法与字符串路径不受影响。「JS 真值但 `String()` 为空串」的
数组格族：`[]`（fix-r1 登记）、`[""]` / `[null]`（fix-r3 按评审补登，同
机制；任意 `String()` 为空串的真值数组同族，登记钉住这三格）：
`ExtensionsReload` 不携带 `JsValue`，转换层无法区分「真值强转后为空」
与「空串原值」，本组件内不可复现 TS 的项目 no-op 分支，按任务书口径登记于
本格族，随 #1320 收敛。修复需要 `ExtensionsReload` / `ConfigChangeNotice` /
`ReloadOutcome` 携带 `JsValue` 并级联 refresh 车道端口（`RefreshRegistry::
invalidate`、`RefreshRouter::mark_project_dirty`）与 session 通知消费者，
跨接线边界，归 #1320 接线单评估（前置于 #1320）。

## 机器判据

`runtime::settings::tests::extensions_reload_branches_on_the_original_project_key_truthiness`
（`gateway_tests.rs`，fix-r2 新增；fix-r3 补 `empty-string-array` /
`null-array` 两格）经 `SettingsGateway::
reload_extensions(&JsValue)` 逐格钉住上述矩阵：0/false/NaN/""/null/缺省 →
全量分支事件流，`{}` / `5` → 项目分支（强转键），`[]` / `[""]` / `[null]` →
当前全量分支（本格族登记差异，测试注释指回本条目），`changedPaths:
[1, true, null]` → `["1", "true", ""]`（本条目主体）。负控
`extensions-truthiness-flipped`
（`tests/fixtures/settings-gateway/negative_controls.py` 第七个变异）去掉
truthiness guard 恢复强转先行分支，由该测试以 `#1318-r2
extensions-truthiness:` 专属消息判杀。TS 期望来自「基线依据」的 fix-r2 /
fix-r3 探针实测。收敛时机：#1320 接线时 `ExtensionsReload` 改携带 `JsValue`，
由 `settings_gateway_replays_the_real_ts_oracle` 逐格比对钉死，本条目随后按更正
流程注销。

## 批准记录

未批准，非已接受差异，待人。#1318 fix-r1 登记于评审 P2-1（按评审建议的「类型
改动留到接线单」路线）；fix-r2 按评审补原值真值分支与钉住测试，`[]` 格登记
为组件内不可复现；fix-r3 按评审补登 `[""]` / `[null]` 两格（真实 TS bundle
实测 + 测试钉住）。不改判分口径，不改金样。
