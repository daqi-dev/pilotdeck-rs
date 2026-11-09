---
id: A8-NATIVE-SNAPSHOT-CAPABILITY-001-R3
original: A8-NATIVE-SNAPSHOT-CAPABILITY-001
---

## 类别

更正 R2 对原生 snapshot marker 读取错误的过宽描述；不改 R2 原字节、实现或冻结判分。

## 基线依据

固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 的 `src/pilot/paths.ts::resolveProjectStorageId`，Node 22.22.0 / ICU 78.2 直接实测：有效 marker 返回登记 ID，无效 UTF-8 不匹配及 chmod 000 不可读时回退 legacy slug，FIFO 在开始调用后三秒内未返回并由监督进程回收。四格原件与源码 SHA 见主仓 `_tmp/20261108-cli-completion/ts-storage-id/`（oracle SHA256 `88a9c508cdb520a7457eb39db040198da6b17de40a246b730111d30e20185b57`）。有限时间观察不证明永久挂起。

## Rust 行为

R2 的“特殊文件、读失败或预算超限均返回 Unsafe”不准确。`HistoryMarkerFiles::read` 成功打开后同 fd 检查发现非普通文件、单文件超限，或 `BoundedSessionMarkers` 聚合/目录项预算超限时置拒绝标志，native storage-id helper 返回 `Unsafe project marker.`；FIFO 的非阻塞实际拒绝见 r80 提交 FIFO 输入探针。相反，打开、fd metadata 或读取直接返回的普通 IO 错误本身不置拒绝标志，`Metadata::marker` 沿既有查找逻辑忽略该 marker，继续查找匹配项或最终回退 legacy slug，不能笼统描述为全部 fail-closed。目录扫描直接返回的普通 IO 错误同样不会自动置拒绝标志，Metadata 沿既有空目录回退处理。非普通文件能否打开取决于文件类型和平台，不能保证所有特殊文件都会到达类型检查。

## 范围

仅更正 native storage-id 查询的错误与回退表述，不改变 TS、旧 R1/R2、非 native `SnapshotProjects`、项目身份或快照格式。同步 submit trait 及普通磁盘 IO 的调用线程限制仍按 R2；不保证故障文件系统 IO 可中断或外部 marker 变更的完整事务隔离。

## 机器判据

r80 真宿主普通 marker 的首次 submit 完成，并在 `000-fixture` 记录 pre-user manifest；chmod 000（`/private/tmp/pdmu202611090514/`）和 Unix 域 socket（`/private/tmp/pdms202611090513/`）下另一个 submit 的 pre-user manifest 使用各自项目的 legacy slug。前者变更前后 manifest SHA256 为 `73f6113eafa39e06e781e930efb43cab835070691d3220c60893eb1dd468ece9` / `bdbaae756f11af087f23d07dc81114705997a5582fa78ead000ba09acbceca68`；后者为 `4f354414b6626298c915dd7d1fbd5bb19d133b4a90fcfa6531cc1692f128d7c0` / `e9dd4a744a21e30774ef77ee96623ed8b6b95fee49da49bf24403a7695b18866`。异常 marker 后的 execute 因独立 TCP requestId 问题被 sidecar 拒绝，故这些 manifest 只证明提交前 workspace-id 回退，不声称后续轮次完成。r80 control/sidecar SHA 分别为 `937d2cc6120567d3e2fe9bc61f8982e1d1eb17c78bcc5868694d1e1679b9a2ef` / `25100f005efefdd484a5c876128ae25a16064a26813ee0ef080bb15c4955d0b0`；原始 wire、返回和进程清理保存在上述独占目录。

## 批准记录

**AI 代批，待人复核；未批准，非已接受差异，无冻结判分豁免。** 本条为事实更正，不改历史 accepted-diffs、R1、R2 或冻结尺子；阶段验收仍需独立审阅和人签字。
