# #2295 A8.3 cron 周期调度夹具（scheduler 生命周期 + CronFire 分发）

本目录把 #2295 的两条新接缝钉在真实 TS 基线（cc7c4e6）上：

- **CronScheduler 生命周期**（`src/cron/runtime/CronScheduler.ts:23-199`）：
  60s 空转轮询 + `isDue` 过滤的定时唤醒、start/stop/poke 三闸、tick 后重臂、
  容量满 `delayTask`；
- **CronFire.runTask 分发**（`src/cron/runtime/CronFire.ts:49-294`）：claim
  （`matchesScheduledSnapshot`）→ submitTurn → 结果分级 → appendRun →
  deliverResult → updateTaskAfterRun 的完整失败梯（每段只 warn 不中断）。

文件：

- `generate.mts` — 金样生成器（baseline cc7c4e6 / Node v22.22.0，pinned 时钟
  2026-01-01T00:00:00Z + `node:test` mock timers + fail-closed esbuild 装载器，
  装载链同 native-cron）；
- `oracle.json` — 10 格金样（wx 发布，Rust 重放前先验 provenance）；
- `probe.mts` — 矩阵探针：金样覆盖不到的 S/T 格逐格实测，每格一行
  `### <id> <json>` 原始输出（RESULT 贴真跑记录）；
- `negative_controls.py` — 7 变异负控（锚点唯一性 `--check` 本地可验；
  全量 KILLED 由协调者在干净树上跑）。

## S 组：调度器生命周期

| 格 | 状态 | 事件 | TS 判据（CronScheduler.ts） | TS 证据 | Rust 断言（scheduler_tests.rs） |
|---|---|---|---|---|---|
| S1 | 未启动 | start | recalc 后 arm 首个 60s，不立即 tick（:31-41） | probe `S1-exact-60s-boundary`（59_999ms 不 fire，+1ms fire）+ oracle `due-once-fires` | `start_arms_the_idle_poll_and_poke_floors_at_250ms`（armed ∈ [59_000, 60_000]） |
| S2 | 已启动 | 再 start | running 闸直接 return（:35-37） | probe `S2-second-start-no-recalc` | `second_start_recalculates_nothing` |
| S3 | 已 stop | 再 start | stopped 闸 return（:32-33） | probe `S3-start-after-stop-no-revive` | `start_after_stop_does_not_revive` |
| S4 | disabled | start | runtime start 前 no-op（CronRuntime.ts:183-186），scheduler 自身也闸（:32） | probe `S4-disabled-never-arms` | `disabled_config_never_arms` |
| S5 | 未绑定 | start | throw "…before bindGateway."（CronRuntime.ts:187-189 / CronManager.ts:87-89） | probe `S5-start-before-bind`（两个报错原文） | `start_before_bind_throws` |
| S6 | 运行中(空闲) | poke | clearTimeout + 250ms 重臂（:55-64,71-86） | oracle `poke-fires-early` + `no-poke-no-early-fire` | `start_arms_the_idle_poll_and_poke_floors_at_250ms`（poke 后 armed ≤ 250） |
| S7 | 已 stop/未启动 | poke | 三闸 no-op（:56） | probe `S7-poke-gates` | `poke_gates_on_running_and_stopped` |
| S8 | tick 进行中 | poke | 另臂一个 250ms 定时器，timer 槽被 tick 收尾重臂覆盖，旧定时器泄漏（:71-86）→ 可重叠 tick（claim 栅栏约束） | probe `S8-poke-during-tick-leaks-second-timer`（泄漏定时器在 60s 窗口内 fire） | **登记差异**：Rust 串行，poke 只影响 deadline；可比重放面由 `poke_tick_gates_on_the_in_flight_run` 覆盖 |
| S9 | 空闲 | stop | stopped+clearTimeout（:43-53） | oracle `stop-no-fire` | `stop_is_authoritative_no_post_stop_tick` |
| S10 | tick 进行中 | stop | await tickInProgress（:50-52）；S8 泄漏竞态下 TS 可再跑一个 post-stop tick | probe `S10-post-stop-tick-leak`（stop 后泄漏定时器仍 fire） | **登记差异**：Rust join 线程，永不 post-stop tick（更严方向） |
| S11 | 任意 | tick 失败 | catch warn "cron scheduler tick failed"，`.then` 仍重臂（:76-84） | oracle `tick-failure-recovers` | 金样重放（`scheduler_replays_the_ts_gold`） |
| S12 | 任意 | runTickOnce | 公开、不受闸（:66-69） | oracle `tick-once-manager-level` | `run_tick_once_after_stop_still_ticks` |

## T 组：任务 × 触发/停止（fire 侧）

