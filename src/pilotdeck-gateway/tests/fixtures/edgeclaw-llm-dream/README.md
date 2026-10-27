# EdgeClaw Dream-side LLM calls (A6.6-edgeclaw-14, #771)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`
的 `core/skills/llm-extraction.ts`（`rewriteUserProfile` 及画像辅助、
`planDreamClusters`、`refineDreamCluster`、`planGeneralProjectMetaMerges`、
`reviewDreamProjectMeta`，prompt `:565-586,696-830`，构造 `:918-994,1235-1312`，
规范化 `:1563-1618`）。`generate.mts` 直接 import 该文件并用 `bindSourceTree`
绑定提交；`dream-review.ts` 只用于导出 `validateExclusiveClusters` /
`validateGeneralProjectMergeGroups`，把“互斥簇校验前的原始输出 → 校验后”两段
都记入金样。

`HOME` 隔离到临时目录，时钟冻结在 `2026-10-22T00:00:00.000Z`，`Math.random`
定序为 `0.25`。每个用例起一个 127.0.0.1 随机端口 loopback 桩；`requests`
记录 method、url、小写头（host 归一为 `$HOST`）、body 原字节与虚拟时间。
`expected.result` 是方法规范化后的返回值；`expected.debug` 是 `debugTrace`
收到的记录（含 `parsedResult`，即互斥簇校验前的原始解析对象）；
`expected.validation` 是同一原始输出交给 dream-review 校验器的结果。

生成两次并核字节（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-dream/generate.mts /tmp/edgeclaw-llm-dream-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-dream/generate.mts /tmp/edgeclaw-llm-dream-2
cmp /tmp/edgeclaw-llm-dream-1/oracle.json /tmp/edgeclaw-llm-dream-2/oracle.json
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_llm_dream
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_llm_dream -- --include-ignored
```

`#[ignore]` 的 `regenerates_identically` 守卫会重跑同一生成器并要求逐字节一致。

## 负控

`negative.py` 用 `negative_process.mutating_sources` 锁定并恢复产品源码，先校验
每个变异锚点在产品源码中恰好出现一次（锚点过期报 `STALE ANCHOR` 并退出 2，
不冒充 SURVIVED），再跑未变异套件要求退出 0；逐格要求测试失败且日志出现该
格专属的 `#771 ...` 断言消息，逐格汇总 KILLED / SURVIVED，有存活退出 1。
cargo 强制使用调用方 `CARGO_TARGET_DIR`。

新文件在提交前不在 HEAD，`mutating_sources` 会按设计拒跑；本树只在
`NEGATIVE_BASELINE=$(git stash create)` 的悬空提交上跑 1–2 个样本，其余由
协调者在提交后跑全量。禁止影子 git、第二份构建目录或任何绕过源码等于
baseline 守卫的方式。
