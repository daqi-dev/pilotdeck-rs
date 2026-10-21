# 连接 context 的真实 TS 金样

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
生成器运行前后调用共享 `../_shared/baseline.mts`，核 HEAD、tracked 状态、十个实际执行模块的提交版/工作版 SHA-256。
回放使用全仓 `crate::test_baseline::oracle`；设置 `PDRS_BASELINE` 时同时核实际源码。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
export TSX_DISABLE_CACHE=1 TMPDIR=/tmp
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/connection-context/generate.mts /tmp/pdrs-connection-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/connection-context/generate.mts /tmp/pdrs-connection-second
cmp /tmp/pdrs-connection-first/oracle.json /tmp/pdrs-connection-second/oracle.json
```

真实 GatewayWsConnection、InteractionReconnect/Replay、GatewayInteractionCoordinator、permission/elicitation bus、session permission registry、协议版本、SkillManagerError/SkillValidationError、DialogGatewayError 整文件由基线现有 esbuild 转译，未修改方法体。
WS 的 Skill barrel 仅接入实际 SkillManager 模块；未用 imports 由 fail-closed Proxy 隔离，访问即失败。
内存 GatewayTextConnection 注册真实构造函数的 message/close 回调，捕获 sendText 和 close；通过原方法的 Promise 包装等待异步完成，不复制校验/dispatch 判断。全程无监听器。
窄 gateway 仅记录参数、提供 describe/业务结果或真实错误类实例，并提供可释放的 submit/describe 等待闸门；reconnect/submit binding/disconnect 调用真实 coordinator/native binding provider。preserveTurn=true 来自真实 permission bus 的显式 pending 注册；不是手写 disconnect 判断。
它不是 session owner/abort adapter 实现。

17 个连接场景记录每一步完整 frames、closes、calls，每步的 in-flight/interaction session 集合，以及最终帧发送瞬间的 in-flight 集合；17 个真实 provider 操作记录完整返回值/异常消息。
覆盖 JSON、hello 缺字段/类型、协议先于 token、未知/空 clientName、认证后非法 frame、错误层级（含 validation/details）、SDK run/control、服务端 binding 覆盖、close preserveTurn 两分支和 close 的集合顺序。
同 session 双 submit 用两个独立闸门保证先后入场、先释放第一个、再 close、最后释放第二个。TS 的 Set.add 去重，任一 finally 都按 session 无条件 delete；所以第一个完成后 close 不 abort。Rust 保留这一行为，不能按 ticket 延长在途状态。interactionSessions 同样按 session 去重，但 submit 完成不删除；场景中 SDK permission_decide 仍注入 run binding，close 仍 disconnect 一次。
认证在 describe 等待前发布：闸门确定性制造 hello_ok 尚未发出而 request 已能处理的真实序列。
close 场景复用 `scripts/a5-19/fixtures-b.mts:71` 和 `production-close-b.mts:117` 的 in-flight/pending close 次序，在连接边界重采真实 TS；不重新宣称新 run 的生产次序可达性。close 场景保留原请求的旧 runId，但 abort 调用只有 sessionKey，沿 A5；没有给旧 socket 添加 run 栅栏。

仅替换真实 crypto.randomUUID 创建、并由生成器记录的 UUID 为按创建顺序的标记；不匹配 UUID 正则、不屏蔽用户提供的标识。
generation 保留实际数值、类型和进程内顺序。Rust 回放用同一产品 issuer 和独立 AtomicU64 模拟一个新 TS 进程，按相同顺序创建连接；不修改观察结果中的 generation。
比较使用解析后的完整 JSON（对象键顺序不计）；两次生成器输出额外逐字节比较。

`BindingOwners` 是无 pending 状态的连接绑定子组件，只能用于此范围；它不提供 pending producer/consumer、断线保留与 replay/answer 的完整端口，A7.3 必须实现后再装配。
`ConnectionGateway` 是连接到业务 owner 的注入 trait。pr2 合入后仍须用真实 live owner 接线 disconnect 与无 runId abort；此包没有实现或声称完成该 adapter。
默认 pairing/dispatch 未改。hello 和 describe 的能力构造归 pr4；submit 的业务接纳、durable accept 和终态闭环归 A7.2。

`negative.py <evidence-dir>` 在独占 /tmp 临时 crate 中复制本包源文件和金样，退出后自动删除；不修改本工作区产品文件、基线或 node_modules。
它先跑原实现，再串行运行十一项行为变异和一项溯源哈希变异，要求编译成功且 oracle/并发断言失败；tampered-source-hash 还须精确命中源码哈希不符消息；另外两项编译期类型负控使用真实 ProjectRuntime，交换 Arc<ProjectRuntime> 和 WS generation 的 u64，必须报 E0308。
临时 crate 只引用仓库已有依赖/固定版本，不联网。原始输出及完整替换表达式写入证据目录。
多线程测试用屏障在陈旧 disconnect 动作前发布新 binding，并在旧 submit 清理前预约同 session 的第二次 submit；旧清理必须删除整个 session。ticket-scoped-in-flight 负控恢复独立 ticket 的保留语义，必须在新增真实 TS 场景失败；submit-clears-interaction-session 负控验证两个集合的不同清理时机。不靠 sleep。

`connection_context_fixtures_regenerate_identically_from_ts` 为默认忽略的真实 TS 再生成测试，复用 `tests/common/baseline.rs` 的逐字节检查；具备完整基线环境后可用 `cargo test -p pilotdeck-gateway connection_context_fixtures_regenerate_identically_from_ts -- --ignored` 运行。
