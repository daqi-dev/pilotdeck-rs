---
name: deepseek-rpm
description: DeepSeek（llm-center）单 key 限 50 RPM，dsh 全局最多同跑 3 个，经 dsh-slot.sh 排队
metadata:
  type: feedback
---

**规则**：dsh（实现和评审都算）全局最多同时跑 3 个。`_tmp/20261021-loop/fix-launch.sh` 和 `_tmp/20261018-a6-split/review.sh` 已经通过 `_tmp/20261021-loop/dsh-slot.sh`（mkdir 原子占槽，`DSH_MAX` 默认 3）排队，别绕开它直接起 dsh。

**Why:** 2026-10-22 16:54，同时开了约 6 个 dsh：3 个实现、1 个修复、2 个评审。两个进程撞上 `RATE_LIMIT: 429 … 单 key 每分钟请求超限（50 RPM）`（code 102503），中途退出：EdgeClaw 修复轮 1 写了一半，插件 loader 的 r2 评审输出为空。dsh 是工具型 agent，一个就能每分钟打出十几次请求，几个就会把 50 RPM 用光。GLM 那边按并发计（12 路实测通过），两家的限法不一样。

**How to apply:** 需要并行更多 DeepSeek 时，评审改用不占 dsh 名额的 `scripts/review-deepseek.py`（单次请求）。看到 dsh 退出码 1，先在 .err 里 grep `RATE_LIMIT`：命中就是限流，按续跑处理（树里的现场保留，写 resume 任务书），不计入「同一失败 3 次」。关联 [[model-runners]]、[[kill-by-pid]]。

**更新（17:15）**：人提议上调到 12，先试 8；但 17:05–17:06 在**仅 3 个** dsh 同跑时又有 3 个撞 429。一个 dsh 每分钟约 15–20 次请求，50 RPM 只够 2–3 个。名额回到 3，靠 `dsh-retry.sh`（等 60 s 续跑，最多 3 次）兜底；要更多 DeepSeek 并行，需要第二把 key，或评审改用单次请求的 `review-deepseek.py`。
