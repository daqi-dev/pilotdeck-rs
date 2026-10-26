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
JSON 线格式同语义（对象里的 `undefined` 属性被丢弃）。畸形输入走
`addThrows()`：记录 V8 的 `name` / `message`，金样条目写成
`"throws": {"name": "TypeError", "message": "..."}`，Rust 侧必须返回
`FileMemoryError::Type` 且消息逐字节相同（#826）。

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
CARGO_TARGET_DIR="$CARGO_TARGET_DIR" python3 \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream/negative_shapes.py \
  /tmp/edgeclaw-dream-negative-shapes
```

`negative.py` 用 `mutating_sources` 锁定并恢复产品源码，先校验每个变异锚点
在产品源码中恰好出现一次（锚点过期报 `STALE ANCHOR` 并退出 2，不冒充
SURVIVED），再跑未变异套件要求退出 0，逐格要求编译成功且运行时断言转红；
逐格汇总 KILLED / SURVIVED，有存活退出 1。脚本 cargo 强制使用调用方
`CARGO_TARGET_DIR`。

`negative_shapes.py` 是 #826 的一组同构变异：每格撤掉
`dream/helpers.rs` 的一个形状守卫，必须被 oracle 的 `throws` 用例杀掉。
`NEGATIVE_CHECK_ANCHORS=1` 只对当前工作树核对 22 个锚点，不做变异；正式
运行仍走 `mutating_sources`，默认基线 `HEAD`。作者树上的 `helpers.rs` 未提交
时会被「源码须等于 HEAD」守卫拒跑（不建影子 git、不建第二份构建目录）：
提交后由协调者在宿主跑全量，或 `NEGATIVE_ONLY=a,b` 跑样本。
