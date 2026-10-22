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

43 个场景、338 次顺序观测；每次比较返回值、故障是否命中、含空目录与符号链接的文件树、
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
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 \
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-snapshots/generate.mts /tmp/pdrs-snapshot-golden
```

当前两次真实生成 SHA-256：
`631319746ea4e744fb028976931aeabe0d79422335d1f49d4f886d1d7f0e5333`。

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


修复轮 1 补充：五个真实 chmod 场景覆盖 Project/memory 只读时 swap、last_dream 只读时
rollback/clear、last_dream/workspace 只读时 clear，其中 memory 根只读 swap 是两实现原本一致的对照；比较失败后的完整文件树。删除沿用 Node 22.22.0
rimraf 默认参数（maxRetries=0），先 rmdir，再按原始文件名字节顺序递归，首错停止。
新增 readlinkLexical 在显式 alias 父路径下直接比较链接目标与词法源路径，避免 `$ROOT`/realpath 归一掩盖差异。Node 目录复制的 C++ 快路径与原 Rust canonicalize 一致；不适用 JS onLink 的单链接复制语义。
另覆盖清理 staged 失败阻断 backup 恢复、restore stage 写失败、restore dispose 掩盖错误。
所有 Rust 金样回放用 en-US 子进程运行；不在多线程测试进程中调用 set_var。

注错覆盖限制：SnapshotIoHook 仅覆盖快照层 mkdir/write/remove/rename/copy 操作边界。
FileMemoryStore 写入及新 stage 内 repair_workspace_manifest 不走该 hook；TS 全局 fs 注错
表达力更大。本语料不宣称覆盖这些写入故障。copy 递归中途真实 ENOSPC/EIO 造成的部分
live 根亦尚未覆盖（Copy hook 是开始前注错）；留给跟进，不能以本次金样判为已验证。
