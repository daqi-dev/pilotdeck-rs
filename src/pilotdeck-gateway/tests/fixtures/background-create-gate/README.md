# #956 创建快照门闩

`dispose-drains` 的 step 2 是立即退出的 `printf`；它须在 step 3 变为 completed，并非等 dispose。原 TS 的创建响应实测为 running；Rust 的两线程回放可能在响应快照前被 supervise 更新成 completed。

`background_tasks.rs::CreateGate` 给所有 oracle 中有原生 pid 的成功 task_create 配本目录 `shell.sh`，通过 FIFO reader/writer 真正连接确认子 shell 到达门闩。严格比较原创建响应之后才写 release；exec 保持 PID，命令字段、输出、终态断言与原 background-tasks 金样不变。同步 spawn 失败及无子进程的异步 spawn 错误不包门闩。连接先尝试真实 FIFO rendezvous；尚无 reader 时检查 child PID 和任务终态，child 活着且 running 即继续等，60 秒只作总上限兜底。退出/取消先于连接则立即拒绝；诊断包含 pid/status/alive/已等毫秒。10ms 轮询不决定释放；其他业务等待上限不变。失败路径沿用进程组清理。

`generate.mts` 在固定 TS cc7c4e6 + Node v22.22.0 下执行真实 task_create/native shell。金样记录 release / stop / dispose 三格：持有 token 前 running 且无输出，release 后 completed/12 字节，不发 token 而 stop/dispose 后 cancelled/0 字节；另外记录原立即退出命令的 16 次创建响应。输出目录递归创建，oracle 独占创建。Rust `creation_gate_lifecycle` 精确核三格覆盖；ignored `creation_gate_regenerates_identically` 重跑并逐字比较。

```sh
export CARGO_TARGET_DIR=/abs/owned/target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export PDRS_BASELINE=/abs/fixed/PilotDeck
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test background_tasks
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test background_tasks creation_gate_regenerates_identically -- --exact --ignored
python3 -I src/pilotdeck-gateway/tests/fixtures/background-create-gate/negative_controls.py --check-anchors
# 提交后且工作树源码等于 HEAD：
python3 -I src/pilotdeck-gateway/tests/fixtures/background-create-gate/negative_controls.py /abs/fresh/evidence
```

十二个负控依次编译、跑绑定测试、恢复正控并汇总，任何非 KILLED 非零退出：去掉创建门闩、shell 立即退出、跳过 release/stop/dispose、矩阵模式遗漏，以及旧 8 秒上限、忽略退出/取消、缺总上限、缺 PID 诊断、缺 watchdog 模式。跳过 release 以真实终态错误判杀，不用超时作杀点。支持 `--compile-only OUT` 单独逐项编译。锚点必须唯一；变异通过 `mutating_sources` 持锁，cargo 通过 `run_owned` 和 `wait_for_slot`，必须使用调用方 CARGO_TARGET_DIR。未提交的新金样会被 HEAD 守卫拒绝，此时交协调者提交后执行，不绕过守卫。

同类扫描：exit-statuses 的立即退出/信号/子输出、list-filters 的 echo、max-tasks 的 echo、non-string-command 均走同一个创建门闩。其他命令已有 go 文件门闩或长任务，仍同样约束创建快照。sdk-projections 的 subTick 20ms/ignore-abort 50ms 窗口由后续 [#957 子代理门闩](../background-subtick-gate/README.md) 消除，沿用原金样和严格断言。

#960 回归矩阵：`generate.mts` 调真实 TS runtime，用第二道 admission FIFO 将 child 确定性停在创建 FIFO 之前；只注入失败上限的 elapsed 时钟。`watchdog` 四格为 delayed-reader（9 s 时 pending，释放 admission 后 connected/running/0）、exited-reader（completed/0，连接拒绝）、cancelled-reader（cancelled/0，连接拒绝）、never-reader（60 s 时拒绝，随后 dispose 清理）。Rust 分别由 `creation_gate_waits_for_live_delayed_reader`、`creation_gate_rejects_exited_reader`、`creation_gate_rejects_cancelled_reader`、`creation_gate_bounds_missing_reader` 逐字段对拍；首 poll 直接判断返回/Pending，负控不靠无关超时杀。原三格和 native 16 次快照保持原值，重生成仍由既有 ignored 守卫逐字核验。
