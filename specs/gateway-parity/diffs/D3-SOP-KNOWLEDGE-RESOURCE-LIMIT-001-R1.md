---
id: D3-SOP-KNOWLEDGE-RESOURCE-LIMIT-001-R1
original: D3-SOP-KNOWLEDGE-RESOURCE-LIMIT-001
---

## 类别

更正原条目的范围说明：原 native SOP HTTP 传输还对普通 SOP 和审批认证响应施加 8 MiB 字节上限。原条目不修改；本更正不改变产品行为或冻结判分。

## 基线依据

固定 TS 基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/sop/staffdeck/StaffDeckSopClient.ts:311` 与 `StaffDeckSopDiscoveryClient.ts:89` 读取 `response.json()`，这些读取点没有 native 的 8 MiB 检查。现有真实 TS 对拍覆盖普通响应，未实测超大响应；审批认证对应 TS 的大响应行为也尚未实测，不能声称其完整等价。

## Rust 行为

`module_host/approvals/native_http.rs` 的 `HyperJsonTransport` 对单次响应体设置 8 MiB 上限。`native_process/assembly.rs` 的实际 controller 使用此 transport，故 SOP discovery、健康检查、prepare 和 submit 的响应也受此限制；`native_process/sop_control.rs` 的实际 admission 与 approver 认证同样使用该 transport。超限报明确传输错误，不把截断响应当成功。原条目所列 knowledge/authority 各自 manifest 与请求的 8 MiB、四异步槽和排队约束不变；四槽约束不因此推广到 SOP 或审批认证。

## 范围

该更正仅覆盖上面列明的 native HTTP 消费者及它们的真实字节上限，不把有限响应阈值称为进程完整内存上界。普通 skills 管理调用应采用独立 proxy 构造并保留既有 `MAX_STRING_LENGTH` 上限；该修复及大于 8 MiB 的实际 TCP 测试须经编译验证后才能宣称生效。相同 TS 合法超限响应与 Rust 的行为存在待复核分叉；不授权豁免其他差异。

## 机器判据

通过源码定位 `HyperJsonTransport` 的实际构造与 `read_body` 上限，以及 SOP controller、admission 和 approver 消费者；选测 `module_host::approvals::native_http` 的有界响应测试。skills 旧上限修复的对应测试 `native_skills_proxy_limit_tests::skills_proxy_transport_accepts_more_than_native_knowledge_limit` 使用真实 loopback HTTP 读取 8 MiB+1；knowledge 超限反控为 `runtime::sop::authority::tests::native_transport_rejects_an_oversize_successful_manifest`。这些新组合检查待协调者在受控 mbp2 上运行；不得以源码审阅代报通过。r127 SOP 真代理正控与失败原件分别在 mbp2 `/private/tmp/pdrs-sop-r127-probes-20261109/proxy-sop-only` 和 `proxy`，仅证明普通响应路径，不证明超限 TS 行为。

## 批准记录

AI 代批，待人复核；未批准、非已接受差异，不改变原条目批准状态，不自动构成判分豁免或阶段验收通过。
