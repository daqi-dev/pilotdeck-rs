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

## 覆盖（8 案例 50 步）

- `list-project-key-validation`（12 步）：缺 projectKey、空串、空白串、数字、
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

## 域限制

所有输入均可 JSON 表示（wire 域）。`new String("session")` 这类装箱字符串只能
通过直接 TS API 调用到达，无法出现在 WS/JSON 参数上，故不入库（实测记录见
#1214 RESULT.md「TS 实测」）；`input.scope === "session"` 的严格等号语义由
`scope: 42`、`scope: null`、`"SESSION"`、`"session "` 等格钉住。

## 负控

`negative_controls.py`：参数校验放松、scope 判别改错、wire 字段改名等变异，
逐项 KILLED / SURVIVED / INVALID 后汇总。新文件提交前不在 HEAD，
`mutating_sources` 拒跑：提交后由协调者在宿主执行
（`CARGO_TARGET_DIR` 由调用方设置）。
