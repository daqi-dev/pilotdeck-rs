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

`tests/plugin_runtime.rs` 为原件的 24 场景、246 步逐一提供测试，回放磁盘操作、refresh/report、state/view、三种贡献租约、并发 start/await/pending/flush 和真实 Rust catalog bundle。每一步结果与错误严格比较，协作者调用按整场景顺序比较（JS microtask 与 tokio 可以把回调移到相邻 await/flush 的另一侧）。`start` 先在调用方轮询 future 一次，复现 JS async 函数到首个 await 前的同步准入，不使用睡眠推测状态。

表示转换仅限临时根路径、对象字段的显式 undefined 标签，以及 TS 的 `Object.freeze` 元信息。旧 Rust snapshot 类型不暴露 `outputStyles`，当前两条 legacy acquire 期望均为空，测试先断言它们确为空再剔除此字段；将来出现非空样本会失败。此测试不声称验证 Rust 类型尚未提供的 legacy outputStyles 属性。

#543 已取消 `runtime_dispose_retired_and_active_together` 的 ignore。原有 20 场景的输入与输出保持逐值一致，新增四个真实 TS 场景：active 插入顺序与并行挂起、多个 retired 世代的租约释放顺序、在途 retired 回调与 active 错误汇总、已退休 builtin 重新激活后的等待顺序。`gate` / `open` 用 Promise（Rust 对应 watch）控制协作者完成，不以耗时猜测顺序；`calls` 在指定步骤对累计调用作精确断言，防止只比较最终日志漏掉串行化。

TS 按 retired Set 插入顺序再按 active Map 插入顺序登记清理，实际调用取决于引用是否已清零。新登记的无引用回调同步执行到首个 await；既有等待者收到通知后按等待队列继续，仍被引用的重新排队。各回调并行，`Promise.allSettled` 等全部结束；错误按 promise 登记顺序而非完成顺序汇总。详见基线 `PluginRuntime.ts:263`、`PluginRegistry.ts:96`、`:138`、`:169`。

本轮行为负控（含恢复旧实现、改为串行、逆序、跳过等待、忽略租约及错误汇总篡改）：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/plugin-runtime/negative-controls.py \
  _tmp/20261023-issue-543-dispose-order/negative
```

脚本依次临时修改产品实现，要求指定测试出现对应行为断言失败，并在 `finally` 恢复原始字节；编译错误不算负控成功。总期限少于 10 分钟，每次只跑一个测试进程、`--test-threads=1`，复用启动器的本树 `src/target`。已有 `runtime-replay-generation` 负控仍位于 plugin-loader 的负控脚本。
