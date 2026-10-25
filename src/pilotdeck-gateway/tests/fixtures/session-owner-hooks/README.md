# #414 session owner hooks 金样

`generate.mts` 直接 import `OpenBMB/PilotDeck@cc7c4e6` 的真实 SessionRouter；注入 fake session、可控时钟与 Promise disposal 闸门，不复制/重写 Router。共享 `_shared/baseline.mts` 在前后核 HEAD、tracked 状态及六个源文件的提交/工作区 SHA-256。无需模型、网络、密钥或安装。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
TSX_DISABLE_CACHE=1 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-owner-hooks/generate.mts \
  _tmp/20261025-issue-414/ts-replay
cmp src/pilotdeck-gateway/tests/fixtures/session-owner-hooks/oracle.json \
  _tmp/20261025-issue-414/ts-replay/oracle.json
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --offline --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --lib runtime::session::owner::tests::hooks
python3 src/pilotdeck-gateway/tests/fixtures/session-owner-hooks/negative.py \
  _tmp/20261025-issue-414/negative-replay
```

每个输出目录须是新目录。生成器以 wx 独占创建 oracle；负控脚本独占创建证据目录。负控只临时修改 owner.rs，逐个成功编译并要求指定测试行为失败，finally 恢复字节，最后原实现重跑；同树不可有另一写入者或 cargo。构建固定为本树 `_build/cargo-target`。运行前确保全局 cargo 名额；沙箱无法 ps 时脚本提示不代表已验证全局进程数。

11 项独立项目/前缀/预约判定；11 种 idle 场景覆盖严格超时、已接纳轮保护、结束刷新 lastUsedAt、驱逐后孤立预约与结束、dispose 闸门、失败排空、快照异常/缺失/动态 length、禁用回调；3 种非 idle 路径验证静默。Rust 额外验证 maintenance/handle activity/completion 组合存在但无 admission 时不构成用户轮。并发证据是受控异步交错，不声称 OS 线程压力测试。

general callback 改变 messages，取到最终数量可排除过早快照。回调回读 owner，证明不持 owner 锁；未配置时 snapshot 零调用。disposal-error 日志的预期 TS warn 和 throw 的预期 Rust panic-hook 日志不代表测试失败。真实输出见 `_tmp/20261025-issue-414/ts-inherited.log`；所有权差异登记 `specs/gateway-parity/diffs/A7.1-SESSION-IDLE-SNAPSHOT-001.md`（未批准）。
