---
name: dsh-channel
description: DeepSeek 首选通道是 dsh（DeepSeek Harness）：实现用 workspace-write、评审用 read-only（沙箱执行层强制）；人定 10-18 起 DeepSeek 承担实现与评审的大头，codex 守第二家评审与难单。
metadata:
  type: feedback
---

**为什么**：F2-61b 试点三轮挂起，原因是走了无工具通道（只吐 patch 文本，看不到真实文件）；改用 dsh 后 A6.3-pr1 首版 5 分钟、一轮修复到位。dsh 只读模式实测拒写。近两天评审统计：codex 13 次抓 P0×7 / P1×13，DeepSeek（多为无工具通道）11 次 P0×0 / P1×6，GLM 14 次 P0×0 / P1×4。

**怎么用**：命令与配置见 `_tmp/20261018-dsh-impl/README.md`（provider 必须 `deepseek-official`，密钥只经 `.env.local` 的 `DEEPSEEK_API_KEY` 走环境变量）。分工：规格明确的实现单 → DeepSeek；难单（并发、协议、串行热点）→ codex / Claude；评审第一家 → DeepSeek（dsh read-only），第二家 → codex，难单再加 Claude；四角色审查 → GLM。谁写谁不审照旧。跑约 10 单后按看板「按实现方」「评审 ≥4 轮」复核门槛。
