# EdgeClaw dream helpers (A6.6-edgeclaw-15)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`：

- `core/review/dream-review.ts:73-531` 的纯函数：`createDreamTrace` /
  `pushStep` / `mutation`、`sortEntries` / `sortProjectMetas`、
  `toDreamMetaInput` / `toDreamRecordInput` / `toHeaderInput` /
  `toRefinedCandidate`、`validateExclusiveClusters`、
  `selectUserNoteWindow`、`validateGeneralProjectMergeGroups`。

`generate.mts` 先 `bindSourceTree` 绑定提交，再对 `dream-review.ts` 做
`sourceClosureSha256`（含相对 import 闭包），随后直接调用真实 TS。helper 未
导出，生成器照 `edgeclaw-text` 的 `service.ts` 做法用 tsx/esbuild 在源文本
末尾追加 `export { ... }`，不改函数体。

`HOME` 固定为 `/fixture/home`，时钟冻结在 `2026-10-22T00:00:00.000Z`。
`Math.random` 由 `withRandom` 定序；`createDreamTrace` 的第三个参数记录
`Math.random().toString(36).slice(2, 10)`，`mutation` 的随机值以 f64 记录，
Rust 侧用既有 `js_value::number_string` 还原 TS 模板串。`toDreamRecordInput`
的金样调用真实 `FileMemoryStore.prototype.toCandidate`（不构造 store、不落盘）。
`validateExclusiveClusters` 的 reason 金样含 U+017F `ſ` / U+212A `K` 以及
`İ` / `ı`：TS 正则不带 `u` 标志，`/i` 只折叠 ASCII；Rust 侧复用
`js_value::ascii_fold_haystack` 屏蔽前两个码点。

`add()` 对 expected 先做 `JSON.parse(JSON.stringify(...))`，让金样与 Rust 的
JSON 线格式同语义（对象里的 `undefined` 属性被丢弃）。

生成两次并核字节（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream/generate.mts /tmp/edgeclaw-dream-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream/generate.mts /tmp/edgeclaw-dream-2
cmp /tmp/edgeclaw-dream-1/oracle.json /tmp/edgeclaw-dream-2/oracle.json
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_dream_helpers
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_dream_helpers -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream/negative.py /tmp/edgeclaw-dream-negative
```

`negative.py` 用 `mutating_sources` 锁定并恢复产品源码，先跑未变异套件要求
退出 0，逐格要求编译成功且运行时断言转红；逐格汇总 KILLED / SURVIVED，有
存活退出 1。脚本 cargo 强制使用调用方 `CARGO_TARGET_DIR`。新增文件在提交前
不在 HEAD，默认 `mutating_sources` 会拒跑；协调者提交后在宿主跑全量，或
`NEGATIVE_BASELINE=$(git stash create) NEGATIVE_ONLY=...` 跑样本。
