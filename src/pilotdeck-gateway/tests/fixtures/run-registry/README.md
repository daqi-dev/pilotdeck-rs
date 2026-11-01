# Run registry TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/gateway/run/RunRegistry.ts`（SHA-256 记在 `oracle.json`）。所有快照与步骤结果都由真实 `FileRunRegistry` 产生，无实现源码复制；环境准备（Node v22.22.0、基线 lockfile 的 tsx 4.22.3、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/run-registry/generate.mts /tmp/fresh-run-registry-output
```

生成器先断言基线 HEAD，并对 normal 与每个 quirk 场景逐步断言结果类别（created/duplicate/conflict/not_found/terminal/TypeError/record/absent/events:N/ok）及关键字段，防止步骤因引用写错而全部落空；`lock-stale-live` 场景要等满 TS 的 30 秒锁超时，整次生成约 30 秒。

场景数固定为 **25**（normal / orphans / paging 各 1、quirk 10、bad 9、lock 3）。`generate.mts` 在发布 `oracle.json` 前、Rust `real_ts_scenarios_match_outcomes_and_snapshot_bytes` 在重放前都精确断言 25，增删场景必须同时更新两侧断言及本段。`count-negative.py` 复用 `../_shared/negative_control.py`、`../negative_process.py` 和 `../negative_suite.py`，仅在 `/tmp` 副本对 Rust 读入的 oracle 和 TS 待发布的 scenarios 分别增加 / 删除 1 项，四项均须因计数错误失败；Rust 变异保留 `cargo test --no-run` 编译回执，TS 生成器变异只记 node 运行回执（cargo 编译的是未变异 Rust，#1139 同步）。生成器失败不得发布 oracle，恢复后正控须通过。全部处理并完成守卫恢复后打印逐项 JSON 与 `SUMMARY:`，任一非 KILLED 即非零退出。Rust 仅测试模块支持 `PDRS_RUN_REGISTRY_ORACLE` 指向副本，其余快照仍取本目录。变异只写副本，沿用调用方的 `CARGO_TARGET_DIR`；所有 cargo 调用（含第一次正控的冷编译）一律 900 秒编译级超时，node 运行 240 秒。

计数负控须提交后独占本树运行：同一树锁窗口内，先校验 oracle、生成器、脚本自身和共享 Python 裁决依赖均等于 HEAD，再发现并校验生成器的完整本地 import 闭包，随后按原仓库相对布局复制。绑定测试在 `--lib` 的 gateway 产品树内，`mutating_sources` 的 #1166 共享 proc-macro2 扫描器已守护全部 workspace 字面构建输入即含 Rust 判据，本 runner 无单一判据入口可锚（recorder 的集成测试入口由其自身 runner 锚定）。闭包追踪 fail-closed：注释分隔或无插值模板的字面量动态 import 仍收编，模板插值 / 变量 / 拼接路径 / 仓库外绝对路径一律拒跑；唯一允许的计算式参数是逐字匹配 `BASELINE_LOADERS` 白名单的两条固定基线加载器（提及 baseline 不算数，其源码 SHA-256 记录在 oracle.json）；`file:` URL 按绝对路径规则处理，`node:` 内建与裸模块名按静态 import 跳过，其余 scheme 拒跑（#1139 同步）。闭包与守卫逻辑与 `../recorder/fix3-negative.py` 逐函数手写同步（`../test_import_closure.py` 断言二者一致，提取共享为 M6 #1099），其单测与 `../closure_guard_negative.py` 元负控两个计数 runner 共用；单测只读仓库、无需树锁（扫描器用例需先 `cargo build --example workspace_inputs` 预构建），元负控须提交后在干净树上运行。守卫覆盖 `negative_control.py`、`negative_process.py`、`negative_suite.py`、`negative_panic.py`、`pending_reanchor.py`，任一输入脏即拒跑。14 个子进程阶段的套件向 scratch 传 **1800 秒**总预算，外层 **1860 秒**保留 60 秒清理余量；共享 scratch 默认值不变。#1142 fix 后 `TreeLock._acquire` 只做 `LOCK_NB` 轮询、不再借用进程定时器，scratch 的 `alarm` 总预算有效（裸跑也有界）。fail-closed，不产生假 PASS。预算容纳实测约 240 秒运行与额外槽位/编译等待，仍可能在持续争用下有界失败。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
timeout 1860 python3 src/pilotdeck-gateway/tests/fixtures/run-registry/count-negative.py
timeout 150 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/run-registry/verify-generation.mts /tmp/fresh-run-registry-verify
```

`verify-generation.mts` 的输出目录必须尚不存在；上面两个代码块分别使用独立的新目录。再次执行时换一个未使用的路径，不能复用普通生成器已写入的目录。

普通生成器为无参 `new Date()` 提供从固定起点逐毫秒递增的快照时钟（#1129）；种子与之后新写的时间必然不同，避免快机器同一毫秒导致旧值集合误判。`Date.now()`、带参 `new Date(ms)`、`Date()`、锁超时、文件 mtime 和 pid 存活探测均保持真实行为。时钟只在普通生成期间安装，结束时恢复；`--verify-rust` 交接路径使用原生时钟。输出目录递归创建，文件独占写入，已有金样不会被覆盖。

双跑程序在同一个真实 Node 进程中独立执行两次普通生成器，保留相同活 pid；每次生成自动重置快照时钟。两次生成后逐文件逐字节比较并打印 oracle 与文件清单 SHA-256，不在生成后改写期望值。两种方式的输出均由真实 TS registry 产生，消费测试仍按下述既有 pid / 时间归一化规则比较。

`verify-timestamps.mts` 将外层无参 Date 固定到同一毫秒，执行真实生成器，检查 11 条孤儿记录的保留/新写时间以及两个 stale-lock accept；旧生成器确定性漏 mask，修复后新写值为 `<new>`，种子原值保留。探针还捕获生成期间实际使用的 Date 构造器，验证其带参构造与 `Date.now()`，并检查生成结束后恢复调用方时钟。三个独占写探针各预置一个文件哨兵，分别要求 initial 拷贝、after 拷贝、oracle 发布以 EEXIST 拒绝且保留原字节。

`timestamp-negative.py` 的六项变异分别撤回时钟安装、删除时钟恢复、去掉带参分支、允许上述三处覆盖。每项绑定具名探针与唯一的单行 `#1129` 断言；其他异常、超时、编译失败均为 INVALID。每项运行正控、应用变异、`cargo test --no-run`、负控、恢复正控，全部处理后原子汇总，任一非 KILLED 即非零退出。cargo 编译只证明 Rust 树可构建；`.mts` 的变异由真实 Node/tsx 执行验证。源码、探针、脚本及共享裁决依赖 `negative_process.py`、`negative_suite.py`、`negative_panic.py`、`pending_reanchor.py` 均须等于 HEAD，调用方必须提供构建目录；完整套件由协调者提交后在宿主运行。

```sh
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/run-registry/verify-timestamps.mts /tmp/fresh-run-registry-collision
python3 src/pilotdeck-gateway/tests/fixtures/run-registry/timestamp-negative.py --check-anchors
python3 src/pilotdeck-gateway/tests/fixtures/run-registry/timestamp-negative.py /tmp/fresh-run-registry-negative
```

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
