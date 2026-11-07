# Kernel 产品会话 backend 候选

本变更把 `ProjectSessionFactory` 的 `SessionBinding` 出口接到 `SessionBackendFactory`，产物为实际 `SessionBackend`。旧 `TurnHostFactory` 包装 stdio host；Kernel backend 直接连接常驻 Kernel 的公开 L-C ABI2 / NDJSON / Unix socket，不创建 `HostClient`，不把 Kernel 当成 MP2 sidecar。产品网关保留 ingress 去重、`TurnPublisher`、`GatewayAgentEventProjector`、coordinator 与 run replay；执行准入、模型、工具、审批、原生历史与资源清理由 Kernel 承担。

## 运行与所有权

新会话默认 `legacy`；显式使用：

```sh
PILOTDECK_SESSION_BACKEND=kernel \
PILOTDECK_KERNEL_SOCKET=/absolute/kernel.sock \
pilotdeck-gateway --native --home /isolated/home --project /isolated/project \
  --listen 127.0.0.1:0 --token-file /isolated/token
```

`PILOTDECK_KERNEL_LOOP_VERSION` 是新会话的可选预期 revision；省略时读取 `execution.open.loop_revision` 并落盘。revision 包含 Kernel 校验过的 provider 指纹，不能假设它等于装配文件中未加工的 revision 字面值。当前产品 profile 只接受无 input controller 的冻结装配，context 固定为 `default`，默认 binding 及实际 revision 由 Kernel 回报；选择 builtin loop 需要另一冻结装配及新会话。

`new_session` 在 Kernel 模式先建立 backend，再回复产品 session key。`<transcript>.backend.json` 以 canonical project、未经路径清洗的产品 session key、Kernel session ID、socket、binding、context、实际 loop revision 与 lease node ID 固定所有者。打开已有会话只读该 owner；环境变量只选新会话，不改老 owner。已有非空 transcript 缺 owner 时归 legacy。路径清洗碰撞或项目别名越界会拒绝，不能借当前默认值重新解释旧历史。

`--sidecar` 在解析时可省略，真正选择 legacy 执行时才要求它；Kernel owner 不依赖 sidecar 二进制。已有 legacy owner 在配置 sidecar 的兼容模式继续走原实现；在 Kernel continuation 流程中可只读 history/export，再显式导入新 Kernel session。连接失败、协议不符、revision 不符不会回退 legacy，也不会双写执行记录。

Kernel journal 是执行、最终 outcome 与 native history 的权威。产品 transcript/RunRegistry 是 UI 或 ingress 投影，不能拿它们恢复 Kernel 活动句柄。最终 UI `turn_completed` 必须等 Kernel 持久化的根 `response`；provider 提前发出的同名 event 会暂存。未知 outcome 保留原 Kernel session/execution identity，用可见错误说明等待已结束不代表外部效果撤销；不会自动重放。明确的新任务仍交给 Kernel 本身的策略裁定。

backend 先取得原生 lease，并在存活期间续租；请求 Future 被取消或超时时，其占用 socket 一并关闭，下一次查询重新握手，不会把迟到响应配给下一条请求。重开时从 `execution.events` 找根调用，再用 `execution.get` 读取活动状态及原始 wait。审批回复经 `execution.respond` 携带原 call/wait ID；客户端连接 generation 仍使用原交互 coordinator 的门。

## 支持范围与真实缺口

`describe_server.kernelProfile` 是当前 profile 的机器可读清单，正本是 `native_process/kernel_profile.rs`。会话模型变更/MCP 动态配置、seed/rewind、旧替换/fork、旧后台任务与旧执行历史写接口在 Kernel owner 上明确拒绝；它们不会先写 PD store 再谎报 Kernel 已应用。legacy owner 不受该 profile gate 影响。`permission_list`、steer 与 run reads 会查询原 Kernel execution，run read 同时保留产品 replay 投影。

