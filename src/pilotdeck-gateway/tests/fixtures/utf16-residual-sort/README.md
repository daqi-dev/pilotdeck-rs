# utf16-residual-sort：#2131 残余 JsString 排序点的孤代理码元

`generate.mts` 用固定基线 `cc7c4e6` 的真实入口驱动 #2131 改动的五个可达排序点，
不复制、不重写任何被测函数体：

| 排序点（TS 原文） | Rust 对应 | case 输出 |
|---|---|---|
| `ToolRegistry.ts:118` `listAll` 的工具名排序 | `runtime/tools/registry.rs` | `registry.listed` |
| `ToolRegistry.ts:179` `listUnavailableEntries` 按 `diagnostic.toolName` 排序 | 同上 | `registry.unavailable` |
| `PluginRuntime.ts:697` `getAllMcpInstructions` 按 `serverName` 排序 | `runtime/plugins/contributions.rs` | `mcpInstructions` |
| `CachePlan.ts:44-46` `left.name.localeCompare(right.name)` 工具排序 | `runtime/context/cache.rs`（指纹哈希排序后的 tools，是顺序的字节级证人） | `cacheFingerprint` |
| `TokenStatsCollector.ts:209` `modelUsageSnapshot` 按 provider/model 排序 | `runtime/model/stats.rs` | `modelUsage` |

每个排序点用同一组键族驱动（上表用码位书写，因为 `U+E000` 单独打印是不可见字形）：

| case | 键（乱序输入） | 真实 TS 顺序（码位） | 判别力 |
|---|---|---|---|
| lone-high-among-valid-code-points | `U+E000`, `U+D800`, `U+FFFD`, `U+10000` | `U+10000` → `U+D800` → `U+E000` → `U+FFFD` | 折叠成 U+FFFD 后 `U+D800` 与 `U+FFFD` 平局，稳定排序把 `U+D800` 排到 `U+E000` 之后 |
| distinct-lone-code-units | `U+DC00`, `U+D801`, `U+D800` | `U+D800` → `U+D801` → `U+DC00` | 折叠后三者全等，稳定排序保留输入序（`U+DC00` 打头） |

两族键都在 `../native-collation/oracle.json` 的 61 键表内；逐 locale 读该文件的
`rows`，14 个 locale 对两族都是严格升序，生成器另在 14 个 locale 上再验一遍，因此
Rust 断言不依赖宿主 `LANG`。`modelUsageSnapshot` 只重放第一组：Rust 的
`per_model_usage` 桶键是 `to_utf8().unwrap_or_default()` 得到的 `String`，非合法
UTF-8 的 provider 全部塌进同一个桶（TS 桶键是 JS 字符串 `${provider}/${model}`，
保留孤立代理，`TokenStatsCollector.ts:543`），三桶变一桶发生在排序之前；该既有
分歧在本单范围外（见 RESULT.md 跟进项），排序语义由桶仍各自独立的混合组覆盖。

`TokenStatsCollector` 的 `filePath` 只选目录（journal 文件名恒为 `stats.jsonl`），
所以每个 case 用独立临时目录；`cacheFingerprint` 一行同理不与其他 case 共享状态。

Rust 侧由挂在 `runtime/plugins.rs` 下的 `regression.rs`（lib 测试，因此
`contributions::collect_mcp_instructions` 等 plugins 子树内部缝无需放宽可见性）逐
case 重放；测试走 `crate::test_baseline::oracle(...)`，改动 `sourceCommit` /
`sourceSha256` 或写坏 JSON 都会在无 `PDRS_BASELINE` 的默认运行里变红。
`sourceSha256` 由共享 `baselineSourceHashes` 记录四个入口的完整 import 闭包。

重生成（本机 `node -v` 必须是 `v22.22.0`、`node -p 'process.versions.icu'` 必须是
`78.2`，否则生成器自己 `assert` 失败）：

```sh
export PDRS_BASELINE=/absolute/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/utf16-residual-sort/generate.mts /absolute/new-output
```

生成器只写新目录里的 `oracle.json`（`flag: 'wx'`）。ignored 守卫
`utf16_residual_sort_regenerates_identically`（本夹具）与
`tests/generator_provenance.rs::utf16_residual_sort_regenerates_identically`
用共享 `regenerates_identically("utf16-residual-sort")` 逐字节重生。
负控 `negative_controls.py`：把五个排序点各自装回折叠的
`compare_utf16`，对应绑定测试必须以其 `#2131 …` 断言消息变红。
