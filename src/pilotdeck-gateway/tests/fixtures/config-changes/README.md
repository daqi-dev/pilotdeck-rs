# Config changes TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/pilot/config/classifyChanges.ts`（SHA-256 记在 `oracle.json`）。所有 `changedPaths` / `changeClasses` / `runPolicyOnly` 都由真实 TS 函数产生，Rust 侧不写死期望值。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/config-changes/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/config-changes
```

生成器先断言基线 HEAD 和源文件路径；覆盖 61 对快照与 20 组直接 classify 输入：

- `classifyPath` 全部分支：`agent.`/`model.`、`extension.includeHookEvents` 精确匹配与其它 `extension.`、`router.`、`alwaysOn.`/`cron.`/`tools.`、`modules`/`modules.`、`proxy`/`proxy.`、`runPolicy`/`runPolicy.`、默认分支；裸 `agent`、`model`、`extension`、`router` 等边界。
- `classifyRouterPath` 全部子路径：scenarios/fallback/tokenSaver.tiers、zeroUsageRetry 两个精确键、tokenSaver.judge 前缀、autoOrchestrate、stats、customRouter、默认。
- diff：数组等长逐项/长度不等/数组换对象、对象字段增删（新增对象值只报父路径）、嵌套数组、根数组与根标量、整数键与 UTF-16 码元排序（含非 BMP 与孤立代理键）、多类混合时的首次出现顺序。
- JS 值边界：`NaN` 与自身相等、`NaN`/`null`/`undefined`/`-0`/`±Infinity` 与删除 `undefined` 键不产生 diff、`null` 对 `undefined`、布尔对数字。
- `isRunPolicyOnlyChange` 正例（单个/多个 runPolicy 路径）与反例（空列表、混入其它路径、`runPolicyX`）。

`oracle.json` 的 `pairs[]` 记录 `previous`/`next`（按下列编码）、`changedPaths`、`changeClasses`、`runPolicyOnly`；`classify[]` 记录直接传入的 `paths` 及 TS 结果。JSON 没有 `undefined`/`NaN`/`Infinity`/`-0`，生成器把这几类值编码成单键对象 `{"$": "undefined" | "NaN" | "Infinity" | "-Infinity" | "-0"}`，其余按普通 JSON；真实 fixture 对象不允许使用保留键 `$`（生成器断言）。

Rust 集成测试 `tests/config_changes.rs` 解码后逐条重放：路径按 `JsString`（UTF-16 码元）比较，类按顺序比较，并额外核对“TS 记录的 changedPaths 在 Rust 里分类结果与 TS 记录一致”、四类覆盖与至少 25 对快照的下限。

范围：只移植纯函数 `diffConfigSnapshots` / `classifyConfigChanges` / `isRunPolicyOnlyChange`（及内部 classifyPath / classifyRouterPath / diffValues）；不含 ConfigStore、reload、watcher、完整快照加载（A6.3-pr2/pr3）。
