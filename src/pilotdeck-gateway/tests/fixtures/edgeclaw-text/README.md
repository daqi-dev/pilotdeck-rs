# EdgeClaw pure helpers (A6.6-edgeclaw-1)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`：

- `core/utils/text.ts`（84 行）
- `core/utils/id.ts`（34 行）
- `core/general-projects.ts`（48 行）
- `core/trace-i18n.ts`（13 行）
- `service.ts:429-541`（113 行；`parseRawRecallSections` /
  `normalizeCaseTraceContextPreview` / `looksLikeRawRecallContext` 未导出，
  生成器用 tsx 的 esbuild 直接在原文件文本末尾追加导出后 import，
  不改写函数体）

`generate.mts` 先 `bindSourceTree` 绑定提交，再对五个入口做
`sourceClosureSha256`（含相对 import 闭包），随后直接调用真实 TS。
`HOME` 固定为 `/fixture/home`，时钟冻结在 `2026-10-22T00:00:00.000Z`；
`normalizeWorkspacePath` 的输出把生成时 cwd 清洗成 `$CWD`，Rust 侧用当前
cwd 展开，因此金样与生成目录无关。没有用 Rust 产生任何期望值。

覆盖 165 个观测：空串 / CJK / emoji / 孤立代理 / 超长文本、`\uXXXX` 与
common escape 两阶段解码（含 `\\u0041`、`\\n`、NUL）、JS `\s` 归一化、
`scoreMatch` 的相等/前缀/包含/词命中/二元组分支、`JSON.parse` 回退；
`hashText` 的 UTF-8 替换语义、全部 id 构造（含 `sourceIds.sort()` 与
`toLowerCase()`）、冻结 `nowIso`；General 常量的绝对/相对/空 HOME、
`path.resolve` 的 `.`/`..`/重复斜杠/尾斜杠、外部 project/record id 与
parse/is 判定；`traceI18n` 的属性顺序、数字/数组 String() 化（含数组内 null/undefined 按 `Array.prototype.join` 渲染为空串）；原始召回
块的四种分节、CRLF、`route=`/样板行/未知分节、`### [user]` 渲染、
multiline 检测；case trace preview 的替换/原样/缺字段。

生成两次并核字节（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-text/generate.mts /tmp/edgeclaw-text-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-text/generate.mts /tmp/edgeclaw-text-2
cmp /tmp/edgeclaw-text-1/oracle.json /tmp/edgeclaw-text-2/oracle.json
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_text
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_text -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-text/negative.py /tmp/edgeclaw-text-negative
```

`negative.py` 用 `mutating_sources` 锁定并恢复产品源码，逐条要求编译成功、
运行时断言转红：hash 截断位、召回分节标题、`normalizeText` 的 trim 语义、
General 路径判定、`route=` 行剥离、L0 空 session 回退、cwd 失败回退、homedir
失败吞错、数组 null 元素、深层解码递归。脚本不再自选构建目录：调用方必须
提供 `CARGO_TARGET_DIR`，未提供时在写任何文件前报错。由于沙箱不能提交，本
单在一份已提交的 `src/`+`scripts/` 完整副本中实跑（原始输出见
`_tmp/20261026-issue-758/fix-r1/`）。
