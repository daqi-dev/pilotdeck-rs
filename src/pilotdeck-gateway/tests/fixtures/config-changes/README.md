# Config changes TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/pilot/config/classifyChanges.ts`（SHA-256 记在 `oracle.json`）。所有 `changedPaths` / `changeClasses` / `runPolicyOnly` 都由真实 TS 函数产生，Rust 侧不写死期望值。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/config-changes/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/config-changes
```

生成器先断言基线 HEAD 和源文件路径；覆盖 91 对快照与 23 组直接 classify 输入：

- `classifyPath` 全部分支：`agent.`/`model.`、`extension.includeHookEvents` 精确匹配与其它 `extension.`、`router.`、`alwaysOn.`/`cron.`/`tools.`、`modules`/`modules.`、`proxy`/`proxy.`、`runPolicy`/`runPolicy.`、默认分支；裸 `agent`、`model`、`extension`、`router` 等边界。
- `classifyRouterPath` 全部子路径：scenarios/fallback/tokenSaver.tiers、zeroUsageRetry 两个精确键、`transientRetry`（容器、enabled/maxAttempts/baseDelayMs/maxDelayMs 与未知子键，TS 均落默认 `next-runtime`）、tokenSaver.judge 前缀、autoOrchestrate、stats、customRouter、默认。
- diff：数组等长逐项/长度不等/数组换对象、对象字段增删（新增对象值只报父路径）、嵌套数组、根数组与根标量、整数键与 UTF-16 码元排序（含非 BMP 与孤立代理键）、多类混合时的首次出现顺序。
- JS 值边界：`NaN` 与自身相等、`NaN`/`null`/`undefined`/`-0`/`±Infinity` 与删除 `undefined` 键不产生 diff、`null` 对 `undefined`、布尔对数字；`-Infinity` 有实际输入对。
- JS 原型查找：`Object.prototype` 的 12 个内建成员（`constructor`、`hasOwnProperty`、`isPrototypeOf`、`propertyIsEnumerable`、`toLocaleString`、`toString`、`valueOf`、`__proto__`、4 个 `__define*`/`__lookup*`）缺失时回落到函数值；JSON.parse 产生的自有 `__proto__` 键；`Object.create([7])` 的显式数组原型（索引、`length`、`Array.prototype` 方法与 `Array.prototype` 的 `__proto__`）；`Object.create({x:1})` 的显式对象原型；`Object.create(null)` 的无内建回落。
- 真实产品路径：`mergeConfigSources → parseModulesConfig` 保留 `modules.tools.tools[].inputSchema` 原对象（含自有 `__proto__` 键），再交给 `diffConfigSnapshots`；生成器断言解析无 diagnostics。
- `isRunPolicyOnlyChange` 正例（单个/多个 runPolicy 路径）与反例（空列表、混入其它路径、`runPolicyX`）。

`oracle.json` 的 `pairs[]` 记录 `previous`/`next`（按下列编码）、`changedPaths`、`changeClasses`、`runPolicyOnly`；`classify[]` 记录直接传入的 `paths` 及 TS 结果。JSON 没有 `undefined`/`NaN`/`Infinity`/`-0`，也没有显式 `[[Prototype]]`，生成器用保留的 `$` 键编码：

- `{"$": "undefined" | "NaN" | "Infinity" | "-Infinity" | "-0"}`：这几个特殊值；真实 fixture 对象不允许使用保留键 `$`（生成器断言）。
- `{"$": "prototype", "prototype": <encoded>, "value": <encoded own entries>}`：原型不是 `Object.prototype` 的对象（如 `Object.create([7])`、`Object.create(null)`）；`value` 仍是普通对象编码，键保持自有键语义（`JSON.parse` 的 `__proto__` 不会被当成原型）。

Rust 集成测试 `tests/config_changes.rs` 解码后逐条重放：路径按 `JsString`（UTF-16 码元）比较，类按顺序比较，并额外核对“TS 记录的 changedPaths 在 Rust 里分类结果与 TS 记录一致”、四类覆盖与至少 25 对快照的下限。

范围：只移植纯函数 `diffConfigSnapshots` / `classifyConfigChanges` / `isRunPolicyOnlyChange`（及内部 classifyPath / classifyRouterPath / diffValues）；不含 ConfigStore、reload、watcher、完整快照加载（A6.3-pr2/pr3）。
