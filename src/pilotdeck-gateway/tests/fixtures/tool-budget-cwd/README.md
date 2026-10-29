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

负控（#996 起为编译期开关，旧 `negative_controls.py` 与 `session_negative.py` 文本
替换表已删除）：源码全部提交后从仓库根执行 `python3 src/pilotdeck-gateway/tests/fixtures/tool-budget-cwd/schemata.py <新证据目录> [开关名 ...]`；调用方必须设置本树 CARGO_TARGET_DIR，脚本不另建 target、也不改任何工作区源码。`--check` 只做锚点预检。6 个开关以 `crate::mutants::on("<名>")` 分支编译进 feature（`pdrs-mutants`）库测试二进制：先 `cargo test --lib --features pdrs-mutants --no-run` 预编译一次，再逐开关以 `PDRS_MUTANT=<名>` 独立进程运行正控/负控/还原相。执行器共享 `tests/fixtures/_shared/mutant_switch.py`；每项记录 KILLED/SURVIVED/INVALID 与原因，任一非 KILLED 退出非零。锚点过期、编译/设置失败、超时均为 INVALID。两个旧入口都经 `matrix_main` 以 #946 nonce 回执判杀，迁移后由共享执行器的 `receipts=True` 保留（#996-pr2 fix-r3）：负控相子进程携带本轮回执配置，只有被绑定断言写出的本轮 nonce 回执加具名 FAILED 行才算 KILLED；setup 阶段失败、诊断文本恰含锚点、回显 nonce 都判 INVALID（#889 replay 反例见 `_shared/test_mutant_switch.py`）。

6 项覆盖：cwd 根目录回退（all-cells，含暖缓存格，两格各自对齐锚点）、syscall/错误
格式、绝对路径误读 cwd，以及组合传播两项（Session kind 映射、live 前缀原样返回）。
每项绑定测试全名及该测试捕获的 `#885` 消息，避免另一用例的 panic 误充判杀。
组合传播负控对应 `host_factory_tests.rs` 的
`session_composition_uv_cwd_error_reaches_stream_unwrapped`（worker 子进程删 cwd），
经 `TurnHostFactory::into_live_session` 公开组合入口断言 submit 先接纳、首次轮询返回与
本 oracle `relative-deleted` 逐字节相同的 uv_cwd 消息、任何生命周期事件之前失败。
契约见 `specs/negative-controls/README.md`。
