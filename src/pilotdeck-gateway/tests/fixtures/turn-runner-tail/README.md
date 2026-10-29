# TurnRunner 尾项金样（#742 / #936）

`generate.mts` 在固定 TS cc7c4e6、Node v22.22.0 上运行真实 AgentSession / TurnRunner / FileArtifactCollector / metadata writer。30 格回合矩阵和 21 格文件策略的期望保存在 `oracle.json`；每次生成另留 `raw/` 的事件、提交回调、JSONL、门闩快照、模型请求与日志。网络入口全部拒绝；辅助模型只有确定性替身。

时钟固定为 `2026-10-27T00:00:00.000Z`、entry ID 固定递增，便于逐字节比较事件与 JSONL。金样不包含 AgentSession 外层的 session_started/setup_completed/session_ended，Rust 用例比较 TurnRunner 负责的事件；外层事件仍在原始报告中。原始绝对路径、耗时、文件 mtime 只留 raw，未作为稳定期望。输出目录递归创建、文件独占创建；重跑应使用新目录。

Rust 的 `SessionTail` 提供可选 `TitleProvider` / `SuggestionGenerator`，其输入输出是类型化的确定性端口。native 模型请求、响应清洗和 native 错误日志属于端口上游：本包保留 TS 证据，但不宣称实现这些策略。端口未注入时不广告辅助模型能力；会话通过 `LiveSessionAdapter::with_tail` 显式绑定。恢复已有 metadata 时调用 `SessionTail::restore`。宿主接线测试也覆盖真实 HostClient、逐个尾项事件的背压和 drop 释放。

标题不阻塞回合终态；finalize 解绑 turn abort，允许晚标题追加 metadata；dispose 则取消并关闭 writer。suggestion 会在 durable turn_result 之后阻塞流终态，消费方 drop 取消等待。never-completing 端口与已进入的同步写入分别有释放/保留所有权测试。#936 的宿主 user_prompt_submitted 拒绝是明确授权差异，见 `specs/gateway-parity/diffs/A7.2-TURN-HOST-PREFIX-001.md`；旧 TS forward 金样保留。

从仓库根目录执行（沿用调用方唯一 target）：

```sh
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::turn::runner::tail::tests::
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::turn::runner::tail::tests::regenerates_identically -- --exact --ignored
python3 src/pilotdeck-gateway/tests/fixtures/turn-runner-tail/schemata.py --check
```

负控（#996 起为编译期开关，旧 `negative_controls.py` 文本替换表已删除）：必须在全部源码已提交且等于 HEAD 后运行；不能用副本仓库或改 HEAD 绕过守卫。46 个开关以 `crate::mutants::on("<名>")` 分支编译进 feature（`pdrs-mutants`）库测试二进制，从仓库根执行 `python3 src/pilotdeck-gateway/tests/fixtures/turn-runner-tail/schemata.py <新证据目录> [开关名 ...]`：先 `cargo test --lib --features pdrs-mutants --no-run` 预编译一次，再逐开关以 `PDRS_MUTANT=<名>` 独立进程运行正控/负控/还原相。执行器共享 `tests/fixtures/_shared/mutant_switch.py`，全程不改工作区源码；未设 CARGO_TARGET_DIR 或 PDRS_BASELINE 时退出。

```sh
python3 src/pilotdeck-gateway/tests/fixtures/turn-runner-tail/schemata.py _tmp/996-pr2-tail-mutants-run
```

每个开关绑定本单断言消息；anchor 过期、编译失败、错误测试失败都是 INVALID，不能算作 KILLED。契约见 `specs/negative-controls/README.md`。46 项完整负控适合交协调者在宿主运行，不在长会话中后台启动。