当前 profile 的流式 live 路径仍使用既有 projector/publisher/RunRegistry；Gateway 重启后 authoritative active execution/wait/history 可以查询，但自动补齐停机期间缺失的所有 PD run frame 并恢复原客户端 sequence 尚无完整组合验收。不能把一个成功的 native text turn 当成 UI 全量恢复通过。Kernel bootstrap 的冻结模型路由、预算/context/tool 能力由 provider 及 Kernel 装配决定，PD 的 session selector 不会暗中覆盖它。

显式历史服务：

- `export_session_transcript {sessionKey}` 只在源 quiescent、无 pending interaction、无 active/Unknown Kernel effects 时生成 `pilotdeck-history-import-v1` 包。完整工具调用必须有对应结果；工具结果只接收可核验的内联文本；图像/文档附件只接收内联 base64 字节并生成 SHA-256 清单。未知 block、动态路径/URL 附件与活动 runtime 元数据拒绝导入。
- `restore_session_transcript {sessionKey:newKey, transcript:exportedEnvelope}` 验证整个包和附件 hash，要求同 canonical project、不同 session key、源已静止、目标是新的 Kernel owner。经 `execution.history.import` 进入 pristine target；原 receipt 与内容 hash 组成幂等依据。完整 checkpoint、active handle、pending approval、lease、unknown effect 都不是可恢复状态。
- `read_session_messages` 对 Kernel 读取 native history；对 legacy 只读现有 JSONL 的消息投影。旧 transcript 不会被导入覆盖。

[补充入口矩阵](kernel-entry-matrix.json)逐项列出原总账全部 678 个 ID 的 Kernel 处置。它不修改原冻结/覆盖账本，不给未验证入口打 PASS。原账本的状态总数为 358 `ts-only`、108 `placeholder`、86 `ts-forwarded`、47 `retained-shell-unverified`、28 `rust-unaccepted`、24 `ts-orchestration-pending`、19 `partial-native`、8 `unsupported`；这些是固定产品基线账本中的登记值，不是本次对 HEAD 678 入口的重新验收结果。

原账本明确 unavailable 的 8 个 ID 为 `gateway.background:always-on`、`gateway.method:always_on_abort`、`gateway.method:always_on_apply`、`gateway.method:always_on_rerun_plan`、`gateway.method:sop_resume`、`gateway.method:sop_status`、`sidecar:factory`、`sidecar:tcp`。当前仍有明确未配置的子代理历史、SOP、always-on 与部分全局 settings/MCP/后台能力；各自保留原错误，不扩成本次重写全部上游功能。其余 UI、桌面、channel、CLI 子命令以矩阵中的 `not_revalidated` 追踪，不能据 crate 编译成功推导已通过。

## 验证边界

本 lane 自证为：gateway `cargo check`；原生 UDS 文本→原 wait permission→同 execution.respond→最终事件；连接丢失保留原 operation identity；transfer 包 hash、完整 tool pair、附件字节与拒绝 resume 元数据。真实组合 WS、CLI run、Kernel 重启、资源/进程回收与 provider 测试由总集成树补证。真人 UI/桌面实效与全入口验收尚未因这些测试关闭。

当前 turn preferences 已从原产品选择器接到原生 payload.agent：permissionMode 逐 turn 取 live store，thinking / output style 在下一次 prepare 取选择后的配置；活跃 Kernel execution 上拒绝改动。模型/binding 仍由 assembly 冻结。原 UI `{mode:"auto"}` 和与装配相同 provider/model 可提交；实际 route 变更与未绑定 SDK/模型调参明确拒绝。

