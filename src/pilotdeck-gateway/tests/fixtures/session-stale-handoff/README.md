# #1020 已 dispose 前驱的 Router 级 TS 金样

固定 cc7c4e6、Node v22.22.0，直接运行真实 `SessionRouter` / `ProjectSessionFactory` /
`createAgentProjectSessionStorage`。四格对应 `_tmp/20261026-issue-1020/plan.md` 的
状态×事件×时机矩阵：

- `router-disposed-previous`：选择前 dispose 已完成。TS Router 无状态过滤、无重试
  （SessionRouter.ts:135-143 直接读 `cached.handle`），第二次、第三次 `getOrCreate`
  都把已 dispose 的 session 交给 `recreateSession`，反复以
  `Session projection driver is disposed.` 拒绝，registry 不变。
- `router-disposed-second-dirty`：拒绝后再次 `markSessionDirty` 仍拒绝。
- `direct-recreate-disposed`：直接 factory 层的同类拒绝（与
  `session-factory/oracle.json` 的 `recreate-disposed` 金样一致的自证格）。
- `router-prepare-disposal`：旧 session 的 dispose 在替代 prepare 的
  `pluginRuntime.refresh`（composition 2）里完成；prepare 后的
  `snapshotForRuntimeReload` 拒绝（Router 级复现 `prepareDisposal`）。

Rust 侧 #1020 的宿主重试策略在这些格上均为**有意差异**，登记在
`specs/gateway-parity/diffs/A7.2-SESSION-DISPOSAL-RETRY-001.md`：owner 选择时过滤
已 Disposed 的前驱（#745 r1 已有），选择与 factory 接管之间完成 dispose 的窗口则
重试一次。`oracle.json` 记录源闭包哈希；Rust `stale_handoff_regenerates_identically`
ignored 守卫逐字节复算。生成器输出目录递归创建、无需预先存在（README 命令可直接传
新路径，fix-r1 起在首次 `makeFactory()` 前创建）；oracle 独占创建（`flag:'wx'`），
`work-*` 临时目录在写 oracle 前全部删除（fix-r1 起按前缀逐个清理，不再依赖不存在的
字面量 `work-` 路径）。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/session-stale-handoff/generate.mts OUT
```
