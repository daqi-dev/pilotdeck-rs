# Compaction snip / micro TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测 TS 入口：`src/context/compaction/SnipEngine.ts` 与 `src/context/compaction/MicroCompactionEngine.ts`，连同 `toolPairIntegrity.ts`、`protectedContext.ts`、`TokenBudgetManager.ts`、`tokenizer.ts` 与 `model/protocol/toolResultContent.ts` 的 import 传递闭包。`oracle.json` 记录整棵 `src` 的来源守卫（`bindSourceTree`）、闭包内每个文件的 SHA-256、en-US locale 断言、探针原值，以及 60 个场景的真实 TS 输入/输出。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
  node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/compaction-snip-micro/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/compaction-snip-micro
cd src
cargo test --locked --offline -p pilotdeck-gateway --test compaction_snip_micro
```

生成器先跑 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`（显式 Git 绝对路径、HEAD 固定、已跟踪树干净、整棵 `src` 逐 blob 比对），再按 import 传递闭包复算 `sourceSha256`。每次运行在输出目录下 `mkdtemp` 一个 `.generate-*` 临时目录，`oracle.json.tmp` 写在该目录内再 rename 到 `oracle.json`（同一文件系统）；清理前先用 `assertOwnedScratch` 确认目标是本次的直接 `.generate-*` 目录。生成器连跑两次逐字节一致（没有随机值；token 数与消息字节都来自真实 TS）。

`oracle.json.scenarios[]`（60 组，Rust 逐场景按序序列化比较）：

| 场景 | 覆盖 |
|---|---|
| unconstrained-head-tail-noop | 无 token target 时基线从尾部保留**所有** turn（代码的 `tailFloor` 只在 constrained 分支生效），因此 8 turn 原样返回；钉住真实行为而非 doc comment |
| noop-within-window | 6 turn ≤ head+tail，S3 无操作 |
| constrained-tail-window | targetTokens=1 的 constrained 路径：只保尾 1 组 + 最近 user request anchor，注入 boundary |
| disabled | enabled=false 原样返回 |
| constrained-custom-head-tail | constrained 下 head 不参与，custom tail 配置 |
| checkpoint-prefix | `<compact-boundary>` + summary 成对前缀原样保留，targetTotalTokens 先减前缀 token |
| tool-cycles-default | constrained 时按 user/assistant 边界切 tool cycle |
| constrained-target-tokens / constrained-total-with-checkpoint | targetTokens / targetTotalTokens 两条预算路径 |
| protected-default-tools / protected-memory-context / protected-custom-names | 默认保护 `read_skill`、`<memory-context>`、自定义 protectedToolNames 及各自 anchor |
| dangling-tool-pairs | 保留侧 tool_call 无 result、保留侧 tool_result 无 call：两侧剥离并上报 `danglingToolCallIds` |
| trailing-assistant | 末尾 assistant 后补 continuation user sentinel |
| empty-assistant-content-dropped / empty-user-content-dropped | TS 的 `stripUnpaired*` 末尾 `.filter(m => m.content.length > 0)` 对所有角色生效：保留侧本来就空的 assistant / user 消息也被删；负控断言输入含空消息、输出不含 |
| keep-head-turns-nan | `keepHeadTurns: NaN` 经 `Math.max`/`Math.min` 仍是 NaN，boundary 文本保留 `headTurns="NaN"`；JSON 用 `{$:'NaN'}` 记录，Rust 回放解码 |
| synthetic-anchor / media-reference-tool-result | `metadata.synthetic` 不作 anchor；`media_reference`（带 toolCallId）按 tool-result-only 归组 |
| few-results-skip | 可压缩 result 数 ≤ keepLatest，原样返回 |
| rewrite-oldest-keep-two | 6 个 read_file result，keepLatest=2，只重写前 4 个 |
| keep-latest-zero / keep-latest-negative / keep-latest-fractional | keepLatest 为 0、-1、1.5 时 TS 取到数组外/非整数下标，`undefined - 1 = NaN`，所有 result 都进重写窗口 |
| non-compactable-names | 非 COMPACTABLE_TOOL_NAMES 原样 |
| protected-tool-result / protected-override-null | protectedToolNames 拦截；input 传 null 时解除保护 |
| trim-to-tokens / trim-to-bytes | trimToTokens 与 trimToBytes→tokens 两条预算 |
| image-pdf-clear-and-size | 旧 user 消息里的独立 image/pdf 清成文本占位，`rewrittenBytes` 计入 data 长度；tool_result 内 image 也被压成文本 preview |
| tool-result-image-url | tool_result 内 `source:'url'` 的 image flatten 成 `[Image: <mime>, <url>]`（不是 base64 字符数），负控断言 preview 保留 url 原文 |
| unicode-token-slicing | 中文/emoji 的 UTF-16 **码元**二分切片；实际切点落在 BMP 文本内，不拆代理对（拆对路径由下一行单独覆盖） |
| surrogate-emoji-only / surrogate-emoji-triple / surrogate-mixed-prefix | 切点落在 UTF-16 码元边界上：`'😀👍🎉'.repeat(200)` + trimToTokens=96 产出 1 个孤立低代理项（oracle 中为 `\udf89`），oracle 保留 `\udXXX` 转义并由 Rust 逐字节回放；另两例分别产出 0 个孤立代理项作对照 |
| target-tokens-quirks / target-total-tokens-quirks | targetTokens 的 NaN、0、-5、+Infinity、2.7 与 targetTotalTokens 的 NaN、2.7、-3、+Infinity：NaN/Infinity 经 `Math.max` 保持不约束，0/负数/小数走 constrained 裁剪 |
| unknown-item-target-quirks | tool_result 含未知 `audio` item 时（原始 JsValue，超出 canonical typed 消息可达范围），targetTokens 取 10/14/15/16/17/18/20/24 的 8 个尾部裁剪结果 |
| micro-budget-quirks | trimToTokens=NaN、trimToBytes=NaN（各重写 2 条）、keepLatest=NaN / -Infinity（重写 3 条） |
| idempotent-second-pass / emergency-tighten-preview | 第二次跑不再改；768→256 应急预算可收紧已有 preview |

数值怪异场景只在 `options` / `callOptions` / `inputOptions` 上过 `jsonSafe`，把 NaN/±Infinity 记成 `{$:'NaN'}` / `{$:'Infinity'}` / `{$:'-Infinity'}`；input 与 result 保持原样。Rust 回放只在解析 option 字段时解码这些标签（`optional_f64`），遇到未知标签直接 panic，不会静默退化成 `None`。

`probes` 原值包括 locale、`TokenBudgetManager` 对 ASCII/CJK/emoji 的 token 数、两条负控（6 turn snip applied=false、2 个 result micro rewritten=0）以及第一条 snip boundary 原文。

范围：只证明不调模型的 snip / micro 两个引擎与它们共享的 tool-pair/protected-context/token 估算；`tryAutoCompact` 调度、全量 CompactionEngine/Orchestrator、durable context 记录接线留给后续 PR。durable 记录（compaction started/completed/failed、replacement、control_boundary）由 `tests/fixtures/compaction`（A6.6-pr2）覆盖。
