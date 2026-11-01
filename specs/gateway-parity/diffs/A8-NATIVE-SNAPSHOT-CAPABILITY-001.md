---
id: A8-NATIVE-SNAPSHOT-CAPABILITY-001
---

## 类别

原生隔离宿主的能力声明收窄：不声明未启用的 snapshot provider。只登记 describe 的广告差异；snapshot 调用恢复基线错误行为，不放宽判分。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0 / ICU 78.2。`src/gateway/client/InProcessGateway.ts:2111` 按三个 callback 是否存在声明 snapshot；`createLocalGateway` 即使未创建 provider 仍传入 callback。

真实调用证据：`src/pilotdeck-gateway/tests/fixtures/native-management/oracle.json` 的 `contract.emptyStorage`、`contract.missingStorage`。两格的 config 均为 null，list/get/restore 均报 `CAPABILITY_UNAVAILABLE` / `Gateway snapshot provider is unavailable.`，但 capabilities 均包含三个 snapshot 方法；同目录 generate.mts 可重生成。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/assembly.rs` 保留原始配置，不为缺省或空值补默认目录。`requests.rs::describe_server` 复用 snapshot 配置解析器；无 provider 配置时过滤 snapshot_list/get/restore，能力由 13 项变为 10 项。调用沿既有领域组件返回与 TS 相同的完整错误。

## 范围

仅原生 isolated Requests 的 describe_server，在 PILOTDECK_STORAGE_ROOT 缺省或空串时。非空配置仍声明三个 snapshot 能力；不改变 TS、proxy 分发、快照格式、恢复目标准入或生命周期。

## 机器判据

`native_process::management_tests::production_host_management_and_restart` 精确比较非空配置的 13 项；`missing_storage_environment_disables_snapshots`、`empty_storage_environment_disables_snapshots` 经真实连接逐项比较三请求错误及 10 项能力。每格有对应 `*-storage-defaulted` 与 `*-storage-advertised` 变异；`capability-add-upload_create` 拒绝多报其他 TS 能力，`capability-duplicate-run_get` 拒绝重复声明。

`native_process::management_tests::regenerates_identically` 为 ignored 逐字节重生成守卫。负控按 negative_controls.py 先编译、精确测试/专属断言判杀、恢复后正控，汇总所有项；未提交修复树的全量由协调者在提交后宿主运行。

## 批准记录

2026-11-01 本轮 #1231 fix-r2 用户明确要求：「键缺省与空串都不声明 snapshot、调用返回 CAPABILITY_UNAVAILABLE」。本条按该授权实施；实测发现 TS describe 仍广告 snapshot，故将广告收窄如实登记（纯空白继承环境仍由既有入口规则拒绝，本轮不改）。授权不代表实效签字或阶段完成，也不增加自动判分豁免。
