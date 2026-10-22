# AgentEvent → GatewayEvent 投影金样

基线为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，运行时须
Node 22（基线根已含 `node_modules/tsx`）。

`generate.mts` 经基线已安装的 esbuild 在内存中把下列**未改写**源文件转为 CJS 并执行：

- `src/gateway/client/GatewayAgentEventProjector.ts`（真实 `mapAgentEvent` facade、
  `GatewayAgentEventProjector.project` 与全部 55 个 switch 分支）
- `src/gateway/client/GatewayToolResultArtifactStore.ts`（仅为可注入端口提供类型/类）
- `src/model/protocol/toolResultContent.ts`（真实 `flattenToolResultBlockText`）
- `src/tool/protocol/result.ts`（真实 `contentToText`）

只注入 `GatewayToolResultArtifactStorePort` 的确定性替身：它照真实实现的 4096 字节
阈值决定是否返回路径，路径根固定在 `/tmp/pdrs-agent-event-projector`，不写文件，
并把每次 `persist` 入参和返回值记入金样。投影本身的排序、截断、附件、错误和
budget 计算均来自真实 TS 执行；未用到的 import 通过访问即报错的 Proxy 隔离。

221 个 case 覆盖 `AgentEvent` 联合的每个成员（含 `observer_report`、`session_started`
与未知/缺 type 的 default 分支）、`model_event`/`subagent_model_event` 的各子类型、
`forwardSubagentText` 真假、timeline/streamBoundary 传播、tool_result 的 bytes/UTF-16
截断边界、4 种 mime 扩展名与 `value` 回退、providerError 的 raw 截断
（在代理对中间切开）、`??` 与 `!== undefined` 的 null/缺失差异，以及 persist 阈值。
另有 79 个结构不合法事件的 `error` case（记录真实 TS 的 `String(error)`，如
`TypeError: Cannot read properties of undefined (reading 'content')`；其中 7 条钉住
附件/media 路径缺字段时 `reading 'trim'/'split'/'toLowerCase'/'length'` 的抛错与
「附件循环先于 errorCode」的求值顺序）与 4 个 `__proto__` 工具数据 case（TS 的属性
赋值语义会丢弃该键）。深度 600 的嵌套数组作为 `text` 进了金样（`Array.prototype.join`
在 512 以上仍正常拼接，离 V8 实测 ~2516 的抛错阈值很远）；更深的 30k 输入无法放进
金样（`JSON.stringify` 自身会抛错），由 `projector_tests.rs` 按同构构造并用
`_tmp/20261022-a72-pr2-projector/probe-fix1.mts` 的真实 TS 输出钉住。

修复轮 3 追加 65 个 case 钉住「字段存在但类型不对」：对 `safeGatewayPathPart`
（`.trim()`）、`extensionForMime`（`.toLowerCase()`）、`item.path`/`block.path`
（`.split()`）逐一给出数字、布尔、数组、对象四种输入——真实 TS 全部抛
`TypeError: <表达式> is not a function`，错误文本按调用点区分（`value.trim`、
`mimeType.toLowerCase`、`item.path.split`、`block.path.split`）。`content.data.length`
与 `block.data.length` 是**属性读取**，不调用字符串方法：数字/布尔渲染 `undefined`
（不抛）、数组取元素个数、对象取自有 `length` 值（含 `{length:[1,2]}` 的 `1,2`）。
`persist` 的 `sessionId`/`turnId`/`toolCallId` 在 4096 字节阈值以下原样记录（保留
数字/布尔/数组/对象类型），超阈值时 `safePathPart(...).trim()` 抛
`TypeError: value.trim is not a function`。

修复轮 4 追加 29 个 case，钉住全部读属性/调方法/拼接位置对 null、undefined、
数字、数组、对象五类值的真实 TS 行为（探针 `_tmp/…/probe-r4.mts`）：

- **join 语义**：text 块的 `text` 为 null/缺失渲染成空串（`"\nb"` 而非
  `"null\nb"`），其它值经 `ToString`（数字 `"123"`、数组 `"1,2"`、普通对象
  `[object Object]`）；
- **nullish 数组元素**：`calls:[null]`、steer/assistant 的 `content:[null]` 抛
  `Cannot read properties of null (reading 'timeline'/'type'/'id')`（find 短路，
  命中后不再读后续元素）；
- **非数组方法接收者**：V8 报错带完整属性访问表达式
  （`event.result.content.map is not a function`、`content.map is not a function` 等）；
- **自有 `toString`**：模板插值（`mimeType`/`path`/`pages`/`data.length`）与
  context_budget 的 `+` 抛 `TypeError: Cannot convert object to primitive value`；
  `previewUnknown`/`safeJsonStringify` 的 `JSON.stringify` 对此类值正常序列化，
  永远到不了 `String()` 回退；
- **`+` 的 ToPrimitive**：数组转串拼接（`[1,2]+20` → `"1,220"`）、字符串拼接
  （`"5"+20` → `"520"`）、`null+20` → 20、`undefined+20` → NaN；
- 字符串 `content` 在 for-of 中逐字符迭代，无一有 `type`，产出 0 帧。

已知覆盖边界：金样回放注入的是不会抛错的确定性 `RecordingStore`，默认
`GatewayToolResultArtifactStore` 超阈值时对非字符串路径字段的抛错由单测
`default_store_throws_on_non_string_path_part_like_ts` 单独守护（Grok r4 P2 #4
已登记，接受现状）。

合法 case 的 `frames` 是 `mapAgentEvent` 返回对象的 `JSON.stringify` 语义快照：
Rust 回放逐条比 `stringify_json` 的**文本**，因此键顺序、数字格式、lone surrogate
转义和 undefined 键的省略都参与比对；`persist` 数组同样逐条比对。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
export TSX_DISABLE_CACHE=1 TMPDIR=/tmp LANG=en_US.UTF-8
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/agent-event-projector/generate.mts /tmp/aep-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/agent-event-projector/generate.mts /tmp/aep-second
cmp /tmp/aep-first/oracle.json /tmp/aep-second/oracle.json
```

生成器开始前调用共享 `../_shared/baseline.mts`，核固定 HEAD、tracked 状态与 6 个
执行源文件的提交版/工作版 SHA-256。Rust 回放统一先走 `crate::test_baseline::oracle`；
设置 `PDRS_BASELINE` 时重新核对真实源码。

`negative.py` 在 `/tmp` 的隔离 crate 中逐条改坏 `projector.rs`，要求真实断言转红并
打印对应 case 名；任一负控未被抓住即退出 1。命令：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/agent-event-projector/negative.py /tmp/aep-negative
```
