---
id: A7-HOST-POST-CANCEL-EXIT-001-R2
original: A7-HOST-POST-CANCEL-EXIT-001
---

## 类别

有意改进（仍为向安全偏离的细化，不改 5 s 期限值）。旧结论的问题：R1 写「到点无终态则 Timeout」，实现上期限在 `select!` 里与已完成的事件应用同级竞争；host 执行 future 只由会话 `next()` 驱动，消费方处理已投递事件期间不被 poll，于是 5 s 宽限把消费方耗时也算了进去，到点时把已经完成、只是还没收集的应用和排队中的 `input_accepted` / final 一起丢掉，合法的 success 被判成 `terminal without proper lifecycle`（Rust 假 FAIL）。

## TS 依据

cc7c4e6（`resources/PilotDeck`，与 `/Users/da/ws/PilotDeck-Rust` 同文件逐字节一致）：`src/agent/modules/transport/agentLoopSidecarClient.ts:547` 每次 `await iterator.next()` 只取一帧，消费慢时整个生成器停住；`:662-678` final 只能在更早帧全部 yield 之后读到；`:931-941` / `:943-958` abort 只置 `cancelRequested` 并发送一次 cancel，没有取消后计时器；`src/agent/turn/TurnRunner.ts:381`、`:402` 逐帧拉取并按序 yield。因此 TS 在「取消后消费方慢」时最终投影 success。结论为静态逐行推断，未跑 TS 运行时（构造入口为私有 `SidecarTurnProtocol`），推断记录见主仓 `_tmp/20261025-issue-589-p1/`（DeepSeek r2 `RESULT.md`）。

## Rust 行为

`src/pilotdeck-gateway/src/host/execution.rs`：取消退出分支的 `tokio::select!` 加 `biased;`，期限分支排在 `callbacks.join_next()` 之后、`receive()` 之前；期限已过且仍有排队或在跑的应用时，循环顶部一次性 `yield_now()`（`cancel_catch_up`），让本轮刚 spawn 或刚完成的回调先把事件写进会话通道。真正阻塞的回调没有进展，下一次期限照常退出。`POST_CANCEL_EXIT_TIMEOUT` 仍为 5 s，不新增时限，不改事件队列与字节预算；期限仍排在 `receive` 之前，持续来帧的对端不能饿死它。

## 范围

只改宿主取消后的退出顺序：到点后先收集已完成或一步可完成的事件应用，再按期限退出。**覆盖不到的情况**（P2-跟进 #643，前置于 #590 取消场景）：应用回调要等不止一步（锁、真实 I/O）、会话通道 `mpsc::channel(1)` 前面还有非 final 事件、帧已在连接缓冲但还没被 `receive()` 读出，这三种情况下到点仍会丢掉合法 lifecycle 并判错（fail-closed，不会假成功）；重连内层 `select!` 也没有调整取件顺序；同一 `select!` 里的 `execution_deadline` 分支（仅在配置了执行期限时生效，默认 `None`）在 `biased` 之下固定排在 `join_next` 之前，同样可能吞掉已就绪的应用；慢消费方的三个用例默认后续帧在第二次 `next()` 时已进入宿主队列，若帧仍在连接缓冲里，这几个用例可能偶发失败。以上都归 #643。决策卡见 `state/decisions-20261025.md`。不改变「到点无已校验终态 → Timeout」「截止前已校验的终态拥有结果权威」「同步阻塞运行时线程的回调不在协作式取消保证内」这些 R1 结论；普通执行仍不限时。会话层另按「已投递 lifecycle」判终态（`runtime/turn/live.rs`，#589/#632），本条不涉及该判据本身。

## 机器判据

`src/pilotdeck-gateway/src/runtime/turn/live_stream_tests.rs` 的 `slow_consumer_does_not_spend_the_cancel_exit_grace`：消费 turn_started、model_event 后 abort，睡 6 s，再要求依次拿到 input_accepted、turn_completed/success、None，且 `disposed == 1`。负控：还原 `execution.rs` 的取件顺序，该用例两次都红（`terminal without proper lifecycle … state: Started`）；恢复后 `cancel` 过滤集 44/44 连续三次绿。同批另有 `slow_consumer_still_receives_a_ready_error_terminal`（慢消费方之后 turn_failed 与 error 终态仍交付，失败配对）、`slow_consumer_with_a_blocked_callback_still_exits_bounded`（回调真正阻塞时仍在 `POST_CANCEL_EXIT_TIMEOUT / 2` 内报错退出，`disposed == 1`），以及 host 层 `cancel_exit.rs` 的 `completed_application_is_collected_before_the_deadline_exit`（暂停时钟；负控为把 `biased` 下 deadline 与 join_next 对调，该用例转红，原件 `_tmp/20261025-issue-589-p1/r3b/03-host-collect-red-swapped.log`）。R1 的 `src/pilotdeck-gateway/src/host/tests/cancel_exit.rs` 全套（静默 peer、重连交错、终态应用 / drain）不变且通过。复算：`cargo test -p pilotdeck-gateway --lib -- --test-threads=4`。

## 批准记录

未批准，非已接受差异。取件顺序的调整为协调者判断（AI 代批，待人复核），5 s 期限值状态同 A7-HOST-POST-CANCEL-EXIT-001 首版。不新增豁免、不改判分语义。
