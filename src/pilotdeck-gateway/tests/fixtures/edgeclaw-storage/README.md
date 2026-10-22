# EdgeClaw sqlite 存储金样（A6.6-pr10a）

由固定 TS 基线 `resources/PilotDeck`（commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`）
的未修改 `MemoryRepository` 在 Node 22 的 `node:sqlite` 上真实执行生成。基线
SQLite 版本 3.51.2；Rust 侧 `rusqlite 0.39.0` + `bundled` 编译 3.51.3（见
RESULT「依赖与 SQLite 版本」）。

## 文件

- `generate.mts`：生成器。先用 `baseline_source_guard.mjs` 校验基线 HEAD、
  工作区干净、`sqlite.ts` 及其相对导入闭包与提交 blob 一致，再执行 TS。
- `observe.mts`：TS / Rust 共用的观察面（pipeline、会话、三类 trace、settings），
  另含 `sqlite_master`、`PRAGMA table_info`、原始行文本的导出。
- `verify.mts`：反向验证器。Rust 写入的 `.db` 交给真实 TS `MemoryRepository`
  重新打开，输出同形状观察面，供 Rust→TS 对拍。
- `oracle.json`：唯一入库的金样。含四个场景的观察值、schema、行文本和
  `.db` 的 base64。`.db` 文件本身不在此目录、不入库。

## 场景

| 场景 | 内容 |
| --- | --- |
| `tsRead` | TS 建库并执行完整脚本；Rust 解码 base64 后打开，字段/顺序/limit/JSON 文本全部一致 |
| `empty` | TS 新库 schema；Rust 新建库的 `sqlite_master.sql` 与 `table_info` 必须逐字一致 |
| `migration` | TS 旧 schema（`state_value` 列）库；Rust 打开后迁移结果与 TS 的 schema/行一致（含 `COALESCE`） |
| `rustWrites` | 同一操作脚本由 Rust 重放；两份快照分别覆盖操作后与 save 侧 `limit` 截断后 |

`tsRead` / `migration` 的 `.db` 由 `oracle.json` 的 base64 还原到临时目录；
`empty` / `rustWrites` 由 Rust 现场建库。`rustWrites.operations` 与
`limitOperations` 是同一份 JSON 脚本，TS 与 Rust 各自执行。

## 再生

```sh
cd <repo>
PDRS_BASELINE=/abs/PilotDeck \
  node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-storage/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-storage
```

生成器冻结 `Date`（2026-10-21T00:00:00.000Z），两次独立运行必须逐字节一致；
A6.6-pr10a 记录为
`sha256 311547d39174af400485deff5e6c98db36f333ce683e609005cdd22faa46ad9e`。

## 回放

```sh
cd src
cargo test --locked --offline -p pilotdeck-gateway --test edgeclaw_storage

# Rust → TS（需要真实基线 checkout 与 node_modules）
PDRS_BASELINE=/abs/PilotDeck \
  cargo test --locked --offline -p pilotdeck-gateway --test edgeclaw_storage -- --ignored
```

默认测试只读 `oracle.json` 与其中的 `.db` base64，不需要 Node；`#[ignore]`
用例才启动真实 TS 读取 Rust 写出的库。
