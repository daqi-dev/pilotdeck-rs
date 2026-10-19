# Compaction snip / micro TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测 TS 入口：`src/context/compaction/SnipEngine.ts` 与 `src/context/compaction/MicroCompactionEngine.ts`，连同 `toolPairIntegrity.ts`、`protectedContext.ts`、`TokenBudgetManager.ts`、`tokenizer.ts` 与 `model/protocol/toolResultContent.ts` 的 import 传递闭包。`oracle.json` 记录整棵 `src` 的来源守卫（`bindSourceTree`）、闭包内每个文件的 SHA-256、en-US locale 断言、探针原值，以及 29 个场景的真实 TS 输入/输出。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
  node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/compaction-snip-micro/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/compaction-snip-micro
cd src
cargo test --locked --offline -p pilotdeck-gateway --test compaction_snip_micro
```

生成器先跑 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`（显式 Git 绝对路径、HEAD 固定、已跟踪树干净、整棵 `src` 逐 blob 比对），再按 import 传递闭包复算 `sourceSha256`。每次运行在输出目录下 `mkdtemp` 一个 `.generate-*` 临时目录，只清理本次创建的目录；`oracle.json` 先写 `.tmp` 再 rename。生成器连跑两次逐字节一致（没有随机值；token 数与消息字节都来自真实 TS）。

`oracle.json.scenarios[]`（29 组，Rust 逐场景按序序列化比较）：

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
| synthetic-anchor / media-reference-tool-result | `metadata.synthetic` 不作 anchor；`media_reference`（带 toolCallId）按 tool-result-only 归组 |
| few-results-skip | 可压缩 result 数 ≤ keepLatest，原样返回 |
| rewrite-oldest-keep-two | 6 个 read_file result，keepLatest=2，只重写前 4 个 |
| non-compactable-names | 非 COMPACTABLE_TOOL_NAMES 原样 |
| protected-tool-result / protected-override-null | protectedToolNames 拦截；input 传 null 时解除保护 |
| trim-to-tokens / trim-to-bytes | trimToTokens 与 trimToBytes→tokens 两条预算 |
| image-pdf-clear-and-size | 旧 user 消息里的独立 image/pdf 清成文本占位，`rewrittenBytes` 计入 data 长度；tool_result 内 image 也被压成文本 preview |
| unicode-token-slicing | 中文/emoji 的 UTF-16 二分切片 |
| idempotent-second-pass / emergency-tighten-preview | 第二次跑不再改；768→256 应急预算可收紧已有 preview |

`probes` 原值包括 locale、`TokenBudgetManager` 对 ASCII/CJK/emoji 的 token 数、两条负控（6 turn snip applied=false、2 个 result micro rewritten=0）以及第一条 snip boundary 原文。

范围：只证明不调模型的 snip / micro 两个引擎与它们共享的 tool-pair/protected-context/token 估算；`tryAutoCompact` 调度、全量 CompactionEngine/Orchestrator、durable context 记录接线留给后续 PR。durable 记录（compaction started/completed/failed、replacement、control_boundary）由 `tests/fixtures/compaction`（A6.6-pr2）覆盖。
