# EdgeClaw sqlite 存储金样（A6.6-pr10a / pr10d）

由固定 TS 基线 `resources/PilotDeck`（commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`）
的未修改 `MemoryRepository` 在 Node 22 的 `node:sqlite` 上真实执行生成。基线
SQLite 版本 3.51.2；Rust 侧 `rusqlite 0.39.0` + `bundled` 编译 3.51.3（见
RESULT「依赖与 SQLite 版本」）。

> #979：Node 的 `node:sqlite` 在 Homebrew 构建下链接共享 `libsqlite3`，写库版本随宿主
> 升级（本机 3.53.2，macmini 3.50.4），而 `process.versions.sqlite` 是编译期版本
> （3.51.2）。数据库头 offset 96 的写者版本只作提示、不参与语义：
> `_shared/sqlite_bytes.mjs` 只把这 4 字节规范到 3.51.2，其余字节仍逐字对拍。

## 文件

- `generate.mts`：生成器。先用 `baseline_source_guard.mjs` 校验基线 HEAD、
  工作区干净、`sqlite.ts` 及其相对导入闭包与提交 blob 一致，再执行 TS。
- `observe.mts`：TS / Rust 共用的观察面（pipeline、会话、三类 trace、settings、
  L0 读取），另含 `sqlite_master`、`PRAGMA table_info`、原始行文本的导出。
- `verify.mts`：反向验证器。Rust 写入的 `.db` 交给真实 TS `MemoryRepository`
  重新打开，输出同形状观察面，供 Rust→TS 对拍。
- `oracle.json`：唯一入库的金样。含六个场景的观察值、schema、行文本和
  `.db` 的 base64。`.db` 文件本身不在此目录、不入库。

## 场景

| 场景 | 内容 |
| --- | --- |
| `tsRead` | TS 建库并执行完整脚本；Rust 解码 base64 后打开，字段/顺序/limit/JSON 文本全部一致 |
| `empty` | TS 新库 schema；Rust 新建库的 `sqlite_master.sql` 与 `table_info` 必须逐字一致 |
| `migration` | TS 旧 schema（`state_value` 列）库；Rust 打开后迁移结果与 TS 的 schema/行一致（含 `COALESCE`） |
| `rustWrites` | 同一操作脚本由 Rust 重放；两份快照分别覆盖操作后与 save 侧 `limit` 截断后 |
| `errors` | TS 会抛 `TypeError` 的 trace 写入；记录真实错误文本、抛错后的 pipeline 状态，Rust 必须同样失败且不写库（字符串状态可迭代，仍成功） |
| `repair` | L0 repair 前后：TS 写库后导出修复前 `.db` 与观察面，依次跑 `filterContent` / `rewriteContent` / `identity` 三个 transform 并记录每步结果；Rust 从同一 `.db` 重放，行文本逐字一致。反向由 `verify.mts` 在 Rust 修复前后各读一次 |

脚本还固定了这些边界：

- `localeCompare` 与 UTF-16 码元序不同的记录（`0Z` / `0a`），含重复 id 的去重赢家。
- `startedAt` 含孤立代理：Node/ICU4C 按码点的 UCA 隐式权重排序
  （`\ud800` < `\ud801` < `\udfff`，且都高于已分配字符、低于 U+FFFD）。
  含连续保存、已有状态查询与重复 id 保留；Rust 侧的单测另覆盖 U+FFFD 边界。
- TEXT 中的 NUL：`session_key`、`timestamp`、`source`、`created_at` 与
  `pipeline_state.state_json`。node:sqlite 读出到首个 NUL 为止（列内字节完整）。
- `insertL0Session` 的空 `source`（→ `"openclaw"`）与空 `createdAt`（→ `nowIso()`）；
  后者在 Rust 重放时按约定 mask `created_at`，由单测单独校验写侧默认值。
- 空白 `MIN(timestamp)` 经 `.trim()` 后返回 `undefined`。
- L0 读取（`sqlite.ts:1028-1078`）：`listUnindexedL0BySession` 的 `indexed = 0`
  过滤与 `(timestamp, created_at)` 升序；`getLatestL0Before` 的两列严格 `<`
  边界（同一 timestamp 用 `created_at` 破平，精确键返回 `undefined`）；
  `markL0Indexed` 的空/重复/不存在 id；`getL0ByIds` 的过滤、去重与降序；
  `listRecentL0` 的 `limit >= 1`、`offset >= 0` 钳制与越界；`listAllL0` 升序。
  `messages_json` 覆盖 NUL 后缀、非法 JSON、非数组 JSON 与
  `normalizeMessages` 的字段缺省。
- `normalizeIndexTraceRecord` / `normalizeDreamTraceRecord` 的短路与非数组
  `storedResults` / 原始类型 `outcome` 形状。

`tsRead` / `migration` 的 `.db` 由 `oracle.json` 的 base64 还原到临时目录；
`empty` / `rustWrites` / `errors` 由 Rust 现场建库。`rustWrites.operations` 与
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
A6.6-pr10a 修复轮 1 记录为
`sha256 0274519e489a5611a50162be77b5faf1670e1400d7db897e7b0c093d1990e202`，
修复轮 2 加入孤立代理记录后为
`sha256 bc1e7a4aadb8e028fa9d0ad362a9de57b0c653924844659e91733cb0e234a845`。
A6.6-pr10d 加入 L0 读取与 `repair` 场景后，两次独立生成均为
`sha256 31b0fefdd97d1828af6b7a7cd4b2eeee9c60445db19c2065b60a781f801de7a4`。

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
