# `rewind_files` 夹具（A6.5-pr6）

`oracle.json` 来自固定 TS 基线（`cc7c4e6`）的真实运行。每个场景在独立的
`mkdtemp` 目录里：真实 `write_file` / `edit_file` / `read_file` 工具经真实
`FileHistoryStore` 产生多轮 `file_snapshot_recorded`，真实
`JsonlTranscriptWriter` 写 transcript，然后**未修改的**
`ProjectRuntimeRegistry.rewindFilesForSdk` 对指定 messageId 执行 dry run /
conflict / rewind。只有两个依赖被替换：

- `registry.resolve(projectKey)` → 该场景的临时 `projectRoot`；
- `registry.createPersistentSessionStorage(...)` → 真实
  `createAgentProjectSessionStorage`（A6.3 的 runtime 资源不在此 PR 范围）。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/rewind/generate.mts /abs/output-parent
```

每次运行在输出父目录下 `mkdtemp`，`flag:'wx'` 写入；连续两次逐字节一致才更新
正本（本次两次运行均为
`sha256(oracle.json) = f832780f11b22530a73f9b53e920e20b3886005942f0602943a9b959348cd66e`）。
场景 7 个、`rewindFilesForSdk` 调用 10 次。

## 场景

| 场景 | 覆盖 |
|---|---|
| `success-multi-file` | 同一条消息编辑两个文件；dry run 不落盘且只给 insertions/deletions；真实 rewind 恢复两个 v1 备份并给 filesChanged |
| `conflict-external-edit` | 检查点后外部改文件 → dry run 与真实调用都返回 conflicts + 与 TS 同形的 error，且不写盘 |
| `deleted-file-restore` | track-only 宿主机（工具端 `markEditCommitted` 可选）下文件被外部删除，rewind 从备份重建；deletions 按备份行数计 |
| `unknown-message-id` | 有快照但目标 messageId 不存在 → `No snapshot for messageId ...`（dry run 同） |
| `no-checkpoint` | 有 transcript 但没有任何 `file_snapshot_recorded` → `No persisted file checkpoint is available for this session.` |
| `older-snapshot-isolation` | 两条消息各自的快照；rewind 旧消息只动它自己的文件，后续消息的文件保持 |
| `missing-backup` | 备份 blob 缺失 → conflicts 为空、diff 把 before 当不存在、rewind 报 `missing` 且不写目标 |

## 归一化与为什么

- **临时根**：所有绝对路径（`trackedFileBackups` 键、`filesChanged`、
  `conflicts`、`missing`、`error` 文本）统一替换为 `<root>`；Rust 回放再替换回
  本次 `Scratch` 根。
- **备份文件名**：真实名是 `sha256(绝对路径)[..16]@vN`，随 run 变。这里按
  `(相对路径, version)` 归一为 `<backup:相对路径>@vN`，并在 `backups` 里同时给出
  内容与 mode；Rust 回放按同一 token 写 blob。备份命名本身由 A6.5-pr5 的
  `write_tools` 夹具逐字节覆盖，本夹具不再重复。
- **transcript**：`entryId`/`parentEntryId` 由固定 `uuid()` 递增序列产生，
  `createdAt` 用固定时钟 `2026-01-01T00:00:00.000Z`，因此两次生成逐字节一致。
- **`expectedFileStates`**：由真实工具 `markEditCommitted` 产生；`deleted-file-restore`
  刻意使用不调用 `markEditCommitted` 的 track-only 端口（该方法是 TS 工具端
  optional），否则外部删除会先被 conflict 守卫拒绝，无法观察恢复路径。
- **工作树**：`treeBefore` 是首次调用前的完整文件树（相对路径 + base64 + mode），
  Rust 回放以它重建调用前状态；`treeAfter` 逐调用保存，用于证明 dry run 不落盘与
  rewind 的最终字节。

## 已知边界

- `backupStorage`（host blob 存储）未移植；与 A6.5-pr5 一致，只覆盖文件系统布局。
- 目标 messageId 的语义与 TS 一致：`findSnapshot` 取**最后一个 messageId 完全相等**
  的快照；不是“严格早于该消息”的搜索。
- 非 UTF-8 文件名、损坏 transcript、`backupFileName` 缺失以外的畸形条目不在本夹具
  范围；解析层对畸形条目返回 error 文本而不是猜测。
