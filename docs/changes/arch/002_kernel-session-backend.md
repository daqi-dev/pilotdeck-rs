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

`describe_server.kernelProfile` 是当前 profile 的机器可读清单，正本是 `native_process/kernel_profile.rs`。会话模型/permission/thinking/style/MCP 动态配置、seed/rewind、旧替换/fork、旧后台任务与旧执行历史写接口在 Kernel owner 上明确拒绝；它们不会先写 PD store 再谎报 Kernel 已应用。legacy owner 不受该 profile gate 影响。`permission_list`、steer 与 run reads 会查询原 Kernel execution，run read 同时保留产品 replay 投影。

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
