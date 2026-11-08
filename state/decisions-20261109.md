# 2026-11-09 决策

## CLI Always-On 原生命令 cwd（AI 代批，待人复核）

- 卡点：mbp2 r103 的真实 snapshot/git-worktree 操作均在 spawn 时返回 ENOTDIR；Darwin 不能把 `/dev/fd/<目录fd>` 当进程 cwd。
- 受影响：Always-On 工作区命令；不改变用户会话的普通 bash，不动冻结 child-signals/unsafe 边界。
- 选择：命令使用已核验的 canonical 实际 cwd；spawn 前后与持有目录 fd 核对 dev/inode，保留复制和持久化的 NOFOLLOW 检查。可撤回，固定 TS 也使用路径式 cwd。
- 未选择：独立 fchdir/exec helper 需要新增进程协议和入口；本轮先完成旧 CLI 功能。
- 限定：此方案不声称排除同用户并发 rename 与 spawn 之间的窄窗，不能作为 sandbox 隔离证明。
- 原则与断言：平台不支持的 fd 路径不能伪装成功；用实际隔离 Git worktree、snapshot、跨仓拒绝及持久 cycle 重开测试判定，保留原 r103 失败证据。

## CLI SDK 恢复身份与结构化输出（AI 代批，待人复核）

- 卡点：固定 TS 的两个不同答案并发恢复会共享一次写入却都返回成功；旧 outputFormat 类型注释承诺 scoped structured_output schema，但实际 compose 仅设置 stopOnStructuredOutput。
- 选择：恢复只确认真正落盘的同一答案，异答返回未投递；同答重试可补完已有部分持久记录。新增独立 dialog-start gate，覆盖 live journal→bus 注册及 cold 判定，避免抢占正在创建的交互，不重入会话长期持有的 transcript fence。
- 选择：outputFormat 使用已有 structured_output 工具的会话局部 schema，由既有 ToolRuntime 输入验证与现成 AgentLoop 停止机制执行。有效 schema/有效答案维持旧调用形式，旧实现漏校验的答案将明确拒绝。
- 依据与边界：用户已允许功能完整而非一比一重构；两项均为可撤回产品行为，不改 frozen 标准/旧账本，不冒称与旧实际缺陷逐字一致。恢复原始 TS race、schema helper来源及采样见主仓 `_tmp/20261108-cli-completion/` 下 `user-dialog-recovery-flight-formatted`、`sdk-deferred-oracle`，runtime断言随产品测试。
- 原则：成功回执必须对应实际落盘身份；机器输出合同必须在同一实际执行通路验证。
