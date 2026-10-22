---
name: glm-scarcity-deepseek-shift
description: 2026-10-19 用户指示 GLM 量告急时尽量用 DeepSeek，可并行多跑取优；双谱系评审约束不变。
metadata:
  type: feedback
---

GLM 额度 2026-10-19 告急（codex 10-19 04:54 已耗尽在先）。用户指示：能用 DeepSeek 尽量用，可并行多跑几个选其中较好的。

**约束不变**：双谱系评审——作者谱系不能审。所以：
- DeepSeek 作者的 PR（当前大多数）：评审仍需 GLM + Claude，DeepSeek 不能审自己。四角色闸门改 `--runner anthropic`（Claude）省 GLM，GLM 只留给最终两家评审之一。
- Claude 作者的 PR（如 A6.3-pr7）：闸门与评审可用 DeepSeek（`scripts/review-deepseek.py` + 角色任务书 `.claude/workflows/review-*.md`），DeepSeek 直连 API 可并发；需要"并行取优"时对同一角色发 N 个 review-deepseek.py 调用择优。
- GLM 作者的 PR：DeepSeek + Claude 评审。

**Why**: GLM 是当前稀缺资源；DeepSeek 直连 API 无状态可并发，额度相对宽裕。
**How to apply**: 先看作者谱系决定能用 DeepSeek 的地方；DeepSeek 作者 PR 的闸门切 Claude 保 GLM 给最终评审。相关 [[model-runners]] [[no-unbounded-recursion]]。