原产品会话索引现在由 Kernel 已提交 `execution.history.snapshot` 的 revision/hash 重建：经过原 transcript writer fence 原子替换 JSONL，只有标记为 Kernel cache 的 accepted_input / durable_message / session_metadata，不生成 PD execution lifecycle 或 turn_result。相同 revision/hash 不重写，revision 回退或同 revision 不同 hash 报错；并发 UI metadata 改动按 writer sequence 拒绝覆盖。build、read、import、实际 Kernel terminal 都同步缓存，list_sessions 从 canonical owner 文件发现已删除的缓存再重建。原 UI read_session_messages 使用 WebMessage 历史 DTO，canonicalMessages 仅供显式导出。session_model_get 读冻结装配的 provider/model，选择器不会因此卡在历史加载；setter 仍拒绝真实 route 变更。此缓存链 focused 自测通过，真实 UI 完整交互由总集成继续验证。

Gateway restart active view 在没有本地 attached pump 时从 Kernel root journal 重建，使用原 Gateway projector，保留 root correlation/run ID、原 child wait ID 和真实终态；有界视图最多 512 帧，截断显式标记。该逻辑没有提交 execution.input，也不把恢复观察当审批。Native Unknown 的公开错误 code 是既有产品 `result_unknown`，附 nativeCode `KERNEL_OUTCOME_UNKNOWN` 和原操作 identity；CLI 既有 SDK 因此保持 result_unknown recovery 语义，不改旧 CLI oracle。

UI history/live 对齐使用原始 root correlation 和模型 block identity：WebMessage 保留 blockId/timeline，读取 provider-authored display metadata.turnId/createdAt/kernelMessageId。旧 canonical 缺注释时，只按 journal 唯一 root block 锚点以及该 root 的精确输入组合补 UI identity，不按全局文本去重；内部模型 timeline.turnId 不改。缺失历史时间的旧记录不能据此推断执行时间，新 provider 注释保存原 turn 创建时间。

Reopen 仅观察时不申请 lease；execution input/respond/import 才从公开 lease_status 读实际 holder/epoch，保存的同 node 活跃 lease 用 renew，inactive lease 才 acquire，foreign holder 不被接管。native resume 的 additive backend/abortSemantics 使 CLI 取消 ACK 后继续观察原 turn；legacy resume 响应/SDK 信号行为不变。UI 派生 timeline.turnId 与外层产品 root 保持一致，避免 retained sessionTimeline 把 canonical 内部 turn 当产品 turn；原 Kernel canonical timeline 不修改。

未 capture 的原生 turn 也有可重建目录索引：原 Kernel accepted root 的 prompt/run/state 以独立 index hash 更新 session_metadata，revision/hash 仍只描述 committed history。接受回执后立即更新，因此 retained UI 的直接目录读取可发现 needs_human 会话。未 capture 用户文本在 history API 中显式 source live，未进入 canonicalMessages/导出；冷读同步原 journal active stream。

实际 native turn 读取 leased router.stats.modelPricing，经 ordered entries 端口保留 JS 枚举/substring 定价顺序，None 保持 bootstrap 默认，显式 empty 保持 empty。每 turn mode/basePermissionMode 通过 native prepare，managed 配置保持原优先级。现 profile 未安装产品 memory resolver、InstructionDiscovery 与 plugin contributions；检测到实际启用/发现内容时只拒绝 effect，readonly 不被阻断。没有配置的 null/false 等不被泛化成拒绝，legacy 不执行这个检查；不以此声称这些产品端口已完成迁移。

显式历史导入的 display metadata 也验证唯一性/形状：空或重复 kernelMessageId、非法 turnId/timestamp/timeline、同声明 turn 的重复 prose block identity 均拒绝。合法 call/result 共享 tool timeline 仍由完整工具配对验证允许；无声明 turn 的旧 block ID 可重用，但 UI 派生 ID 用确定性目标 session-local namespace 防止合并，原始 canonical 字节语义不改。展示标识不是可恢复执行句柄。

Deadline 只经广告的 scheduler.input-limits.v1 通用面：prepared operationDeadline 的绝对 UTC 毫秒进入 execution.input outer limits，未广告即明确拒绝这项配置。模型轮数与 Scheduler step 数没有自动对应。pending 用户行时间优先原 Kernel call.accepted payload.accepted_at_ms；旧 journal 的首次 receipt observation 时间仅作 UI cache 字段持久化并标来源，刷新不改为 now。它不是执行状态权威，也不据此声称原 UI replay 的所有 activity 时长已恢复。

