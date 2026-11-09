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

#996 起本夹具的负控是编译期开关（`crate::mutants::on`，仅 `--features
pdrs-mutants` 的库测试二进制可激活），由三份 schemata 声明并经共享执行器
`_shared/mutant_switch.py` 预编译后逐行执行（正控 → 变异 → 还原，判杀锚 = 绑定测试
的具名 FAILED 行 + 其自身断言消息）；旧文本替换脚本 `negative.py` 与
`negative_close_shutdown.py` 已删除，迁移对账见各轮 RESULT。

```sh
export CARGO_TARGET_DIR=/absolute/target PDRS_BASELINE=/absolute/PilotDeck
python3 -I src/pilotdeck-gateway/tests/fixtures/session-owner/schemata.py \
  _tmp/<date>-issue-996/negative-owner            # 16 行，全部必须 KILLED
python3 -I src/pilotdeck-gateway/tests/fixtures/session-owner/schemata_close_shutdown.py \
  _tmp/<date>-issue-996/negative-close-shutdown   # 12 行必须 KILLED
python3 -I src/pilotdeck-gateway/tests/fixtures/session-owner/schemata_equivalent.py \
  _tmp/<date>-issue-996/equivalent-ticket-release  # 等价行，必须 SURVIVED
# 只读锚点预检：--check
```

`schemata.py` 十六行 = 旧 `negative.py` 的十三项源码变异（旧 handle 冒充当前
owner、所有 key 共用 factory 队列、忽略 maintenance、忽略停止后的发布保护、
live abort 不复核目标、旧 close 删除新预约、旧 complete 释放新 owner、旧
end_turn 释放重新接纳的同 runId 预约、非 Active handle 仍接纳活动、idle 忽略
进行中创建/排队 close、close 不按 ticket 串行、空串 project 被 pause 阻止、
idle 三连闸门变异）加 `owner-tampered-source-hash`（旧名 `tampered-source-hash`
重命名，避免与 session-abort-contract 的同名不同 oracle 开关共享
`PDRS_MUTANT` 选择值）：开关在 `owner_tests.rs::case()` 加载点对加载态 JSON 的
SessionRouter.ts 哈希做内存零化，provenance panic 先于数据读取，与旧盘上改写
同形。

`schemata_close_shutdown.py` 十二行必须 KILLED，是 #1245（close 在 shutdown 进行中
的错误语义对齐 TS，含 fix-r1 的 close-先于-shutdown 同 tick 格、fix-r2 的混合格
与 drain 守卫、fix-r3 的 settle-before-release）与 #1365 fix-r1 的专用负控：close
的 shutdown 窗口变异回旧的整个 shutdown 聚合分支、else 分支不 await 就放行、
close/idle sweep/shutdown drain 三处 closing 登记移除、shutdown 的 drain 清单
抢走已有未结算 closing 的 key 的 record、skip 提前 `break` 不再排空其余 key、
per-key drain 退回裸 `tokio::spawn` 失去完成守卫、closing 守卫先释放 serial
ticket 再结算条目、成对删 close 任务与 idle sweep 的 settle 写（守卫写 + 重定向
`done` sender 两个位点归同一开关，`ClosingGuard` 的 `write_settle` 旗标把
`Drop` 的写关闭，保证每个开关名单调用点）、close 任务 panic 失去完成守卫。
一行可绑定多个测试（对齐锚点列表逐格判杀）。

`schemata_equivalent.py` 是 #1365 fix-r1 登记的等价变异 `ticket-release-write-removed`
（单删 serial ticket 的释放写）：sender 在 `Drop` 末仍被丢弃，`Ticket::enter`
的 `let _` 吞掉 `wait_for` 的 `Err(RecvError)`，排队 close 照常排空，
`ticket-released` 探针也照常记录——实测 SURVIVED，无产品可观测。执行器要求
全 KILLED 才退出 0，故该行单独成 runner：发布 summary 中恰一行 SURVIVED 才算
绿；若某天转为 KILLED，运行报错并要求把该行移回 `schemata_close_shutdown.py`
的必杀集合。TS 依据为真实路由探针
`_tmp/20261026-issue-1245/probe/close-during-shutdown.out`、
`_tmp/20261026-issue-1245/fix-r1/probe/close-first-same-tick.out` 与
`_tmp/20261026-issue-1245/fix-r2/probe/skip-only-closing-key.out`
（作者树 `_tmp/20261026-issue-1245/` 下；交付轮副本在同目录的
`remote-run/artifacts/round/probe/`；Node v22.22.0），差异与修正在
`specs/gateway-parity/diffs/A7.2-SESSION-CLOSE-DURING-SHUTDOWN-001{-R1,-R2,-R3,-R4,-R5}.md`。

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