| 格 | 状态 | 事件 | TS 判据（CronFire.ts / CronScheduler.ts） | TS 证据 | Rust 断言 |
|---|---|---|---|---|---|
| T1 | scheduled+到期 | tick | claim → submitTurn → 终态记录（:69-95） | oracle `due-once-fires` | 金样重放 |
| T2 | scheduled+未到期 | tick | isDue 假（:190-199） | oracle `not-due-no-fire` | 金样重放 |
| T3 | running | tick | isDue 假（:191-193） | #1325 夹具格 6（runTaskNow 面） | `running_task_is_not_due` |
| T4 | 到期但容量满 | tick | delayTask：nextRunAt=now+60_000、revision+1（:97-99,176-187） | oracle `capacity-delays-second-task` | `capacity_defers_by_the_constant_sixty_seconds`（60_000 常量字节断言） |
| T5 | 快照过期 | claim | matchesScheduledSnapshot 不匹配→aborted、无记录无 turn（:82-85,283-288） | probe `T5-stale-claim-aborts` | `stale_claim_aborts_without_record` |
| T6 | once 已 claim | turn 完成 | 删任务+release（:242-255） | oracle `due-once-fires` | 金样重放 |
| T7 | cron 已 claim | turn 完成 | 重排、status scheduled、revision+1、版本 3（:257-278） | oracle `due-cron-reschedules` | 金样重放 |
| T8 | turn 中 | error turn_timeout | failed + {code:"cron_run_timeout"}（:126-133） | probe `T8-turn-timeout` | `turn_errors_classify_by_code` |
| T9 | turn 中 | error agent_aborted | aborted + 原 code（:138-139） | probe `T9-agent-aborted` | `turn_errors_classify_by_code` |
| T10 | turn 中 | error 其他 code | failed + 原 code（:138-139） | probe `T10-session-busy` | `turn_errors_classify_by_code` |
| T11 | seam 抛错 | submit 边界 | catch→failed + cron_run_failed（:142-149） | probe `T11-seam-throws` | `codeless_rejection_folds_to_cron_run_failed` |
| T12 | run 进行中 | stopRequested | finally 覆写 outcome="stopped"（:151-154） | probe `T12-stop-requested`；#1325 夹具格 8（stopTask 面） | `stop_requested_overrides_the_outcome` |
| T13 | 终态记录写失败 | appendRun | warn "cron run terminal record write failed"，后续照走（:160-178） | probe `T13-append-run-fails` | `append_run_failure_warns_and_continues` |
| T14 | 递送失败 | deliverResult | warn "cron result delivery failed"（:188-194） | probe `T14-delivery-fails` | `delivery_failure_warns` |
| T15 | run 后更新失败 | updateTaskAfterRun | warn "cron task post-run update failed"（:195-201） | probe `T15-post-run-update-fails` | `post_run_update_failure_warns` |
| T16 | claim 后任务被并发改动 | updateTaskAfterRun | matchesRunningTask 不匹配→不删（:288-294） | probe `T16-stale-post-run-keeps-task` | `stale_post_run_update_keeps_the_task` |

探针的事件梯（T8-T16）用直构 `new CronFire(deps)`：submit 生成器逐格产出单个
error 事件 / 抛错 / 中途改库，`now` 钉 2026-01-01T00:00:00Z，所以每格的
run-history 行、重排 nextRunAt（`2026-01-01T00:05:00.000Z`）与金样同源可比。
T8-T10 的逐事件 `appendRunEvent` 行（probe 的 `runEvents` 字段）是 TS 侧真产物，
本仓端口折在同步 submit 边界上不可表示（差异账本，见 RESULT）。

## 边界（登记入 RESULT）

- **S8/S10 定时器泄漏**：TS 的 poke-during-tick 可与在跑 tick 重叠、泄漏定时器
  可在 stop 后再跑一个 tick；Rust 调度线程串行、stop join 后永不 tick（更严
  方向）。probe `S8`/`S10` 两行就是该泄漏的 TS 实测。
- **事件流折在 submit_turn**：assistant 文本累积、逐事件 appendRunEvent、
  interaction_required 中止、onPhaseEvent/onTurnEvent 在本端口不可表示。
- TZ 别名与 once 本地墙时间语义（A8.3-CRON-TZ-ALIAS-001 /
  A8.3-CRON-ONCE-LOCALWALL-001）沿用既有登记，不让步。
- poke 位点任务书计 3 处，TS 实为 4 处（CronRuntime.ts:277/:349/:363/:379：
  create/update/delete/stop）。

```sh
# 金样重放（lib；含 10 格字节比对）：
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  runtime::cron::scheduler_tests
# 负控锚点校验（全量 KILLED 需干净树，协调者跑）：
python3 -I src/pilotdeck-gateway/tests/fixtures/cron-scheduler/negative_controls.py --check
# 金样重生成守卫（需 Node v22.22.0 + PDRS_BASELINE）：
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  cron_scheduler_oracle_regenerates_identically -- --exact --ignored
# TS 探针（RESULT 贴原始输出；LC_ALL/LC_MESSAGES 须 unset，不能置空）：
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 PDRS_BASELINE=<baseline> \
  <node-v22.22.0> --disable-warning=ExperimentalWarning \
  --import <baseline>/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/cron-scheduler/probe.mts <out-dir>
```
