# #1082 同 rootDir 两代代际并发矩阵（TS 金样）

Node 必须是 v22.22.0，`PDRS_BASELINE` 指向 cc7c4e6 的只读 checkout。
`observe.mts` 直接 import 基线
`src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts` 的
`MemoryRepository`（与生产同一条 `node:sqlite` `DatabaseSync` 路径）。

生成器参数是调用方新建的输出目录：

```sh
PDRS_BASELINE=/abs/PilotDeck node --disable-warning=ExperimentalWarning \
  --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-same-root-generation/generate.mts NEW_OUTPUT
```

输出 `oracle.json` 使用 `wx`；`work/` 是本轮临时 SQLite/文件树，不进入比较。

## oracle 的两组格

- `serial`：TS 生产语义。一个 Node 事件循环，`DatabaseSync` 调用是同步块，
  同根两代之间不可能交叠；两代都成功打开、互相读到对方提交的写、旧代 close
  不影响新代写。Rust 用例必须对齐这些格。
- `threaded`：确定性对照，不是生产 TS。worker 线程用同一个 `node:sqlite`
  `DatabaseSync` 持 `BEGIN IMMEDIATE` 写锁，主线程执行操作；这展示
  `busy_timeout = 0` 在**真实交叠**下的行为：已初始化库上的构造/读仍成功，
  首个写立即 `database is locked`（`ERR_SQLITE_ERROR`）；legacy schema 需要
  migration 时，构造本身立即 `database is locked`。Rust 多线程能制造 Node
  不能制造的交叠，所以进程内必须按文件串行化以保持 `serial` 语义，跨进程仍
  保持这里的立即失败契约。

`edgeclaw-native-default` 的 M21 `generation-*` 格使用相同 projectRoot 但
**不同** `rootDir`；本 fixture 补的是相同 `rootDir` 的 SQLite 写竞争。

Rust 回归：`same_root_generation_matches_ts_matrix`（比较本 oracle 语义）、
`same_root_*` 矩阵用例（含 #1082 专属断言消息）、ignored
`same_root_generates_identically`（重生成守卫）。
