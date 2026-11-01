# #1220 宿主事件 timeline / streamBoundary 投影金样

`oracle.json` 由固定基线 `cc7c4e6`、Node 22.22.0 的真实 TS 生成：真
`createAgentLoopSidecarRuntimeFactory` + 假模块连接（hello 绑定 `moduleId:'fixture'`、
capabilities、execute 应答）与 `modules.event.drain` 宿主队列。生成器不 clean：全量事件
（含最终 `turn_completed`）与 run() 返回值 `{result, messages}` 原样入库。mock timers
（`apis:['Date','setTimeout']`，now:0）驱动 500ms pump 节奏与心跳 `durationMs`；每个 case
至少一个 wire 事件（Rust 侧 StreamIntegrity 的 Running 前置），host 步骤后必有 tick 再接
wire 步骤（TS wire 分支连带排空 drainHostEvents，Rust #821 载荷不排空，不 tick 则两实现
顺序不同）——唯一例外是登记微差③的 `host-pending-wire-same-tick`：host 事件入缓冲后不接
tick、同拍发 wire，把两实现的顺序差显性化。final 帧在两实现都排空宿主缓冲（TS 在
`next.done` 前排空；Rust `event()` final→payload=None 走排空）。cancel 步骤经
AbortController 触发，但取消不停止 pump：其后入队的宿主事件照常排空并投影（cancel-window
case）；`compaction: true` 的 final 走 TS client:1348 的 abortedResult(input) 合成路径
（compaction-incomplete-final case）。

```sh
export PDRS_BASELINE=/Users/macmini-01/ws/pdrs-runner/resources/PilotDeck
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/host-event-timeline/generate.mts /tmp/new-host-event-timeline-fixture
```

消费：`runtime/turn/host_timeline_tests.rs`（经 `#[path]` 挂进 live_stream_tests.rs）逐
case 对拍全部事件与终态坐标；`live_stream_tests.rs` 的
`submit_assembles_one_timeline_across_wire_host_and_terminal` 经 `LiveSessionAdapter::submit`
走生产装配断言连续坐标。`host_timeline_regenerates_identically`（ignored）为重生成
守卫。坐标分配语义的 Rust 实现在 `runtime/turn/timeline.rs`。负控 `schemata.py` 覆盖
`timeline-order-frozen` / `timeline-boundary-omitted` / `timeline-derived-unprojected` /
`timeline-compaction-unpositioned` / `wire-drains-host` / `timeline-forwarding-unwired` /
`timeline-terminal-unwired` 七枚编译期变异，前五枚绑回放测试、后两枚绑 submit 测试
（`_shared/mutant_switch.py`：不改源，可提交前本地跑；`--check` 为只读预检）。

已登记微差与对拍适配（A7.2-INTERACTION-CONSUME-001-R6，不在本切片对齐范围）：① TS
`recordKnownTerminal` 在协议 final 帧内、排空 pending 宿主事件之前定位终态块（金样在
final 前总以 tick 排空缓冲）；② TS 协议 yield 的 `turn_completed` 经 project() 追加
moduleId，Rust 网关自有的 turn_completed 不带（回放按 events[i<i_end] 去掉 TS 末位对齐，
result/messages 分别对齐）；③ `host-pending-wire-same-tick` 交叉对拍两实现相反的 yield
顺序、以字面量钉住两侧四组坐标；④ 形状错误 TS 读取即抛、Rust 跳过（wire decoder 与
StreamIntegrity 先行拒绝，流上不可观察）。适配：不完整 compaction final 合成的
abortedResult 时间戳来自 Rust 宿主真实 wall clock（TS 为 mock Date），对拍剥除
startedAt/completedAt 后逐格比较，并断言两时间戳同刻、UTC Z 尾。另：runner 的
`await protocol?.close(...) ?? connection?.close?.(...)` 因 protocol.close 返回 void 而
二次关连接，生成器断言 closed=2。
