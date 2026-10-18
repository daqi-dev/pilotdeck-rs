# Model cost / local token estimate fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测源码：

- `src/router/utils/modelPricing.ts`——定价表、configured 覆盖、`calculateInputCost`/`calculateCacheReadCost`
- `src/router/stats/TokenStatsCollector.ts:183-186,378-421`——`estimateCost` / `estimateCostBreakdown`
- `src/agent/loop/AgentLoop.ts:2390-2403`——router estimate 之后回退 `usage.nativeCost`
- `src/context/budget/TokenAccountingRuntime.ts:167-190`——本地估算分支
- `src/context/budget/TokenBudgetManager.ts`——块/消息计数
- `src/model/protocol/toolResultContent.ts`——tool result 展平

> 固定基线的 `src/model/catalog/providers.ts` 只有模型清单和能力，**没有费率字段**；全部内置费率在 `modelPricing.ts` 的 `DEFAULT_PRICING`。生成器枚举 `providers.ts` 的 57 个模型逐个走真实 lookup，`oracle.json.pricingLookups` 保留 provider/model 与结果。

## 生成

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-cost/generate.mts [outdir]
```

生成器断言基线 HEAD 与固定 commit，且 `git status --porcelain --untracked-files=no` 为空（不检查未跟踪文件）；每个 `sourceSha256` 取 `git show <commit>:src/<path>` 的 blob 哈希，并与工作树内容核对，随后把真实 TS 数值写入 `oracle.json`（临时文件 + rename 原子替换）。无时间戳、pid 或本机路径，连续跑两次字节一致；省略 `outdir` 时就地写入夹具目录。当前产出：273 个定价查找、885 个费用算术用例、1653 个 usage 费用用例、51 个请求形状。设置 `PDRS_BASELINE` 时，Rust 回放 `oracle_comes_from_the_fixed_baseline` 会复跑同一组干净性与 blob 哈希校验。

- `pricingLookups[]`：catalog 全模型 ×（无覆盖 / `gpt` 子串覆盖）、语料之外的模式边界（大小写、优先级、`o1/o3/o4`、qwen/llama/mistral 等）、JS `/i` 非 Unicode 折叠与 `.` 行终止符边界（`K`、`ſ`、`\r`、`\n`、U+2028/U+2029 的拒绝；`\v`/`\t` 的正例）、configured 精确键/子串键/空条目/插入顺序/整数键。
- `costs[]`：真实 `calculateInputCost`/`calculateCacheReadCost` 的逐项结果（含 0、小数、十亿级 token）。
- `usageCosts[]`：真实 `TokenStatsCollector.estimateCost`；`breakdown` 由导出的 lookup 重算各分项并断言 `total` 与真实方法相等；`costUsd` 复现 AgentLoop 的 router estimate → `nativeCost` 回退。覆盖 `nativeCost` 为 0/正数/负数、负 token 让 router estimate 失效、`usage` 缺省。
- `propertyOrder`：`Object.entries` 的整数键优先规则。`integerKeys` 按源插入顺序 `"12" → "2"` 记录条目，`objectEntries` 记录真实枚举顺序 `["2","12"]`，`lookup` 记录 `model12` 的真实结果（2/4）；`mixedKeys` 记录数字键升序提前、普通键保序。
- `requests[]`：51 个请求形状（空消息、纯文本、多段、工具调用、tool result、图片/PDF/audio 占位、PDF `pages` 的 `1e-7`/`1e21`/`2.5e21`/`0.1+0.2`/`-0`、reference、中文、emoji、10k 文本、MCP/memory 系统块、工具 schema 键序等），记录 `estimateRequestBreakdown` 的 `total/system/tools/messages/mcp/memory`，并断言加和不变量与 `estimateRequestInput` 一致。

## 回放

Rust 测试 `src/model_cost_tests.rs` 用 `parse_json`（保留对象插入顺序）重建 canonical request，所有数字用 `==` 精确比较，无 epsilon。设置 `PDRS_BASELINE` 且加 `--ignored` 时，`model_cost_fixtures_regenerate_identically_from_ts` 会重跑生成器到临时目录并与已提交 `oracle.json` 逐字节比较。
