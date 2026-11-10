# sidecar-liveness-1891 — #1891 放宽余量的负控

#1891 把九个同源测试位点的活性看门狗放宽到 #1851 的 60 s 形态（首字节
`FIRST_BYTE`、`bounded_liveness`/`bounded`/`next` 看门狗、真实 peer 的
`Limits` 余量）。fix-r2 起行表覆盖全部九位点（`module_host/tests.rs` 的
`FIRST_BYTE` 是 fix-r1 漏登记的第五个首字节位）。本目录证明放宽没有把看门狗
变成建议：每行把一个放宽后的
窗口缩到任何健康 fixture 都不可能完成的值，因此判死不需要任何环境负载，
杀招全部落在位点自己的稳定 oracle 文本上（这些断言在 #1891 同一改动里接
上了 `negative_receipt` 协议；无 `PDRS_NEGATIVE_RECEIPTS` 环境时行为与
原先逐字节等价）。A 组（fix-r1）缩到 `Duration::ZERO`：`elapsed() < ZERO`
恒假、捕获环必不进入，判死不依赖任何时钟粒度假设——原先的 1 ns 留了一个
「响应已躺在 socket 缓冲里、恰好在 1 ns 窗口内读到」的竞态。B 组缩到
`from_nanos(1)`：各行被包 future 的首个 peer 依赖 await 可证 pending
（等待的事件/握手需要一次尚未发生的 Python 往返），超时必然先赢。

## 行表

| 行 | 变异（缩窗） | 绑定测试 | kill 断言 |
|---|---|---|---|
| api-web-first-byte-shrunk | `FIRST_BYTE` 60 s→`ZERO` | `api_web::tests::projects_ok` | `#1348 cell projects-ok produced no bytes and stayed open` |
| approvals-first-byte-shrunk | 同上 | `module_host::approvals::tests::auth_missing` | `#1350 cell auth-missing …` |
| native-http-first-byte-shrunk | 同上 | `native_http::tests::health_get` | `#1209 cell health-get …` |
| native-ws-first-byte-shrunk | 同上 | `native_ws::tests::upgrade_budget_boundary_matches_ts` | `#1351 every boundary cell produced bytes or closed` |
| module-host-first-byte-shrunk | 同上（fix-r2 补：第五个同源位） | `module_host::tests::auth_missing` | `#1349 cell auth-missing produced no bytes and stayed open` |
| host-factory-liveness-shrunk | `bounded_liveness` 超时实参 1 ns | `runtime::turn::host_factory::tests::live_session_shares_recorder_and_reads_current_history` | `sidecar liveness wait timed out` |
| live-tail-bounded-shrunk | `bounded` 超时实参 1 ns | `runtime::turn::live::tail_tests::live_tail_progress_and_drop` | `#742 live tail bounded` |
| interaction-host-liveness-shrunk | `bounded_liveness` 超时实参 1 ns | `runtime::project::factory::tests::interaction_host::host_consumes_actual_generations_across_reload_and_live_stdio` | `sidecar liveness wait timed out` |
| live-stream-next-shrunk | `next` 超时实参 1 ns | `runtime::turn::live::stream_tests::host_normal_lifecycle_streams_events_and_projects_authoritative_final_once` | `#741i/#855: LiveTurn must make progress …` |
| resume-liveness-shrunk | `liveness_limits()` 握手 60 s→1 ns | `host::tests::resume::resume_sends_accepted_cursor_and_replays_from_next_sequence` | `#1891 resume peer spawn failed` |
| host-limits-execution-shrunk | `limits()` 执行 60 s→1 ns | `host::tests::handshake_execute_ack_and_terminal` | `#1891 host fixture run failed` |

## 运行

表是 HEAD 守卫的（`matrix_main` 经 `mutating_sources` 逐字节对
`--baseline HEAD`）。候选提交后、干净树上运行：

```sh
export CARGO_TARGET_DIR=<target-dir>
python3 -I src/pilotdeck-gateway/tests/fixtures/sidecar-liveness-1891/negative_controls.py \
    <NEW_OUTPUT_DIR>            # 全部 11 行：positive → 变异 → 编译 → kill → 还原 → restored
python3 -I .../negative_controls.py --check   # 只验锚（过期即报错退出，绝不判 SURVIVED）
```

任何一行不是 KILLED（含锚过期、编译失败、还原失败）→ 退出码非零。
运行时读入（写守卫集合的一部分）：`tests/fixtures/live-session/peer.py`、
`scripts/a6-host/fixture.py`、`scripts/a6-host/lifetime.py`、
`scripts/a6-host/resume_fixture.py`（fix-r1 补：resume 绑定行跑 `reconnect-*`
模式，`fixture.py` 经 runpy 执行它）。

## 范围与诚实声明

- 缩到 `from_nanos(1)` 证明的是「窗口仍在被强制执行」；它不重演 #978 的
  负载红（那需要 48 份并发副本，由 #1891 的 before/after 扫描承担）。
- `host-limits-execution-shrunk` 的锚取 `limits()` 整块三行（`close_timeout
  1 s` 区分于 `protocol_limits()` 的 5 s），变异只动 `execution_timeout`。
- `live-stream-next-shrunk` 的 kill 断言保留原文的括注「timeout is not a
  mutation kill」——该括注约束的是产品码变异的判死口径；这里杀死的是测试
  侧窗口变异自身，两者不冲突。
