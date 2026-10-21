# 内存接纳与 completion fence 的真实 TS 夹具

固定基线为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`oracle.json` 的 15 个场景来自真实 `SessionRouter`、
`GatewayTurnCompletionFence` 和 `InProcessGateway.abortTurn`；不是按源码手写期望。
生成器在运行前后调用共享 `../_shared/baseline.mts` 守卫，核 HEAD、tracked 状态和
7 个实际执行的源文件的提交版/工作版 SHA-256。每个场景都使用全新的对象；时钟固定为 epoch 0，idle timeout 为 60 秒，
防止真实 sweepIdle 在接纳前因毫秒翻转回收 fake session。

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
export TMPDIR=/tmp TSX_DISABLE_CACHE=1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-admission/generate.mts /tmp/a71-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-admission/generate.mts /tmp/a71-second
cmp /tmp/a71-first/oracle.json /tmp/a71-second/oracle.json
```

运行环境：本次 Node v22.22.0；tsx/esbuild 使用基线现有安装，无网络、无依赖安装。
源码整文件经现有 esbuild 转为 CJS，方法体不裁剪、不替换。
Router 的 agent barrel 改为真实的 AgentFactoryProvider、AgentRegistry、AgentHandle、
ScopedServiceRegistry 四个窄依赖。未使用的 Gateway 服务 import 被 fail-closed Proxy
隔离，访问即抛错；Gateway 构造函数未执行，以原 prototype 和显式注入的
真实 Router/fence/原因表调用完整的 `abortTurn` 方法。
fake AgentSession 仅记录 abort 调用、提供 seedReadState 的可释放闸门与 dispose；
不模拟接纳、取消匹配、完成身份或 Gateway abort 判断。

场景覆盖同会话重入、跨会话接纳、maintenance 占位、活动 turn 拒绝 maintenance、旧 end、shutdown、空 runId、
同 runId 的不同 handle、旧 complete、重复 cancel 的首个原因、无 handle wait、
Router 接纳但 fence 尚未安装、显式旧 abort、新 run 替换后的旧 abort。
另录制丢弃旧 resolver 后其 Promise 仍 pending 的行为。每一步录制返回值、所有 handle 的取消状态/原因、等待者是否已返回、session abort
调用以及 Gateway 原因表。异步先后由 Promise/闸门控制，不使用 sleep。
`maintenance_start` 记录真实 `seedReadState` 是否获准；拒绝时校验 TS 原始错误消息，
仅把接纳结果映射为布尔值，与 Rust 的内存预约接口比较。活动 turn 场景还验证拒绝后
原 turn 不变、另一会话可接纳，以及 turn 结束后维护可正常开始和释放。
除将 `undefined` 查询结果表达为 JSON null 外，不屏蔽状态或标识；不存在时间戳、
随机 UUID 或临时路径的归一化。真实 AgentHandle 内部 UUID 不进入观察结果。

Rust 测试 `runtime::session::tests::real_ts_admission_completion_and_gateway_abort`
按夹具步骤调用产品组件并逐步比较整个结果；额外测试覆盖多线程原子接纳、UTF-16
键、首轮 poll 前的替换/完成/取消以及原因清理。
回放先调用全仓统一的 `crate::test_baseline::oracle` 守卫，校验 commit、路径与哈希格式；
设置 `PDRS_BASELINE` 时逐项核对提交版和工作版源码哈希。

```sh
cd src
cargo test -p pilotdeck-gateway --lib runtime::session::
```

`negative.py <evidence-dir>` 在独占 `/tmp/pdrs-a71-negative-*` 下复制真实 Cargo workspace
（排除 target、git、node_modules、env 文件）与 specs，复用原 manifests、依赖和
共享 `tests/common/baseline.rs`；在当前生命周期模块中运行 admission 测试，结束即删除。
不改工作区产品文件，不写基线或 node_modules。先跑原实现基线（含多线程回归），再串行注入 19 个
变异：跳过 busy、全局键、忽略 end 的 runId、跳过 shutdown、跳过 maintenance、
用 runId 当 handle 身份、旧 complete 删除新 handle、忽略 cancel 的 runId、
空 cancel 当无 ID、覆盖首个取消原因、丢失 completion、丢弃 resolver 冒充完成、不等待 drain、无条件 abort、
丢失 Router-before-fence abort 分支、maintenance 忽略活动 turn、
恢复原 abort 前缀（两次 active 读取、sink 不带 runId），去掉 end_turn 的预约身份复核，以及篡改 oracle 的 sourceSha256。
16 项行为变异必须编译成功、oracle 测试以断言失败退出；活动 turn 变异还须命中新增场景。
`end_turn` 身份变异单独运行
`exact_end_requires_original_admission_identity_but_empty_end_is_unfenced`，
必须命中 `stale exact end released new identity` 断言。
恢复 abort 前缀的变异运行屏障控制的
`old_abort_never_targets_replacement_across_threads`，要求 2000/2000 轮新 run 被命中，
从而证明竞态回归确实拒绝旧写法；修复实现必须为 0/2000。
哈希变异把 `SessionRouter.ts` 的记录值改为格式有效的 64 个零，必须由共享守卫的
源码哈希不匹配断言拒绝，不能以格式检查或其它行为失败代替。
编译错误/超时不算抓到负控。原始日志与含完整变异表达式的 JSON 汇总写入指定目录。
`CARGO_HOME` 与 `PDRS_BASELINE` 必须由环境提供，缺省立即报错；后者确保哈希负控
实际校验固定基线源码。每次调用将 `CARGO_TARGET_DIR` 覆盖为独占 `/tmp` 副本内的
`src/target`，不与真实树或另一装置共享，结束时与副本一起删除。
首次 baseline 冷构建限时 600 秒，其余变异限时 120 秒；编译前执行 cargo-slots。

abort sink 签名为 `(Option<JsString>, JsString)`，分别传入请求 runId 与 reason。
显式空 runId 仍是 `Some(空串)`，只有 `None` 才允许按当前会话 abort。
接线时 sink 必须在 live owner 的发布锁内按 runId 筛选并选中 handle，再释放锁；
组件不会在锁内调用 sink 或 await。回归测试的两个线程在 sink 选 handle 前通过
屏障强制完成 `end_turn(old)` + `begin_turn(new)`，交替覆盖有/无 completion handle，
无需概率或 sleep。

范围仅为 A7.1-pr1 的内存组件。真实 session 创建/退役和 abort sink 由 pr2 接线，
WS binding、auth/clientName、错误与无 runId socket close 由 pr3 验收；hello/describe
共源留 pr4，RunRegistry 抢陈旧锁留 pr5。没有切换默认代理或接入 durable accept。

`real_ts_admission_regenerates_identically` 是与 session-owner 相同的 ignored 再生守卫：

```sh
cd src
TMPDIR=/tmp TSX_DISABLE_CACHE=1 cargo test --offline -p pilotdeck-gateway --lib \
  runtime::session::tests::real_ts_admission_regenerates_identically -- --ignored
```

需先设置上述 `PDRS_BASELINE`。普通回放仍先经 `test_baseline::oracle` 核来源。
Rust `begin_turn` 返回 `Option<TurnReservation>`；回放将其 `is_some()` 与 TS 布尔值比较，
并保留成功凭证传回 `end_turn`。非空 runId 的释放同时核对凭证身份，保护 close 后
同 runId 再接纳的预约；这是本轮明确要求的 Rust 防护，TS 原方法仅比较字符串。
缺省/空 runId 的无身份限制释放仍与 TS 一致。
