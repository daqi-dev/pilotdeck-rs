# 内存接纳与 completion fence 的真实 TS 夹具

固定基线为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`oracle.json` 的 14 个场景来自真实 `SessionRouter`、
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

场景覆盖同会话重入、跨会话接纳、maintenance 占位、旧 end、shutdown、空 runId、
同 runId 的不同 handle、旧 complete、重复 cancel 的首个原因、无 handle wait、
Router 接纳但 fence 尚未安装、显式旧 abort、新 run 替换后的旧 abort。
另录制丢弃旧 resolver 后其 Promise 仍 pending 的行为。每一步录制返回值、所有 handle 的取消状态/原因、等待者是否已返回、session abort
调用以及 Gateway 原因表。异步先后由 Promise/闸门控制，不使用 sleep。
除将 `undefined` 查询结果表达为 JSON null 外，不屏蔽状态或标识；不存在时间戳、
随机 UUID 或临时路径的归一化。真实 AgentHandle 内部 UUID 不进入观察结果。

Rust 测试 `runtime::session::tests::real_ts_admission_completion_and_gateway_abort`
按夹具步骤调用产品组件并逐步比较整个结果；额外测试覆盖多线程原子接纳、UTF-16
键、首轮 poll 前的替换/完成/取消以及原因清理。

```sh
cd src
cargo test -p pilotdeck-gateway --lib runtime::session::
```

`negative.py <evidence-dir>` 在独占 `/tmp/pdrs-a71-negative-*` 下复制这四个产品源文件
与 oracle，以现有 protocol/tokio/serde_json 依赖构建临时测试 crate，结束即删除。
不改工作区产品文件，不写基线或 node_modules。先跑原实现基线，再串行注入 15 个
变异：跳过 busy、全局键、忽略 end 的 runId、跳过 shutdown、跳过 maintenance、
用 runId 当 handle 身份、旧 complete 删除新 handle、忽略 cancel 的 runId、
空 cancel 当无 ID、覆盖首个取消原因、丢失 completion、丢弃 resolver 冒充完成、不等待 drain、无条件 abort、
丢失 Router-before-fence abort 分支。每项必须编译成功、oracle 测试以断言失败退出；
编译错误/超时不算抓到负控。原始日志与含完整变异表达式的 JSON 汇总写入指定目录。
资源环境沿本任务固定的 CARGO_HOME/CARGO_TARGET_DIR，编译前执行 cargo-slots。

范围仅为 A7.1-pr1 的内存组件。真实 session 创建/退役和 abort sink 由 pr2 接线，
WS binding、auth/clientName、错误与无 runId socket close 由 pr3 验收；hello/describe
共源留 pr4，RunRegistry 抢陈旧锁留 pr5。没有切换默认代理或接入 durable accept。
