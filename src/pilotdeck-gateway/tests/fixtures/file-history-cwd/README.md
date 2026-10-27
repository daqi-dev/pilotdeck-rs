# FileHistory cwd（#879 pr1）

Node v22.22.0 实跑固定基线 cc7c4e6 的 `FileHistoryStore.trackEdit` / `markEditCommitted`。
`generate.mts` 每格新子进程，原入口导入与绝对路径准备在 chdir 前；删除 cwd 后不提前读取它，避免污染 Node 的 cwd 缓存。输出 recursive mkdir + `flag:'wx'`，oracle 保存源码 SHA256。

12 格：两个入口 × 相对/绝对 × 健康/删除 cwd（8）；mark 无 snapshot × 健康/删除（2）；两个入口 × 相对/暖缓存后删除（2）。
健康路径包含 `.`、`..`、重复斜杠，核对词法解析、备份字节/版本/权限/时间、expected state、transcript 次数。失败核对错误消息与原状态不变。暖缓存两格是未批准差异 `A6.6-FILEHISTORY-CWD-001`，不能记为对拍通过。

Rust `storage::file_history::cwd_tests` 使用共享有界子进程（每格 30 s），父进程持有临时目录；没有修改产品并发或锁生命周期。新测试及 helper 的可失败调用均带 `#879` 专属消息。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/file-history-cwd/generate.mts <新目录>
```

ignored 重生成守卫：在 `src/` 下运行 `cargo test -p pilotdeck-gateway --lib storage::file_history::cwd_tests::file_history_cwd_regenerates_identically -- --ignored --exact`。

负控先 `python3 src/pilotdeck-gateway/tests/fixtures/file-history-cwd/negative_controls.py --check` 核 10 个唯一锚点。
提交后指定 `<新证据目录> [变异名 ...]` 运行。脚本要求调用方设置本树 `CARGO_TARGET_DIR`，经 `mutating_sources` 默认 HEAD 守卫、`wait_for_slot` 和 `run_owned` 执行正控 → 变异 → 恢复；逐项记 KILLED/SURVIVED/INVALID 与原因，全部结束后汇总，任一非 KILLED 则非零退出。锚点过期、编译/设置失败、超时均为 INVALID。

10 项覆盖：cwd 根目录回退、syscall/错误格式、绝对路径误读 cwd、两个入口吞错、mark 提前无 snapshot 早退、track 报错前污染状态、`.` / `..` 归一化。每项同时绑定测试全名及该测试捕获的 `#879` 消息，避免另一用例的 panic 误充判杀。fix-r1（#889）起判杀改用共享 `negative_suite.target_hits`：外层 libtest `failures:` 清单（仅 stdout 最后一段，嵌套 worker 的清单与 panic 回显都在 stderr，不参与）点名该测试，且该测试自身 panic 块（内嵌 worker 输出）携带专属消息；fix-r2（#889）起消息只认内嵌 worker 自己的 panic payload 里的断言位，worker stdout 普通诊断、父块自身文案或断言值里的回显都不判杀；fix-r3（#889）起 payload 仅取 worker 真实 stderr 段（父块消息按 stdout→stderr 拼接、stdout 止于 worker libtest 汇总行，整段旧 panic 的 stdout 回显不参与），同一块多个 payload 相互冲突即拒判。相邻 `... FAILED` 拼写或别处 panic/回显里的消息同样不判杀。全量可能超过 20 分钟，协调者提交后分批跑，不在未提交树伪造 HEAD。提交前样本可用 `NEGATIVE_BASELINE=<临时 git commit-tree 对象>` 指向同字节的临时基线（不移动 HEAD），默认值仍是 HEAD。
