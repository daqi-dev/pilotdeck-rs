---
id: A8-NATIVE-SNAPSHOT-CAPABILITY-001-R1
original: A8-NATIVE-SNAPSHOT-CAPABILITY-001
---

## 类别

更正（生产接线完成）：原条目两项陈述已失效——「生产入口 U3 snapshot 恒禁用」与「生产接线另由跟进单处理」。#1323 新增 `--storage-root` CLI 参数并完成 assembly/services 接线后，传入非空路径时生产入口可启用 snapshot，原两项声明不再成立。其余行为与机器判据保持不变。

## 基线依据

同原条目固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0 / ICU 78.2。TS 行为不变：`src/gateway/client/InProcessGateway.ts:2111` 按 callback 存在性声明 snapshot 能力；`oracle.json` 的 `contract.emptyStorage`、`contract.missingStorage` 格仍以 CAPABILITY_UNAVAILABLE 为基线。本更正仅修订 Rust 侧行为描述与范围声明。

## Rust 行为

`native_process.rs::Options` 新增 `storage_root: Option<String>` 字段，接受 `--storage-root <path>` CLI 参数（可选；缺省等价于原禁用行为）。`Options::environment()` 在 `storage_root` 非空时注入 `PILOTDECK_STORAGE_ROOT`；为 `None` 时显式删除 `PILOTDECK_STORAGE_ROOT` / `PILOTDECK_SNAPSHOT_ROOT`，禁用路径与原条目一致。`runtime/project/persistence.rs::WorkspaceSnapshots.recorder` 改为 `Arc<ContentAddressedWorkspaceSnapshotRecorder>`，允许 `assembly.rs` 在启动时提取并保持跨轮次共享。`native_process/assembly.rs` 在 `snapshot_enabled` 为真时额外提取 `snapshot_recorder`，与 `snapshot_enabled` 共用相同的项目 lease 与错误处理模式。`native_process/services.rs::Services` 新增 `snapshot_recorder: Option<Arc<...>>` 字段；`SubmitHostServices::snapshots()` 在字段非空时返回 `Some(Arc<NativeSubmitSnapshots>)`，生产 `snapshots_required()` 仍返回 `false`。

## 范围

本包 native isolated 生产入口及对应测试装配边界。`--storage-root` 缺省时禁用语义与原条目一致；传入非空路径时生产入口可启用 snapshot，原「恒禁用」声明不再适用。`validate_environment` 禁止继承非空 `PILOTDECK_STORAGE_ROOT` 的检查不变（`--storage-root` 通过 `environment()` 注入，不经继承检查）。TS 的三个 snapshot 方法、management 路由、SnapshotGateway、快照格式、恢复目标准入均不变。

## 机器判据

原条目判据在缺省（无 `--storage-root`）情形下仍有效：`production_entry_snapshot_capabilities_are_disabled` 走 `Options::parse` + `validate_environment`，两 key 缺省/空串四种组合精确比较 10 项；`missing_storage_environment_disables_snapshots`、`empty_storage_environment_disables_snapshots` 逐项比较三请求错误及 10 项能力。传入非空 `--storage-root` 时 `production_host_management_and_restart` 比较 13 项。负控 `missing-storage-defaulted`、`empty-storage-defaulted`、`missing-storage-advertised`、`empty-storage-advertised` 覆盖缺省/空串禁用路径；`snapshot-state-disabled/enabled` 绑定装配状态与 13/10 项断言；`production-*-authority-accepted` 绑定入口拒绝；三个 `snapshot-*-filter-omitted` 绑定生产 10 项。复算命令同原条目。

## 批准记录

AI 代批，待人复核（#1323；前置于 A8.6）。
