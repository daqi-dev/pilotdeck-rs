# #741 宿主 start 通知边界

Node v22.22.0 直接 import 固定 cc7c4e6 的 TurnRunner/recorder。显式 start gate 观察 success/failure/pending/queued return，5 秒上限仅作失败，未放行无事件。oracle 的聚合 notice 不是 TS 产物；Rust 扩展按 A6.9-DISCARD-NOTICE-001-R1 标为差异，closed/stale token 也不是 TS 语义。

在仓库根设置 PDRS_BASELINE 为只读 TS 基线后，用 `node "$PDRS_BASELINE/node_modules/tsx/dist/cli.mjs" src/pilotdeck-gateway/tests/fixtures/turn-start-notices/generate.mts <空输出目录>` 生成；mkdir recursive，oracle 写入 wx。Rust `runner::tests::notices::regenerates_identically` 为 ignored 守卫，必须显式执行。

负控（#996 起为编译期开关，旧 `negative_controls.py` 文本替换表已删除）：源码全部提交后从仓库根执行 `python3 src/pilotdeck-gateway/tests/fixtures/turn-start-notices/schemata.py <新证据目录> [开关名 ...]`；调用方必须设置本树 CARGO_TARGET_DIR，重生成开关（`notices-regeneration`）另需 PDRS_BASELINE。`--check` 只做锚点预检。9 个开关以 `crate::mutants::on("<名>")` 分支编译进 feature（`pdrs-mutants`）库测试二进制：先 `cargo test --lib --features pdrs-mutants --no-run` 预编译一次，再逐开关以 `PDRS_MUTANT=<名>` 独立进程运行正控/负控/还原相。执行器共享 `tests/fixtures/_shared/mutant_switch.py`，全程不改工作区源码；每项记录 KILLED/SURVIVED/INVALID 与原因，任一非 KILLED 退出非零。锚点过期、编译/设置失败、超时、无关 panic 均为 INVALID。
