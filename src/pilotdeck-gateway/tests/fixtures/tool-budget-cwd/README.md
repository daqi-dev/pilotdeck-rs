# ToolResultBudget cwd（#885）

Node v22.22.0 实跑固定基线 cc7c4e6 的 `new ToolResultBudget({ toolResultsDir })`
（`src/context/budget/ToolResultBudget.ts:94` 构造器里的 `resolve`）。
`generate.mts` 每格新子进程，原入口导入在 chdir 前；删除 cwd 后不提前读取它，避免污染
Node 的 cwd 缓存。输出 recursive mkdir + `flag:'wx'`，oracle 保存源码 SHA256。

6 格：相对/绝对 × 健康/删除 cwd（4）；相对 × 暖缓存后健康/删除（2）。
成功格核对 `(budget as any).toolResultsDir` 的词法解析（含 `.`、`..`、重复斜杠）与
cwdReads；失败格核对错误消息。暖缓存删除格是未批准差异
`A7.4-TOOLBUDGET-CWD-001`，不能记为对拍通过。

Rust `runtime::context::tool_budget::cwd_tests` 使用共享有界子进程（每格 30 s），
父进程持有临时目录；没有修改产品并发或锁生命周期。新测试及 helper 的可失败调用
均带 `#885` 专属消息。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/tool-budget-cwd/generate.mts <新目录>
```

ignored 重生成守卫：在 `src/` 下运行 `cargo test -p pilotdeck-gateway --lib runtime::context::tool_budget::cwd_tests::tool_budget_cwd_regenerates_identically -- --ignored --exact`。

负控先 `python3 src/pilotdeck-gateway/tests/fixtures/tool-budget-cwd/negative_controls.py --check` 核 4 个唯一锚点。
提交后指定 `<新证据目录> [变异名 ...]` 运行。脚本要求调用方设置本树 `CARGO_TARGET_DIR`，经
`mutating_sources` 默认 HEAD 守卫、`wait_for_slot` 和 `run_owned` 执行正控 → 变异 → 恢复；
逐项记 KILLED/SURVIVED/INVALID 与原因，全部结束后汇总，任一非 KILLED 则非零退出。
锚点过期、编译/设置失败、超时均为 INVALID。

4 项覆盖：cwd 根目录回退（含暖缓存格）、syscall/错误格式、绝对路径误读 cwd。
每项同时绑定测试全名及该测试捕获的 `#885` 消息，避免另一用例的 panic 误充判杀。
全量可能超过 20 分钟，协调者提交后分批跑，不在未提交树伪造 HEAD。
