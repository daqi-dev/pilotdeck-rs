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
`"throws": {"name": "TypeError" | "RangeError", "message": "..."}`，Rust 侧必须
返回 `FileMemoryError::Type` / `FileMemoryError::Range` 且消息逐字节相同
（#826）。`String(reason)` 走 `ToPrimitive` 与 `Array#join`：`{toString:null}`
（直接或数组元素）抛 `TypeError: Cannot convert object to primitive value`；
3000 层数组抛 `RangeError: Maximum call stack size exceeded`（V8 实测阈值
2516，Rust 保守常量 2000，3000 两边都抛）。3000 层数组在 `oracle.json` 里写成
无损哨兵 `{"$deepArray": 3000}`（`decode` 展开为 3000 层、叶子 `"x"`）：真实
TS 调用用的是真数组，但逐层缩进的金样会膨胀到约 14 MB。

`negative_shapes.py` 的每个变异体在 `oracle.json` 里登记目标
`(fn, args[0], throws)`：脚本算出该用例的稳定 ID `<fn>#<fn 内序号>`，只有
日志同时出现 `test <test> ... FAILED` 与该 `case=<id>` 才算 KILLED；先失败的
兄弟用例、无关 panic、编译失败都不算。目标签名找不到即 `STALE TARGET` 退出 2。

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
`dream/helpers.rs` 的一个形状守卫（或 `String(reason)` 的错误映射），必须被
oracle 指定的那条用例杀掉；逐格汇总 KILLED / SURVIVED，有存活退出 1。
`NEGATIVE_CHECK_ANCHORS=1` 只对当前工作树核对 24 个锚点，不做变异；正式
运行仍走 `mutating_sources`，默认基线 `HEAD`。作者树上的 `helpers.rs` 未提交
时会被「源码须等于 HEAD」守卫拒跑（不建影子 git、不建第二份构建目录）：
提交后由协调者在宿主跑全量，或 `NEGATIVE_ONLY=a,b` 跑样本；未提交树可用
`NEGATIVE_BASELINE=$(git stash create)` 指到当前工作树的悬空提交。
