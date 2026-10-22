# EdgeClaw FileMemoryStore 真实 TS 金样

正本：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`generate.mts` 执行未修改的 `core/file-memory.ts`；接线场景执行真实
`core/storage/sqlite.ts` 的 `MemoryRepository`。输出值、错误码与最终文件树均来自
TS，未手写期望值。`observe.mts` 只固定时钟、回放输入和规范化临时根路径。
生成前 `bindSourceTree` 校验 Git 对象与整个 `src/` 树；`sourceClosureSha256`
登记两个入口的完整相对 import 闭包（含 type import）。Rust 回放通过共享
`tests/common/baseline.rs` 验证溯源。

```sh
PDRS_BASELINE=/abs/PilotDeck node \
  --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-file-memory/generate.mts /tmp/file-memory-run
```

Node v22.22.0，固定时间 `2026-10-22T00:00:00.000Z`。两次独立执行的
`oracle.json` 逐字节一致，SHA-256：
`63bae8e370612453c8a56b2019f5421c5d487b40db14d1c9950c4c018367e99f`。

14 个脚本场景覆盖：single、general、duplicates、append、disabled、errors、preview、
edge-cases、number-render、edit-meta-whitelist、general-utf16、notes-null、origin、concurrent。每步记录 API 返回值；每组追加只读 API 回读
（含 bundle、snapshot、版本散列），最后保存磁盘文件树。输入中的 `@name.field`
引用之前调用的原始返回值；输出中的 `$ROOT` 仅替代临时目录前缀。接线场景另记录
workspace/global 两个 Store 的回读与文件树（另覆盖两者同根），不把 SQLite 二进制放入金样。
`edit-meta-whitelist` 验证 single 模式只透传 projectName/description/status：传入的
dreamUpdatedAt 不会新建或改写已有 dream 时间。

覆盖 UTF-16 代理对与未配对代理、截断形成的未配对代理路径、写盘替换与 API 字符串
的差别、JSON 预览截断、解析/渲染、元数据、用户事实、manifest 损坏重建、目录缺失、
真实 EACCES 读写错误和 EISDIR。权限场景必须实际失败，否则生成器拒绝产出。
TS `countMemoryEntries` 在运行时会把额外的 limit/offset 透传给 list，金样保留此行为。

Rust 集成测试 `edgeclaw_file_memory.rs` 包含两个独立方向：写入金样中的真实 TS 文件后
用 Rust 读；Rust 回放同一脚本后由 `verify.mts` 用真实 TS 重新打开。并发测试用一个
Store 和多个同根 Store 写入，不同来源并发与 TS 顺序执行的最终字节比较；相同来源并发与 TS 的去重结果比较。
同根 provider 回归在 5 秒内完成写入与读回，并对照真实 TS 的文件字节和两种 Store 的选项效果。
进程内按词法解析后的根路径共享锁，同一线程 provider 重入复用外层持锁；跨进程仍按 TS 的单写者约束使用。

`negative.py` 是可复现的产品变异负控：先跑正控，再分别恢复非重入锁（须触发 5 秒断言）、
提前释放操作锁（须使相同来源并发测试转红）、恢复元数据整包透传（须使金样回放转红）。
脚本要求独占本工作树，临时修改 file_memory.rs，每条结束在 finally 中恢复原文，记录前后
SHA-256，最后重跑正控；编译失败不计检出。不要与此树其他编译或写入同时执行。

```sh
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-file-memory/negative.py /tmp/file-memory-negative
```

```sh
# 普通回放（无需 Node）
cargo test -p pilotdeck-gateway --test edgeclaw_file_memory
# 实际反向重开 + 再生核验（必须有固定基线）
PDRS_BASELINE=/abs/PilotDeck cargo test -p pilotdeck-gateway \
  --test edgeclaw_file_memory -- --include-ignored --nocapture
```

`FileMemoryStore` 是文件层；`EdgeClawFileStores` 组合 pr10a 的公开 SQLite API 与
workspace/global 文件 Store。MemoryRepository 的目录扫描、外部工作区映射、dream
快照换根和检索等后续拆单不由本夹具声明完成。

修复轮 1：旧回放中的两个越界 id 改为普通缺失 id（`a/missing`、`Project/missing`），
并由未改动真实 TS 重生成金样两次；所有返回值和磁盘期望不变。越界输入的安全偏离
集中由 `edgeclaw_entries` 的入口矩阵及 `file_memory` 的直接写入测试验证，期望错误取自
真实 TS `editEntry(missing)`，避免继续把旧的静默跳过行为当作 Rust 期望。
