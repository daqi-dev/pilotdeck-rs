# #1097 D1-P07 JSON / JSONL 输出金样

`source.json` 保留 18 个 D0 宿主原件与 Node v22.22.0 真 `runSdkCli` / SDK 函数探针的 66 个补充格；`generate.mjs` 只从这些原件机械提取 `oracle.json`（84 格）。`probe.mjs` 的 WebSocket double 只接受隔离用的 28999 URL，不代表真实网络或 OS 隔离验收；每格的 stdout/stderr bytes、退出状态、wire 顺序和初末磁盘快照均保留。TS 源文件及 dist 的 SHA-256 随补充原件保存。

覆盖 run/resume × json/jsonl/both × completed（有文本/无文本/结构化/结构化 null）/failed/result_unknown（空流/明确错误码），以及完整事件映射、24 个 context 字段、首终态、partial 过滤、原型名称、UTF-16、sequence 覆盖、重复 wire 帧、usage 类型、错误字段和换行。help 与 sessions 错误入口的格式组合复用 D0 原件。aborted/信号取消和 model-without-project 的未实现行为仍属于 P06/P08；不据此声明 D1 全部完成。

`tests/sdk_format_exit.rs` 精确比较 stdout、stderr、退出码/信号和完整落盘；不解析后重排实际输出。仅 TS stderr 开头的两行 SQLite 警告按 `specs/cli-parity/normalization.md` §6 剥离一次。回放中生成的新 UUID 仅实例化期望值中的已登记字段：wire envelope `id`、wire `params.runId` / `event.runId`、JSONL 公共事件顶层 `runId`、最终结果 `recovery.runId`（`--json` 时位于 `result.recovery.runId`）。实际 stdout、普通文本、嵌套 payload、其他字段均不改；原件 UUID 留在 source 中，替换点要求唯一。

输出层序号依赖现有 transport 的严格不变量：`start_stream` 从 -1 开始，`next_sequence` 只接受 previous+1 并先跳过重复。每次 `output.event` 的序数因而等于 frame.seq；在过滤 partial/重复终态前展开事件并覆盖 sequence。`run-jsonl-sequence-only`、`run-jsonl-duplicate-frame` 与全映射格约束覆盖值、次序和过滤。

## 重现

运行前检查 `scripts/cargo-slots.sh`；从仓库根调用，必须设置该 worktree 独立的 `CARGO_TARGET_DIR` 与两个 debug profile，Node 为 v22.22.0：

```sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli --test sdk_format_exit -- --include-ignored --show-output --test-threads=1
node src/pilotdeck-cli/tests/fixtures/sdk-format/probe.mjs <全新原始记录目录>
node src/pilotdeck-cli/tests/fixtures/sdk-format/generate.mjs <全新重生成目录>
```

`format_oracle_regenerates_identically` 为忽略的金样重生成守卫；正常测试主动包含它。生成器独占写文件，拒绝覆盖。

## 负控

`src/pilotdeck-gateway/tests/fixtures/sdk-format/negative_controls.py` 登记 99 项；使用共享 `mutating_sources`、`run_owned`、`wait_for_slot`，编译每个变异后才按完整测试名与专属 nonce receipt 判杀。每项正控、变异 no-run、负控、恢复正控都有日志，遍历全部项目后原子发布汇总；编译失败、无关 panic、超时、守卫拒绝均为 INVALID。

```sh
python3 src/pilotdeck-gateway/tests/fixtures/sdk-format/negative_controls.py --check
python3 src/pilotdeck-gateway/tests/fixtures/sdk-format/negative_controls.py --compile-only <全新编译证据目录>
python3 src/pilotdeck-gateway/tests/fixtures/sdk-format/negative_controls.py <全新负控证据目录>
```

只读 `--check` 可在补丁树执行。新文件或任一受保护源码不等于 HEAD 时拒绝变异，不允许影子仓库或替代基线。提交后由协调者宿主执行逐变异编译及全量。P05 的格式拒绝变异已由 P07 格式开关正反控替代；其余 P05/P06 旧负控只同步被输出重构改变的锚点。
