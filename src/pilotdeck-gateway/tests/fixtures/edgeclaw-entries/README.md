# EdgeClaw repository entries / clear 金样

真相源为固定 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的
`src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts:1752-2004`，
包含调用到的 file-memory、general-projects 与工具闭包。生成和重开均先通过
`baseline_source_guard.mjs` 核对 commit、工作树与闭包哈希；不改基线或 node_modules。

`generate.mts` 调真实 `MemoryRepository` 创建数据库和文件，逐步记录返回值、完整文件树、
SQL schema、L0 与 pipeline 行。`oracle.json` 的数据库 base64 和期望值均由真实 TS 产出。
复用 snapshots 的磁盘观测与固定时钟；只归一化临时根、SQL updated_at，保留完整 JSON 文本
及字段顺序、文件内容、错误类别和 SQL 原文。固定 Node v22.22.0、locale en-US。

27 场景、146 步覆盖 count、按原 ids 保序和重复、跨 global/workspace/external 的读取、编辑、
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
```

普通测试恢复 TS 创建的库和文件逐步回放。ignored 反向测试由 Rust 原生创建数据库、文件、
metadata 和 pipeline，再跑 CRUD/clear，最后让真实 TS 重开所有成功和失败产物；另一个 ignored
守卫重新生成整个 oracle（含双进程探针），要求与入库字节一致。

负控逐项改坏生产实现、要求运行到具体 scenario/observation 后断言失败，finally 恢复原字节并
记录 SHA256；编译失败不算杀死。`PDRS_ENTRIES_CASE` 可筛选单场景（仅调试用，未命中即失败）。

修复轮 1：IO 结果同时比较 errno 和完整 Node message（含 syscall、路径；仍只归一化临时根）。
`invalid-entry-operations` 的复合 mutation 不再混入 `../escape`：安全偏离由专用测试覆盖，
真实 TS 金样仍原样保存 TS 行为，不把 Rust 安全期望伪装成 TS 输出。
`entry_paths_reject_escape_without_touching_outside_files` 覆盖 workspace/global repository 与
store 读、edit、delete、deprecate、restore；`direct_entry_write_rejects_unsafe_paths` 覆盖底层写入。
包含 `..`、绝对路径、越界文件/目录 symlink、缺失叶节点和根内 `..`，逐次核对根外文件未动。
拒绝错误取自真实 TS `editEntry(missing)` 金样，只替换输入路径。
`negative_fix1.py --out <目录>` 分别移除每个入口的路径校验、snapshot-version 操作锁，
必须运行时断言失败；既有 `negative.py` 还覆盖 IO message、global id 分类和映射回归。
两个负控脚本均保留调用者的 Cargo 环境，恢复并核对源文件字节。
