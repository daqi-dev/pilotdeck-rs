# GatewayRuntimeRefreshBundle 的真实 TS 原件

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
被测入口是基线 `src/cli/GatewayRuntimeRefreshBundle.ts` 的真实 class；Rust 对应
`src/runtime/refresh.rs`，回放在 `tests/runtime_refresh.rs`。

## 内容

`oracle.json` 共 **23** 个场景、155 步（Rust 精确断言条数与 id 不重复）。生成器构造
真实 bundle，注入记录调用的假依赖（config store、registry、router、通知 server、
SDK 回调、memory maintenance / diagnostics、log / warn），每步记下该步引起的全部调用
（按发生顺序）与返回值。`real-store-*` 两个场景改用真实 `createPilotConfigStoreSync`
读 YAML（变更路径与分类来自真实 diff/classify）。registry 的 `reload()` 返回由步骤
`resolve` / `reject` 手动结算的 promise，所以多次变更并发挂起与乱序结算是确定的；
`flush` 只排空微任务与 `setImmediate`，不依赖真实计时。

覆盖：attach 幂等、dispose 退订与清 server；空 changeClasses；全部 restart-required
只 warn、混合时走刷新；runPolicy-only（含 `runPolicyX` 前缀、空路径负例）；刷新成功
顺序（log → SDK → registry.reload → 结算后 getRouter → runtime_invalidated 诊断 →
markAllDirty → broadcast）与失败只 warn；两次变更乱序结算、先败后成；无 router / 无
诊断 / 无 SDK / 无 server；dispose 后结算不广播；registry 同步抛错被 store 吞掉；
reloadConfig 捕获最后一次事件、失败也退订；refreshConfigBeforeTurn；reloadExtensions
（空 key 走全量、孤立代理项 key 与路径原样）；afterTurnCompleted（对象展开键序、
undefined 成员、`projectKey ?? projectRoot` 保留空串）。

## 生成与回放

溯源守卫：`../../baseline_source_guard.mjs` 的 `bindSourceTree` + tracked 树干净断言；
`sourceSha256` 是 bundle 与 `PilotConfigStore.ts` 相对 import 闭包（652 个文件，共享
`sourceClosureSha256`）的 commit blob SHA-256；断言 ICU 默认 locale 为 `en-US`。
所有文件场景在一个 `mkdtemp` 根下各占子目录，只删除该根；路径记为 `<ROOT>`，
`undefined` 记为 `{"$":"undefined"}`。两次生成逐字节一致。

```sh
PDRS_BASELINE=/path/to/PilotDeck LANG=en_US.UTF-8 \
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/runtime-refresh/generate.mts <输出目录>
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test runtime_refresh --locked --offline
```

回放按 TS 键序重建每步结果对象，序列化后逐字节比较。warn 的第二参数：路径串原样，
错误记 `{name, message}`（Rust `RuntimeError` 对应基线 `Error`）。
