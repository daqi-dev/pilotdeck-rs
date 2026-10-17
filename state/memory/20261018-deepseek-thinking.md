---
name: deepseek-thinking-budget
description: DeepSeek 直连评审空回复（finish_reason=length）的根因是思考 token 计入 max_tokens，与输入大小无关；评审时关闭思考或给足额度，任务书写明无工具。
type: feedback
---

# DeepSeek 空回复是思考吃光了额度

**为什么**：2026-10-17 协调者在 A6.2（约 134KB 输入）上连续三次拿到 `finish_reason=length` 的空回复，一度误判为「大输入不可用」并停用该通道。守护会话诊断（`_tmp/20261017-deepseek-diag/`）：输入只有约 37K token，开思考时 reasoning_tokens=16384 恰等于 max_tokens、正文 0 字；关思考 19 秒给出完整 VERDICT。另一次无工具时输出了臆造的工具调用，是任务书写了「若有文件工具」。

**怎么用**：用 `scripts/review-deepseek.py` 时默认关思考（#202 起 `--thinking disabled` 为默认）；空回复时看报错里的 reasoning_tokens 判断是否额度问题，而不是改输入大小。任务书必须写「你没有任何工具，只依据附件」。DeepSeek 恢复为可用的第二家非作者评审。教训：同一现象三次失败后先查根因（看 usage 字段），别直接给通道下「不可用」的结论。
