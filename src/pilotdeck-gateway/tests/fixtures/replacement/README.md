# 替换协调器真实 TS 金样

`generate.mts` 通过共享 `_shared/baseline.mts` 守卫固定
`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`：运行前后核 HEAD、
tracked 状态、四个完整执行源文件的提交版与工作版 SHA-256。使用基线已安装的
esbuild 在内存转译完整源文件，不改写方法、不写基线或 node_modules。

执行真实 `GatewayTurnReplacementCoordinator`，并经真实 `InProcessGateway`
prototype 上的 fork/replace/finalize 方法调用。仅注入假的 session/storage、警告接收器
和受控定时器；未用 import 的属性访问直接抛错。维护错误优先级另外执行真实
`SessionRouter.compact`，不调用 provider 或用户的 session。

`cases[].steps` 是输入脚本，`events` 和 `values` 全部由 TS 执行生成；没有手写期望。
54 个场景覆盖写预约、并发替换、不同会话独立、替换中 dispose、active run 冲突、
abort/close/storage 失败清理、claim/release/accepted commit、显式 finalize 互斥及重试、
超时回滚（active 推迟、失败重试）、dispose 清理、provider 缺失和 fork 原样转发。
`dispose` 按 TS 清空当下状态；正在等待 storage 的 replace 随后仍可发布新 pending。
另覆盖 dispose 后旧 finalize/timeout 失败不得重置新 submit claim。
#518 新增场景覆盖同步抛错与异步拒绝（Rust 分别注入端口调用/轮询 panic）、
放弃 submit claim、dispose 后旧 replace 覆盖/清除新 pending，以及旧 finalize、accepted
commit、timeout 成功按 key 清除新 pending。原有 28 场景的输入、事件与结果保持不变。
生成器还运行真实 `InProcessGateway.submitTurn`：在已 claim 的 preflight 端口等待时调用
iterator.return()，验证 return 不会打断 await；端口拒绝后才释放 claim。该实测记录在
`submitAbandonment`。单独 claim 而不 release/commit 与 TS 一样会一直停在 submitting；
后续 gateway 装配必须持有放弃调用方后的清理责任。
Rust 另检查旧定时器、丢弃等待者后的任务排空、panic 后再 dispose、同步 getter 不污染
协调器锁，以及意外任务 panic 不在等待者中触发二次 panic。
超时 hasActiveTurn getter 在 TS 的 try 外；Rust 将其 panic 告警并重新约时，避免锁污染。
这是本轮 panic 清理要求下的防护，正常 getter 行为与既有金样不变。
定时器使用受控单轮触发，Promise 用有界 microtask 排空；Rust 用 Tokio 暂停时钟回放。
不等待墙钟、不监听网络、不产生随机 ID。完整输入与返回值保留额外字段及可选 project。

在仓库根执行（Node v22）：

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
export TMPDIR=/tmp TSX_DISABLE_CACHE=1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/replacement/generate.mts /tmp/replacement-first
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/replacement/generate.mts /tmp/replacement-second
cmp /tmp/replacement-first/oracle.json /tmp/replacement-second/oracle.json
shasum -a 256 /tmp/replacement-first/oracle.json /tmp/replacement-second/oracle.json
```

Rust 测试先调用共享 `crate::test_baseline::oracle`；设置 `PDRS_BASELINE` 后，
额外对真实源文件复核完整溯源。运行前按任务要求检查 cargo slots：

```sh
/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh
cd src
cargo test --offline -p pilotdeck-gateway --lib runtime::replacement::
cargo test --offline -p pilotdeck-gateway --lib runtime::session::
cargo test --offline -p pilotdeck-gateway --lib replacement_fixture_regenerates_identically -- --ignored
```

本包交付可注入的运行时协调器与 SessionOwner 适配，不把 A8 的 WS 装配或
A7.5 后续持久化事务实现混入本 PR。`fork_session` 按 TS 直接委托 provider；
不替 storage 实现 durable fork/replace/finalize。

负控在当前独占工作树内逐项临时修改产品源码，每项编译并运行一个测试，
在 `finally` 中恢复原始字节；编译失败、未转红或外部超时均中止且不计成功。
每次开跑前检查 cargo slots，不允许与其它编辑/编译并发。26 项变异、精确替换式、
源码 SHA-256 和原始输出记录在传入的证据目录：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/replacement/negative.py \
  _tmp/20261023-a75-pr1-replacement/negative-final
```

#518 的新增行为负控另用下列脚本。七组编译后变异分别移除 panic 边界、恢复等待者
二次 panic、漏掉成功清理、漏掉旧 replace 覆盖/失败清理、破坏 claim 相位，以及篡改
真实 submit 观察值。每个新增 panic 路径必须独立报告 FAILED，且 finally 恢复原始字节。
不要与任何编辑或 cargo 运行并发；设置 PDRS_BASELINE 后执行：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/replacement/negative-518.py \
  _tmp/20261023-issue-518-replacement/negative
```
