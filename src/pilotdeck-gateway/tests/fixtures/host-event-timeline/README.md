# #1220 宿主事件 timeline / streamBoundary 投影金样

`oracle.json` 由固定基线 `cc7c4e6`、Node 22.22.0 的真实 TS 生成：真
`createAgentLoopSidecarRuntimeFactory` + 假模块连接（hello 绑定 `moduleId:'fixture'`、
capabilities、execute 应答）与 `modules.event.drain` 宿主队列。生成器不 clean：全量事件
（含最终 `turn_completed`）与 run() 返回值 `{result, messages}` 原样入库。mock timers
（`apis:['Date','setTimeout']`，now:0）驱动 500ms pump 节奏与心跳 `durationMs`；每个 case
至少一个 wire 事件（Rust 侧 StreamIntegrity 的 Running 前置），host 步骤后必有 tick 再接
wire 步骤（TS wire 分支连带排空 drainHostEvents，Rust #821 载荷不排空，不 tick 则两实现
顺序不同）。final 帧在两实现都排空宿主缓冲（TS 在 `next.done` 前排空；Rust `event()`
final→payload=None 走排空）。

```sh
export PDRS_BASELINE=/Users/macmini-01/ws/pdrs-runner/resources/PilotDeck
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/host-event-timeline/generate.mts /tmp/new-host-event-timeline-fixture
```

消费：`runtime/turn/host_timeline_tests.rs`（经 `#[path]` 挂进 live_stream_tests.rs）逐
case 对拍全部事件与终态坐标；`host_timeline_regenerates_identically`（ignored）为重生成
守卫。坐标分配语义的 Rust 实现在 `runtime/turn/timeline.rs`。负控 `schemata.py` 覆盖
`timeline-order-frozen` / `timeline-boundary-omitted` / `timeline-derived-unprojected`
三枚编译期变异（`_shared/mutant_switch.py`：不改源，可提交前本地跑；`--check` 为只读
预检）。

已登记微差（A7.2-INTERACTION-CONSUME-001-R6，不在本切片对齐范围）：TS
`recordKnownTerminal` 在协议 final 帧内、排空 pending 宿主事件之前定位终态块（金样刻意
只留不可定位事件 pending 于 final）；TS 协议 yield 的 `turn_completed` 经 project() 追加
moduleId，Rust 网关自有的 turn_completed 不带（回放按 events[i<i_end] 去掉 TS 末位对齐，
result/messages 分别对齐）。另：runner 的 `await protocol?.close(...) ??
connection?.close?.(...)` 因 protocol.close 返回 void 而二次关连接，生成器断言 closed=2。
