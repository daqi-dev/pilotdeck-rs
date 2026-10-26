# EdgeClaw heartbeat helpers (A6.6-edgeclaw-12)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的
`src/context/memory/edgeclaw-memory-core/src/core/pipeline/heartbeat.ts`
第 22-446 行（纯函数部分，`HeartbeatIndexer` 类留给后续包）。

`generate.mts` 先 `bindSourceTree` 绑定提交，再对 `heartbeat.ts` 的相对
import 闭包做 `sourceClosureSha256`，随后用 tsx/esbuild 在原文件末尾追加
私有函数导出（不改函数体）并直接调用真实 TS。`HOME` 固定为
`/fixture/home`，时钟冻结在 `2026-10-22T00:00:00.000Z`；`heartbeat.ts`
不读 `Math.random`，`createBatchTrace` 的 `nowIso()` 由冻结时钟决定。

覆盖 232 个观测：

- `sameMessage` / `hasNewContent`：undefined、空数组、等长/增删、角色或
  内容差异、`msgId` 差异、CJK / 孤立代理；`system` / `tool` / `developer`
  等任意字符串角色参与比较（#852 P1-1）；
- `tokenizeSearchText`：21 个停用词、CJK 整段（2–10 单元）与 2/3 单元
  切片、Han/非 Han 混排、astral Han 的 UTF-16 单元切片（会产生孤立代理）、
  JS `\s`（含 U+FEFF）、全角字母数字、组合附加符、大小写折叠（ß/İ/ı）；
- `buildGeneralProjectShortlist`：精确名权重、token 命中、`latestMemoryAt`
  回退、`workspace_external` / `workspace_external_mirror` 映射、稳定排序、
  `localeCompare` 降序平局、30 条截断、空目录；
- `buildCandidateRoutingQuery` / `buildCandidateMemoryPreview`：可选字段、
  数组去空、`||` 回退链、feedback/project/user 分节、3000 单元截断；
- `flattenBatchMessages` / `commonPrefixLength` / `mergeSessionMessages` /
  `deriveFocusTurns`：空 prev、共享前缀、前缀 0 拼接、重复 `l0IndexId` 的
  `Map.set` 覆盖语义、只保留 user 焦点轮、seed 透传；跨批次
  `[system:"A", user:"question"]` / `[system:"B", user:"question"]` 的公共
  前缀为 0，第二批 user 仍是新焦点轮（#852 P1-1）；
- trace：`buildIndexTraceId`、`normalizeTrigger`、`previewText`（220/180/0
  与边界）、`inferStorageKind`、`exposeStoredRelativePath`；
- `textDetail` / `noteDetail` / `listDetail` / `kvDetail` / `jsonDetail`：
  `labelI18n` 可选、转义解码、`String(value ?? "")` 的 null/undefined/
  number/bool/array/object、JSON 递归解码；`kvDetailCatching` 真实 TS 抛
  `TypeError: Cannot convert object to primitive value` 的自有 `toString`
  （对象、数组元素、嵌套数组），`kvDetailDepthProbe` 钉住 1999/2000/2001
  层容器：TS 均得到 `"1"`，Rust 共享 2000 门限在 2001 层报 `RangeError`
  （已登记 `A6-JSVALUE-DEPTH-001`，不在本包改阈值）；
- `createStep`：无 options、truthy 才写入的 `refs/metrics/details/
  promptDebug/*I18n`、空对象/空数组仍写入、step 序号、连续三步；
- `createBatchTrace`：空批次、空时间戳过滤、`Array#sort` 默认码元排序、
  `fromTimestamp`/`toTimestamp`、冻结的 `startedAt`。

生成两次并核字节（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
export LANG=en_US.UTF-8
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-heartbeat/generate.mts /tmp/edgeclaw-heartbeat-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-heartbeat/generate.mts /tmp/edgeclaw-heartbeat-2
cmp /tmp/edgeclaw-heartbeat-1/oracle.json /tmp/edgeclaw-heartbeat-2/oracle.json
```

`verify.mts` 用同一份源码哈希重放全部输入，要求字节一致：

```sh
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-heartbeat/verify.mts
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_heartbeat
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_heartbeat -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-heartbeat/negative.py /tmp/edgeclaw-heartbeat-negative
```

`negative.py` 有 36 条可逆负控：`sameMessage` 角色、删除任意角色、等长新
内容、共享前缀阈值/新消息切片、`commonPrefixLength` 差一、flatten seed、
焦点轮 user 过滤、checked `String(value)` 回退、转换错误 panic、2001 层共享
深度门限、共享 `js_truthy` / `join_text_refs`、3 单元 CJK 切片、Han 整段
8/9 边界、停用词、精确权重、`localeCompare` 排序方向、30 条截断、路由
stage、preview 分节/3000 截断、`previewText` 边界、UserNotes 目录、
`normalizeTrigger` 大小写折叠、`buildIndexTraceId` id 分隔符、
`exposeStoredRelativePath` 的 `global/` 前缀、kv null 值、text/note/list/json
`detail` 的转义解码、step 序号/truthy 选项、空时间戳过滤、批次码元排序。
29–36 号来自 #831，其中 `trace-id-separator` / `expose-global-prefix` 的锚点
按 #852 的共享 `text::join_text_refs` 重写，语义不变。每条要求编译成功且
命中对应 oracle 分组或测试断言，逐格汇总 KILLED/SURVIVED，有存活退出 1；
每个变异体运行前校验其锚点在基线源码中恰好出现一次，锚点过期时打印
`ANCHOR EXPIRED` 并退出 2（不记成 SURVIVED）。全部变异恢复后再跑一遍未
变异套件，`restored.log` 是「恢复以后通过」的原始输出。

新增文件在提交前不在 HEAD，`mutating_sources` 会拒绝运行；协调者在提交后
执行即可。作者树可用一个包含工作树的悬空 commit 作为 `NEGATIVE_BASELINE`
跑样本（不建第二份构建目录）；cargo 使用调用方传入的
`CARGO_TARGET_DIR`，未设置时提前报错。
