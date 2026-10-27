# #741 宿主 start 通知边界

Node v22.22.0 直接 import 固定 cc7c4e6 的 TurnRunner/recorder。显式 start gate 观察 success/failure/pending/queued return，5 秒上限仅作失败，未放行无事件。oracle 的聚合 notice 不是 TS 产物；Rust 扩展按 A6.9-DISCARD-NOTICE-001-R1 标为差异，closed/stale token 也不是 TS 语义。

在仓库根设置 PDRS_BASELINE 为只读 TS 基线后，用 `node "$PDRS_BASELINE/node_modules/tsx/dist/cli.mjs" src/pilotdeck-gateway/tests/fixtures/turn-start-notices/generate.mts <空输出目录>` 生成；mkdir recursive，oracle 写入 wx。Rust `runner::tests::notices::regenerates_identically` 为 ignored 守卫，必须显式执行。

源码提交后运行 `python3 src/pilotdeck-gateway/tests/fixtures/turn-start-notices/negative_controls.py <新证据目录>`；调用方必须设置本树 CARGO_TARGET_DIR 和 PDRS_BASELINE。脚本经 negative_suite 使用 mutating_sources/run_owned/wait_for_slot，只接受 HEAD，不支持自设基线或备用构建目录。`--audit` 只核锚点；输出目录后加 `--sample` 仅跑 wiring，或给变异名列表。每项记录 KILLED/SURVIVED/INVALID 并全部汇总，任一非 KILLED 退出非零。编译失败、无关 panic、失配锚点不是 KILLED。
