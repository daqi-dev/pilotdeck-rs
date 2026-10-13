# 003 · 之前那次 PilotDeck Rust 迁移里，哪些可以照搬、教训是什么

> 2026-10-14 起。四步走第二步：系统分析。**分析即取舍。**

## 这次要决定的

之前那次 PilotDeck Rust 迁移里，哪些可以照搬、教训是什么

## 五份文件，按这个顺序走

| # | 文件 | 干什么 | 状态 |
|---|---|---|---|
| 1 | [must-know.md](must-know.md) | **决策点**——不可妥协的几条 | ☐ |
| 2 | [sources.md](sources.md) | 两类来源：自有积累 / 他人积累 | ☐ |
| 3 | [assign.md](assign.md) | **派活模板**——怎么把材料交出去读 | ☐ |
| 4 | [gap.md](gap.md) | **差距表**——我该先做哪一件 | ☐ |
| 5 | [decision.md](decision.md) | **取舍卡**——选什么、舍什么、哪里要你拍板 | ☐ |

新增积累的实验住 `docs/experiments/expNNN-<slug>/`——**不在本目录下另开一处**。

## 取材

```bash
bash skills/aias-meta-research/scripts/clone.sh <url>          # 浅克隆进参考区
bash skills/aias-meta-research/scripts/clone.sh <url> -d 200   # 要挖提交历史就加深度
```

## 什么时候算够

> **能列出「需要你拍板的地方」，就够了。**
> 列不出来，是还没读懂；列出来了，就该停。

**分析的终点不是把材料读完，是把决策权交回人手上。**
