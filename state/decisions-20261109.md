# 2026-11-09 决策

## CLI Always-On 原生命令 cwd（AI 代批，待人复核）

- 卡点：mbp2 r103 的真实 snapshot/git-worktree 操作均在 spawn 时返回 ENOTDIR；Darwin 不能把 `/dev/fd/<目录fd>` 当进程 cwd。
- 受影响：Always-On 工作区命令；不改变用户会话的普通 bash，不动冻结 child-signals/unsafe 边界。
- 选择：命令使用已核验的 canonical 实际 cwd；spawn 前后与持有目录 fd 核对 dev/inode，保留复制和持久化的 NOFOLLOW 检查。可撤回，固定 TS 也使用路径式 cwd。
- 未选择：独立 fchdir/exec helper 需要新增进程协议和入口；本轮先完成旧 CLI 功能。
- 限定：此方案不声称排除同用户并发 rename 与 spawn 之间的窄窗，不能作为 sandbox 隔离证明。
- 原则与断言：平台不支持的 fd 路径不能伪装成功；用实际隔离 Git worktree、snapshot、跨仓拒绝及持久 cycle 重开测试判定，保留原 r103 失败证据。
