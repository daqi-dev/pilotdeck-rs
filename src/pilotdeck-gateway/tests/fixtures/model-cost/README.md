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
  src/pilotdeck-gateway/tests/fixtures/model-cost/generate.mts
```

生成器断言基线 HEAD 与固定 commit，记录被测源码 SHA-256，并把真实 TS 数值写入 `oracle.json`；无时间戳、pid 或本机路径，连续跑两次字节一致。当前产出：248 个定价查找、800 个费用算术用例、1504 个 usage 费用用例、46 个请求形状。

- `pricingLookups[]`：catalog 全模型 ×（无覆盖 / `gpt` 子串覆盖）、语料之外的模式边界（大小写、优先级、`o1/o3/o4`、qwen/llama/mistral 等）、configured 精确键/子串键/空条目/插入顺序。
- `costs[]`：真实 `calculateInputCost`/`calculateCacheReadCost` 的逐项结果（含 0、小数、十亿级 token）。
- `usageCosts[]`：真实 `TokenStatsCollector.estimateCost`；`breakdown` 由导出的 lookup 重算各分项并断言 `total` 与真实方法相等；`costUsd` 复现 AgentLoop 的 router estimate → `nativeCost` 回退。覆盖 `nativeCost` 为 0/正数/负数、负 token 让 router estimate 失效、`usage` 缺省。
- `requests[]`：46 个请求形状（空消息、纯文本、多段、工具调用、tool result、图片/PDF/audio 占位、reference、中文、emoji、10k 文本、MCP/memory 系统块、工具 schema 键序等），记录 `estimateRequestBreakdown` 的 `total/system/tools/messages/mcp/memory`，并断言加和不变量与 `estimateRequestInput` 一致。

## 回放

Rust 测试 `src/model_cost_tests.rs` 用 `parse_json`（保留对象插入顺序）重建 canonical request，所有数字用 `==` 精确比较，无 epsilon。
