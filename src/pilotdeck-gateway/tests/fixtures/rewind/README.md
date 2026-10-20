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
`sha256(oracle.json) = eda586c2ef5e092d52a0eefaf0e6f1856189764c5eec3917bcae8ae445cd4503`）。
场景 19 个、`rewindFilesForSdk` 调用 34 次。生成器在 `mkdtemp` 后
`process.chdir(scratch)`：畸形条目枚举出的相对路径（`"0"`）按 TS 语义相对
cwd 解析，不能让它们有机会落到基线源码树。

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
| `snapshot-missing-then-directory` | 快照记 `a.txt` 不存在（null backup + Missing 指纹），随后 `a.txt` 变成目录；dry run 与真调用都返回 `EISDIR: illegal operation on a directory, read`，树不变 |
| `snapshot-present-then-directory` | 快照记 `a.txt` 存在（Present 指纹），随后换成目录；同样 EISDIR，不再退化成 conflict |
| `entry-missing-tracked-backups` | 畸形条目缺 `trackedFileBackups` → `Cannot convert undefined or null to object` |
| `entry-missing-message-id` | 畸形条目缺 `messageId` → 按 `undefined` 回放，最终 `No snapshot for messageId msg-1` |
| `restore-mkdir-error` | track-only 备份仍在、目标父路径被换成普通文件；dry run 正常预览 0/2，真回滚在 `mkdir '<root>/p'` 报 Node 同文 `EEXIST: ...`，不带 `(os error N)` |
| `entry-tracked-backups-string` | `trackedFileBackups` 是字符串 `"ab"`：`Object.entries` 枚举 UTF-16 码元 `"0"`/`"1"`；dry run 预览 0/0，真回滚在 `path.join(dir, undefined)` 抛 `The "path" argument must be of type string. Received undefined` |
| `entry-tracked-backups-array` | `trackedFileBackups` 是数组：`Object.entries` 按下标枚举；备份不存在时真回滚把相对路径 `"0"` 报进 `missing`，不写盘 |
| `entry-tracked-backups-null-element` | `[null]`：replay 的 `backup.backupFileName` 属性访问在 dry run / 真回滚都抛 `Cannot read properties of null (reading 'backupFileName')` |
| `entry-tracked-backups-null-value` | `{x: null}`：同一条 replay 期 TypeError，证明对象值与数组元素一致 |
| `success-astral-path` | 非 BMP 文件名 `😀.txt` 经真实写工具记录 / 回滚；`filesChanged` 与文件树逐字节一致 |
| `entry-tracked-backups-string-astral` | 字符串容器 `"😀"`：`Object.entries` 枚举两个 UTF-16 码元作为键 `"0"`/`"1"`（不是按码点） |
| `entry-backup-file-name-types` | `backupFileName` 为数字 `5` / 布尔 `true` / 对象 `{}` 时 dry run 即返回按类型变化的 `path.join` TypeError；布尔 `false` 为假值，dry run 正常预览，真回滚才报 `Received type boolean (false)` |

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
  `snapshot-missing-then-directory` 用真实 `FileHistoryStore.trackEdit` +
  `markEditCommitted`（文件缺失时）直接产生 Missing 指纹——真实写工具在写完前不会
  记录 Missing，这是该状态唯一可复现的宿主端口形态。
- **畸形条目**：`entry-missing-*` / `entry-tracked-backups-*` /
  `entry-backup-file-name-types` 直接把畸形快照交给真实
  `JsonlTranscriptWriter.recordFileHistorySnapshot`；TS 的 replay / catch
  路径未修改。字符串容器的值是原始字符，不是备份对象，因此归一化只改写
  “项目根内的对象形 backup”；数组下标 `"0"` 也不是项目路径，其字面备份名
  （`array-missing@v1`）保持原样。非字符串 `backupFileName`（数字/布尔/对象）
  原值保留，不能替换成备份 token，否则 `path.join` 的类型错误就测不出来。
- **工作树**：`treeBefore` 是首次调用前的完整文件树（相对路径 + base64 + mode），
  Rust 回放以它重建调用前状态；`treeAfter` 逐调用保存，用于证明 dry run 不落盘与
  rewind 的最终字节。

## 已知边界

- `backupStorage`（host blob 存储）未移植；与 A6.5-pr5 一致，只覆盖文件系统布局。
- 目标 messageId 的语义与 TS 一致：`findSnapshot` 取**最后一个 messageId 完全相等**
  的快照；不是“严格早于该消息”的搜索。
- 非 UTF-8 文件名、损坏 transcript 与其他畸形条目不在本夹具范围；已覆盖
  `trackedFileBackups` 缺失、字符串（含非 BMP）、数组、元素/值为 `null`，
  `messageId` 缺失，`backupFileName` 缺失（未定义）与非字符串
  （数字/布尔/对象）这些已实测的 TS 行为。
- `expectedFileStates` 是字符串/数组时，TS 先做对象 spread 再按
  `Object.entries` 遍历；Rust 只接受对象，暂不对齐该畸形形态。
- 目录本身没有独立 tree 行（`walk` 只记文件）；涉及目录的场景都放一个
  `keep.txt` 子文件，确保目录存在与否可在 `treeBefore`/`treeAfter` 中逐字节观察。
