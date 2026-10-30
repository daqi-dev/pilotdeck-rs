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

`tests/plugin_runtime.rs` 为原件的 25 场景、259 步逐一提供测试，回放磁盘操作、refresh/report、state/view、三种贡献租约、并发 start/await/pending/flush 和真实 Rust catalog bundle。每一步结果与错误严格比较，协作者调用按整场景顺序比较（JS microtask 与 tokio 可以把回调移到相邻 await/flush 的另一侧）。`start` 先在调用方轮询 future 一次，复现 JS async 函数到首个 await 前的同步准入，不使用睡眠推测状态。

表示转换仅限临时根路径、对象字段的显式 undefined 标签，以及 TS 的 `Object.freeze` 元信息。旧 Rust snapshot 类型不暴露 `outputStyles`，当前两条 legacy acquire 期望均为空，测试先断言它们确为空再剔除此字段；将来出现非空样本会失败。此测试不声称验证 Rust 类型尚未提供的 legacy outputStyles 属性。

#543 已取消 `runtime_dispose_retired_and_active_together` 的 ignore。原有 20 场景的输入与输出保持逐值一致，新增四个真实 TS 场景：active 插入顺序与并行挂起、多个 retired 世代的租约释放顺序、在途 retired 回调与 active 错误汇总、已退休 builtin 重新激活后的等待顺序。`gate` / `open` 用 Promise（Rust 对应 watch）控制协作者完成，不以耗时猜测顺序；`calls` 在指定步骤对累计调用作精确断言，防止只比较最终日志漏掉串行化。

TS 按 retired Set 插入顺序再按 active Map 插入顺序登记清理，实际调用取决于引用是否已清零。新登记的无引用回调同步执行到首个 await；既有等待者收到通知后按等待队列继续，仍被引用的重新排队。各回调并行，`Promise.allSettled` 等全部结束；错误按 promise 登记顺序而非完成顺序汇总。详见基线 `PluginRuntime.ts:263`、`PluginRegistry.ts:96`、`:138`、`:169`。

行为负控覆盖改为串行、逆序、跳过等待、忽略租约及错误汇总篡改。#1122 的 `reverse-mixed-lease-release` 只在 draining 且同一 lease 持有 retired 与原 active 实例时反转释放队列，绑定 `runtime_dispose_retired_and_active_together` 及其 `#1122 "dispose-retired-and-active-together" calls` 专属断言，要求金样的 `keep → old` 顺序：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/plugin-runtime/negative-controls.py \
  _tmp/20261023-issue-543-dispose-order/negative
```

脚本通过 `mutating_sources` 验证产品源码等于 HEAD，并在退出时恢复、复核原始字节。每个变异依次跑正控、变异编译、绑定负控与恢复正控；编译错误及过期锚点均为 INVALID，不算判杀。逐项跑完后汇总，任一非 KILLED 即非零退出。每次 cargo 调用与名额等待各有 900 秒上限，只跑一个测试进程、`--test-threads=1`；必须由调用方设置本树的 `CARGO_TARGET_DIR`，脚本不创建第二份构建目录。可在输出目录后追加 `reverse-mixed-lease-release` 单跑本单变异，或加 `--compile-only` 只核变异编译。已有 `runtime-replay-generation` 负控仍位于 plugin-loader 的负控脚本。

原 `restore-old-order`（整文件恢复修复前提交 `dbaa207c…` 的实现）已于 #1010 删除：#1010 给 lease 结构新增字段与参数后，旧实现整体不能再编译；其「旧等待者排在新等待者之前」的顺序语义由 `old-waiters-before-new` 覆盖（同一锚点同一替换，不再保留重复行）。启动测试前逐条断言变异字节与当前源码不同，结果记录各变异 SHA-256，避免提交或合入后出现空变异。

修复轮 3 增加真实非 async dispose 同步抛错场景；TS 将抛错变为 promise 拒绝，继续同批其余清理并聚合错误。Rust 的 DisposeFn 构造、首 poll 与后续 poll 的 unwind 都走这一错误路径。锁内只预留实例，锁外按批次启动屏障依次首 poll，同批前奏全部执行后才恢复 Pending 回调；回调可访问 runtime state。

```sh
timeout 600 python3 src/pilotdeck-gateway/tests/fixtures/plugin-runtime/panic-negative.py /tmp/pdrs-543-panic-negative
```

该负控依次恢复固定提交 `6286ed744c4e04ffea76fd7458772316a2364040` 的旧逻辑、只捕获首 poll、恢复持 state 锁调用；前两条要求 5 秒 dispose 期限断言失败，第三条要求锁重入断言失败，finally 恢复字节。总期限低于 10 分钟，一次仅一个测试进程。
