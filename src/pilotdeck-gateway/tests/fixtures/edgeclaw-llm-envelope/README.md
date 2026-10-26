# EdgeClaw LLM envelope and normalizers (A6.6-edgeclaw-7)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`：

- `core/skills/llm-extraction.ts:832-844`（`truncate` /
  `normalizeWhitespace`）
- `core/skills/llm-extraction.ts:1313-1419`（`extractFirstJsonObject` /
  `extractLooseJsonEnvelope` / loose 字符串与布尔解析 /
  `tryParseLooseMemoryCreatePayload`）
- `core/skills/llm-extraction.ts:1619-1658`（`truncateForPrompt` /
  `normalizeStringArray` / `uniqueStrings`）
- `core/skills/llm-extraction.ts:2025-2041`（`normalizeMemoryRoute` /
  `normalizeBoolean`）

`observe.mts` 用 tsx 的 esbuild 把真实源文件转成 ESM，在文本末尾追加导出后
以 data URL import；该文件的唯一 import 是 `import type`，转译时被擦除，函数
体未改写。`generate.mts` 先 `bindSourceTree` 绑定提交，再对入口做
`sourceClosureSha256`（含 `core/types.ts` 闭包），随后直接调用真实 TS。
`HOME` 固定为 `/fixture/home`，时钟冻结在 `2026-10-22T00:00:00.000Z`，
`Math.random` 固定为 `0.5`。

覆盖 238 个观测：extractFirstJsonObject 的前后缀散文、代码围栏、截断 JSON、
转义引号/反斜杠、嵌套大括号、多对象、BOM、孤立代理、字符串内的 `}`；
extractLooseJsonEnvelope 的 first-`{`/last-`}`、多对象、`}{`；loose 布尔的
非 unicode `/i`（`"SKIP"`、`"é"`/`"É"`、`"σ"`/`"ς"` 命中；`"skıp"`、`"ſkip"`、`"ß"`/`"ẞ"`、`"Å"`/`"Å"` 不命中，Node 实测）、正则元字符
键转义、JS `\s`（NBSP/FEFF）；loose 字符串的 lazy（下一键）与 greedy
（`\s*}\s*$`）两种回溯、空捕获、转义解码顺序（含字面 `\r\n` 先于单独 `\r`/`\n` 解码、双反斜杠保留）；payload 的必填三字段、
`skip` 三态、错误传播；`normalizeStringArray`/`uniqueStrings`/`truncate` 的
JS `slice(0, max)` 负值/NaN/Infinity/小数语义、UTF-16 截断、JS trim；route
大小写敏感与布尔字符串归一化。

生成两次并核字节（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-envelope/generate.mts /tmp/edgeclaw-envelope-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-envelope/generate.mts /tmp/edgeclaw-envelope-2
cmp /tmp/edgeclaw-envelope-1/oracle.json /tmp/edgeclaw-envelope-2/oracle.json
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_llm_envelope
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_llm_envelope -- --include-ignored
CARGO_TARGET_DIR="$CARGO_TARGET_DIR" python3 \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-envelope/negative.py /tmp/edgeclaw-envelope-negative
```

`negative.py` 有 26 个变异体（object 短路/转义扫描、envelope 边界、解码末步与
CR-LF 顺序、lazy/greedy 捕获与尾部空白、负 slice、去重、prompt 先归一化、route
大小写、布尔小写、payload 必填/吞错、ASCII 与非 ASCII 折叠，以及 r1 P2 补的
`normalizeStringArray` 单串/空白/非串/非数组、空捕获、布尔 `false` 分支、
`key_at` 键折叠、`comma_key_at` next-key 过滤）。每个变异都要求编译成功且
指定测试运行时转红；一例存活不会中止，脚本逐例跑完并汇总
`KILLED / SURVIVED / INVALID`，有存活或无效即退出 1。

```sh
# 列表与单例（宿主可先跑 decode-crlf 样本）
python3 .../negative.py --list
CARGO_TARGET_DIR="$CARGO_TARGET_DIR" python3 .../negative.py /tmp/edgeclaw-envelope-negative decode-crlf
```

26 个变异体逐个重编译；本会话按任务只跑 `decode-crlf` 样本（实测单例编译 6.6 s），其余 25 例
由协调者在宿主执行。
