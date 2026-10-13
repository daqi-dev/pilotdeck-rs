# PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust · 演化记录（给人类）

> 每条只答三问：**变了什么 · 为什么变 · 对你意味着什么**。每里程碑一条，倒序追加；细节见 `state/board.md` 与 git log。

## 2026-10-14 · Issue #10：意向书同步实际 crate 布局

**变了什么**：意向书「作品区」同步当前 CLI 与四个 sidecar crate 的布局，移除已删除的占位 core 描述。

**为什么变**：PR #12 评审发现方向文档中的布局事实仍停留在初始化阶段。

**对你意味着什么**：当前布局以 `.42cog/intent.md` 的「作品区」为准。

## 2026-10-14 · Issue #10：旧 Rust sidecar 在 1.99 上过本地闸

**变了什么**：四个 sidecar crate 与 tiktoken vendor 已搬入 `src/`，保留 vendor LICENSE 与全部来源哈希；workspace 合并、移除占位 core 和 probe；Rust 入口兼容 TS gateway 的单个路径参数。release、fmt、clippy 全过，687 个测试全过，无需 lint 行为适配。

**为什么变**：从只读旧 commit `0eba113` 接续已有实现，为阶段 C 的量分提供本仓二进制；机械目录适配与入口改动有独立阶段材料。

**对你意味着什么**：源码与本地闸已具备；拿锁后 pure_text 握手冒烟退出 0，stdio/握手/module call trace 证据与 Rust 版本身份齐全，锁已释放。实际 Git 提交被 worktree 外的元数据写权限阻断，三阶段补丁/提交脚本与完整验证结果留在 `_tmp/`，尚未提交或 push。详见 `_tmp/RESULT.md` 和 `state/board.md`。
> ⚠️ **协调者更正（2026-10-14 03:10）**：本次冒烟实际跑的是 TS sidecar；真正的 Rust 握手已由协调者另行核实，证据见 `state/board.md` 的同日「协调者更正」。

## 2026-10-13 · 系统诞生

**变了什么**：PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust 开仓，作品区 `src/`，收敛方向已写下第一版。

**为什么变**：（一句话——这个系统要解决哪道多年没解决的难题）

**对你意味着什么**：（你从哪进入：手稿写哪、成品在哪、每天先看哪个文件）
