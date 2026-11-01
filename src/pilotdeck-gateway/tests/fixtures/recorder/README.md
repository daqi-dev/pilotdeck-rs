# Durable recorder TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测入口 `src/agent/session/AgentSessionEventRecorder.ts` 的 `startTurn`、`recordModelRequest`、`recordModelEvent`、`recordToolCalls`、`recordToolResults`；同批源码（含 `durableModelInvokerPort.ts`、`durableToolPort.ts`、writer/runtime/域校验/JSONL 持久化）的 SHA-256 记在 `oracle.json`。所有记录由真实 `AgentSessionEventRecorder` + `JsonlTranscriptWriter` + `SessionRuntime` + `JsonlSessionPersistence` 写入每次运行独占的临时会话目录，无实现源码复制；环境准备（Node v22.22.0、基线 lockfile 的 tsx、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/recorder/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/recorder
```

生成器先断基线的已跟踪文件无改动，并按 `cc7c4e6` 的提交内容复算每个来源的 `sourceSha256`。每次运行在输出目录下 `mkdtemp` 一个 `.generate-*` 临时目录，只清理本次创建的目录；会话 transcript 与步骤返回值都取自真实 TS，随机 UUID/目录归一化后写入 `oracle.json`；确定性场景的 `id-N` 和递增时间原样保留，以验证失败调用是否消耗源。

`oracle.json.scenarios[]`：`name`、可选 `options`（时钟/ID 源）、`steps[]`（输入 + TS 返回值或 `{error:{name,code?,message}}`）、`files`（会话目录下全部文件原文）。覆盖：

| 场景 | 覆盖 |
|---|---|
| model | 首次请求开 step 1；事件逐条落盘；第二次请求先写 `step_completed` 再开 step 2；request 中的 `raw` 保留 |
| tools | `tool_call` 先于执行、`tool_result` 后于执行；空结果集也会关闭 step；`step_completed` 的序号与 parent 链 |
| raw | `model_stream_event`、`tool_call`、`tool_result` 递归剔除 `raw`（含数组与嵌套对象），request 不剔除 |
| errors | 未 start 的 model/tool 调用；重复 start；未开 step；`tool_result_without_call`；`duplicate_tool_call`；`step_has_open_tool_calls`（前一条 result 已落盘）；null call/result 的 V8 TypeError；失败不发布 sequence |
| restart | 重开 writer 并 `SessionRuntime.restore` 后继续，序号/parent 链与域从持久化历史恢复 |
| interleaved-writer | 同一 writer 的 accepted_input / metadata 与 recorder 交错追加、恢复后继续；整份日志逐字节相等 |
| proto-payloads | JSON.parse 构造自有 `__proto__`；event/call/result 递归丢弃，request 保留 |
| typed-tool-ids / numeric-id-mismatch / reference-tool-ids / id-coercion-error | 数字、布尔、null、undefined、字符串保留类型；对象/数组按每条 materialize 后的引用语义；错误消息的 ToString 失败 |
| failure-source-order / request-failure-source-order | 递增秒钟、显式 ID 验证重复 turn/call、null 值、缺 request、未完成 call 等失败前后的源消耗与提交序号 |
| restore-*（11 组） | 原评审四种非法历史及 step/request/tool 边界；恢复失败后原 turn/call 仍可完成，序号/父链/域均未半发布 |
| envelope-*（4 组） | 空 ID、自指父链、恢复后的历史 ID 重复、安全整数溢出；全部拒绝且不落盘 |

共 **28** 个真实 TS 场景。生成器与 Rust 默认测试 `real_ts_scenarios_match_outcomes_and_bytes` 均断言场景数精确等于 28，防止金样再生或合并时静默缩水；新增或删除场景须同步更新两处计数与本 README。Rust 逐场景重放，把步骤结果与整份 transcript 文件序列化后与 oracle 逐字节比较。归一化只有两条：场景目录 → `<dir>`；v4 UUID → `<id-N>`（按整份场景文本首次出现顺序）。

计数负控须提交后独占本树运行：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
timeout 1860 python3 src/pilotdeck-gateway/tests/fixtures/recorder/fix3-negative.py
```

`fix3-negative.py` 复用 `../_shared/negative_control.py`、`../negative_process.py` 和 `../negative_suite.py`，仅在 `/tmp` 副本对 Rust 读入的 oracle 和 TS 待发布的 scenarios 分别增加 / 删除 1 项，四项均须因 `recorder oracle scenario count` 计数断言失败；Rust 变异保留 `cargo test --no-run` 编译回执，TS 生成器变异只记 node 运行回执（cargo 编译的是未变异 Rust）。生成器失败不得发布 oracle，恢复后正控须通过。全部处理并完成守卫恢复后打印逐项 JSON 与 `SUMMARY:`，任一非 KILLED 即非零退出。Rust 仅测试构建经仅测试可见的 `PDRS_RECORDER_ORACLE` 读入副本，其余快照仍取本目录。变异只写副本，沿用调用方的 `CARGO_TARGET_DIR`；所有 cargo 调用（含第一次正控的冷编译）一律 900 秒编译级超时，node 运行 240 秒。

