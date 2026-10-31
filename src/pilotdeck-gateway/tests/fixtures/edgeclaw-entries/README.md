# EdgeClaw repository entries / clear 金样

真相源为固定 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的
`src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts:1752-2004`，
包含调用到的 file-memory、general-projects 与工具闭包。生成和重开均先通过
`baseline_source_guard.mjs` 核对 commit、工作树与闭包哈希；不改基线或 node_modules。

`generate.mts` 调真实 `MemoryRepository` 创建数据库和文件，逐步记录返回值、完整文件树、
SQL schema、L0 与 pipeline 行。`oracle.json` 的数据库 base64 和期望值均由真实 TS 产出。
> #979: the SQLite writer-version field (header offset 96) in the embedded `.db`
> base64 is canonicalised to 3.51.2 by `_shared/sqlite_bytes.mjs`; every other
> database byte is still compared exactly.

复用 snapshots 的磁盘观测与固定时钟；只归一化临时根、SQL updated_at，保留完整 JSON 文本
及字段顺序、文件内容、错误类别和 SQL 原文。固定 Node v22.22.0、locale en-US。

28 场景、152 步覆盖 count、按原 ids 保序和重复、跨 global/workspace/external 的读取、编辑、
meta、重复 deprecate/restore/delete、非法路径与 archive 错误、snapshot version、三种 clear。
clear 故障包含 SQL 第二语句失败的已提交 L0 删除、恢复设置失败、workspace/global/snapshot
删除失败的文件树残留、entry/meta 写盘失败，以及符号链接和悬空根。
生成器断言每个以 `-fail` 命名的场景确实到达错误，避免空目录 chmod 探针没有触发故障。操作层不加事务或回滚。

`concurrency.mts` 运行两个真实 Node 进程：第一实例 clear-all 在删库后、删 workspace 前暂停；
第二实例写入、clear-current、再写入，第一实例继续。Rust 用同一 IO 边界与 barrier，逐项比较
两实例结果和最终磁盘。保留 TS 在重叠 clear 时删除另一实例新写文件的行为。
同进程测试同时操作两个 repository 与 manifest provider，所有成功操作检查结果，20 秒超时抓死锁。

```sh
PDRS_BASELINE=/absolute/PilotDeck LANG=en_US.UTF-8 node \
  --import /absolute/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-entries/generate.mts /tmp/entries-golden-1
# 第二个全新输出目录重复一次，cmp oracle.json 必须逐字节一致。
PDRS_BASELINE=/absolute/PilotDeck cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_entries -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-entries/negative.py \
  --out _tmp/20261023-a66-pr10g-entries/negative
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-entries/negative_ids.py \
  --out _tmp/20261025-issue-680/fix-r1/negative-ids
```

普通测试恢复 TS 创建的库和文件逐步回放。ignored 反向测试由 Rust 原生创建数据库、文件、
metadata 和 pipeline，再跑 CRUD/clear，最后让真实 TS 重开所有成功和失败产物；另一个 ignored
守卫重新生成整个 oracle（含双进程探针），要求与入库字节一致。

负控逐项改坏生产实现、要求运行到具体 scenario/observation 后断言失败，finally 恢复原字节并
记录 SHA256；编译失败不算杀死。`PDRS_ENTRIES_CASE` 可筛选单场景（仅调试用，未命中即失败）。
原地改写生产源码的负控统一走 `negative_process.mutating_sources`：先取树内固定
`.negative-source.lock` 的 `fcntl.flock`（内核在进程退出、含 SIGKILL 时自动释放；不做 pid 判活或
旧锁回收），再逐字节比较 `git show HEAD:<path>`；有残留变异或人的未提交改动就报错并提示
`git checkout -- <path>`，绝不把现状当原件覆盖。基线通过后快照、yield、finally 逐字节恢复并再次
比对 HEAD；等 Cargo 槽位与变异写入都在锁内。`edgeclaw-entries`、`edgeclaw-accessors`、
`edgeclaw-export`、`edgeclaw-file-memory`、`edgeclaw-dangling-links` 中改写同一组
`entries.rs` / `accessors.rs` / `file_memory.rs` / `snapshots.rs` / `overview.rs` 的脚本都调用同一
实现；plugin-runtime 与 session-domain 等其它原地改源码脚本也一样。`summary.json` 只在恢复运行和
字节校验都成功后写 `restoredPass:true`。

`idAudit` 回放容器形状与单元素 ids；`idBatchAudit` 另采 15 条真实 TS 多元素批次
（`["合法 id", 5]`、`[null, 5]`、`[]`，含 `archiveTmpEntries` 的 `{ids}` 入口），每例用独立新库，
锁住「先校验全部元素、再碰 store」以及空批次不报形状错误。空批次会修复 manifest，因此该例同时
记录完整残留树 `diskAfter`，Rust 逐字节比对而不只比较「磁盘变过」。
`negative_ids.py` 的短锚点分别改坏容器、容器 nullish、元素、nullish 元素、元素顺序与空数组分支。

修复轮 1：IO 结果同时比较 errno 和完整 Node message（含 syscall、路径；仍只归一化临时根）。
`invalid-entry-operations` 的复合 mutation 不再混入 `../escape`：安全偏离由专用测试覆盖，
真实 TS 金样仍原样保存 TS 行为，不把 Rust 安全期望伪装成 TS 输出。
`entry_paths_reject_escape_without_touching_outside_files` 覆盖 workspace/global repository 与
store 读、edit、delete、deprecate、restore；`direct_entry_write_rejects_unsafe_paths` 覆盖底层写入。
包含 `..`、绝对路径、越界文件/目录 symlink、缺失叶节点和根内 `..`，逐次核对根外文件未动。
拒绝错误取自真实 TS `editEntry(missing)` 金样，只替换输入路径。
`negative_fix1.py --out <目录>` 分别移除每个入口的路径校验、snapshot-version 操作锁，
必须运行时断言失败；既有 `negative.py` 还覆盖 IO message、global id 分类和映射回归。
`negative.py` / `negative_ids.py` / `negative_fix1.py` 均保留调用者的 Cargo 环境，恢复并核对源文件字节。

`negative_round6.py` / `negative_round7.py` 只接受本树 `_build/cargo-target`
（未设置时同样使用此目录），scanner 与 Cargo 共用它；共享准入守卫拒绝 `src/target`。
从仓库根运行：

```sh
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-entries/negative_round6.py --out <全新输出目录>
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-entries/negative_round7.py <全新输出目录>
```
