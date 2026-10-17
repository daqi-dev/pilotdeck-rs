---
name: model-runners
description: 本机已核验的 OpenCode 与 DeepSeek 通道（2026-10-17 起 DeepSeek 首选直连 API 脚本，dsh 仅兜底）；适用于接续评审，不是永久工具选型。
type: feedback
---

# 本机评审入口：2026-10-15 经验

**2026-10-17 更新（用户指示，优先于下文 dsh 用法）**：DeepSeek 谱系**首选** `python3 -I scripts/review-deepseek.py --prompt <任务书> --input <diff/源码>... --out <out.md>`（默认 `deepseek-flash`，可选 `--model deepseek-v4-pro`）。它直连 API、密钥先取环境变量 `DEEPSEEK_API_KEY`，没有再读 `.env.local`（先当前检出根，再主仓根，所以在 `_build/wt-*` 里跑也找得到；已在 .gitignore），模型无工具、天然只读；需要的上下文必须用 `--input` 显式附上（它读不到仓库；合计上限 2 MiB，超了分批）。长评审建议显式传 `--max-tokens`（不传用 API 默认输出上限，截断时按失败退出、费用已花）。`--out` 已存在默认拒绝（保护上一轮原件），只有 exit 0 才算有完整回复，失败不写 out。冒烟原件 `_tmp/20261017-deepseek-smoke/`（exit 0、SMOKE-OK）。脚本报 HTTP/余额错误时才回退 dsh。不要读取、打印或复制 `.env.local`。

**为什么**：用户指定的是 Z.AI Coding Plan 的 GLM5.3，不是 OpenCode 当时默认的 LLMcenter。DeepSeek Harness 默认 headless 的 `deepseek-official` 曾返回 402，但桌面使用的 `deepseek-account` 可正常响应；单个通道失败不能推断整个 Harness 不可用。实际调用与代码评审原件在主仓本机 `_tmp/20261015-multi-model/`，这是本机经验索引，不是 CI 的证据依赖。

**怎么用**：后续 OpenCode 显式选 `--model zai-coding-plan/glm-5.3 --agent plan`。Harness 使用其正式 `--patch` 参数临时匹配桌面模型选择；本次成功组合为 `deepseek-account / deepseek-flash / high`，仅选择模型，不包含凭据，也不改全局配置。提示词通过文件/stdin 传入；核退出码、final 事件和实际审阅结论。已成功通道在配置未变时沿用，增量任务限制输入范围，避免重复全仓评审；通道变化再做最小核验。不要读取或复制账号凭据。候选代码的评审通过不代表产品优先级、完整 pr-ready 或最终行为兼容。
