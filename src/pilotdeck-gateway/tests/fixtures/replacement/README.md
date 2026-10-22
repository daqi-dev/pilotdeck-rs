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
28 个场景覆盖写预约、并发替换、不同会话独立、替换中 dispose、active run 冲突、
abort/close/storage 失败清理、claim/release/accepted commit、显式 finalize 互斥及重试、
超时回滚（active 推迟、失败重试）、dispose 清理、provider 缺失和 fork 原样转发。
`dispose` 按 TS 清空当下状态；正在等待 storage 的 replace 随后仍可发布新 pending。
另覆盖 dispose 后旧 finalize/timeout 失败不得重置新 submit claim。
Rust 另用两项内部并发不变量检查已撤销的旧定时器和丢弃等待者后的任务排空。
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
