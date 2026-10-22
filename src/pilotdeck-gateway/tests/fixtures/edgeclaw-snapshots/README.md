# EdgeClaw dream snapshot 金样

固定参照 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
`generate.mts` 直接运行真实 `MemoryRepository`，输出 `oracle.json`；不含手写期望值。
溯源守卫验证 Git 对象、整个基线源码树以及 `sqlite.ts` 的相对 import 闭包。
运行时仅向临时目录写入，导入基线前把 HOME 指向临时目录。

对应 TS `src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts`：

- :1179–1245：workspace/global options、runtime capture/restore、live capture。
- :1248–1349：last-dream 路径、目录替换、snapshot stage/load/clear/overview。
- :1352–1492：repository stage/dispose、两根替换与失败回补、install、rollback。
- 直接辅助函数：:239–243、:422–477、:1494–1507、:1521–1532。

34 个场景、281 次顺序观测；每次比较返回值、故障是否命中、含空目录与符号链接的文件树、
文件内容、SQLite schema、全部 L0 行和 pipeline 行。种子 `.db` 是 TS 关闭真实数据库后读取的
原始字节（base64）。包含 project/feedback/user、无关 pipeline 行与需保留的 L0 行。

覆盖项包括：四种 dream status、空值/错误类型/空白过滤与原字符串保留、中文/重音排序、
MEMORY.md 不参与版本、global 版本独立防漂移、stage SQL 隔离、双向 rollback、可选 metadata、
损坏/缺根 metadata、snapshot 中的非托管文件及跳过 symlink、路径拒绝、stage dispose 与重复 clear。
失败场景包含 workspace/global/metadata 写入、stage 第二次 mkdir、两根 backup/install rename、
EPERM/EACCES copy 回退及 symlink、copy 失败、清理失败覆盖原异常、best-effort 清理、
rollback 先安装反向快照后 stage 失败、global 失败回补、逐键删除/写入的 SQLite trigger 失败，
以及真实 chmod 造成的目录不可写。所有注入点必须命中，否则生成器失败。

`concurrency.mts` 还执行受控的两进程重叠：两个实例完成 capture 后同时 stage；实例一在备份
工作区后暂停，实例二完成 workspace/global 替换，再放行实例一。真实 TS 实测实例一报
`ENOTEMPTY`、实例二成功；连同残留备份、最终 live 文件与 SQL 一起写入金样。
Rust 使用自己的两进程重放相同屏障。另有三实例、16 轮的 manifest provider 并发测试，
用有界收信检查快照与 FileMemoryStore 的 workspace→global 取锁顺序。

唯一规范化项：临时根及其 realpath 别名替换为 `$ROOT`，随机兄弟目录后缀替换为 `TEMP`；
`pipeline_state.updated_at` 必须是 ISO 毫秒时间串，然后替换为 `<CLOCK>`。
L0 的显式时间、metadata 与 memory 文件内容中的注入时钟值保持逐字比较；WAL/SHM 的物理
字节不作为数据库逻辑内容。生成器本身冻结 Date，因此整个 oracle 仍须两次逐字节一致。

再生（在仓库根执行，输出目录必须是新的目录）：

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-snapshots/generate.mts /tmp/pdrs-snapshot-golden
```

当前两次真实生成 SHA-256：
`4a4bf0e8d4cfe2fd91e04a12a2cfcd2ac11c19e76cb506efe25b53c7c7d3b0d0`。

验证：先按项目规则检查 cargo slots；在 `src/` 内执行：

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
cargo test --locked --offline -p pilotdeck-gateway --test edgeclaw_snapshots -- --include-ignored
```

两个普通金样测试分别从 TS `.db`/文件树、Rust 原生 seed 重放全部操作。
`real_ts_reopens_rust_seed_and_snapshot_artifacts` 用真实 TS 重放 Rust seed，并对每个最终 Rust
产物重新打开 `MemoryRepository`，执行 capture、overview，再检查整棵文件树与数据库。
`real_ts_regenerates_identically` 再次运行真实生成器并要求与入库 oracle 完全一致。

本单另执行 26 条临时实现变异负控，每条要求测试断言转红、随后恢复原始实现；
其中锁顺序变异须被并发测试的有界等待识别。逐条补丁、原始输出、恢复校验和见本单 `_tmp/20261023-a66-pr10e-snapshot/RESULT.md`。
