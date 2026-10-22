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

81 个连接场景记录每一步完整 frames、closes、calls，每步的 in-flight/interaction session 集合，以及最终帧发送瞬间的 in-flight 集合；27 个真实 provider 操作记录完整返回值/异常消息。
本轮补录（#474 后续）：`lone-surrogate-{id,method,client-name,session-key,params}` 五个场景与 `ascii-describe-pins-top-level-order` 由真实 TS 录制器生成；这些场景及 submit / close-preserve / request-after-close 另记录 `framesText`（真实 `sendText` 原文，仅把实际 `randomUUID` 换成已记录标记）。Rust 回放逐字节比较该原文，对象键序、`\ud800` 转义和数字格式都必须与 TS `JSON.stringify` 一致；未记录 `framesText` 的场景仍按「对象键序不计」的解析后比较。
深嵌套不由 128 层截断：Node 22 `JSON.parse` 实测到 1,000,000 层仍接受；`parse_json` 的 `RawValue` 语法预检走迭代 `ignore_value`，同样接受到 1,000,000 层（protocol 测试另有 30,000 层构造/克隆/丢弃）。连接层 `deeply_nested_*` 测试固定 200/1,000/30,000 层合法帧正常 dispatch、非法深层帧仍 close 4002 `invalid_json`；证据见 `_tmp/20261022-a72-pr0-connection/ts-depth-probe.json`。
覆盖 JSON、hello 缺字段/类型、协议先于 token、未知/空 clientName、认证后非法 frame、错误层级（含 validation/details）、SDK run/control、服务端 binding 覆盖、close preserveTurn 两分支和 close 的集合顺序。
同 session 双 submit 用两个独立闸门保证先后入场、先释放第一个、再 close、最后释放第二个。TS 的 Set.add 去重，任一 finally 都按 session 无条件 delete；所以第一个完成后 close 不 abort。Rust 保留这一行为，不能按 ticket 延长在途状态。interactionSessions 同样按 session 去重，但 submit 完成不删除；场景中 SDK permission_decide 仍注入 run binding，close 仍 disconnect 一次。
新增 permission_decide 的 null／缺失 params 场景：真实 dispatch 仍收到注入 binding 后的对象并成功，随后读取原 frame.params.sessionKey 抛错，返回 gateway_request_failed；null 与 undefined 的错误文案分别保留。空对象 params 成功且不发 permission_changed。null-params-as-object 负控把原始 null／缺失值改成对象，必须精确命中新场景 step 1。
认证在 describe 等待前发布：闸门确定性制造 hello_ok 尚未发出而 request 已能处理的真实序列。
close 场景复用 `scripts/a5-19/fixtures-b.mts:71` 和 `production-close-b.mts:117` 的 in-flight/pending close 次序，在连接边界重采真实 TS；不重新宣称新 run 的生产次序可达性。close 场景保留原请求的旧 runId，但 abort 调用只有 sessionKey，沿 A5；没有给旧 socket 添加 run 栅栏。

仅替换真实 crypto.randomUUID 创建、并由生成器记录的 UUID 为按创建顺序的标记；不匹配 UUID 正则、不屏蔽用户提供的标识。
generation 保留实际数值、类型和进程内顺序。Rust 回放用同一产品 issuer 和独立 AtomicU64 模拟一个新 TS 进程，按相同顺序创建连接；不修改观察结果中的 generation。
比较使用解析后的完整 JSON（对象键顺序不计）；两次生成器输出额外逐字节比较。

`BindingOwners` 是无 pending 状态的连接绑定子组件，只能用于此范围；它不提供 pending producer/consumer、断线保留与 replay/answer 的完整端口，A7.3 必须实现后再装配。
`ConnectionGateway` 是连接到业务 owner 的注入 trait。pr2 合入后仍须用真实 live owner 接线 disconnect 与无 runId abort；此包没有实现或声称完成该 adapter。
默认 pairing/dispatch 未改。hello 和 describe 的能力构造归 pr4；submit 的业务接纳、durable accept 和终态闭环归 A7.2。

`negative.py <evidence-dir>` 在独占 /tmp 临时 crate 中复制本包源文件和金样，退出后自动删除；不修改本工作区产品文件、基线或 node_modules。
它先跑原实现，再串行运行十五项行为变异和一项溯源哈希变异，要求编译成功且 oracle/并发断言失败；`serde-json-inbound` 恢复旧 serde 解析后必须在新增 `lone-surrogate-id` 场景 step 1 失败；tampered-source-hash 还须精确命中源码哈希不符消息；另外两项编译期类型负控使用真实 ProjectRuntime，交换 Arc<ProjectRuntime> 和 WS generation 的 u64，必须报 E0308。
两处 cargo 槽位脚本均从负控装置所在 checkout 的 fixture.parents[4] 推导仓根，包含空格的路径作为独立参数传入 bash。
临时 crate 使用自身独立 target（不复用正控构建产物），只引用仓库已有依赖/固定版本，不联网。原始输出及完整替换表达式写入证据目录。
多线程测试用屏障在陈旧 disconnect 动作前发布新 binding，并在旧 submit 清理前预约同 session 的第二次 submit；旧清理必须删除整个 session。ticket-scoped-in-flight 负控恢复独立 ticket 的保留语义，必须在新增真实 TS 场景失败；submit-clears-interaction-session 负控验证两个集合的不同清理时机。不靠 sleep。

