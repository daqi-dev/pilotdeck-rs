---
id: D3-SOP-KNOWLEDGE-RESOURCE-LIMIT-001
---

## 类别

安全 / 资源 bug 修复：为 native CLI 的外部 knowledge 查询与 SOP capability authority 请求增加可撤回的资源边界，避免并发响应无节制地占用宿主内存与网络任务。

## 基线依据

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/composition/HttpModuleClient.ts:38-58,96-140` 使用 fetch 和 response.json，并以 binding timeout（缺省 10000 ms）约束请求；这些代码不包含本条所述的 8 MiB 响应上限或四请求共享信号量。真实 TS wire 复采器 `src/pilotdeck-gateway/tests/fixtures/sop-knowledge-authority/capture-wire.mts` 和 `wire.json` 覆盖正常的 authority / knowledge manifest 与调用，不覆盖超大响应或并发阈值，不能当作该阈值的 TS 对拍。

## Rust 行为

`src/pilotdeck-gateway/src/skills/external.rs` 的 native `HyperModuleTransport::with_proxy_settings` 对 authority 与 knowledge 的 manifest 和调用响应按原始响应体字节设置 8 MiB 上限；超限仍在同一请求 deadline 内排空，完成后映射为明确的模块协议错误，迟迟不结束则按 timeout 报错。`ExternalSkillsPort::call_module` 在 manifest、网络与响应解析期间持进程共享的四个异步槽之一；等待槽的请求可取消，且按同一 binding timeout 值限制单次排队等待，超时明确报 MODULE_TIMEOUT；取消会 drop 实际 async I/O 并释放槽。既有 skills 外部调用仍用原先 transport/上限和调用路径，不受这两个新限制。

## 范围

仅 native CLI 实际装配的 external knowledge.query 与 SOP capability authority.resolve，包含各自 manifest；不调整其他外部模块、协议字段、权限规则、判分或 TS 的行为。大于 8 MiB 的合法响应在 Rust 被拒绝；超过四个并发请求的后续调用最多按 binding timeout 排队，随后按各次 manifest/HTTP 的原有 timeout 执行；因此总耗时不以单个 timeout 为上界，排队超时不发实际 HTTP 请求。该限制是资源上限，并非完整进程内存上界，也不声称 TS 大响应或并发行为已有对拍。

## 机器判据

`runtime::sop::authority::tests::native_transport_rejects_an_oversize_successful_manifest` 使用真实本地 TCP HTTP 响应，验证超过 8 MiB 的本可解析 manifest 失败；`abort_drops_network_future_and_returns_shared_slot` 验证四槽占满时第五个请求按配置超时而未进入 transport，另一等待请求在取消一个实际在途 future 后进入。`authority_and_knowledge_wire_match_fixed_ts` 与上述 `wire.json` 验证普通请求的真实 TS envelope / credential / authority context；`fixed_ts_projection_and_input_cases` 验证投影与输入 13 格。排队超时增强测试待下一轮运行；r125 mbp2 原始 `sop.log` 为 13 PASS，来源 `_tmp/20261108-cli-completion/verify-r125/`（远端 `_tmp/20261109-completion-r125/sop.log`）；选测命令 `cargo test -p pilotdeck-gateway --lib runtime::sop::authority::tests -- --nocapture`，受项目 cargo 名额规则约束。取消/超时的真实 CLI + sidecar + 假 provider 原件见 `/private/tmp/pdrs-sop-knowledge-e2e-20261109-b/{timeout,cancel}-bounded/`；cancel 最终错误文本仍另行与 TS 核对，不能借此声称完整终态一致。

## 批准记录

2026-11-09 用户授权在完整 CLI 范围内实现真实 authority 与 knowledge 能力，要求明确超时和取消；协调者采用 native 8 MiB / 四异步槽方案。可撤回资源取舍由 AI 代批，待人复核；本条尚未经人批准，属于未批准、非已接受差异，不自动增加判分豁免或宣称阶段验收通过。
