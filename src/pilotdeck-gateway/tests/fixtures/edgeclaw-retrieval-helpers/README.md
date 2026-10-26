# EdgeClaw retrieval helpers (A6.6-edgeclaw-9)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`：

- `core/retrieval/reasoning-loop.ts:23-363`（`normalizeQueryKey`、
  `buildTraceId`、`render{UserSummary,ProjectMeta,SelectedRecords}Block`、
  `renderContext`、`recentUserMessages`、CJK 分词与停用词、
  `buildProjectShortlist`、`createTrace` / `pushStep`、`fallbackSelection`）

`generate.mts` 先 `bindSourceTree` 绑定提交，再对 `reasoning-loop.ts` 做
`sourceClosureSha256`（含相对 import 闭包），随后用 tsx 的 esbuild 在原文件
文本末尾追加导出后直接 import，真实调用 TS。`HOME` 固定为 `/fixture/home`，
时钟冻结在 `2026-10-22T00:00:00.000Z`，`Math.random` 用固定序列替换；随机数
先由 TS 转成 `toString(36).slice(2, 10)` 后作为参数记录进金样，Rust 侧只接收
这个注入串。`buildProjectShortlist` 的 `localeCompare` 使用 Node 默认 locale
（生成时断言为 `en-US`），Rust 侧显式传入 en-US 的 ICU collator，不读宿主
locale。

生成两次并核字节（从仓库根目录运行；必须 `LANG=en_US.UTF-8` 且移走
`LC_ALL`，否则 Node 默认 locale 是 `und`）：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 node \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-retrieval-helpers/generate.mts /tmp/e766-1
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 node \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-retrieval-helpers/generate.mts /tmp/e766-2
cmp /tmp/e766-1/oracle.json /tmp/e766-2/oracle.json
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_retrieval_helpers
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_retrieval_helpers -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-retrieval-helpers/negative.py /tmp/e766-negative
```

`negative.py` 用 `mutating_sources` 锁定并恢复产品源码，逐条要求编译成功、
运行时断言转红：`\p{Script=Han}` 切分、`localeCompare` 排序（`"a"` 在
`"B"` 前，字节序相反）、normalize 的 trim、project meta description 回退、
user 文件去重、trace 时钟注入、fallback 3 条上限、最近 4 条用户消息、shortlist
30 条上限、step 序号、`hasUserSummary`、CJK 停用词 `当前` / ASCII 停用词
`general`、exact 权重 10、render 路由白名单 user/project/mix（`auto` 不得
进入、`none`/`auto` 落空串）。

脚本先跑一遍未变异套件并要求通过，再逐格执行；单格失败不中止，最后打印
`KILLED`/`SURVIVED` 汇总，有存活项时退出 1。每格必须命中对应测试的
`assert_eq!` 失败且日志含该函数的 `args=` 锚点；`could not compile` 不算杀死。
全部字节恢复后再跑一遍同一套件并要求通过（`restored.log`）。
`NEGATIVE_ONLY=a,b` 可只跑指定格做样本（未变异基线仍先跑）。脚本必须由
调用方提供 `CARGO_TARGET_DIR`，未提供时在写任何文件前报错；
`mutating_sources` 要求 gateway 产品树与基线一致（默认 `HEAD`，可用
`NEGATIVE_BASELINE` 指定悬挂提交）。本树 `helpers.rs` 已在 HEAD，产品树干净
时可直接运行；全量 16 格由协调者在宿主执行。
