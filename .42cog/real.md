# PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust — 现实约束

<meta>
  <document-id>pdrs-real</document-id>
  <version>1.0.0</version>
  <project>PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust</project>
  <type>Reality Constraints</type>
  <created>2026-10-13</created>
</meta>

## Document Purpose

这个系统**改不了的那些条件**。

决策前置化的载体：**开工前把需要你拍板的现实一次性定死，干活中不再反复问你。**
这里写的是**现实**（就是这样），不是**规矩**（应该这样）——规矩在 `CLAUDE.md`。

---

<real>

## 日程

- 截止日期 / 交付节奏：
- 一次交付的形态是什么：
- 谁在等这个东西：

## 底线红线

**越过就出事的那几条**，不是「最好不要」，是「不可以」。

- 合规 / 保密 / 隐私：
- 不可对外说的：
- 必须保住的最低标准（＝验证红线）：见 `intent.md` 的「作品区」小节

## 资源现实

- 模型 / 账号 / 额度：
  - 优先用两家（2026-10-22 人定）：GLM `glm-5.3-mb`、DeepSeek `deepseek-v4.1-flash-mb`，都经 llm-center（`https://llm-center.modelbest.co`），密钥在 `.env.local` 的 `ZAI_API_KEY` / `DEEPSEEK_API_KEY`。实现默认 DeepSeek，评审默认 GLM + DeepSeek；作者是其中一家时，第二家评审才由 codex 补位。codex（`gpt-6.1-sol`）走 OpenAI 自家后端，不经 llm-center。
  - Grok Bot（经 Cursor API，「代码分析」bot，`scripts/grokbot-call.py`）2026-10-19 人批进入评审谱系：只评审、不在本机写代码；材料会发到 Cursor 云端。DeepSeek 单 key 限 50 RPM，只够 2 个 dsh 同跑，所以 DeepSeek 主要用于实现，评审默认 GLM + Grok，codex 补位。
  - 模型名必须带 `-mb`。GLM 跑在 `claude -p` 里时，stderr 的 `unrecognized_model` 是上下文窗口提示，不是失败。
  - 怎么调、已知坑：正本是 `state/memory/20261015-model-runners.md` 的 2026-10-22 段。
  - 详细清单与行号：`_tmp/20261022-model-channels-inventory/`（不入库）
  - `channels.json`：结构化清单（通道名、模型、基址、密钥变量）
  - `locations.md`：每个配置的文件路径与行号
  - `README.md`：清单用途与维护约定
  - 本机协调工具（10-31 人认可）：在仓库外 `~/ws/pdrs-ops/`（本地 git，不入库、不评审，只做编排不做判定）；入口与当前版本以其 `registry.tsv` 为准。宿主 Node 基线：本机 homebrew node@22（ICU 78.2 / libuv 1.52.1），cb8 / mbp2 / macmini 已从源码对齐（`state/decisions-20261031-node-baseline.md`）。
  - 2026-10-31 现状：Codex 额度用尽（11-04 恢复）；实现以 GLM 为主，评审 opus + DeepSeek。GLM / DeepSeek 的 llm-center key 已放 cb8 / mbp2 / macmini（人批 #1191，DeepSeek 可高并发），实现默认派远端。
- 硬件 / 网络：
- 人手：Claude 协调，尽量把活派给其他智能体，智能体可以支持并发。

## 使用者现实

**给谁用，他们真实是什么条件**——这一节最常被跳过，也最容易让系统白做。

- 他们手上有什么工具、什么水平：
- 一次能记住几件事（认知负荷上限）：
- 在什么环境里用（系统、设备、有没有网）：

</real>

---

**任何一条变了，回来改。** 现实变了而这份没变，后面所有判断都是按旧世界做的。