`connection_context_fixtures_regenerate_identically_from_ts` 为默认忽略的真实 TS 再生成测试，复用 `tests/common/baseline.rs` 的逐字节检查；具备完整基线环境后可用 `cargo test -p pilotdeck-gateway connection_context_fixtures_regenerate_identically_from_ts -- --ignored` 运行。

本轮追加 sessionKey 为 5 / true / 0 / false / null / 空字符串的真实 TS 场景，原值保留至 reconnect/disconnect/abort；submit 登记和 requested 通知使用 JS 真值判断。SDK permission_decide 仍按 TS 独立要求字符串，不能用通用真值替换。non-string-session-as-empty 负控恢复字符串筛选，须精确失败于 session-key-5 step 1。
submit 的直接抛错、先 completed 后抛错、无终态、permission_request 均采真实 TS。result_unknown 就是基线的无终态 fallback；抛错回 ok:false response，不额外发 final 事件。
真实 TS disconnect 抛错会中止循环并保留集合；金样记录两次 close rejection 和后续 session 未处理，不擅自改为聚合错误。close 后消息和随后再次 close 也从真实文本适配器回调采集；本层无永久 closed 标记，实际 WS 适配器的投递可达性留 pr2 接线验证。
receive 返回 future 必须由传输适配器保留并驱动至完成，不能放入会因 socket close 落败而丢弃的 select! 分支。调用 closed() 的同时继续驱动已有请求任务，按 TS Promise 生命周期等待完成。本层没有任务宿主，drop 会取消请求，只清理 in-flight；没有伪造 final/error。cancelled_receive_sends_no_invented_terminal 固定该取消边界，pr2 必须补真实 owner/任务 drain 验证。
text_callbacks_can_synchronously_reenter_connection 在 sink 回调中先 try_lock 以失败代替挂死，再真实同步进入 receive/closed；覆盖通知、hello、事件和全部 close 错误分支。

`close-union-reverse-lexical-order` 先 reconnect a、再挂起 submit b。真实 TS 在 close 前为 inFlight=[b]、interactions=[a,b]（submit 同时登记两个 Set，GatewayWsConnection.ts:124-127）；close 的 Set 并集按插入序得到 [b,a]（:49-58），记录 disconnect b → abort b → disconnect a。该逆字典序场景与 `sessions_preserve_in_flight_then_interaction_insertion_order` 单测共同防止把 sessions() 改成排序；单测另覆盖 inFlight=[b]、interactions=[a] 的直接输入。

修复轮 4 的真实 TS 补采：submit_turn / elicitation_respond / reconnect_interaction 分别覆盖 null、缺失和空对象 params，三个方法都成功 dispatch（submit 发正常终态；elicitation 返回 accepted；reconnect 首次 no_pending，随后同绑定 reconnected），且两个 session 集合均为空。这与 permission_decide dispatch 后读原 params 抛错不同，不能推广 permission_decide 的错误分支。
`reconnect-null-and-missing-previous-binding` 同时覆盖无 current / 有 current：connection 必须原样保留显式 previousBinding:null（缺失时不出现该键），真实 coordinator/native provider 把 null 当作缺失处理，分别 no_pending / reconnected。仅回放适配器在解析可选 previous 前按 TS 真值过滤；必填 WsBinding::from_json(null) 的拒绝不变，产品 context 转发也不变。
bindingSteps 追加 same next/current + 错 previous → stale_binding、有 current 保持不变；disconnect 后无 current + previous → stale_binding、仍无 current；随后显式 null previous 的首次 / 重复 reconnect 分别 no_pending / reconnected。binding.rs 谓词与实测 TS 一致，不改产品行为。

修复轮 5 在 bindingSteps 末尾追加 previous 为 0、false、空字符串的三次同绑定 reconnect；真实 TS 均返回 reconnected，绑定保持 d / 4、requests 为空。它们固定 JS 假值即缺失的语义，防止把回放适配器的 truthy 过滤缩成仅过滤 null。

修复轮 6 用同一真实 TS 录制器追加四方法 × UI/SDK × 五类 params 的 40 个场景：字符串 `"str"`、数组 `[1,2]`、数字 `5`、布尔 `true`，以及 UTF-16 字符串 `"A😀中"`。submit_turn / elicitation_respond 保留下标键并注入 binding；permission_decide 的 UI 同样注入，SDK control 仍不注入；reconnect_interaction 只提取既定字段，所有这些输入均得到空 sessionKey。数字/布尔不产生可枚举键。每格记录 hello、请求结果、close 的完整 frames/calls/集合。
字符串按 UTF-16 码元展开，所以补充平面字符分成两个孤立代理项；ConnectionGateway 的 request/submit params 使用既有 JsValue，不能转成 serde_json::Value 或替换成 U+FFFD。回放对完整 JsValue 排序对象键后比较，保留所有字符串码元和数组顺序，只有原有实际创建 UUID 的映射。`non_object_string_params_match_ts` / `non_object_array_params_match_ts` 分别独立回放字符串/数组格，保持原进程的 generation 序号并断言确实执行 16/8 格。
`negative.py <evidence-dir> non-object-params-as-empty` 先跑完整正控，再恢复旧的「非对象一律空对象」；必须编译成功、退出 101，同时在字符串和数组各自的 step 1 出现完整值断言失败。第二个及后续参数可选择其他已定义变异；无选择时仍跑全部行为与类型负控。
