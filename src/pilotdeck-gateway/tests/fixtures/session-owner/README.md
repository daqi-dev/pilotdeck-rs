# Live session owner 的真实 TS 夹具

基线为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`generate.mts` 执行完整、未改写的 SessionRouter、AgentFactoryProvider、AgentRegistry、
AgentHandle 和 ScopedServiceRegistry。只注入 fake AgentSession 的 snapshot、abort、
dispose、compact，以及可控制的 factory/setup；不模拟 Router 的接纳、发布或停止判断。
另执行真实 InProcessGateway.abortTurn / GatewayTurnCompletionFence 与
ProjectRuntimeRegistry 的 lease/retire/dispose 方法。后者使用 prototype 上的原方法，
仅注入资源 bundle 和 background shutdown，未运行无关的 provider 构造。

完整源文件经基线已安装的 esbuild 在内存中转为 CJS；Router 的 agent barrel 指向真实
scope 模块，未用到的其它 import 通过访问即报错的 Proxy 隔离。所有异步交错使用
Promise 闸门和有界 microtask 检查，无 sleep、网络、模型请求或依赖安装。
时钟受控，idle timer 在生成器中关闭；无 UUID、路径或时间的结果归一化。
19 个场景的返回值、错误消息、handle 状态、发布/排空顺序和资源调用均来自真实执行。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
export TMPDIR=/tmp TSX_DISABLE_CACHE=1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-owner/generate.mts /tmp/owner-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-owner/generate.mts /tmp/owner-second
cmp /tmp/owner-first/oracle.json /tmp/owner-second/oracle.json
```

生成器运行前后均调用共享 `../_shared/baseline.mts`，核固定 HEAD、tracked 状态和
8 个执行源文件的提交版/工作版 SHA-256。Rust 回放统一先走
`crate::test_baseline::oracle`；设置 `PDRS_BASELINE` 时重新核对真实源码，不另写溯源判断。

```sh
cd src
cargo test --offline -p pilotdeck-gateway --lib runtime::session::
```

四个拆分判据分别对应 `dirty_identity_and_publication_matches_ts`、
`slow_factory_is_per_session_matches_ts`、`maintenance_blocks_turn_and_idle_matches_ts`、
`shutdown_during_creation_matches_ts`。其它回放验证 close、idle 严格边界、setup/factory
失败保留旧 writer、工具过滤合并、维护取消、shutdown 并行排空、project pause/resume、
带 runId 的 live abort、abort 抛错不等 completion，以及 fence 尚在但 Router 预约已移除的
精确 owner。项目 lease 回放之外，还验证真实 RuntimeLease 保留到 handle 消费者退出和
session dispose 完毕，retired project runtime 与新 runtime 使用不同 Arc 身份。

多线程回归用两个 OS 线程和屏障将新 owner 替换强制塞入 pr1 判断和真实 live sink
选目标之间，交替覆盖有/无 fence 的前缀；100 轮必须零命中新 writer。
另一回归在 close 等待资源排空时结束旧预约、以相同 runId 建立新预约，再触发旧
completion/close cleanup，验证只释放精确身份。另验取消等待者仍会排空、周期 idle
sweep 会停止，以及发布时旧 handle 的 admission cutoff 无需执行资源回调。

`negative.py <evidence-directory>` 使用独占、结束即清理的 `/tmp` 工作区副本，原样复用
全部 Cargo manifests、依赖和共享溯源守卫；不会修改真实产品文件、基线、node_modules、
冻结件或 state。全量源码副本仅用于编译变异，specs 副本仅满足已有 include_str。
先跑原实现，再注入十三项变异：旧 handle 冒充当前 owner、所有 key 共用 factory 队列、
忽略 maintenance、忽略停止后的发布保护、live abort 不复核目标、旧 close 删除新预约、
旧 complete 释放新 owner、旧 end_turn 释放重新接纳的同 runId 预约、篡改 sourceSha256、
非 Active handle 仍接纳活动、idle 忽略进行中创建/排队 close、close 不按 ticket 串行、
空串 project 被 pause 阻止。每项必须成功编译并在对应测试的指定断言
处失败；编译失败、外部超时或非预期失败不计通过。全局等待的负控由测试内有界进度
断言拒绝；abort 负控要求实际观察到 100/100 次命中新 writer。
原始输出及完整替换表达式记录在指定目录。`CARGO_HOME` 和 `PDRS_BASELINE` 必填；
每次调用将 `CARGO_TARGET_DIR` 覆盖为独占 `/tmp` 副本内的 `src/target`，
不与真实树或另一装置共享；结束时与副本一起删除。首次 baseline 冷构建限时 600 秒，
其余变异限时 240 秒。每次编译前运行任务指定的 cargo-slots 命令。

`negative_close_shutdown.py <evidence-directory>` 是 #1245（close 在 shutdown 进行中
的错误语义对齐 TS，含 fix-r1 的 close-先于-shutdown 同 tick 格、fix-r2 的混合格与
drain 守卫）的专用负控，走共享
M6 执行器（`mutating_sources` 原位变异 + `negative_suite.run_cases` 逐项
KILLED/SURVIVED/INVALID；登记在 `EQUIVALENT` 表的等价变异另由本脚本反向核验，
要求仍 SURVIVED 且 restore 后测试通过），要求提交后源码等于 `HEAD`，与 `negative.py`
的独立 `/tmp` 副本方式并存。十三行变异（十二行必须判杀 + 一行登记为等效变异）：close
的 shutdown 窗口变异回旧的整个 shutdown 聚合分支、else 分支不 await 就放行、close
任务的 closing 登记、idle sweep 的
closing 登记、shutdown drain 的 closing 登记、shutdown 的 drain 清单抢走已有未
结算 closing 的 key 的 record（最后一项去掉后 close-first-same-tick 格漂移为
Ok + `shutdown` 原因）、skip 提前 `break` 不再排空其余 key（fix-r2）、per-key
drain 退回裸 `tokio::spawn` 失去完成守卫（fix-r2，panic 后条目悬空、后续 close
答 lifecycle-stopped）、closing 守卫先释放 serial ticket 再结算条目
（fix-r3 `settle-after-release`，窗口②回归，由两个顺序探针测试的事件序列断言
判杀）、成对删 close 任务的 settle 写与 `Completion` 补发（#1365 fix-r1
`closing-settle-write-removed`，单删守卫写是实测等价变异——同一 poll 内
`Completion::finish` 补发同值、无 await 间隙；成对删后 close 答
lifecycle-stopped）、idle sweep 同型的成对 settle 写删除（#1365 fix-r1
`idle-settle-write-removed`，close 等 mid-flight idle 驱逐时答 lifecycle-stopped
而非透传 dispose 聚合）、close 任务 panic 失去完成守卫（#1365 fix-r1
`close-panic-guard-removed`，退回裸 `tokio::spawn`，panic 后条目无人结算）、
serial ticket 的释放写删除（#1365 fix-r1 `ticket-release-write-removed`；单删是
实测等价变异——sender 在 `Drop` 末仍被丢弃，`Ticket::enter` 的 `let _` 吞掉
`wait_for` 的 `Err(RecvError)`，排队 close 照常排空，`ticket-released` 探针也照常
记录；登记在脚本 `EQUIVALENT` 表，运行时要求该行仍 SURVIVED 且 restore 后测试
通过，若转为 KILLED 则报 INVALID 要求转回判杀集合）。一行变异可绑定多个测试
（每测试一个 `Case`），marker list 走 `negative_suite.classify` 的 alternatives
语义——绑定测试在任一列名断言失败即 KILLED，不把多个 marker 拆成多个 Case；
多 `Case` 的行的证据按 `<变异>-<测试短名>-{positive,negative,restored}.log` 分文件
落盘（单 `Case` 行沿用 `<变异>-…` 的历史名），控制台结论用同一 stem，所以按
`Case` 计的 KILLED 数可以逐条追到测试名。`EQUIVALENT` 与 `MUTATIONS` 之间有交叉
校验：等价名必须在变异表内、绑定的测试必须是该行绑定的测试、且不得同时被判为必杀
（漂移即 `REGISTRY DRIFT` 拒绝运行）；等价侧若守卫失败（源码被判改动）则发布整份
`INVALID` 的 `equivalent-summary.json`，超时同样把原始输出与异常写进 `{label}.log`；
带第六元素的行是成对变异（主替换 + 附加 `(before, after)` 位点，经 `Case.scope`
进本脚本 `mutate`，锚点由 `--check-anchors` 一并核数）。TS 依据为真实路由探针
`_tmp/20261026-issue-1245/probe/close-during-shutdown.out`、
`_tmp/20261026-issue-1245/fix-r1/probe/close-first-same-tick.out` 与
`_tmp/20261026-issue-1245/fix-r2/probe/skip-only-closing-key.out`
（作者树 `_tmp/20261026-issue-1245/` 下；交付轮副本在同目录的
`remote-run/artifacts/round/probe/`；Node v22.22.0），差异与修正在
`specs/gateway-parity/diffs/A7.2-SESSION-CLOSE-DURING-SHUTDOWN-001{-R1,-R2,-R3,-R4,-R5}.md`。
`--check-anchors` 预检锚点；`--compile-only` 只证明每个变异体能编译；
`--sample` 只跑 `close-else-awaits-nothing` 一项抽样。

本包提供真实生命周期组件和可注入的 LiveSession/SessionFactory 接口，不提供默认
空 session。完整 submit/持久接纳、seed/compact 业务执行、MCP/native host adapter
和默认代理切换仍分别属于 A7.2、A7.5/A7.6、后续 provider/domain 装配。
RefreshBundle 通过现有 RefreshRouter trait 直接调用此 owner，未改 project/refresh 实现。

空串 project 场景实跑 `closeProject("")`：等待既有的同项目创建，关闭快照中
`projectKey === ""` 的 writer，但 pause 不阻止新的空串 project 创建/发布；
`projectKey` 缺省的 writer 保留。Rust 保留这项 TS 的半生效行为。
`begin_turn` 现在返回 `Option<TurnReservation>`，消费者必须保留成功返回的凭证，
传入 `end_turn(key, run, reservation.as_ref())`；非空 runId 同时匹配字符串和身份。
实测 TS 在 close 后同 runId 再接纳时，旧 endTurn 会删掉新预约；Rust 的身份防护
是本轮明确要求的差异，与已有 close/complete 防护一致。缺省/空 runId 仍无身份限制。

重建窗口内再次 mark dirty 的真实 TS 序列见
`dirty_mark_during_recreation_is_cleared`；重建完成后下一次 get 复用新 writer，
不会再重建，Rust 通过同名回放保持这一行为。
