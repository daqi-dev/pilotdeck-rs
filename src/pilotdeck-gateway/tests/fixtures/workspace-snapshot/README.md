# Workspace snapshot TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/storage/workspaceSnapshot.ts`（SHA-256 记在 `oracle.json`）。所有快照目录、manifest 与步骤结果都由真实 TS 的 `ContentAddressedWorkspaceSnapshotRecorder` / `createWorkspaceSnapshotProvider` 产生，无实现源码复制；环境准备（Node v22、基线 lockfile 的 tsx、`PDRS_BASELINE`）同 `../run-registry/README.md`。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/workspace-snapshot/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/workspace-snapshot
```

生成器先断言基线 HEAD，并对每个场景的关键结果做自检（提交/失败状态、缓存命中、marker 早退、排序、manifest 字节等于 `JSON.stringify(, 2)`、树内容），防止步骤引用写错导致断言全部落空。写 `oracle.json` 前断言其中不含临时目录、输出目录与基线路径；Rust 读取时断言不含 `.generate-`。生成器把进程时钟固定在 `2026-01-01T00:00:00Z`（`tick` 每次把假时钟推进 1 ms），因此唯一由真实时钟决定的 `createdAt` 也不再随运行变化，连跑两次 `oracle.json` 逐字节一致；Rust 回放仍用真实时钟，所以比较时继续对 `createdAt` 做 `<ts>` 掩码。

`oracle.json`：

- `configCases`：`resolveWorkspaceSnapshotConfig` 的 env 表（cwd 屏蔽为 `<cwd>`）。
- `scenarios[]`：`workspace`（起始工作区：文件 b64、符号链接、空目录、非 UTF-8 文件名字节）、`steps[]`（capture/list/get/restore 及对快照根与工作区的写删；`<dir>` 为场景目录占位）、`dynamicIds`（本场景由真实 recorder 写出的快照 ID）、`tree`（快照根完整树：路径+kind+大小+sha256）。
  - **掩码范围**：只对 `dynamicIds` 对应分区的 `manifest.json` / `_FAILED.json` 顶层 `createdAt` 掩成 `<ts>`，marker 的 `error` 值内嵌的场景目录掩成 `<dir>`；`failureReason`、`entries.path`、`linkTarget`、手工 manifest 的固定时间、restore 出的用户 `.json` 一律原始比较。size 与 sha256 同取投影后字节（原始 size 含 scratch 路径长度，跨运行不可比）。生成器自检断言 `mask-scope` 的三处日期仍在投影结果里、用户 `.json` 的 sha256 等于原始字节，`provider-quirks` 的固定 `createdAt` 不被投影。
  - capture 默认用**最新** recorder（`recorder` 步骤让后续 capture 换新实例；completed 缓存连失败结果一起缓存，旧实例读不到磁盘上的 marker 变化）。
- `cross`：normal 场景 TS 写出的快照树（文件 b64 + 目录表）与 list/get/restore 的 TS 结果，供 Rust 正向交叉（Rust 读 TS 写的快照）。
- `hostBoundaryCases.unpairedSurrogateCollation`：手工 manifest 的 `createdAt` 依次为 `\ud800`、`\ufffd`、`\ue000`、`\u{10000}`，记录真实 TS `list()` 的排序。Node 让孤立代理项保留独立权重，ICU4X 把它们折成 U+FFFD，因此 Rust `list()` 在键含孤立代理项且实际需要比较时返回宿主边界错误（R19），不返回另一种顺序。

Rust 测试 `tests/workspace_snapshot.rs` 逐场景回放：结果按 stringify 深比较，树逐行比较，另验无 `.tmp-` 残留；oracle.json 缺失时测试明确失败（提示先生成）。

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
| provider-quirks | 手工 manifest：数字/缺字段/非字符串 createdAt/重复键/null 条目/绝对路径/`..` 路径/目录名叫 manifest.json——list 校验跳过、get 不过校验、restore 只动作可执行条目；对象路径为目录时 restore 报 `EISDIR ... read` |
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
| nul-capture-root | snapshotRoot 含 NUL：capture 在失败分区 `mkdir` 处 reject（TypeError），不落 marker |

范围：只证明 WorkspaceSnapshot 存储契约（布局、字节、marker 语义与 TS 一致）；WS 入口与投影是 A6.6-pr4，本 PR 不接 host/gateway 方法。已知差异见 `src/storage/workspace_snapshot.rs` 文件头（未实测 errno 回落 Rust Display、同步 recorder）；磁盘回读的字符串保留原始 `JsString`，`createdAt` 含孤立代理项时的 `localeCompare` 按 R19 返回宿主边界错误（合法 Unicode 与 U+FFFD 仍原生排序）。并发共享：生成器用两条同键的并发 promise 自检真实 TS 的拒绝共享与事后重试；Rust 侧由 `hard_failure_is_shared_with_waiters_and_not_cached` 单元测试确定性覆盖等待者拿到同一 `Err` 且新调用重试，`panicking_leader_publishes_failure_to_waiters` 覆盖 leader panic 时 in-flight 条目仍会发布失败。
