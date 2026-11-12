# 开发约束

本目录为用户授权接续开发的完整 Git 工作树，分支 `codex/gateway-wave1-completion`。起点为 Rust `7d7c0ab2dad32b4c71bb2ed465147cdff179f374`，已核验并接收先前 `gateway-dev` 快照的 45 个交付文件。摘取来源与补丁原件仍保存在 `/Users/da/ws/pilotdeck-rs-codex/gateway-dev`；只读源 `/Users/da/ws/pilotdeck-rs/_build/wt-gateway-integration` 不作本轮编辑目标。当前用户指令优先于历史队列，不接回旧看板循环。

- 本机禁止构建、测试及语法检查；所有构建/测试只在获准的 cb8 环境执行。静态阅读、编辑、复制、哈希与清单核验可以在本机进行。
- 先前摘取环境的 SSH 限制已经改变；用户此前明确授权 `ssh cb8`，本轮已验证可用。使用 cb8 自有工作树、独立 target 和现行 cargo 资源闸门；不绕过沙箱或权限控制。A7 先前被拒绝的范围仍保持 NOT_RUN，不重试、不改写绕过。
- 不改源目录、其他工作树或用户运行服务；不碰 27a0、~/.pilotdeck、hi.md、vault/raw、他人 notes；未知改动先保留。密钥与真实用户数据不得进入快照。
- 主智能体独占共享注册、FROZEN 与最终集成。冻结规则和差异追加规则保持原义，不为变绿降低标准；未跑、阻塞、失败、通过分别如实记录。
- TS 固定基线为 OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85；按真实同输入行为和已批准差异验收。谁写谁不审，完成须当前版本适用回归、独立双审及真实证据。

原摘取子任务的「不 SSH、不建 worktree、不初始化 Git、不提交」仅适用于当时的复制职责。本轮已建立真实集成提交并同步到 cb8；构建与证据必须记录实际 HEAD，旧快照哈希或其他工作树的二进制不能替代。推送前必须 gitleaks，PR 保持 Draft；未完成正式验收不得切换用户服务。

适用源规则指针（只读）：/Users/da/ws/pilotdeck-rs/_build/wt-gateway-integration/AGENTS.md；同源 .42cog/intent.md。历史自动提交、远端执行、看板更新与 worktree 默认流程不覆盖本轮明确指令。