完整产品会话与模型上下文现在分开读取。广告 `scheduler.history-archive.v1` 的 Kernel 提供只读 `execution.history.archive`：以 snapshot revision 固定本次目标，逐页重建原已提交 canonical history；跨页产生的更新留给下次读取，不冒充原子多页快照。按原生 message ID 保留压缩前完整内容，synthetic summary 只留在模型 context，不成为用户消息；不同 ID 的相同内容始终保留多条。旧无 ID 行只按相邻上下文的有序 occurrence 对齐，不做全会话文本去重。已提交 checkpoint 的 compactMetadata 产生一条展示 boundary。JSONL 可以删除后从 Kernel 原版本重建，不能把上次 PD 缓存当全历史权威。`read_session_messages.canonicalMessages` 用于完整已提交对话的验证导出，`contextMessages` 单独返回当前模型上下文；活跃尚未 capture 的用户行仍只属于 source live。

原 `/compact` admission 现接持久 Kernel owner 的私有 command port；旧 legacy 未配置 port 时保留原错误。必须同时有 archive feature 及实际保存 binding 的 `loop_permissions.resources` 含 `native.command.compact.v1`，provider 还会校验已安装 bootstrap `native_commands:["compact/1"]`。入口持有原 SessionOwner/handle maintenance fence，提交 `nativeCommand:{kind:"compact",version:1,manualForce:true}`，不附普通消息/提示；摘要和 history CAS 都由 Kernel native Loop 内的原完整算法执行。网关只消费原 durable response 的 `command.result`，然后使用既有 manual compaction 展示协议。普通 browser submit 注入 nativeCommand 明确拒绝；超时取消发原执行 control 后继续等原终态，Unknown 保留标准 result_unknown 和原 session/execution ID。代码接线与 focused 测试不等同于实际模型压缩/重启/原 UI 组合验收，后者由总集成补证。

历史迁移的源验证使用独立的只读边界。legacy export/import 都拒绝 reader 的 malformed/truncated/too-large 等 diagnostics，复用原 DomainState 与 AgentLoop operation ledger 确认无 open turn、queued work、pending 生命周期及未决 operation；不更改旧 reader 的容错规则，不修写源文件。首次导入在同一 canonical transcript fence 内持有外部 LedgerLock（未 attached）或原本地 writer 的 handle maintenance（已 attached），重新导出当前 canonical 包并比较完整 hash，锁保持至 Kernel import 收据返回。源 owner/backend/project/session 不符、旧导出过期或完整性不明均拒绝。

Rust LedgerLock 不会停止任意旧 TypeScript 进程：原 TS writer 没有这项锁。因此 detached legacy 导入还要求调用者明确传 `sourceRuntimeStopped:true`，表示已停旧 runtime；这只是有意的 caller attestation，不是网关伪造的 PID 检查或 shutdown receipt。现有 retained desktop 的 process-scope 能验证其自己拥有的子进程退出，但没有通用持久化 TS gateway shutdown receipt API；部署迁移应先停止其拥有的旧 runtime 并保留实际退出证据，才作上述声明。冻结 fixture 的 writer 已退出/源不可变是由构造和进程记录证明，不等于任意外部文件的 runtime 都已停止。

成功导入的丢响应重试保留原 Kernel receipt，即使静止源后来增加了有效历史。产品先读取 target 的实际 history revision：revision>0 的 target 已不 pristine，此时以完全相同 key/body 请求 import 只能取回原 receipt 或被 Kernel 拒绝，不能新写一次；receipt0/未导入目标仍必须通过当前源 hash 比对。产品没有私有 receipt 权威，也不会因源变化重新导入到旧 target。源实际 active/Unknown、损坏或未明确停止仍拒绝。
