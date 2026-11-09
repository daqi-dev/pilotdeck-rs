---
id: D2-NATIVE-MULTIPROJECT-SESSION-ISOLATION-001
---

## 类别

原生多项目会话及活动流隔离；可撤回的安全行为偏离。通用非 native replay / coordinator API 保持原合同。

## 基线依据

固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node 22.22 / ICU 78.2：真实独立 A/B 项目使用相同 session key，先提交 A 再显式请求 B，仍复用 A 的 factory cwd、model 与 permission cwd；两轮持久化到 A，读取 B 的 durable 为空但合并了 A 的 coordinator stream。原件在 mbp2 control-cli-20261108/_tmp/multi-project-sol-20261109/capture-switch/oracle.json，SHA256 e714990fd897849dbeff6301c7e3e7b6caf979880976840fa694518db7850453；自写 driver capture-switch.mjs SHA256 cf0c0fa8c72228a5623f6d962d9f0d180187b91abc4113e8b9da6a108626afe3。原 provider 尝试因隔离网络限制被拒，不能称真实 provider 成功。

## Rust 行为

同 session key 已由项目 A 占有时，显式指定 B 的 native 请求返回 PROJECT_SESSION_MISMATCH；未指定项目沿现有 owner 选择。活动流及历史合并按选定项目筛选 replay，跨项目、无来源 replay 显示为空；已结束但尚保留的 replay 仍携带其原项目。绑定 publisher 的旧代迟到写入、emit 与终态清理按实际 registration 身份拒绝，不影响新代，即使 runId 相同。

## 范围

仅 native 项目绑定和项目选定的请求视图；旧无绑定通用 coordinator/replay API 不变。项目隔离不等同不同认证主体的授权隔离；未扩展该 token 的权限模型。此条不改变冻结判据，也不声称正式阶段验收或全部多项目能力完成。

## 机器判据

真实 host A/B 隔离、缺省项目、相同 runId 代际交错、owner 关闭后终态保留及一般 API 正控需在当前源码指纹下执行并保留原件；本条本身不冒称测试已通过。对应原始材料、命令及实际结果由主控收于 _tmp/20261108-cli-completion/。

## 批准记录

**AI 代批，待人复核；未批准，非已接受差异，无冻结判分豁免。** 原 accepted-diffs 文件不改，仅追加本条；人签实效与正式阶段验收另行处理。
