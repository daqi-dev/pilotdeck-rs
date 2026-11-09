---
id: A8-NATIVE-SNAPSHOT-CAPABILITY-001-R2
original: A8-NATIVE-SNAPSHOT-CAPABILITY-001
---

## 类别

更正生产接线描述并登记原生读取边界。R1 所述 submit 与管理路由共同使用无界 `SnapshotProjects::workspace_id` 已失效；两路改用同一个有界 storage-id helper。R1 原字节保留，本更正不改变冻结判分标准。

## 基线依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85、Node v22.22.0 / ICU 78.2，直接调用 `src/pilot/paths.ts::resolveProjectStorageId`（源码 SHA256 `6766426f9c8310e8a17fb0338319a4c4a1574c54a0b66690cdf609f2846c5a2a`）。2026-11-09 隔离实测四格：有效匹配 `.cwd` 返回注册 ID；无效 UTF-8 和 chmod 000 的不可读 marker 回退到 legacy slug；FIFO 在输出 entered 后三秒未返回，监督进程终止并回收自有进程组。后三秒观察不是“永久挂起”的实测证明。原件在主仓 `_tmp/20261108-cli-completion/ts-storage-id/`，oracle SHA256 `88a9c508cdb520a7457eb39db040198da6b17de40a246b730111d30e20185b57`。

TS `InProcessGateway.ts` 的快照提交分支读取 `snapshotForRuntimeReload().cwd`；普通 `AgentSession.snapshot()` 不含 cwd。本轮不扩展公开 session snapshot 的 JSON 字段。

## Rust 行为

`native_process/management.rs::bounded_snapshot_workspace_id` 使用 `Metadata<BoundedSessionMarkers>`，执行 storage-id 查询后检查 `was_refused`。管理解析器与 `Services::workspace_id` 均调用此 helper；有效普通 marker 的注册 ID 优先与无匹配时的 legacy slug 保持原语义。特殊文件、读失败或预算超限返回 `Unsafe project marker.`，不把安全读取失败当作可接受的 legacy 回退。边界由 `methods/session_dir.rs::BoundedSessionMarkers` 实现，读取使用同一打开描述符、非阻塞普通文件检查与现有每文件/聚合/目录项预算。

提交内部经 `LiveSessionHandle::workspace_cwd` 获取工作目录。生产 `OwnedSession` 从已准入的 `FactoryStorage.cwd` 提供路径，首次 prepare 前即可读取且不做磁盘 IO；旧 adapter 的默认实现继续读取其自身 snapshot.cwd。缺失或空值仍返回原错误，公开 snapshot 字节保持不变。

## 范围

仅原生 isolated 宿主和启用 `--storage-root` 的提交/管理接线；无 storage-root 的禁用合同不变。非 native `SnapshotProjects` 的原始行为、快照文件格式和恢复目标准入不改。同步 submit trait 未异步化：普通磁盘 IO 仍会占用调用线程；预算不代表故障文件系统内核 IO 可中断，也不是完整目录事务隔离。memory 的同类 marker 读取同样使用原生有限 reader，不能据此宣称与 TS 在所有异常文件输入下完全相同。

## 机器判据

`native_submit_workspace_fifo_is_bounded_subprocess` 真实调用 Services，核有效 marker ID 与 FIFO 拒绝；`native_snapshot_marker_fifo_is_bounded_subprocess` 核管理解析。`production_workspace_cwd_is_storage_owned_without_extending_public_snapshot` 通过生产 factory 核首次提交前 cwd、reload cwd 一致、公开 snapshot 不变及资源回收。既有 `submit_snapshot_wiring_matches_management_workspace_id`、`snapshot_success_matches_ts` 与 submit 场景继续回归。

mbp2 r78 真宿主 FIFO 提交会阻塞独立 describe 的失败原件保留；修复轮 r80 的编译、上述正控、submit、management 和 FIFO 测试均通过。完整真实 turn→snapshot list/get/restore 与最终安装证据以主仓 `_tmp/20261108-cli-completion/` 的不可变二进制/原始记录为准，未完成的运行不能凭本文计为通过。

## 批准记录

**AI 代批，待人复核；未批准，非已接受差异，无冻结判分豁免。** 属当前 CLI/机器能力范围内可撤回的原生安全边界修复及生产接线补齐；人复核前不以此标记阶段 D 或 A8 验收完成。历史 accepted-diffs、R1 和冻结尺子不变。
