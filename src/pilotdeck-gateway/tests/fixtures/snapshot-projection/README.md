# Snapshot 管理投影 TS 夹具（A6.6-pr4）

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测行为是
`src/cli/createLocalGateway.ts:1271-1313` 的 `snapshotList` / `snapshotGet` /
`snapshotRestore`；生成器还直接读 `src/storage/workspaceSnapshot.ts`、项目注册和
`resolveProjectStorageId`，这些入口文件的 SHA-256 记在 `oracle.json.sourceSha256`
并对照 `cc7c4e6` 的 Git blob。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
export PDRS_GIT="$(command -v git)"   # 可选；必须是绝对路径
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/snapshot-projection/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/snapshot-projection
```

生成器先接 #280 的共享溯源守卫 `tests/baseline_source_guard.mjs` 的
`bindSourceTree`：显式绝对 Git、HEAD==`cc7c4e6`、已跟踪文件干净，并把基线 `src/`
每个已跟踪文件逐字节对照该提交的 blob。另断言 Node 22，记录
`node` / `v8` / `platform` / `arch` / `locale`（`Intl.Collator().resolvedOptions().locale`）
与整棵 `src/` 的 `sourceTreeSha256` / `sourceTreeFiles`。`sourceSha256` 只列本投影
的直接入口文件，避免把整个 `createLocalGateway` 的 import 闭包写成上千条回放检查。

## 生成方式（真实 TS，不是重写）

1. `ContentAddressedWorkspaceSnapshotRecorder` 在每次运行独占的 `mkdtemp` 下写快照：
   7 个已提交快照（`sess-a` 4 个、`sess-b` 3 个）、1 个其他工作区快照、1 个
   `workspaceStable:false` 的失败 marker。
2. 手工 manifest 补 provider 边界：缺 `createdAt`、`objectKey` 为 `../outside.txt`
   或绝对路径、缺 `objectKey` 的 file 条目、`..` 路径、对象文件不存在、空 `entries`。
3. 用真实 `createLocalGateway` 调 44 个投影操作：默认/显式 limit、1/2/3/500 分页、
   连续 cursor 走完、末项 cursor、坏 cursor（不存在/他工作区/被 sessionKey 过滤/空串/
   null/数字）、`limit` 0/负/小数/501/1e20/字符串/布尔/null、sessionKey 过滤、
   跨工作区 list/get、未知 projectKey、三种错误码、get 原样 manifest、restore 到
   `targetProjectKey` 与省略 target 的分支、restore 目录树、恢复失败后残留的
   mkdtemp 目录。
4. 另三个场景：`provider-syntax`（坏 JSON → `SyntaxError`）、`provider-null`
   （manifest `null` → `TypeError`）、`unconfigured`（无 provider →
   `CAPABILITY_UNAVAILABLE`，且先于 project 解析）。

固定进程时钟（`2026-01-01T00:00:00Z`，`tick` 每次 +1 ms）让 recorder 的
`createdAt` 跨运行不变。`mkdtemp` 的随机后缀不固定：`snapshotRestore` 的
`workspaceKey` 与目标树分别投影成 `<restore>` 和“相对 restore 目录的树”。
运行目录统一掩成 `<dir>`；`escape-object` 的绝对 `objectKey` 是唯一需要把
`<dir>` 写进 manifest 字节的值，回放时再替换回本次 scratch。写 `oracle.json`
前断言它不含本次 scratch、固定 `/tmp/pdrs-a66p4-*` 或基线路径。生成器连跑两次
`oracle.json` 逐字节一致。

## 删除归属

所有删除走 `removeOwned`：先 `resolve` 再 `relative` 判 `..`/绝对，越界立即抛错，
不触达文件系统。负控把旧固定路径 `/tmp/pdrs-a66-pr4-legacy` 和一个真实越界哨兵
分别交给记录桩与真实 remover，断言零调用、哨兵仍在。`finally` 只删本次
`mkdtemp` 根。

## `oracle.json`

- `sourceCommit` / `sourceTreeSha256` / `sourceTreeFiles` / `sourceSha256` /
  `node` / `v8` / `platform` / `arch` / `locale`：溯源。
- `scenarios[]`：`name`、`tree`（相对 snapshot storage root 的完整树，文件用
  base64；递归删除后的失败 marker 仍在树里）、`snapshotRoot`、`absoluteFile`
  （回放用的 snapshot 根外绝对对象）、`projects`（projectKey → workspaceId）、
  `fallbackProjectRoot`、`operations[]`。
- `operations[]`：`op`、`input`（路径已掩成 `<dir>`）、`outcome`（成功为
  `{ok: value}`，失败为 `{error: {name, code?, message}}`）、restore 的
  `targetTree` / `restoreDir`。

## Rust 回放

`tests/snapshot_projection.rs` 先 materialize 记录树，再用
`runtime::snapshot::SnapshotProjection` 重放 53 个操作：

- 精确钉住 4 个场景名、每场景操作数 `44/3/3/3`、树行数 `60/8/8/0`，空/缩水
  oracle 直接失败（#293 的零场景类 P2）。
- 逐操作比较成功值或 `{name, code, message}`；恢复成功再比较相对 restore 目录的
  完整树，并断言 mkdtemp 目录 mode 为 `0700`。
- project 解析与 `resolveProjectStorageId` 是注入依赖（对应 A6.3 的项目 registry
  与 paths 尚未移植）；resolver 的 `PROJECT_NOT_FOUND` 文本与 TS 相同。
- 回放之外另有单测覆盖 gateway 入口挡住的投影分支：省略 `projectKey` 走
  `fallbackProjectRoot`、空 `sessionKey` 不过滤、provider 缺失先于 project 解析。

```sh
cd src
cargo test -p pilotdeck-gateway --locked --offline snapshot_projection
```

范围：只证明投影（过滤、排序、分页/cursor、limit、ownership、错误、restore
mkdtemp 与目录树）与 TS 一致；不接 WS 入口（A8）。`#293` 中投影相关的
`createdAt` 缺失排序、`objectKey` 不校验（实测 TS 同样不校验 `../` 与绝对路径）、
Node/locale 记录、删除归属与零场景断言在此覆盖；并发 capture、重复 JSON/quoted
实现、`scripts/parity/round.py` 的 env 隔离（冻结件）不在本 PR。
