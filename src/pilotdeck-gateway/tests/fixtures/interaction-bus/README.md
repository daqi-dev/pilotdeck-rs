# 权限 / 提问 pending 与协调器金样

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node 22。
生成器逐个构造真实 `GatewayPermissionBus`、`GatewayElicitationBus` 与
`GatewayInteractionCoordinator`，加载完整未改写的 TS 模块；关联的 reconnect、replay、
规则 registry、entry 转换也执行真实源码，未使用的 import 一经访问立即报错。
共享 `_shared/baseline.mts` 前后核对 HEAD、tracked clean 和七个来源文件的提交 blob SHA-256。
不写基线、不读取任何权限配置或用户目录。`oracle.json` 全由生成器输出，不手写期望。

每一步录制返回值 / 逐字错误文本、回调顺序及回调时的 pending 数量、registration.active、
各会话 pending/count/has、分 kind 的 snapshot、共享 replay snapshot、current binding、
session allow rules。保留中文、空串、NUL 分隔规则和 UTF-16 单代理项。

计时使用 Node 原生 `node:test` MockTimers 的 setTimeout 队列：同一截止时间的应答和 timeout
以两种注册顺序执行。timeout 是调用者的清理，不是 bus 内建 deadline；timeout 捕获精确
registration，防止迟到计时器删除同 id 的新请求。Rust 逐步重放同一操作队列，另用两线程
Barrier 对两个 kind 各争抢 32 次，断言一个回调、无残留 replay、registration 失活。
另用 channel 固定 reject 回调暂停点，两种 kind 各覆盖迟到应答、第二个 reject、close 三种交错；
不依赖 sleep，断言没有重复结算或 delivered observer。同步重入由真实 TS 金样另外钉住。

payload 只在 Rust 的 `JsValue` 边界内表示；支持 undefined / 非有限数字归一化、
`__proto__` 赋值语义、嵌套对象与数组，迭代遍历不增加 Rust 调用栈深度限制。
Date、BigInt、函数、循环对象等宿主值使用 `register_producer` 图值入口转换，
见相邻 `interaction-values/README.md`；未来 producer 接线须调用该入口。
注册句柄须显式 dispose；丢弃句柄本身不取消，与 TS 引用生命周期一致。

复现（从仓库根，输出目录必须为新目录）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/interaction-bus/generate.mts /tmp/interaction-golden-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/interaction-bus/generate.mts /tmp/interaction-golden-2
cmp /tmp/interaction-golden-1/oracle.json /tmp/interaction-golden-2/oracle.json
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::interaction
```

负控（独占本 worktree，不与任何编译或写入并发）：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/interaction-bus/negative_controls.py \
  _tmp/20261023-a73-pr1-interaction/negative-controls-new
```

每次仅临时改一处产品实现，要求 `interaction_real_ts_stepwise_oracle` 出现断言失败；
编译错误、信号退出不算杀死。每轮 finally 恢复原始源码并核 SHA-256。
输出每条补丁、原始日志、资源检查、失败摘要，结束后应重跑正常测试和 fmt/clippy。

本 PR 不修改或接入 `runtime/turn/`，不代表 A7.3 完整轮次验收完成。

修复轮 1 金样为 46 场景、268 步：新增精确 dispose 的同会话隔离、共享 port 的同 kind
重复、close 清除历史 binding 后重新注册、同步嵌套 reject、reject 回调 consume 自身、
deny 回调 dispose 后续条目、reject→deny→register，以及 Bash 的 CR/LS/PS 边界。

issue #516 后金样为 48 场景、281 步：新增两条 `injectedOptions` 场景，把共享 port、
两个 bus 与外部 grants registry 全量注入 coordinator，钉住 `InteractionOptions` 的
Some 分支；Rust 侧 `Harness::injected()` 按同一标志构造，另以
`interaction_injected_options_share_the_supplied_components` 从外部句柄断言注入组件
被真实使用（port snapshot、bus consume、grants 读写与 dispose）。

TS 的两个细节须按实测解释：deny 先摘桶，所以 dispose 后续 registration 不会删除
迭代器持有的旧 Map 条目，旧条目仍结算；嵌套 reject/deny 删除 bySession 后，外层
reject 仍遍历原 Map。外层末尾无条件删除 owner，因此回调中新建桶会被删除而 replay
保留；`reject-deny-register-original-map` 锁定这项基线现状，未作为差异豁免或修复完成宣称。

`reject_session` 末次扫描与末尾删桶在同一临界区完成：TS 的耗尽循环与
`bySession.delete` 是相邻语句（GatewayElicitationBus.ts:127-138），真实 TS 探针确认
回调内注册进同一 Map 会被循环结算、注册进循环不持有的替换 Map 则被末尾删除丢弃
（僵尸条目：回调不触发、registration 保持 active、replay 残留）。合并后，与末次
扫描竞速的跨线程 register 要么被扫描结算（同桶）、要么在删除后新建桶正常存活；
落进「另一个 bulk 已删桶、外层循环只持旧桶」的 register 仍按 TS 基线丢弃。
`interaction_register_racing_reject_window_matches_ts_map_semantics` 用 channel 固定
两种交错的确定性断言。纯并发时序的「扫描与删除间隙」本身不含任何用户代码、无
回调暂停点，无法确定性构造，由临界区合并结构性消除（见 issue #516 RESULT）。

`InteractionOptions.permission_grants` 的注入实例由 coordinator 拥有生命周期（dispose
清空整个 registry，与 TS 相同）；Arc 可供外部观察，不可跨独立 coordinator 共享所有权。
connection 的 `BindingOwners` 仍只是无 pending 的连接边界子集；后续接线必须委托本模块
的 reconnect port，不能把两套状态机并列用作 pending 的真相源。
