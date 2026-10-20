# Gateway async hooks · 真实 TS 序列

来源为固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`generate.mts` 直接 import `InProcessGateway` 与 `SteerMailbox`，运行真实注册、提交、过期、结算、保留清理和 steer 方法；38 个场景、285 个操作的返回值、事件与持久化 mutation 均来自这次 TS 执行。Rust 回放精确断言场景数、操作数和完整输出。

## 装配取舍

完整 Gateway 构造会装配存储、附件、遥测等无关服务；这里用 `Object.create(InProcessGateway.prototype)` 提供最小宿主：两个原生 Map、活动 run 查询、事件收集器和真实 SteerMailbox。`steerTurn` 与 `prepareAttachmentTurn` 均为原方法，附件组装用真实 `GatewayAttachmentTurnComposer`（`gateway/dialog/GatewayAttachmentTurnComposer.ts:56-72`，无附件时返回文本输入与空授权数组）；仅把 `funasrInstallCommand` 固定为占位串，避免包根查找（该值只用于附件提示，本夹具不产生）。router 只提供 `activeTurnRunId` 与转发到真实 SteerMailbox 的 `steer`（对应 `gateway/SessionRouter.ts:307-315`、`agent/session/AgentSession.ts:284-297` 的转发；会话缺失 / 非 running 由 Rust 侧的 turn 查找返回 None 表达，不在本夹具内）。全局 `Date.now` 与 timer scheduler 可控，支持只推进墙钟而不执行 timer，以区分显式期限校验和回调清理。每个场景独立调度表；回调按 deadline、登记序号执行，模拟 Node 的 delay 截断/范围转换，所有回调函数来自 TS 实现。

`extension/hooks/execution/AsyncHookRegistry.ts:57-169` 是另一套 hook 执行器生命周期表，没有 session/run、过期或 outcome 保留计时。仅录它无法证明本任务行为，因此本夹具直接录 Gateway；未把它当成计时逻辑的替身，也未在此包新增那套执行器生命周期 API。

关键源码：

- `gateway/client/InProcessGateway.ts:218-219`：默认期限 60,000 ms，保留 300,000 ms。
- `:699-729`：登记、同轮幂等、跨轮/跨会话冲突，计时器 unref。
- `:1859-1883`：steer 的 run fence、用户消息及授权投影。
- `:1895-1932`：输入校验、outcome 优先、deadline、共享 delivery。
- `:1934-1999`：steer/空结果的 fence、事件、过期、保留、按轮过期。
- `:3516-3548`：deferred output 限制与 context 包装。
- `agent/session/SteerMailbox.ts:47-74`：持久化前后仍须匹配原 turn。

保留两处容易被“理想化”改错的实测行为：登记只检查 pending，旧 outcome 可以遮住同 ID 的新 pending；尚在持久化的 steer 若先遇 hook 过期、后被接受，会再记 delivered 并从成功时刻重计保留期。`expiry-during-write-reregister-then-accepted` 钉住二者交错：持久化中先过期、同 ID 重登记、旧投递再被接受——TS 按身份删 pending（新登记存活）、按 ID 覆写 outcome（delivered），新计时器到点再过期一次并多发一条 expired 事件。空 additionalContext 字符串仍生成包装文本并经过 steer；完全没有 context 则只检查 run，不写 mailbox。

深度边界（#312）：`hookEventName` 的 `String()` 用共享 `js_value` 的 2000 层上界（`MAX_ARRAY_DEPTH`）。Node v22.22.0 顶层 `String(嵌套数组)` 实测在 2622 层才抛 RangeError（2621 成功），且该阈值随调用栈深度缩小；`(2000, V8 实际阈值)` 是已接受的不可判定带——TS 在此带内报 `INVALID_ASYNC_HOOK_RESULT`「belongs to …」，Rust 报栈溢出。oracle 只录两侧都失败的 5000 层，不录带内点。

## 覆盖与安全

期限、delivered 保留、expired 保留均覆盖到点前 1 ms、恰好到点、到点后；另测回调未调度、默认/零/负数/小数 timeout（含不推进 timer、只推墙钟时期限被钳到 1 ms）、同轮重登不延期、保留 timer 替换、跨会话 unknown、并发 duplicate、失败共享与重试、旧 run（提交前/持久化中）、关闭 mailbox、按轮过期、事件开关、payload 错误优先级及 UTF-16 字符串。

生成前后调用共享 `bindSourceTree`，校验固定 commit 和整个 tracked src 的实际字节，并断言整棵 tracked 树干净（`git status --porcelain --untracked-files=no` 为空，同 `workspace-snapshot/generate.mts`）；`sourceSha256` 经共享 `baseline_source_guard.mjs::sourceClosureSha256` 记三个入口的相对 import 传递闭包（650 个文件，均取固定 commit 的 blob 并核工作区字节）。回放经 `common/baseline.rs::oracle`，设置 `PDRS_BASELINE` 时再核本地 checkout。每次生成用 `mkdtemp` 独占目录并切入其中，finally 恢复 cwd/clock/timer；删除前核对临时父目录及所有权前缀。无固定路径清理，也不读用户配置。

## 复现

从仓库根运行（Node 22，使用基线现有 tsx，不安装依赖）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
scratch=$(mktemp -d "${TMPDIR:-/tmp}/pdrs-async-hooks-verify.XXXXXX")
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/async-hooks/generate.mts "$scratch/first"
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/async-hooks/generate.mts "$scratch/second"
cmp "$scratch/first/oracle.json" "$scratch/second/oracle.json"
cmp "$scratch/first/oracle.json" src/pilotdeck-gateway/tests/fixtures/async-hooks/oracle.json
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test async_hooks
```

测试另将真实 TS `concurrent-empty` 的双返回值和事件流用两条 Rust 线程重放 64 次；生产 Tokio timer 用 paused time 验证登记时就固定 deadline 及 drop 取消。真实运行时不会等待 hook timer 保活，timer 与 registry 之间用弱引用，registry 释放时取消剩余 timer（`dropping_registry_cancels_pending_and_retention_timers`：释放后 pending 期限与 outcome 保留两个 timer 任务均被 abort，推进时钟无回调、无事件）。

TS 的 submit 同步前缀（outcome → pending → deadline → expire）不可打断；Rust 的过期分支在状态锁下返回实际结算结果（自己结算为 expired，否则按已存 outcome 映射 delivered→duplicate），旧 run 分支持有 admission 锁直到过期完成。两条多线程测试分别复现期限分支与旧 run 分支的竞态。

事件顺序：TS 结算与发事件在同一同步段。Rust 在状态锁内把事件按结算顺序入队，再由单一排空者在所有 registry 锁之外逐个、按序调用 sink；`expire_for_turn` 在一次加锁内结算整轮。sink 可以重入 registry（不会死锁，重入产生的事件排在当前调用之后）；另一线程正在排空时，操作可能先于自身事件返回。多线程测试 `expiry_event_precedes_delivery_that_settled_after_it`、`expire_for_turn_burst_is_not_interleaved` 把排空者卡在 sink 内并在另一线程结算，要求 TS 顺序；`sink_may_reenter_registry_without_deadlock` 覆盖重入。

本轮负控与完整自证原始输出在任务交接 `RESULT.md` 所引目录中；负控只改 Rust 实现，不改 oracle。
