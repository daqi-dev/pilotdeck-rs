# EdgeClaw transcript normalization (A6.6-edgeclaw-4)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`
的 `src/context/memory/edgeclaw-memory-core/src/message-utils.ts`（645 行）。

`generate.mts` 先 `bindSourceTree` 绑定提交，再对 `message-utils.ts` 的
相对 import 闭包做 `sourceClosureSha256`，随后用 tsx/esbuild 在原文件末尾
追加私有函数导出（不改函数体）并直接调用真实 TS。`HOME` 固定为
`/fixture/home`，时钟冻结在 `2026-10-22T00:00:00.000Z`。`message-utils.ts` 不读
`Math.random`；混合字符串语料用固定种子的 LCG，重复生成字节一致。

覆盖 2424 个观测（含 80 条确定性混合字符串，逐函数对拍）：

- 文本抽取：type 白名单/黑名单、`message_text`、优先键与列表键、去重、
  深度 3/4/5/6 的数组与对象链边界、孤立代理；
- `stripInjectedMemoryContext` 的 canonical / legacy / recall 三段、
  CRLF、缺失 footer、`System:` 前导行；
- `compactWhitespace`、`normalizePluginNoiseProbe` 的 JS `\s`（含
  U+FEFF）、`**`/反引号/行首标记、插件状态行与段落的 4 标记阈值；
- sender 元数据（fence / inline、JSON 坏值、去重、小写折叠）、
  `chat history`/`Replied message`、`[Chat messages since your last reply…]`
  前瞻（含无 `[Current message…]` 的 `$` 分支）；
- 时间戳前缀、附件脚手架（媒体行、`To send an image back,`、`<file>`）、
  当前消息信封（320/321 单元、120/121 标签、emoji 计码单元）；
- 前导 @ 提及、信封噪声行全集、slash / bang 命令、session marker；
- thinking 块（`</think>`、成对、属性、贪心多段、中文元推理）；
- 大小写折叠边界：JS 非 Unicode `/i` 的 `Canonicalize` 不把 ſ、K
  (U+212A)、ß/ẞ、İ/i、ı/I 折到 ASCII，Σ/σ/ς 与 µ/Μ 则互折；每个
  `(?i)` 正则与 `ci_eq`/`starts_with_ci`/`find_ci` 都有对应观测；
- 两个 `[Current message - respond to this]` 时 `extractCurrentMessageSegment`
  取最后一个（TS `lastIndexOf`）；
- inspect 的 canonical / nested / tool call / toolUseResult / isMeta 等
  拒绝分支，`normalizeMessages` 的 `captureStrategy × includeAssistant ×
  maxMessageChars` 组合；
- 孤立代理、astral 字符（每个 emoji 计两个 UTF-16 单元）、补充私用区；
- LCG 生成的 80 条混合字符串（含 `[`/`]`/`:`/`@`/`!`/`\r`/U+2028/孤立代理/emoji）
  跑 17 个 strip / 判定 / 命令函数与 `extractTextFromContent`。

生成两次并核字节（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-messages/generate.mts /tmp/edgeclaw-messages-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-messages/generate.mts /tmp/edgeclaw-messages-2
cmp /tmp/edgeclaw-messages-1/oracle.json /tmp/edgeclaw-messages-2/oracle.json
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_messages
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_messages -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-messages/negative.py /tmp/edgeclaw-messages-negative
```

`negative.py` 有 15 条可逆负控（深度、插件阈值、session 阈值、sender hint
前导空格、前瞻、媒体行最后一个 `]`、toolCall、bang 空白、时间戳年份宽度、
last_turn 切片、thinking 贪心、大小写 canonicalize / 正则 / ci_eq、当前消息
取第一个标记），每条要求编译成功且运行时断言转红。脚本不会在第一个存活
变异体处中止：它逐个跑完，最后打印 KILLED/SURVIVED，有存活即退出 1。新增
文件在提交前不在 HEAD，`mutating_sources` 会拒绝运行；脚本由协调者在提交
后执行，cargo 使用调用方传入的 `CARGO_TARGET_DIR`，未设置时提前报错。
作者树可用 `NEGATIVE_BASELINE=$(git stash create)` 指向当前工作树的悬空
提交来跑样本，默认仍是 `HEAD`。
