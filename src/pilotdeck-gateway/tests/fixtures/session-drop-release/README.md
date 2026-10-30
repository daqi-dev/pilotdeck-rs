# #1010 handle 丢弃侧的 TS 金样

固定 cc7c4e6、Node v22.22.0（`--expose-gc`），直接运行真实 `AgentHandle`
（session 缝隙为确定性桩，只触 lifecycle 路径）。三格对应
`_tmp/20261026-issue-1010/plan.md` 的状态×事件×时机矩阵：

- `handle-drop-gc`（格 1/10）：丢掉全部引用 + 强制 GC。句柄被回收但
  `onDispose` 计数为 0 —— TS 的 `AgentHandle` 没有确定性析构（无
  FinalizationRegistry / Symbol.dispose），自定义异步 release/close 只有
  显式 `dispose()` 才会跑。Rust 侧格 1–3 由 `handle_drop_tests` 的
  `Drop for Inner` 补齐，登记为有意差异。
- `abandoned-waiter`（格 4）：`dispose()` 的等待者被丢弃不影响进行中的
  `finishDispose` 完成（JS promise 不因丢 waiter 而取消）。
- `process-exit`（格 3/8）：子进程 `process.exit()` 是「runtime 消失」的 TS
  等价物 —— 同步 cutoff 已到 `draining`，pending 的异步清理不落地。

`oracle.json` 记录源闭包哈希；Rust `drop_release_regenerates_identically`
ignored 守卫逐字节复算。生成器输出目录递归创建、无需预先存在；oracle
独占创建（`flag:'wx'`）；`work-*` 根为本次自建自清。GC 轮次内不 `deref()`
（deref 会把目标钉到当前 job 结束，同 job 的 gc() 永远观察不到回收），
只在轮次结束后读取一次。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --disable-warning=ExperimentalWarning --expose-gc \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-drop-release/generate.mts OUT
```
