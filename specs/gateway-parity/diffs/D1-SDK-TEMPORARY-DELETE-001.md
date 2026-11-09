---
id: D1-SDK-TEMPORARY-DELETE-001
---

## 类别

bug 修复：重复临时会话删除、SDK 状态清理失败重试不能删除同名持久原件。有意改进：保留删除身份需资源上界；达到上界拒绝新身份，不淘汰已有保护。

## 基线依据

固定 TS resources/PilotDeck（cc7c4e65）`src/gateway/client/InProcessGateway.ts:2229` 先调用 deleteEphemeralSession，返回 false 才落持久 deleteSession；`src/cli/ProjectRuntimeRegistry.ts:871–915` 成功删除临时目录后移除 map 条目并清 SDK 状态，没有已删除身份，重复请求会 false。`src/gateway/SessionRouter.ts:327–338` 普通 close 只传在飞 eviction 自己的结果；SDK 清理不属于该结果。这是源码实证，未新增实际 TS 执行金样；不把旧缺陷宣称正确行为。

## Rust 行为

`native_process/sdk_ephemeral.rs` 保留已删除临时身份；delete 命中该身份仍返回 true，重试 SDK clear，绝不落同名持久删除。只有同 key 的新 durable RecordingPlan.open 真正成功后，清除旧删除身份；新临时 acquire 成功也替换旧身份。live roots 加 deleted identities 总数最多 4096，所有 retained key 总 UTF-16 单元最多 1048576；恰好上界可接受，新增超界以 `SDK temporary session identity budget exceeded.` 拒绝，不新增目录，不逐出旧身份。计数与 UTF-16 单元分别计算，lone surrogate 原样计数。

`runtime/session/owner.rs` native close_then 保留 full cleanup completion 与 same-key publication ticket，但 TS closings 只承载 eviction completion。必须先取得既有 close 的真实成功结果才可清临时目录；普通 close 不继承 SDK callback 错误。public waiter 取消不取消 owned cleanup，shutdown 仍等完整 pending。

## 范围

仅本进程拥有的 SDK persistSession:false 临时存储与 native 删除消费者，不追溯跨进程 tombstone，不改普通持久删除的 not-found 合同。身份解除点是成功打开新 durable recording（实际文件锁/读取完成），并非保证新 host 后续已成功发布；打开失败不会清身份。该 wrapper 只在同 key owner publication ticket 内调用。4096 身份及 1048576 UTF-16 总单元为本轮可撤回资源选择，不是 TS 阈值；不会静默回收或错删原件。

## 机器判据

`native_process::methods::session_dir::sdk_delete_concurrency_tests` 新增真实 eviction-failure/delete、普通 close 与 callback error 隔离、串行重复/clear-failure retry、实际并发双 delete、真实 durable RecordingPlan reopen 后可复用的消费者测试。原取消 waiter/同 key create 两格由实际 serial ticket watch 屏障断言替换 60ms 负向窗口。`native_process::sdk_ephemeral::identity_budget_tests` 注入已拥有身份验证精确 count/UTF-16 阈值，不实际创建 4096 个目录；原私有 temp 与失败 drain 回归保留。此候选尚未运行 Rust，需根代理在 mbp2 执行后填原始日志，不能以待跑测试称 PASS。

## 批准记录

未批准，非已接受差异；不增加判分豁免，不修改 accepted-diffs.md、冻结装置或阈值。2026-11-09 根代理同意 4096 总状态保护方案，并要求本条新增登记；这是 AI 可撤回实现选择，待人复核。1048576 UTF-16 retained key 总预算一并列为 AI 代批待复核，不能当人批。旧原件保留优先于释放保护后的新请求成功。
