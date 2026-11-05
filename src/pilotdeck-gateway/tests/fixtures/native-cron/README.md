# #1325 cron 管理方法原生夹具（repeat-trigger / cancel / restart）

`tests/cron_manager_native.rs` 以 include_str 读本目录的固定任务库快照，驱动
#1325 从 #1305（CronFire/CronScheduler）契约分到的三条接缝：

- **活动 run 注册表**（`registerActiveRun`/`unregisterActiveRun`，
  CronRuntime.ts:412-420）：cancel 与停机排空经由它观察；
- **runTaskNow 的 `already_running` 闸**（CronRuntime.ts:392）：run 进行中的
  重复触发不再执行；
- **recoverInterruptedRuns**（CronRuntime.ts:502-562）：进程重启把被打断的
  run 补记为 failed，而非重执行。

| 格 | 夹具文件 | Rust 回归（tests/cron_manager_native.rs） | 断言 |
|---|---|---|---|
| 6 | repeat-trigger.json（running cron 任务） | repeat_trigger_does_not_re_execute | `{started:false, reason:"already_running", taskId}`；body 零 run_turn（不重执行） |
| 11 | repeat-trigger.json | repeat_trigger_second_submit_rejected_by_admission | 同 session 第二次 submit 在第一个 turn 的接纳围栏内被拒：`session_busy` `Session cron:task-repeat already has an active turn.`（InProcessGateway.ts:1043-1067） |
| 8 | cancel-once.json（running once 任务） | cancel_aborts_run_and_deletes_once_task | stopTask（客户端形状 `{taskId, runId}`，无 projectKey，经 lastRunId 扫描定位）：stopRequested、abortTurn 无 reason、once 任务删除、`close_session … cron/task-removed`；注册表项留给 run 的 finally（#1305） |
| 13 | restart-unrecorded.json | restart_records_interrupted_run_without_re_execution | 重启恢复：补 failed run 记录（runId=lastRunId、startedAt=任务 updatedAt、error.code=`cron_run_interrupted`）；任务重排 revision 2、nextRunAt 由 pinned now 重算；零 run_turn（不重执行） |
| 14 | restart-terminal.json + restart-terminal-history.jsonl | restart_with_terminal_run_reschedules_only | lastRunId 已有终态记录（finishedAt+outcome）→ 只重排 revision 2，run-history 不新增记录；零 run_turn |

pinned 时钟 `Date.UTC(2026, 0, 1)`（#1216 金样同款）、计数 taskId `task-N`。

## 边界（登记入 RESULT）

- `submit_turn` 同步返回：本端口接纳围栏内不消费事件流——异步 drain 归
  #1305/#1326（矩阵格 22）。重复触发夹具因此从 `run_turn` 体内再入同一
  admission，这正是 TS `SessionRouter.beginTurn` 在调度器二次点火时看到的
  形状。
- stop() 只标记/abort，注销注册表归 CronFire 的 finally（#1305）——cancel
  格与 S1 停机格都按此断言。
- S5 已落地本目录：`generate.mts`（真实 TS 金样生成器，baseline cc7c4e6 /
  Node v22.22.0，pinned 时钟 + fail-closed esbuild 装载器）、`oracle.json`
  （10 格金样：create 生命周期/延迟折叠/柏林时区/错误、update 全分支、
  扫描 not_found 与跨项目歧义、run-now 形状、全局 list 合并、禁用配置、
  running 删除 + 6 方法未配置错误）、`manager_tests.rs`
  `six_methods_replay_the_ts_gold` 字节级重放、`negative_controls.py`
  （8 变异：6 行绑该重放测试，1 行绑全局 list 的 nullish 上限单测，1 行绑
  接纳栅栏 panic 用例，全 KILLED）。
- runTaskNow 成功格：结果不含时间戳，即便 TS 侧 runAt 由真实时钟折叠
  （重放仍字节级相等）。这是夹具自身观测不到的点，**不是** Rust/TS 差异，
  不进 `specs/gateway-parity/diffs/`。

## 已知覆盖缺口

- **`unconfigured` 半边是人工转写**：`generate.mts` 直接
  `new DialogGatewayError('CAPABILITY_UNAVAILABLE', ...)`，不走真实的
  `requireCron`，所以负控 `gate-code-swapped` 证明的是「Rust == 手抄值」。
  手抄值本身已对基线源码核过：`InProcessGateway.ts:3016-3020`（基线
  cc7c4e65，按 `oracle.json` 的 `sourceSha256` 逐字节选出的副本）为
  `throw new DialogGatewayError("CAPABILITY_UNAVAILABLE", "Cron runtime is not
  configured on this gateway.")`，code 与 message 都与夹具一致；
  `specs/gateway-coverage/ledger.json` 记的也是同一个 code。残留缺口只是
  「没有真去执行 requireCron」，改进办法（在 `generate.mts` 里加载该模块取
  真实错误对象）登记在 #1645。
- **全局 list 的 nullish `limit` 无金样格**：`oracle.json` 的
  `list-global-merge` 只跑显式 `limit: 2` 与 `limit: 0`，合并后只有 4 条
  run，`50` 与「全量」在两种输入下同值，金样分辨不出。该分支现有两条独立
  证据：单测 `global_list_caps_the_merged_run_page_at_the_nullish_default`
  与负控 `list-global-nullish-default-ignored`（绑该单测）。语义本身对基线
  `CronManager.ts:139` 的 `.slice(0, input.limit ?? 50)` 核过（同一份
  sha256 选出的基线副本）；补金样格前置于下次有基线的金样重生成（#1645）。
- **`recoverInterruptedRuns` 无 TS 金样**：`oracle.json` 十格无一格触发恢复；
  `tests/cron_manager_native.rs` 的重启格只断言 Rust 对已提交夹具的行为，
  属自证。恢复只在 `start()` 路径上跑，而金样重放不 start manager。
- `#[ignore] native_cron_oracle_regenerates_identically` 再生成守卫需
  Node v22.22.0（`/Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin`
  置于 PATH 首位）与 `PDRS_BASELINE`；金样重放测试本身不生成 Node。

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test cron_manager_native
# 金样重放（lib）：
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  runtime::cron::manager_tests::six_methods_replay_the_ts_gold -- --exact
# 负控（先提交本目录全部文件；CARGO_TARGET_DIR 由调用方提供）：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-cron/negative_controls.py --check
python3 -I src/pilotdeck-gateway/tests/fixtures/native-cron/negative_controls.py \
  [--compile-only] NEW_OUT [mutation ...]
```
