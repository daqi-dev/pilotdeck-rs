---
id: A8-NATIVE-SNAPSHOT-CAPABILITY-001
---

## 类别

原生隔离宿主的能力声明收窄：不声明未启用的 snapshot provider；同时明确既有入口拒绝非空（包括纯空白）继承配置的差异。不放宽判分。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0 / ICU 78.2。`src/gateway/client/InProcessGateway.ts:2111` 按三个 callback 是否存在声明 snapshot；`createLocalGateway` 即使未创建 provider 仍传入 callback。

真实调用证据：`src/pilotdeck-gateway/tests/fixtures/native-management/oracle.json` 的 `contract.emptyStorage`、`contract.missingStorage`。两格的 config 均为 null，list/get/restore 均报 `CAPABILITY_UNAVAILABLE` / `Gateway snapshot provider is unavailable.`，但 capabilities 均包含三个 snapshot 方法；同目录 generate.mts 可重生成。

纯空白 storage 的固定 Node 实测：`_tmp/20261026-issue-1231/fix-r3/storage-probe-tsx.log`（复跑 `fix-r2/storage-probe.mts`）。TS 接受 `PILOTDECK_STORAGE_ROOT="   "` 启动，config null、三个请求同样 unavailable，describe 仍广告 snapshot。此格不与 Rust 的入口拒绝混称一致。

## Rust 行为

`src/pilotdeck-gateway/src/native_process.rs` 的 `Options::parse` 不提供 storage/snapshot 参数；`run` 先调用 `validate_environment`，拒绝继承非空 `PILOTDECK_STORAGE_ROOT` / `PILOTDECK_SNAPSHOT_ROOT`，包括纯空白。`assembly.rs` 不为缺省或空值补默认目录。因此本包生产入口 U3 snapshot 恒禁用：describe 精确 10 项，snapshot_list/get/restore 返回上述完整 CAPABILITY_UNAVAILABLE 错误。

配置由 persistence stage 解析并构造 provider；`assembly.rs` 从该实际 provider 的存在性缓存 `Requests.snapshot_enabled`。`requests.rs::describe_server` 使用缓存和显式的三个能力名过滤，不再次解析配置。13 项与 snapshot 成功路径只在测试经 `assembly::start` 显式配置出现。

## 范围

本包 native isolated 生产入口和对应测试装配边界。生产入口不能通过继承环境启用 snapshot；纯空白的有意入口拒绝沿用 #1071 隔离规则。仅测试绕过该继承检查、向 `assembly::start` 传非空隔离 storage 时声明 13 项并验证 snapshot 成功路径。生产接线另由跟进单处理，前置于 A8 U3 生产入口验收；当前不能据测试成功声称生产可用或阶段完成。不改变 TS、proxy 分发、快照格式、恢复目标准入或生命周期。

## 机器判据

`native_process::management_tests::production_entry_snapshot_capabilities_are_disabled` 明确走 `Options::parse` + `validate_environment`：两 key 缺省/空串四种组合经真实连接精确比较 10 项，两 key 非空/纯空白均拒绝。`production_host_management_and_restart` 只在测试显式非空配置下比较 13 项；`missing_storage_environment_disables_snapshots`、`empty_storage_environment_disables_snapshots` 逐项比较三请求错误及 10 项能力。各精确集合均交叉校验为 TS contract.capabilities 子集。

负控：`snapshot-state-disabled/enabled` 绑定实际装配状态与 13/10 项断言；`production-*-authority-accepted` 绑定入口拒绝；三个 `snapshot-*-filter-omitted` 绑定生产 10 项；`wired-capabilities-outside-ts` 绑定 TS 子集。既有 `*-storage-defaulted` / `*-storage-advertised`、多报与重复变异保留。`snapshot-storage-omitted` 专属绑定 `#1231 snapshot route with explicit storage`，不接受其他管理路由失败。

`native_process::management_tests::regenerates_identically` 为 ignored 逐字节重生成守卫。负控按 negative_controls.py 先编译、精确测试/专属断言判杀、恢复后正控，汇总所有项；未提交修复树的全量由协调者在提交后宿主运行。

## 批准记录

2026-11-01，AI 代批，待人复核；未取得持久人签记录，非已接受差异，不自动增加判分豁免。依据：`_tmp/20261026-issue-1231/fix-r2/task.md:13` 协调者补充要求缺省/空串禁用；`_tmp/20261026-issue-1231/fix-r3/task.md`「协调者补充」明确「本包不新增 CLI 参数」「本包生产入口 U3 snapshot 恒禁用」「13 项与 snapshot 成功路径只在测试经 assembly::start 显式配置出现；生产接线另由跟进单处理」。本条不再将协调者代批误写成人批，后续人复核应以持久决策指针登记，不以本轮实现或测试替代实效签字。
