# ConfigStore 的真实 TS 原件

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
被测入口是基线 `src/pilot/config/PilotConfigStore.ts` 的
`createPilotConfigStoreSync`（`DefaultPilotConfigStore`：`reload`、`subscribe`、
`getDiagnostics`、`startWatching`）。Rust 对应 `src/config/store.rs`。

## 目录与条数

`oracle.json` 共 **15** 个场景（`count` 与 `scenarios.length` 一致，Rust 测试精确
断言条数）。每个场景是一串步骤（`steps`），`results[i]` 是第 i 步的真实结果：

- `create`：初始快照的 `version` 与 `getDiagnostics()`，或初始加载的错误；
- `reload`（`count` 次同步连发）：每次调用的结果（`version` 或错误的
  `name`/`code`/`message`/`diagnostics`），以及 `sameResult`（所有调用是否得到
  同一个快照对象 / 同一个错误对象）；
- `snapshot`、`diagnostics`：当时的 `getSnapshot().version` 与 `getDiagnostics()`；
- 每一步之后收到的订阅事件 `events`：监听者名、`previousSnapshot`/`nextSnapshot`
  的 version、`changedPaths`、`changeClasses`、回调内看到的 store version、
  `getSnapshot() === nextSnapshot`、回调内 `getDiagnostics().length`；可重入
  监听者另记其 `reload()` 是否得到同一快照。

覆盖：版本递增（失败不占号）、并发合并（3 连发、2 连发、失败连发）、
`PilotConfigError` 替换保留诊断而 `RangeError`（别名环）不替换、成功清空、
文件缺失、监听者抛错隔离、回调内 `reload` 并入当前 reload、发布中途订阅（被访问）
与退订（被跳过）、同一监听者重复订阅只算一次、项目配置增删、`startWatching`
去抖（30 ms 间隔三连写只触发一次）、失败的 watch reload 只留诊断、停止后不再
reload、缺失文件改监视目录、目录也不存在时不监视。

## 生成

溯源守卫与 `workspace-snapshot` 相同：`../../baseline_source_guard.mjs` 的
`bindSourceTree`（显式绝对 Git、固定 HEAD、每个 tracked `src` 字节对 Git blob），
加 tracked 树干净断言；`sourceSha256` 是 `PilotConfigStore.ts` 相对 import 传递
闭包（650 个文件）的 commit blob SHA-256。生成器断言 ICU 默认 locale 为
`en-US`。所有场景在一个 `mkdtemp` 根下各占子目录，只删除该根。

```sh
PDRS_BASELINE=/path/to/PilotDeck LANG=en_US.UTF-8 \
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/config-store/generate.mts <输出目录>
```

watch 场景用真实 `fs.watch` 与真实计时器；写入间隔 30 ms、等待窗口为去抖的
数倍，连续三次生成逐字节一致。

## 归一化与回放

- 场景根目录记为 `<ROOT>`；`undefined` 成员记为 `{"$":"undefined"}`。
- 回放按步骤重建同键序的结果对象，序列化后逐字节比较（不排序键）。
- 非 `PilotConfigError`（如 `RangeError`）在基线上没有 `code` 与
  `diagnostics`；Rust `Error.code` 对这类错误是网关线协议码，回放按基线形状记为
  `undefined`、不记 `diagnostics`。
- 时间走 Tokio 暂停时钟：`wait` 推进虚拟时间，watcher 的轮询与去抖计时确定性
  触发，不依赖真实 sleep。Rust 以轮询（默认 100 ms）代替 `fs.watch` 事件，监视
  对象的选取（存在则监视自身、否则监视父目录、父目录也不存在则不监视）与基线一致。

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test config_store --locked --offline
```
