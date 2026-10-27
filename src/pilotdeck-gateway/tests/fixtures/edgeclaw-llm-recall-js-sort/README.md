# 回退排序可达域金样（#765 fix-r3）

本 fixture 锁定 `chooseBestRecallProjectFallback` 在**真实生产可达输入域**内与 TS
`Array#sort` 一致：`exact ∈ {0,2}`、`score = exact * 10 + matchedTokenCount`
（`matchedTokenCount` 0..12）、`sourceType ∈ {general_local, workspace_external}`、
`updatedAt` 取 4 个固定值（含重复值以制造平局）。

`generate.mts` 直接 import 固定提交 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`
的 `LlmMemoryExtractor`，用空配置构造（无模型时走 catch 后的
`chooseBestRecallProjectFallback` 路径），逐组记录真实 TS 选中的 `projectId`；
212 组，长度覆盖 2–70（含 70 跨过 V8 TimSort run 边界），外加全等、source 平局、
updatedAt 平局三个定点样本。随机数使用固定种子 mulberry32。

NaN / Infinity 不在金样内：真实入口 `buildProjectShortlist` 只产生上述有限值，
而含 NaN 的比较器不构成全序，V8 与 Rust 不保证一致（见 `recall.rs`
`js_sort_order` 注释与 RESULT）。

生成与守卫：

```sh
PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck \
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 node \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-recall-js-sort/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-recall-js-sort

PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_llm_recall -- --include-ignored \
  regenerates_js_sort_identically
```

Rust 逐组对拍：`js_fallback_sort_matches_ts_reachable_domain`；负控
`negative.py ... sort-reachable-order`（把 score 比较改成升序）必须让该测试以
`#765 reachable-domain sort first mismatch` 失败。
