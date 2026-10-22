# PluginRuntime 真实 TS 回放

`generate.mts` 固定 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，通过共享 `bindSourceTree` 与相对导入闭包哈希守卫，运行真实 `PluginRuntime` 和 `GatewayCommandCatalogBundle`。只有 builtin dispose、catalog resolver/list/warn 是记录调用的协作者。所有场景只访问自建临时目录。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
mkdir -p /tmp/pdrs-runtime-one /tmp/pdrs-runtime-two
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/plugin-runtime/generate.mts /tmp/pdrs-runtime-one
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/plugin-runtime/generate.mts /tmp/pdrs-runtime-two
cmp /tmp/pdrs-runtime-one/oracle.json /tmp/pdrs-runtime-two/oracle.json
cmp /tmp/pdrs-runtime-one/oracle.json src/pilotdeck-gateway/tests/fixtures/plugin-runtime/oracle.json
/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test plugin_runtime
```

`tests/plugin_runtime.rs` 为原件的 20 场景、184 步逐一提供测试，回放磁盘操作、refresh/report、state/view、三种贡献租约、并发 start/await/pending/flush 和真实 Rust catalog bundle。每一步结果与错误严格比较，协作者调用按整场景顺序比较（JS microtask 与 tokio 可以把回调移到相邻 await/flush 的另一侧）。`start` 先在调用方轮询 future 一次，复现 JS async 函数到首个 await 前的同步准入，不使用睡眠推测状态。

表示转换仅限临时根路径、对象字段的显式 undefined 标签，以及 TS 的 `Object.freeze` 元信息。旧 Rust snapshot 类型不暴露 `outputStyles`，当前两条 legacy acquire 期望均为空，测试先断言它们确为空再剔除此字段；将来出现非空样本会失败。此测试不声称验证 Rust 类型尚未提供的 legacy outputStyles 属性。

显式 ignored 的范围外差异与复现命令见本次 `_tmp/20261023-issue-484-plugins/RESULT.md`；不将 ignored 计入通过。原 oracle 字节保持不变。负控 `runtime-replay-generation` 临时改坏 Rust generation，必须使真实回放断言失败。
