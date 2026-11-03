---
id: A8-NATIVE-SNAPSHOT-CAPABILITY-001-R1
---

## 更正内容

#1323 为原条目「生产接线另由跟进单处理」的跟进单。本更正记录生产接线完成后原条目范围的语义变化：原条目中「生产入口 U3 snapshot 恒禁用」与「生产接线另由跟进单处理」两项已失效，其余机器判据保持不变。

## 变化

- `native_process.rs::Options` 新增 `storage_root: Option<String>` 字段，接受 `--storage-root <path>` CLI 参数（可选；缺省等价于原禁用行为）。
- `Options::environment()` 在 `storage_root` 非空时注入 `PILOTDECK_STORAGE_ROOT`；为 `None` 时显式删除 `PILOTDECK_STORAGE_ROOT` / `PILOTDECK_SNAPSHOT_ROOT`，禁用路径与原条目一致。
- `runtime/project/persistence.rs::WorkspaceSnapshots.recorder` 改为 `Arc<ContentAddressedWorkspaceSnapshotRecorder>`，允许 `assembly.rs` 在启动时提取并保持跨轮次共享。
- `native_process/assembly.rs` 在 `snapshot_enabled` 为真时额外提取 `snapshot_recorder: Arc<ContentAddressedWorkspaceSnapshotRecorder>`，与 `snapshot_enabled` 共用相同的项目 lease 与错误处理模式（acquire 失败时先 dispose 再返回错误）。
- `native_process/services.rs::Services` 新增 `snapshot_recorder: Option<Arc<ContentAddressedWorkspaceSnapshotRecorder>>` 字段；`SubmitHostServices::snapshots()` 在字段非空时返回 `Some(Arc<NativeSubmitSnapshots>)`，生产 `snapshots_required()` 仍返回 `false`（TS 不要求）。

## 不变

- `snapshot_enabled` 缓存与 `describe_server` 的三项能力过滤逻辑不变。
- `validate_environment` 禁止继承非空 `PILOTDECK_STORAGE_ROOT` 的检查不变（`--storage-root` 通过 `environment()` 注入，不经继承检查）。
- TS 的三个 snapshot 方法、management 路由、SnapshotGateway、快照格式、恢复目标准入均不变。
- 原条目的机器判据（`production_entry_snapshot_capabilities_are_disabled` 等）覆盖 `--storage-root` 缺省情形，仍有效。

## 批准记录

AI 代批，待人复核（#1323；前置于 A8.6）。
