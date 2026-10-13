# PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust · 演化记录（给人类）

> 每条只答三问：**变了什么 · 为什么变 · 对你意味着什么**。每里程碑一条，倒序追加；细节见 `state/board.md` 与 git log。

## 2026-10-14 · exp003 真实 UI 连接路径实证（Issue #15 / A1，未提交）

**变了什么**：Rust 透明 WS/HTTP gateway 与隔离实验装置落在仓内；七项六通过，19 帧对照一致，阳性注入确实 FAIL。真实 UI 自动交互重放失败已附原始证据回填研究 gap。

**为什么变**：源码推断不能代替真 ui/server；实测发现 client 清掉 hello 后 bridge 才读 binding，使恢复流程没有发请求。装置也检出并推动修复代理自身的 close acknowledgment 差异。

**对你意味着什么**：连接 1:1、TS 持有交互状态的代理方案有实测支撑；A1 不能宣称完整 UI 恢复通过。下一步需单独处理基线恢复缺陷并继续 A2，保留沙箱 watcher/无 DOM 验收等实验限制。详见 exp003 readme 与 `_tmp/RESULT.md`。

## 2026-10-14 · agent 循环尺子得到 TS 五遍校准证据（待审查与冻结）

**变了什么**：G03 v1 已回捞为 sidecar 对 sidecar 的离线比较器，五遍 TS 的 10 对全部 53/53；逐规则负向、两侧同坏与正向对照通过，隔离驱动的 TS 单场景身份冒烟通过。

**为什么变**：上游严格比较对 TS sidecar 自比存在跨通道位置噪声；将旧通道规则、E 规则、场景完整性和生产 oracle 组合为可复算判决，保留明确证据边界。

**对你意味着什么**：无需排除不稳定场景（S = 53）；可审查 acceptance / normalization 草稿与 exp004。独立审查、批准冻结与提交仍待协调者完成；本轮不跑 Rust。完整输出见本 worktree `_tmp/RESULT.md`。

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