同一树锁窗口内，先校验 oracle、生成器、脚本自身与共享 Python 裁决依赖均等于 HEAD——`mutating_sources` 经 #1166 共享 proc-macro2 扫描器证明全部 workspace 字面构建输入等于 HEAD，Rust 判据链（`tests/recorder.rs` 经 `mod`/`#[path]`/`include_*!` 编入的 `tests/common/baseline.rs` → `regeneration_masks.rs` → `_shared/regeneration-masks.tsv`）随之在守卫内，脚本不携带任何手写 Rust 词法器（#1174），只锚定判据入口 `tests/recorder.rs` 必在扫描器输入集内，判据挪出即在任何正控前报错——再发现并校验生成器的完整本地 import 闭包（`generate.mts` → `../_shared/baseline.mts` → `./generator_guard.mjs` 与 `../../baseline_source_guard.mjs`，#1139），随后按原仓库相对布局复制到 scratch。闭包追踪 fail-closed：注释分隔或无插值模板的字面量动态 import 仍收编（含尾随注释与尾逗号），模板插值 / 变量 / 拼接 / 仓库外绝对路径一律拒跑；唯一允许的计算式参数是逐字匹配 `BASELINE_LOADERS` 白名单的两条固定基线加载器（提及 baseline 不算数，其源码 SHA-256 记录在 oracle.json）；`file:` URL 按绝对路径规则处理，`node:` 内建与裸模块名按静态 import 跳过，其余 scheme 拒跑。守卫覆盖 `negative_control.py`、`negative_process.py`、`negative_suite.py`、`negative_panic.py`、`pending_reanchor.py`。子进程阶段的套件向 scratch 传 **1800 秒**总预算，外层 **1860 秒**保留 60 秒清理余量。fail-closed，不产生假 PASS。

闭包与守卫语义由 `../test_import_closure.py` 单测覆盖（纯函数 + #1166 扫描器输入集断言，无需树锁、不启动 cargo；扫描器用例需先 `cargo build --example workspace_inputs` 预构建），`../closure_guard_negative.py` 在共享源码守卫内——守卫同时覆盖两个被变异 runner 与被绑定判据 `test_import_closure.py` 本身——对闭包分类与判据锚做十八项变异（含 run-registry 镜像、helper 漂移与判据锚探针）、进程内跑被绑定单测并断言命中专属失败消息，任一非 KILLED 即非零退出；`--check-anchors` 只验锚点唯一与变异体可解析，不进树锁；SIGTERM/SIGHUP 会经 finally 恢复被变异源码。

交叉测试 `real_ts_and_rust_continue_each_others_transcripts` 由 `generate.mts <dir> --cross-ts-write` / `--cross-ts-verify` 驱动：

```sh
cd src
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway --test recorder -- --include-ignored
```

TS 写 14 条 → Rust restore 后续写 7 条 → TS restore 校验序号/parent 链后再写 7 条；反向 Rust 写 14 条 → TS 校验并续写 7 条 → Rust 再续写。两侧都断言序号连续、parent 链完整；先确认 `model_stream_event` 类型，再精确比较 event 全值，call/result 也比较实际载荷。`cross_event_check_rejects_wrong_entry_missing_payload_and_raw_leaks` 为同一检查注入错误索引（model_request）、缺 event、raw 泄漏，确保不会恒真通过。

额外 Rust 回归覆盖持久化失败后的域/历史 ID 不发布、两个 recorder 共享域，以及 recorder 与 writer 并发追加时的连续序号、唯一 ID、完整父链。会话集成须用 `Recorder::from_writer(Arc<TranscriptWriter>)` 注入现有 writer；`new/with_clock/with_sources` 是独立会话的便利构造器，不能另建后与已有 writer 指向同一路径。

范围：只证明 durable model/tool 记录与 A6.2 transcript 存储同格式；未接 host、`ProjectRuntimeRegistry` 或 gateway method，不宣称宿主侧 model/tool port 已本地化。compaction 生命周期由 `tests/fixtures/compaction` 单独覆盖；question/permission/inbox 生命周期留给后续 PR；`completeTurn` 的独立耐久验收见 [`turn-runner`](../turn-runner/README.md)。
