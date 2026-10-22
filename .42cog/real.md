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

## 模型通道配置（2026-10-22 登记）

**现状**：模型名、网关 URL、密钥变量散在 8 处（4 个仓库内脚本 + 2 个仓外配置 + 2 份文档），各改各的，没有集中控制点。

**当前会话已走 llm-center**：环境变量 `ANTHROPIC_BASE_URL=https://llm-center.modelbest.co`，GLM、DeepSeek、Claude 通道都经过这个网关。**codex 例外**：虽然 `~/.codex/config.toml` 定义了指向 llm-center 的 provider，但实测 `provider: openai`，`gpt-6.1-sol` 来自 `openai-bundled` marketplace，走的是 OpenAI app 后端，不是 llm-center。

详细清单与行号：`_tmp/20261022-model-channels-inventory/`（不入库）
- `channels.json`：结构化清单（通道名、模型、基址、密钥变量）
- `locations.md`：每个配置的文件路径与行号
- `README.md`：清单用途与维护约定

**集中化方向**：新建 `scripts/model-channels.json`，仓库内 4 个脚本从它读取。仓外配置（codex、opencode）只登记期望值，用验证脚本比对。这属于协作工具改动，按项目规则只在挡住产品开发时才优先做。

- 合规 / 保密 / 隐私：
- 不可对外说的：
- 必须保住的最低标准（＝验证红线）：见 `intent.md` 的「作品区」小节

## 资源现实

- 模型 / 账号 / 额度：
- 硬件 / 网络：
- 人手：谁真的有时间参与，各能投入多少

## 使用者现实

**给谁用，他们真实是什么条件**——这一节最常被跳过，也最容易让系统白做。

- 他们手上有什么工具、什么水平：
- 一次能记住几件事（认知负荷上限）：
- 在什么环境里用（系统、设备、有没有网）：

</real>

---

**任何一条变了，回来改。** 现实变了而这份没变，后面所有判断都是按旧世界做的。
