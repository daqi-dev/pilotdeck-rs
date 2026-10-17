# Run registry TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/gateway/run/RunRegistry.ts`（SHA-256 记在 `oracle.json`）。所有快照与步骤结果都由真实 `FileRunRegistry` 产生，无实现源码复制；环境准备（Node v22.22.0、基线 lockfile 的 tsx 4.22.3、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/run-registry/generate.mts src/pilotdeck-gateway/tests/fixtures/run-registry
```

生成器先断言基线 HEAD，并对 normal 与每个 quirk 场景逐步断言结果类别（created/duplicate/conflict/not_found/terminal/TypeError/record/absent/events:N/ok）及关键字段，防止步骤因引用写错而全部落空；`lock-stale-live` 场景要等满 TS 的 30 秒锁超时，整次生成约 30 秒。

`oracle.json.scenarios[]`：`initial`（起始快照文件，`null` 为全新）、`lock`（预置锁内容与 mtime 年龄）、`openError`（TS readiness 拒绝）、`steps[]`（accept/append/get/events/mark 的输入与 TS 结果）、`after`（TS 操作后的快照原字节）、`lockLeft`。Rust 默认测试 `real_ts_scenarios_match_outcomes_and_snapshot_bytes` 逐场景重放：结果按值比较，快照按字节比较，只屏蔽本次运行新写入的 `ownerPid`（等于本进程 pid 且不在起始文件中）与 `acceptedAt`/`updatedAt`（不在起始文件中）；Rust 新写的时间戳另验 ISO 格式。

夹具与机器无关的两条归一化：
- **本进程 pid → `"<self>"`**：生成器把步骤结果里等于 `process.pid` 的 `ownerPid` 一律写成 `"<self>"`，其它 pid 原样保留；Rust 侧对自己的 pid 做同样替换。为此由真实 TS 写出的种子快照（`realSnapshot`，paging/lock-* 的起始文件）里仍等于生成器 pid 的 `ownerPid` 改写为 i32::MAX（这些 run 均已终态，恢复不 fence），生成器断言起始文件不含自身 pid。`oracle.json.generatorPid` 只用于屏蔽 `after` 快照字节。
- **临时目录 → `<dir>`**：TS 错误文案中的生成临时目录前缀替换为 `<dir>`（如 `Timed out acquiring run registry lock: <dir>/lock-stale-live.json.lock`）；Rust 比较时把自己的注册表路径替换为 `<dir>/<场景名>.json` 后逐字比较。生成器写 `oracle.json` 前断言其中不含临时目录、输出目录与基线路径，Rust 读取时断言不含 `.generate-`。

| 场景 | 来源与受控变更 |
|---|---|
| normal | 全新注册表：重复 accept、冲突、input/delta/completed、failed 后可续为 completed（task_budget）、aborted/interrupted、三种 terminal 拒绝、not_found（含 project 隔离）、空 projectKey 与缺省撞键、原始字符串事件、孤立代理与整数键序 |
| orphans | 真实 TS 写出 11 条 run 后改写 ownerPid：i32::MAX（ESRCH，死）、缺省/0/负数/小数/字符串（非整数，死）、1（EPERM，活）、i32::MAX+1（Node `process.kill` 抛 ERR_INVALID_ARG_TYPE，活）；completed/failed 不受 fence；fence 后重复提交只回放、不再执行 |
| paging | 真实 TS 的 3 事件 run；afterSeq/limit 的 0、负数、小数；手工加入 seq 为字符串/null/布尔/缺省/原始值/十六进制/数组/科学计数/非数的条目，及无 record 的事件键 |
| quirk-* | TS 读入后会改写的形状（步骤用不带 projectKey 的 `bare()` 引用，确实命中目标记录）：重复 record 键（保第一位置、取最后值、丢顶层多余字段）、空 projectKey 撞键、数字/数组/null 键字段与 `+1` 拼接（`null`/`"1"`/`true`/`[2]`/`{}`）、falsy record（get/append 视为不存在，`events` 按 `Map.has` 仍可读，accept 原位替换）、数组 record（append 成功、事件落盘、record 字节不变）、events 为数组/字符串（含 CJK、拆开的代理对与孤立代理，按 UTF-16 码元拆键）、非数组事件表、含 null 条目的事件表可追加但不可读、null 事件（TypeError）、无效 UTF-8 字节 |
| bad-* | 坏尾（normal 截掉末 9 字节）、空文件、schemaVersion≠1、records 非数组、events 为 null/缺省/0、快照为 null、record 为 null：readiness 拒绝，原字节不改，锁已释放 |
| lock-* | 预置 `<path>.lock`：陈旧（>60 s）死 pid 或不可解析内容被抢；陈旧活 pid（`"pid" : 1`，含 JS `\s`）等满 30 s 超时并保留锁 |

`generate.mts <rust-registry> --verify-rust` 由 Rust 忽略测试 `real_ts_reads_rust_registry_and_honours_its_lock` 调用：Rust 先持有新鲜锁，TS 必须等待（不读不写）；释放后 TS 读取 Rust 写出的记录与事件、只 fence 死 owner、继续写入；TS 退出后 Rust 重开，把 TS 留下的 accepted run 判为孤儿。

```sh
cd src
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway run_registry -- --include-ignored
```

范围：只证明 RunRegistry 存储契约（与 TS 共用快照格式与 `<path>.lock` 互斥协议）；未接入 host、会话运行时或 gateway method，默认代理路由不变。锁文件、临时文件与新建目录同 TS 用默认权限（受 umask 约束），跨用户 handoff 时双方都能读到锁内 pid。已知差异：Rust 在 rename 前 sync 临时文件；写锁文件失败时 Rust 删除自己的锁（TS 会遗留）；JSON 语法与 TypeError 的错误文案不声称与 V8 相同，只对拍错误层与 `RunRegistryError` code/文案、`invalid run registry snapshot` 与锁超时文案。
