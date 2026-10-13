# PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust · 状态板（给 AI · 跨会话唯一接续点）

> 开工先读 `CLAUDE.md` + **`.42cog/` 四份**（`intent` · `real` · `cog` · `meta`）+ 本文件 + `state/memory/MEMORY.md`。
> **非轮规则：每轮有效工作必更新本文件**（倒序追加，新的在上，带日期与 commit hash）。
> 每完成一个可命名的逻辑单元存一次；破坏性操作之前也存一次。这是给你自己留的后路，不是给别人看的历史。

> ## 2026-10-14 第三轮 · 公开 + 分支保护 + 撞闸 + 评审配齐（main @ cfd7841）
> **做了**：重写 PR #1 合并提交作者（4172dc2 → 10442bd，强推 main）· 仓库改**公开**（人定，接受 PR #1 记录里仍可见真实邮箱）· PR #2 合并：AGPL-3.0 `LICENSE`（与 gnu.org sha256 一致）、README 来源说明、`docs/research/001`、intent 两条不做什么 · `main` 分支保护：必需检查 `check`（strict）、管理员不例外、禁强推/删除 · **直推 main 被拒**（GH006 “Required status check "check" is expected”）· opencode 2.0.25 装好，`.claude/dev-launch.review.md` 配 codex(GPT) + opencode(glm-5.3)，两路只读冒烟通过 · 装齐 aias-meta-research/proto/evolve 与 aias-dev 四件。
> **人定**：渐进策略采用（Rust 网关在前、未实现的转发给 TS），第 6 步去验证；第二谱系 = opencode + GLM。
> **方法缺口（新增）**：⑤ `--rebase` 合并不能避免邮箱暴露（提交者换成主邮箱），CLAUDE.md §五已更正 ⑥ `dev-launch` §8 写「service 人工合并，绝不自动 merge」，与 CLAUDE.md 第 14 条放权冲突，发车前要人定。
> **下一步**：第 6 步调研。

> ## 2026-10-13 第二轮 · 初始化第三步 + 第一个 PR（main @ 4172dc2）
> **做了**：origin = `git@github.com:zhengdaqi/pilotdeck-rs.git`（私有）· PR #1 合并：`rust-toolchain.toml` 锁 1.99 · `src/` workspace（`pilotdeck-core` 库 + `pilotdeck-cli` 二进制 `pilotdeck`）· CI `check` 跑 fmt/clippy/test · `check-tools.sh` 专属段（含 TS 基线要的 node 22 / pnpm）。案例调研落 `docs/research/001`（未提交，随下一个 PR）。
> **方法缺口（已记）**：① 免费账户私有仓开不了分支保护（API 403）② cargo 1.99 上空 workspace 三条检查全挂，课件「绿的空气」不成立 ③ 模板 `.gitignore` 漏 `.claude/skills/` 等 ④ 42plugin 0.4.x 更新用 `update` 子命令。
> **待人定**：分支保护怎么办 · 是否采用「Rust 网关在前、未实现的转发给 TS」的渐进策略 · 第二条评审谱系用谁。
> **下一步**：第 6 步调研——`/aias-meta-research`，把 PilotDeck@cc7c4e6 克隆进 `resources/`，证实 intent 里那条协议边界假设。

> ## 🌱 系统开仓（2026-10-13 第一轮）
> `aias-meta-init` 生成六组骨架：① `README.md` `CLAUDE.md` `.42cog/` `specs/` ② `vault/` `notes/` `resources/` ③ `skills/` `scripts/` `plugin.json` ④ `src/` ⑤ `state/` `docs/` ⑥ `_build/` `_tmp/` `_archive/`（忽略）。
> **收敛方向**（草稿，待人确认）：见 `.42cog/intent.md`——**那句话只有一份，别抄到这里来**。
> **下一步**：确认收敛方向 → `pdrs-research` 找依据、排真相源权重 → 回来改这一句。方向第一版粗是正常的，四步是循环。
