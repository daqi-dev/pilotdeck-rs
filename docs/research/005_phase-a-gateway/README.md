# 005 · 阶段 A：用 Rust 替换 18789 gateway

> 2026-10-15 起。承接调研 002（边界选型）与 exp002（连接级状态盘点），只回答阶段 A 自己的落地问题。

## 这次要决定的

Rust gateway 以什么形态站上 18789、按什么粒度从 TS 手里接 method、拿什么装置证明「接过去之后行为没变」。

## 三份文件，按这个顺序走

| # | 文件 | 干什么 | 状态 |
|---|---|---|---|
| 1 | [must-know.md](must-know.md) | **决策点**——不可妥协的几条（已逐条回源核对） | ☑ |
| 2 | [gap.md](gap.md) | **差距表**——基线对三条命门的现状，每条带 `路径:行号` | ☑ |
| 3 | [decision.md](decision.md) | **取舍卡**——选什么、舍什么、哪里要拍板（技术项 AI 代拍待人复核；红线项待人拍板不代拍） | ☑（第 1 轮修改） |
| 4 | [method-inventory.md](method-inventory.md) | **冻结清单**——101 个 method 全分类（(a)15/(b)46/(c)4/待定 36），计划 002 指标的唯一分母 | ☑ 冻结 |

落地蓝图见 `docs/plan/002_phase-a-gateway.md`。新增实验住 `docs/experiments/expNNN-<slug>/`。
