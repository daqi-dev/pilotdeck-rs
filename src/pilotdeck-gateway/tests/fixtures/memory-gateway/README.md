# memory 网关适配夹具（#1214 / A8.2-pr4）

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。出处：
`src/gateway/client/InProcessGateway.ts:2794-2808`（`memoryList` / `memoryWipe`
包装层：`requireManagementProjectKey`、`requireOptionalManagementSessionKey`、
`scope === "session"` 检查、capability 顺序）与 `src/cli/createLocalGateway.ts:1238-1259`
（内层投影）。源码 SHA-256 位于 `oracle.json.sourceSha256`（闭包全表）；
生成器先用共享 `tests/_shared/generator_guard.mjs` 的 `bindBaseline` 校验整个
`src/` 树，再逐文件核对固定提交的 blob。

实测环境：Node v22.22.0（ICU 78.2）。oracle 不含 node 版本字段，跨 patch
版本逐字节稳定（v22.22.0 与 v22.22.1 实测一致）。

## 生成

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/memory-gateway/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/memory-gateway
```

重复执行两次 `oracle.json` 必须逐字节一致。生成器不创建或删除固定路径；
所有期望值来自真实 TS 执行，不使用手写 replica。

## 测的是什么

- 真实 `InProcessGateway` 类（`new InProcessGateway({}, { memoryList, memoryWipe })`，
  假 router）：包装层的参数校验顺序、错误码与文案、capability 检查。
- 内层方法体从 `createLocalGateway.ts` 原文提取，注入录制型
  `dialog.projects.resolveProjectKey` / `registry.resolve` / `memoryManagement`。
- 每一步记录 `params`（object / undefined / null）、`input`、`resultJson` 或
  `error`（`{name, code, message}`）、以及 seam 调用序列 `calls`。

## 覆盖（10 案例 74 步）

- `list-project-key-validation`（11 步）：缺 projectKey、空串、空白串、数字、
  布尔、null、数组、params 为 undefined/null（Node TypeError 文案）、原始值参数。
- `list-session-key-validation`（8 步）：sessionKey 空串、空白、null、数字、
  布尔、数组、对象 → `INVALID_SESSION_KEY`；缺省（undefined）放行。
- `list-filter`（5 步）：sessionKey 真值过滤、无匹配、孤立代理项按码元比较。
- `wipe-scope-discrimination`（14 步）：`scope === "session"` 判别；未知 scope
  （global / null / 42 / 缺省 / 大小写不同 / 尾随空格）一律 project 分支；
  session 分支的 `INVALID_MEMORY_SCOPE`；`'  s1  '` 不 trim 直传 `clearSession`。
- `wipe-validation-order`（2 步）：sessionKey 校验先于 scope 检查；projectKey
  校验先于 sessionKey。
- `capability-absent`（4 步）：options 无 memoryList/memoryWipe 时，校验之后
  （wipe 的 scope 检查也在 capability 之前）报 `CAPABILITY_UNAVAILABLE`。
- `management-unavailable`（3 步）：内层 `memoryManagement` 缺席，解析之后报
  `CAPABILITY_UNAVAILABLE`。
- `project-key-lone-surrogate`（3 步）：`"\ud800"` 与 `"�"` 是不同 project，
  拒绝文案保留原始码元。
- `whitespace-trim-boundary`（21 步）：包装层的空白判定是
  `String.prototype.trim` 的完整空白集，不只是 ASCII 空格：U+00A0 / U+FEFF /
  U+2028 / U+2029 / U+3000 视为空白（projectKey → `PROJECT_NOT_FOUND`、
  sessionKey → `INVALID_SESSION_KEY`、wipe 的 session-scope 检查放行）；U+180E
  与 U+200B 不是空白，原样放行（projectKey 继续解析、sessionKey 真值过滤、
  `clearSession` 收到原始码元）。实测记录见 #1214 fix-r1 RESULT。#1226 追加
  5 格 projectKey 空白码点（U+1680 / U+2000 / U+200A / U+202F / U+205F →
  `PROJECT_NOT_FOUND`），并配合 `runtime/memory/gateway.rs` 的穷举单测钉住全部
  25 个码点（Node v22.22.0 全码元实测，#1226 交付证据
  `_tmp/20261026-issue-1226/whitespace-probe.log`）。
- `project-key-padded-passthrough`（3 步，#1226）：带首尾空白的 projectKey
  成功用例——`requireManagementProjectKey` 只 trim 判空、原样放行 key：
  `resolveProjectKey` 收到原始带填充码元（ASCII 填充与 U+2003/U+2009、
  U+1680/U+205F 填充），memoryList 与 session-scope memoryWipe 均走通。

## 域限制

所有输入均可 JSON 表示（wire 域）。`new String("session")` 这类装箱字符串只能
通过直接 TS API 调用到达，无法出现在 WS/JSON 参数上，故不入库（实测记录见
#1214 RESULT.md「TS 实测」）；`input.scope === "session"` 的严格等号语义由
`scope: 42`、`scope: null`、`"SESSION"`、`"session "` 等格钉住。

## 负控

`negative_controls.py`：参数校验放松、scope 判别改错、wire 字段改名、
`requires_host → Forward` 改 `Response`、`from_projection` 改回 disabled、
`js_is_space_unit` 删一个空白码点等变异，每项绑定其专属点名测试与断言消息，
逐项 KILLED / SURVIVED / INVALID 后汇总。#1226 追加 6 行：两个区间边界收缩
（`0x2000..=0x200A` 尾收、`0x0009..=0x000D` 首收）、删 `0x1680` / `0x202F` /
`0x205F`、`require_management_project_key` 返回 trim 后的 key；前 5 行绑定
lib 穷举单测 `js_is_space_unit_pins_the_ts_trim_whitespace_set`，末行绑定
`require_management_project_key_only_trims_for_emptiness`。新文件提交前不在 HEAD，
`mutating_sources` 拒跑：提交后由协调者在宿主执行
（`CARGO_TARGET_DIR` 由调用方设置）。
