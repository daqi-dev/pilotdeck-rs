# 写文件工具族 + FileHistoryStore 夹具（A6.5-pr5）

`oracle.json` 来自固定 TS 基线（`cc7c4e6`）的真实运行：用真实
`createWriteFileTool` / `createEditFileTool` / `createEditNotebookTool` /
`createReadFileTool` 与真实 `FileHistoryStore`，在**每个场景独立的临时目录**里跑同一组
setup 与调用，逐条记录 `validateInput` / `execute` 返回值、调用后的目标文件字节、
file-history 目录树（文件名/内容/mode）、`file_snapshot_recorded` 回调记录、warn、rewind/diff 结果。
`sourceSha256` 覆盖五个入口的相对 import 闭包；生成器用 `bindSourceTree` 校验基线
干净树与 Git blob，并断言 locale 为 `en-US`。

```sh
PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/write-tools/generate.mts /abs/output-parent
```

调用输入里的 `<root>` / `<outside>` 在生成器内先替换成真实临时路径（与 Rust 回放同一替换），结果再归一化。每次运行在输出父目录下 `mkdtemp`，`flag:'wx'` 写入；连续两次逐字节一致才更新正本
（本文件提交时 `sha256(oracle.json) = 0339d78a8a78723dffb386d5739e10e78ffdbb6b56e976e78cd5151b98315175`，
两次运行均为此值）。场景 42 个、调用 80 次。

## 归一化与为什么

- **`mtimeMs`**：写盘后的真实时钟值无法跨运行稳定，结果里的 `metadata.mtimeMs`
  统一记为 `<mtime>`；setup 用显式 mtime（`utimes` / `File::set_modified`），
  读后写判断仍用真实值。
- **备份文件名**：`sha256(绝对路径)[..16]@vN` 的路径含每 run 的临时目录，记录与目录树
  里统一归一为 `<backup>@vN`；哈希前缀本身另由 `backupFileNames`（固定路径
  `/fixed/project/a.txt`、`/fixed/project/中文 文件.txt`）与 Rust
  `get_backup_file_name` 直接比对。
- **notebook 生成 cell id**：TS `Math.random().toString(36).slice(2,15)` 长度可变
  （探针实测 10–13 位，见 `_tmp/20261019-a65-pr5/probes/run.out`）。insert 到
  nbformat≥4.5 时生成 id；夹具把该次生成的 id、`updated_file` 中的出现、结果文本和
  `bytesWritten` 归一为 `<cellid>`。Rust 用 UUID hex 前 13 位，形态不同但语义相同。
- **目录树排序**：按归一后的名字 + 内容 base64 排序，避免归一后同名的两个备份因
  run 不同哈希前缀而换序。

## 已知边界（本 PR 明确不做/刻意更严）

- **语法诊断**：`formatSyntaxDiagnostics` 的 JSON/TypeScript/Python/YAML/HTML/CSV/
  Markdown 检查器不在本 PR 范围；实现恒返回 `undefined`，因此夹具只写这些检查器
  不报错的输入。这是与 TS 的已知偏离，留 A6.5-pr5 后续或按需开单。
- **硬链接**：TS `NodeFsPort` 不拦硬链接；本实现按任务书「写盘限定在允许根内」，
  对已存在且 `nlink > 1` 的目标拒绝（`Writing to <rel> is not allowed by default.`）。
  这是有意的更严安全口径，见 `tests/write_tools.rs::hardlink_and_symlink_writes_stay_inside_root`。
- **符号链接**：解析层用 `realpath`/最深已存在祖先做根外拒绝；损坏链接按链接目标
  词法归一后判断。TOCTOU 不在本层保证。
- **`backupStorage`**：宿主 blob 存储未移植；本 PR 与夹具只使用文件系统布局。
- **notebook `cells` 非数组/顶层非对象**：返回 `notebook.cells is not an array.` /
  `Notebook is not valid JSON.`，与 TS 对原始值赋属性的 TypeError 文案不同；夹具不覆盖。
- **file update notifier**：`didChange` / `didSave` 挂点仍是 `ToolRuntimeContext`
  的 opaque 端口，本 PR 未接；写入/回滚的文件与快照语义不受影响。
- **`onSnapshotRecorded`**：本实现为同步回调（返回 `Result`，失败向上抛）；TS 允许
  async sink 并 await。持久化适配器接入时需要同步写或另加异步桥。
- **`read_file` 的 BOM 快照**：TS 全量读记录的是去 BOM 内容哈希，而写入侧
  `readTextFile` 保留 BOM；夹具 `edit-bom-crlf` 用 offset/limit 读规避该基线行为，
  另有 `write-bom-crlf-create` 直接创建 BOM 文件。
