# Snapshot 管理投影 TS 夹具（A6.6-pr4 + A8.2-pr2）

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。被测行为是
`src/cli/createLocalGateway.ts:1271-1313` 的 `snapshotList` / `snapshotGet` /
`snapshotRestore`；生成器还直接读 `src/storage/workspaceSnapshot.ts`、项目注册和
`resolveProjectStorageId`，这些入口文件的 SHA-256 记在 `oracle.json.sourceSha256`
并对照 `cc7c4e6` 的 Git blob。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
export PDRS_GIT="$(command -v git)"   # 可选；必须是绝对路径
out="$(mktemp -d)"                     # 生成器用 flag:'wx'，输出目录必须是空的
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/snapshot-projection/generate.mts \
  "$out"
cmp "$out/oracle.json" src/pilotdeck-gateway/tests/fixtures/snapshot-projection/oracle.json
```

生成器先接 #280 的共享溯源守卫 `tests/baseline_source_guard.mjs` 的
`bindSourceTree`：显式绝对 Git、HEAD==`cc7c4e6`、已跟踪文件干净，并把基线 `src/`
每个已跟踪文件逐字节对照该提交的 blob。另断言 Node 22，记录
`node` / `v8` / `platform` / `arch` / `locale`（`Intl.Collator().resolvedOptions().locale`）
与整棵 `src/` 的 `sourceTreeSha256` / `sourceTreeFiles`。`sourceSha256` 只列本投影
的直接入口文件，避免把整个 `createLocalGateway` 的 import 闭包写成上千条回放检查。
#322-4 起固定时钟、Git 溯源前导与安全删除都来自共享
`tests/fixtures/_shared/generator_guard.mjs`，与 `workspace-snapshot/generate.mts`
同一份实现（两个生成器重跑后 oracle 逐字节不变）。

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
5. `restore-missing-target`：登记一个 registry 认识、磁盘上已被删除的 General
   workspace target，用真实 gateway 调 `snapshotRestore`，把 Node `mkdtemp` 的
   `ENOENT` 原文（含实际随机名）录进 oracle。`workspaceId` 与 storage 路径里的
   每次运行 slug 掩成 `<workspaceId>`，随机名掩成 `<random>`。
6. `restore-partial`：一个条目的 manifest 先写 `written.txt`，后一个条目的
   `../` 路径才失败；记录恢复目录里已写的文件、失败错误和 `restoreDir=true`，
   让回放同时验证失败分支的 `targetTree` 与 0700。
7. `gateway-native`（A8.2-pr2）：用真实 `createLocalGateway(...).gateway` 调 58 个
   操作，覆盖 `InProcessGateway` 包装层参数校验（params undefined/null、
   projectKey/sessionKey/snapshotId/targetProjectKey 的类型与空白、校验先于
   `CAPABILITY_UNAVAILABLE`）以及真实 `resolveProjectKey` /
   `resolveProjectStorageId`：绝对与相对 `.cwd` 标记、General workspace
   （pilotHome）、尾斜杠/`..` 归一、标记指向文件/不存在、未注册路径、无标记
   fallback slug、跨 workspace 的 get/restore、restore 目标缺失/文件/未知、
   `../` 条目半途失败。该场景的 `native` 字段描述 pilot home、registry id 与
   `.cwd` 内容，Rust 回放据此重建真实目录，不使用注入 resolver。
8. `storage-id-missing-dotdot` / `storage-id-symlink-dotdot`（#888 fix-r2）：
   General workspace 的 `.cwd` 标记写成 `<pilotHome>/missing/..` 或
   `<pilotHome>/link/..`（`link` 指向文件）。两者的词法 `resolve` 都是 pilotHome，
   但 TS 对原始标记 `statSync` 分别得到 `ENOENT` / `ENOTDIR`，必须跳过该标记，
   由 `p-general` 拥有 storage ID；每个场景 1 个 `snapshotList`，若错误地 stat
   规范化路径则 `p-a-raw-marker` 抢先命中并返回空列表。

固定进程时钟（`2026-01-01T00:00:00Z`，`tick` 每次 +1 ms）让 recorder 的
`createdAt` 跨运行不变；该时钟与溯源前导来自共享 `generator_guard.mjs`。
`mkdtemp` 的随机后缀不固定：`snapshotRestore` 的
`workspaceKey` 与目标树分别投影成 `<restore>` 和“相对 restore 目录的树”；
失败错误里的实际随机名投影成 `<random>`。运行目录统一掩成 `<dir>`；
`escape-object` 的绝对 `objectKey` 是唯一需要把 `<dir>` 写进 manifest 字节的值，
回放时再替换回本次 scratch。写 `oracle.json` 前断言它不含本次 scratch、固定
`/tmp/pdrs-a66p4-*` 或基线路径。生成器连跑两次 `oracle.json` 逐字节一致。

## 删除归属

所有删除走共享 `generator_guard.mjs` 的 `removeOwned`：先 `resolve` 再
`relative` 判 `..`/绝对，越界立即抛错，不触达文件系统。负控把旧固定路径
`/tmp/pdrs-a66-pr4-legacy` 和一个真实越界哨兵分别交给记录桩与真实 remover，
断言零调用、哨兵仍在。`finally` 只删本次 `mkdtemp` 根。`workspace-snapshot`
生成器使用同一实现，两份守卫不会各自漂移。

## `oracle.json`

- `sourceCommit` / `sourceTreeSha256` / `sourceTreeFiles` / `sourceSha256` /
  `node` / `v8` / `platform` / `arch` / `locale`：溯源。
- `scenarios[]`：`name`、`tree`（相对 snapshot storage root 的完整树，文件用
  base64；递归删除后的失败 marker 仍在树里）、`snapshotRoot`、`absoluteFile`
  （回放用的 snapshot 根外绝对对象）、`projects`（projectKey → workspaceId）、
  `fallbackProjectRoot`、`operations[]`；`gateway-native` 与两个
  `storage-id-*` 场景另有 `gateway:true` 与 `native`（pilotHome、cwd、registry
  id + `.cwd` 内容、marker 指向的文件、`symlinks` 的路径与目标）。
- `operations[]`：`op`、`input`（路径已掩成 `<dir>`）或 `params`（`undefined` /
  `null` 的 params 对象，JSON 的 `input` 带不动）、`outcome`（成功为
  `{ok: value}`，失败为 `{error: {name, code?, message}}`）、restore 的
  `targetTree` / `restoreDir`。

## Rust 回放

`tests/snapshot_projection.rs` 先 materialize 记录树；前六个场景用
`runtime::snapshot::SnapshotProjection`，`gateway-native` 与两个
`storage-id-*` 场景用生产 `runtime::snapshot::SnapshotGateway` +
`SnapshotProjects`，合计 115 个操作：

- 精确钉住 9 个场景名、每场景操作数 `44/3/3/3/1/1/58/1/1`、树行数
  `60/8/8/0/8/10/30/8/8`，空/缩水 oracle 直接失败（#293 的零场景类 P2）。
- 逐操作比较成功值或 `{name, code, message}`；restore（成功和失败）都再比较相对
  restore 目录的完整树，并断言 mkdtemp 目录 mode 为 `0700`。失败树尤其覆盖
  “前面条目已写、后面条目才失败”的 `restore-partial`。成功 restore 先断言
  `workspaceKey` 等于本次目录差集观测到的新目录（按 Node `path.join` 词法归一），
  再用该目录掩码并比较树；避免“新建目录但返回上次 restore 目录”被自身遮罩掩盖。
- `restore-missing-target` 的 `ENOENT` 文案来自真实 TS/Node，不再手写；`<random>`
  在回放侧按实际随机后缀掩码。
- `gateway-native` 的 project resolver 是生产实现：pilot home、`.cwd` 标记与
  marker-to-file 冲突由 oracle 的 `native` 字段在 scratch 重建；不再注入
  `TestResolver`。其余场景保留注入表（TS 侧 project registry 的替身）。
- #322-1：mkdtemp 十次 `EEXIST` 后返回 `NODE_IO_REQUIRED`（空消息 +
  `requires_host`），不再手写 `EEXIST: ... mkdtemp '<template>'`；确定性后缀
  注入单测覆盖该不可达分支。
- #322-2：`requires_host()` 文档与测试同时钉住 `NODE_IO_REQUIRED` 与
  `JS_HOST_REQUIRED` 两种形状；普通投影错误不置位。
- 回放之外另有单测覆盖 gateway 包装层挡住/放行的分支：省略 `projectKey` 走
  `fallbackProjectRoot`（只经投影入口）、空 `sessionKey` 不过滤、provider 缺失
  先于 project 解析、校验先于 capability、未知方法保持转发、HostRequired 经
  `dispatch` 返回 `Forward`；以及孤立代理项 snapshotId 的原始 UTF-16 比较、
  mkdtemp 前的 `path.join` 词法规范化、native resolver 的相对标记与 slug 回退。

```sh
cd src
cargo test -p pilotdeck-gateway --locked --offline --test snapshot_projection
```

范围：证明投影（过滤、排序、分页/cursor、limit、ownership、错误、restore
mkdtemp 与目录树）和 A8.2-pr2 的 `SnapshotGateway` / `SnapshotProjects` 适配层
（参数校验顺序、project resolver、storage ID、错误映射）与 TS 一致；适配层
**未注册**进默认 dispatch，`snapshot_*` 仍转发 TS 宿主，native WS 入口由 A8 负责。
`#293` 中投影相关的 `createdAt` 缺失排序、`objectKey` 不校验（实测 TS 同样不校验
`../` 与绝对路径）、Node/locale 记录、删除归属与零场景断言在此覆盖；并发 capture、
重复 JSON/quoted 实现、`scripts/parity/round.py` 的 env 隔离（冻结件）不在本 PR。
