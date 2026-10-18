# Workspace snapshot TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/storage/workspaceSnapshot.ts` 及其 import 传递闭包（逐文件 SHA-256 记在 `oracle.json.sourceSha256`，对照 `cc7c4e6` 的 Git blob）。所有快照目录、manifest 与步骤结果都由真实 TS 的 `ContentAddressedWorkspaceSnapshotRecorder` / `createWorkspaceSnapshotProvider` 产生，无实现源码复制；环境准备（Node v22、基线 lockfile 的 tsx、`PDRS_BASELINE`）同 `../run-registry/README.md`。每次运行用 `/tmp/pdrs-r10-*` 的 `mkdtemp` 独占目录，绝不使用固定的 `/tmp/pdrs-r9-*`；清理只删本次 `mkdtemp` 创建的树。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
export PDRS_GIT="$(command -v git)"   # 可选；默认同 `which git`，必须是绝对路径
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/workspace-snapshot/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/workspace-snapshot
```

生成器先接 #280 的共享溯源守卫：`tests/baseline_source_guard.mjs` 的 `bindSourceTree` 用显式绝对路径 Git 断言 HEAD==`cc7c4e6`、已跟踪文件干净，并把基线 `src/` 每个已跟踪文件逐字节对照该提交的 blob（同时发现 `assume-unchanged`/`skip-worktree` 改动）；生成器再断言 `git status --porcelain --untracked-files=no` 为空。`sourceSha256` 覆盖从 `workspaceSnapshot.ts` 出发的相对 import 传递闭包（每个文件同时对照 `git show cc7c4e6:<path>` 的字节），不是只哈希入口文件。随后对每个场景的关键结果做自检（提交/失败状态、缓存命中、marker 早退、排序、manifest 字节等于 `JSON.stringify(, 2)`、树内容），防止步骤引用写错导致断言全部落空。`mkdtemp` 根目录预放 `pdrs-r9-linkout`/`pdrs-r9-path`/`pdrs-r9-target`/`ws-snapshot-scenario` 四个哨兵文件，全部场景跑完后断言仍在。删除的主负控是**路径归属检查**：生成器的 `removeOwned` 与 Rust 的 `remove_owned` 都在调用 `rm` 前要求目标位于本次运行独占目录内，越界立即抛错，与哨兵放在哪里无关。生成器每次运行用录制桩注入旧的固定路径 `/tmp/pdrs-r9-linkout` 和一个真实越界哨兵，断言两者都被拒绝、桩零调用、哨兵仍在；Rust 侧由 `out_of_bounds_removal_is_refused_and_sentinel_survives` 覆盖同样两条。`PDRS_INJECT_LEGACY_DELETE=<path>` 可显式重放注入，正确实现会在 guard 处中止（见第十一轮证据）。写 `oracle.json` 前断言其中不含本次临时目录、输出目录与基线路径；Rust 读取时断言不含固定的 `/tmp/pdrs-*` 路径。生成器把进程时钟固定在 `2026-01-01T00:00:00Z`（`tick` 每次把假时钟推进 1 ms），因此唯一由真实时钟决定的 `createdAt` 也不再随运行变化，连跑两次 `oracle.json` 逐字节一致；Rust 回放仍用真实时钟，所以比较时继续对 `createdAt` 做 `<ts>` 掩码。临时目录保持 20 字符（`/tmp/pdrs-r10-` + 6），使含 NUL 的 `ERR_INVALID_ARG_VALUE` 引文落在 Node 的 128 UTF-16 单元 inspect 上限内；`nul-capture-root` 用 `dirName: '.'` 直接占用该 20 字符根目录。

`oracle.json`：

- `sourceCommit` / `sourceSha256`：固定提交与 import 传递闭包的逐文件 SHA-256 对象（当前闭包只有入口文件，因为 `workspaceSnapshot.ts` 只 import `node:*`；将来新增相对 import 会自动纳入）。
- `configCases`：`resolveWorkspaceSnapshotConfig` 的 env 表（cwd 屏蔽为 `<cwd>`）。
- `scenarios[]`：`workspace`（起始工作区：文件 b64、符号链接、空目录、非 UTF-8 文件名字节）、`steps[]`（capture/list/get/restore 及对快照根与工作区的写删；`<dir>` 为场景目录占位，绝对目标同样落在本次 `mkdtemp` 下）、`dynamicIds`（本场景由真实 recorder 写出的快照 ID）、`tree`（快照根完整树：路径+kind+大小+sha256）。
  - **掩码范围**：只对 `dynamicIds` 对应分区的 `manifest.json` / `_FAILED.json` 顶层 `createdAt` 掩成 `<ts>`，marker 的 `error` 值内嵌的场景目录掩成 `<dir>`；`failureReason`、`entries.path`、`linkTarget`、手工 manifest 的固定时间、restore 出的用户 `.json` 一律原始比较。size 与 sha256 同取投影后字节（原始 size 含 scratch 路径长度，跨运行不可比）。生成器自检断言 `mask-scope` 的三处日期仍在投影结果里、用户 `.json` 的 sha256 等于原始字节，`provider-quirks` 的固定 `createdAt` 不被投影。
  - capture 默认用**最新** recorder（`recorder` 步骤让后续 capture 换新实例；completed 缓存连失败结果一起缓存，旧实例读不到磁盘上的 marker 变化）。
- `cross`：normal 场景 TS 写出的快照树（文件 b64 + 目录表）与 list/get/restore 的 TS 结果，供 Rust 正向交叉（Rust 读 TS 写的快照）。
- `hostBoundaryCases.unpairedSurrogateCollation`：手工 manifest 的 `createdAt` 依次为 `\ud800`、`\ufffd`、`\ue000`、`\u{10000}`，记录真实 TS `list()` 的排序。Node 让孤立代理项保留独立权重，ICU4X 把它们折成 U+FFFD，因此 Rust `list()` 在键含孤立代理项且实际需要比较时返回宿主边界错误（R19），不返回另一种顺序。

Rust 测试 `tests/workspace_snapshot.rs` 逐场景回放：结果按 stringify 深比较，树逐行比较，另验无 `.tmp-` 残留；oracle.json 缺失时测试明确失败（提示先生成）。读入 oracle 时走 #280 的共享校验助手 `tests/common/baseline.rs` 的 `baseline::oracle`：始终断言 `sourceCommit==cc7c4e6` 且 `sourceSha256` 非空、路径与哈希形状合法；设了 `PDRS_BASELINE` 时再用 Git 重算闭包文件的 blob、对照工作树，并断言基线已跟踪文件干净。

反向交叉：`generate.mts <snapshot-root> --verify-rust` 由 Rust 忽略测试 `real_ts_verifies_rust_written_snapshots` 调起（需要 `PDRS_BASELINE` 与 Node/tsx）：Rust 先写出至少两个快照，真实 TS provider 对该根 list/get（manifest 字节必须等于 `JSON.stringify(manifest, null, 2)`、对象摘要与内容一致）并 restore。

```sh
cd src
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway workspace_snapshot -- --include-ignored
```

| 场景 | 覆盖 |
|---|---|
| normal | 多文件/深目录/中文与代理对文件名/去重对象/大文件（200 KB）/空文件/点文件/UTF-16 排序；pre+post 捕获、同键缓存、新 recorder 的 `_COMMITTED` 早退（工作区已删）、list 排序、get、restore（符号链接与断链）、manifest 字节 |
| quiescent | `workspaceStable:false` 失败落 `_FAILED.json`、缓存命中、marker 早退回读 error、损坏 marker（非 JSON）读不出 error、删 marker 后新 recorder 恢复提交 |
| failure-fields | post_agent 的 failureKind/failureReason 进 manifest 与 marker，undefined 字段从 JSON 消失 |
| identity-sanitize | workspaceId/sessionId 消毒（`[^A-Za-z0-9_.-]+`→`-`、首尾剥离），描述符保留原始身份 |
| identity-invalid | `..`/纯非 ASCII/空白/含 `\0` 身份拒绝且不落盘 |
| snapshot-inside | 快照根在工作区内：扫描排除该子树、相似前缀不排除 |
| workspace-missing | scandir ENOENT 落失败 marker（Node 文案） |
| root-equals-workspace | 快照根等于工作区根：拒绝并只留失败 marker |
| non-utf8 | 非 UTF-8 文件名：readdir 损失解码后 lstat ENOENT（`bad-�.txt` 文案）。平台相关：需字节透明文件系统，生成器先探测，APFS/HFS+ 拒绝该字节（EINVAL）即整场景跳过，Rust 回放同探测跳过。Linux 路径待 #283 |
| provider-quirks | 手工 manifest：数字/缺字段/非字符串 createdAt/重复键/null 条目/绝对路径/`..` 路径/目录名叫 manifest.json——list 校验跳过、get 不过校验、restore 只动作可执行条目；对象路径为目录时 restore 报 `EISDIR ... read`。`entries` 为 `5`/`{}`/`"x"`/`null` 时（非数组）按 TS `workspaceSnapshot.ts:115` 的 `Array.isArray` 分支恢复成空条目、`restored:true` 且目标树为空，get 原样返回该值 |
| provider-null | manifest 为 `null`：list/get 抛 TypeError 文案 |
| provider-order | 目录遍历按 Node readdir（libuv 字节序）：重复 `snapshotId` 取最后一份；`createdAt` 用 ICU `localeCompare`（`Z` 排在 `a` 前），相等值稳定保持遍历序 |
| mask-scope | 掩码范围负控：failureReason/entries.path/linkTarget 的 ISO 日期与 restore 出的用户 `.json` 都按原文比较 |
| manifest-syntax | 损坏 manifest：V8 `SyntaxError` 文案（与 CLI 设置命令共用 `pilotdeck_protocol::json_syntax`；夹具随模块移到 protocol） |
| manifest-read-dir | `manifest.json` 是指向目录的符号链接：readFile 报 `EISDIR ... read`（无路径） |
| nul-workspace-dir | workspaceDir 含 NUL：Node fs.validatePath 的 `ERR_INVALID_ARG_VALUE` TypeError 文案落失败 marker |
| surrogate-readback | 手工 manifest 的五个身份字段与 marker 的 `error` 含孤立代理项：list 描述符与 marker 早退回读都保留原始 UTF-16 单元（不折成 U+FFFD） |
| nul-restore-target | restore 的 `targetRoot` 含 NUL：`mkdir` 的 `ERR_INVALID_ARG_VALUE` TypeError（参数名 `path`） |
| nul-restore-object | entry 的 `objectKey` 含 NUL：`readFile` 的 `ERR_INVALID_ARG_VALUE`（`path`） |
| nul-restore-link | entry 的 `linkTarget` 含 NUL：`symlink` 的 `ERR_INVALID_ARG_VALUE`（参数名 `target`） |
| nul-restore-link-output | entry 的 `path` 含 NUL 且为 symlink：`symlink` 的 `ERR_INVALID_ARG_VALUE`（`path`） |
| nul-restore-path | entry 的 `path` 含 NUL 且为 file：`writeFile` 的 `ERR_INVALID_ARG_VALUE`（`path`，在 readFile 成功之后） |
| nul-restore-link-existing | 目标已有输出文件且 `linkTarget` 含 NUL：TS 先 `rm(output)` 再 `symlink`，所以失败后旧文件已消失；同目录预置的 `sentinel` 必须保留（只删条目自己的输出，不递归删目录） |
| nul-link-target-surrogate | `linkTarget="tar\ud800\0get"`：错误引文保留原始 UTF-16（`tar\ud800\x00get`），不折成 U+FFFD |
| nul-output-surrogate | symlink 的 `path="a\ud800\0b"`：输出路径引文保留孤立代理项 |
| nul-object-surrogate | `objectKey="obj\ud800\0key"`：readFile 的源路径引文保留孤立代理项 |
| nul-file-output-surrogate | file 的 `path="a\ud800\0b.txt"`：readFile 成功后 writeFile 的引文保留孤立代理项 |
| nul-capture-root | snapshotRoot 含 NUL：capture 在失败分区 `mkdir` 处 reject（TypeError），不落 marker；20 字符根目录使引文完整保留 64 位 digest |

范围：只证明 WorkspaceSnapshot 存储契约（布局、字节、marker 语义与 TS 一致）；WS 入口与投影是 A6.6-pr4，本 PR 不接 host/gateway 方法。已知差异见 `src/storage/workspace_snapshot.rs` 文件头（未实测 errno 回落 Rust Display、同步 recorder）；磁盘回读的字符串保留原始 `JsString`，`createdAt` 含孤立代理项时的 `localeCompare` 按 R19 返回宿主边界错误（合法 Unicode 与 U+FFFD 仍原生排序）。并发共享：生成器用两条同键的并发 promise 自检真实 TS 的拒绝共享与事后重试；Rust 侧由 `hard_failure_is_shared_with_waiters_and_not_cached` 单元测试确定性覆盖等待者拿到同一 `Err` 且新调用重试，`panicking_leader_publishes_failure_to_waiters` 覆盖 leader panic 时 in-flight 条目仍会发布失败。
