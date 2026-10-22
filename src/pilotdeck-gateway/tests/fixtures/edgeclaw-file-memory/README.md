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
`365c3574b80c814f888b550325c56e15c3f748fbe1519acabc6556b668877094`。

13 个脚本场景覆盖：single、general、duplicates、append、disabled、errors、preview、
edge-cases、number-render、general-utf16、notes-null、origin、concurrent。每步记录 API 返回值；每组追加只读 API 回读
（含 bundle、snapshot、版本散列），最后保存磁盘文件树。输入中的 `@name.field`
引用之前调用的原始返回值；输出中的 `$ROOT` 仅替代临时目录前缀。接线场景另记录
workspace/global 两个 Store 的回读与文件树，不把 SQLite 二进制放入金样。

覆盖 UTF-16 代理对与未配对代理、截断形成的未配对代理路径、写盘替换与 API 字符串
的差别、JSON 预览截断、解析/渲染、元数据、用户事实、manifest 损坏重建、目录缺失、
真实 EACCES 读写错误和 EISDIR。权限场景必须实际失败，否则生成器拒绝产出。
TS `countMemoryEntries` 在运行时会把额外的 limit/offset 透传给 list，金样保留此行为。

Rust 集成测试 `edgeclaw_file_memory.rs` 包含两个独立方向：写入金样中的真实 TS 文件后
用 Rust 读；Rust 回放同一脚本后由 `verify.mts` 用真实 TS 重新打开。并发测试用一个
Store 和多个同根 Store 写入，不同来源并发与 TS 顺序执行的最终字节比较；相同来源并发与 TS 的去重结果比较，并有释放操作锁的专项负控。进程内按词法解析后的
根路径共享锁；跨进程仍按 TS 的单写者约束使用。

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
