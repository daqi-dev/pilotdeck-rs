---
name: model-runners
description: 本机已核验的 OpenCode 与 DeepSeek Harness 通道；适用于接续评审，不是永久工具选型。
type: feedback
---

# 本机评审入口：2026-10-15 经验

**为什么**：用户指定的是 Z.AI Coding Plan 的 GLM5.3，不是 OpenCode 当时默认的 LLMcenter。DeepSeek Harness 默认 headless 的 `deepseek-official` 曾返回 402，但桌面使用的 `deepseek-account` 可正常响应；单个通道失败不能推断整个 Harness 不可用。实际调用与代码评审原件在主仓本机 `_tmp/20261015-multi-model/`，这是本机经验索引，不是 CI 的证据依赖。

**怎么用**：后续 OpenCode 显式选 `--model zai-coding-plan/glm-5.3 --agent plan`。Harness 使用其正式 `--patch` 参数临时匹配桌面模型选择；本次成功组合为 `deepseek-account / deepseek-flash / high`，仅选择模型，不包含凭据，也不改全局配置。提示词通过文件/stdin 传入；核退出码、final 事件和实际审阅结论。已成功通道在配置未变时沿用，增量任务限制输入范围，避免重复全仓评审；通道变化再做最小核验。不要读取或复制账号凭据。候选代码的评审通过不代表产品优先级、完整 pr-ready 或最终行为兼容。
