---
name: model-runners
description: 模型通道正本——2026-10-22 起优先 glm-5.3-mb 与 deepseek-v4.1-flash-mb（llm-center），含各入口、已知坑与误报判别；旧段按日期递减，已被新段覆盖。
type: feedback
---

# 本机评审入口：2026-10-15 经验

**2026-10-22 核验（人定：优先 `glm-5.3-mb` 与 `deepseek-v4.1-flash-mb`；本段优先于下文）**
- **分工**：实现默认 DeepSeek（dsh + `scripts/dsh-headless-llm-center.patch.yml`，`model: deepseek-v4.1-flash-mb`）。评审默认两家：GLM（`scripts/run-review-roles.py` 默认 `--runner zai`，模型 `glm-5.3-mb`）+ DeepSeek（`scripts/review-deepseek.py` 默认 `deepseek-v4.1-flash-mb`，或 dsh read-only）。作者是 GLM 或 DeepSeek 时，第二家评审才用 codex（`gpt-6.1-sol`，走 OpenAI 自家后端，不经 llm-center）。Claude 只做难单。
- **模型名必须带 `-mb`**。不带 `-mb` 的 `glm-5.3` 网关也会应答，但不是指定通道，出错时不容易察觉。10-22 查出 `_tmp/20261018-a6-split/glm_review.py` 写死了 `--model glm-5.3`，已改成 `glm-5.3-mb`。派单前先 `grep -n -- '--model\|model:'` 看一眼启动脚本或 patch。
- **不是失败的输出**：`claude -p` 跑 GLM 时，stderr 会打 `Claude Code doesn't recognize the model "glm-5.3-mb"…` 和 `[claude-code:unrecognized_model]`，意思只是按 200k 估上下文窗口。判成败只看三样：exit 码、jsonl 里有没有 `"type":"result"` 行、`.md` 末尾有没有 `VERDICT:` 行。网关回显的展示名 `GLM-5.3` 已由 `RESP_MODEL_ALIAS` 放行。
- **中断不是失败**：jsonl 停在 `thinking_tokens`、没有 result 行，说明进程被打断了（比如上一会话退出连带杀掉）。作废重跑，不计入「同一失败 3 次」。
- **`--runner anthropic` 现在跑不了**：它只认 `.env.local` 里的 `ANTHROPIC_API_KEY`，现在没有这把钥，会报「anthropic 通道缺 ANTHROPIC_API_KEY」后退出，不产 summary.md。外层包装脚本的 `exit=$?` 可能被后续命令吃掉而显示 0，要以 summary.md 是否生成为准。需要 Claude 评审时用 `_tmp/20261018-a6-split/review.sh … claude`（走本会话的 llm-center 环境；10-22 未实测）。
- **下文过时项**：10-17 段的 `--model glm-5.3`、`api.z.ai`、`ANTHROPIC_AUTH_TOKEN`，以及「怎么用」里的 `zai-coding-plan/glm-5.3`、`deepseek-flash`、`deepseek-official`，都不要再用。OpenCode 手工评审改用 `opencode run -m llmcenter/glm-5.3-mb`。

**2026-10-20 两家都改走 llm-center（用户指定，优先于下文所有旧 URL/模型名）**：网关 `https://llm-center.modelbest.co`，Anthropic Messages 协议（`POST /v1/messages`，头 `x-api-key` + `anthropic-version: 2023-06-01`），密钥仍是 `.env.local` 的 `ZAI_API_KEY` / `DEEPSEEK_API_KEY`。GLM 模型 `glm-5.3-mb`：`run-review-roles.py` 把 `ANTHROPIC_BASE_URL` 设为**主机根**（Claude Code 自己拼 `/v1/messages`，带 `/v1/messages` 会拼成双路径 404）；网关在响应体里回显展示名 `GLM-5.3`、`modelUsage` 却是请求名，守卫用 `RESP_MODEL_ALIAS` 只放行这一对。DeepSeek 模型 `deepseek-v4.1-flash-mb`：`review-deepseek.py` 已从 OpenAI chat 改成 Messages（`max_tokens` 必填，默认 16384；`--thinking enabled` 发 `budget_tokens`）。实测原件 `_tmp/20261020-llm-center-smoke/`。另：旧文写的 `ANTHROPIC_AUTH_TOKEN` 不准，代码实际设的是 `ANTHROPIC_API_KEY`。

**2026-10-18 人定模型分工（经守护会话转达，正本见 `state/decisions-20261017.md`「卡 · 模型分工」）**：评审默认 GLM + DeepSeek，只有难单（并发、协议、冻结装置、安全边界）才请 Claude 审；实现由 codex / GLM / DeepSeek 轮换，难单优先 codex 或 Claude；谁写谁不审，必须两家非作者。GLM、DeepSeek 先各试一张小单，看质量再定是否常态化。

**2026-10-17 GLM 走 Claude Code（首选）**：`claude -p` 设 `ANTHROPIC_BASE_URL=https://api.z.ai/api/anthropic`、`ANTHROPIC_AUTH_TOKEN=$ZAI_API_KEY`（`.env.local`）、`--model glm-5.3` 即跑 GLM，init 报 `model=glm-5.3`、工具白名单照常在执行层生效（实测原件 `_tmp/20261017-review-roles-glm/`）。`scripts/run-review-roles.py` 默认就走它（`--runner zai`）。比 opencode 好在只读可证明；opencode 只剩人工对话用途。GLM 开几个角度都只算**一个谱系**，双谱系评审仍需另一家非作者。

**2026-10-17 更新（用户指示，优先于下文 dsh 用法）**：DeepSeek 谱系**首选** `python3 -I scripts/review-deepseek.py --prompt <任务书> --input <diff/源码>... --out <out.md>`（默认 `deepseek-flash`，可选 `--model deepseek-v4-pro`）。它直连 API、密钥先取环境变量 `DEEPSEEK_API_KEY`，没有再读 `.env.local`（先当前检出根，再主仓根，所以在 `_build/wt-*` 里跑也找得到；已在 .gitignore），模型无工具、天然只读；需要的上下文必须用 `--input` 显式附上（它读不到仓库；合计上限 2 MiB，超了分批）。长评审建议显式传 `--max-tokens`（不传用 API 默认输出上限，截断时按失败退出、费用已花）。`--out` 已存在默认拒绝（保护上一轮原件），只有 exit 0 才算有完整回复，失败不写 out。冒烟原件 `_tmp/20261017-deepseek-smoke/`（exit 0、SMOKE-OK）。脚本报 HTTP/余额错误时才回退 dsh。不要读取、打印或复制 `.env.local`。

**为什么**：用户指定的是 Z.AI Coding Plan 的 GLM5.3，不是 OpenCode 当时默认的 LLMcenter。DeepSeek Harness 默认 headless 的 `deepseek-official` 曾返回 402，但桌面使用的 `deepseek-account` 可正常响应；单个通道失败不能推断整个 Harness 不可用。实际调用与代码评审原件在主仓本机 `_tmp/20261015-multi-model/`，这是本机经验索引，不是 CI 的证据依赖。

**怎么用**：后续 OpenCode 显式选 `--model zai-coding-plan/glm-5.3 --agent plan`。Harness 使用其正式 `--patch` 参数临时匹配桌面模型选择；本次成功组合为 `deepseek-account / deepseek-flash / high`，仅选择模型，不包含凭据，也不改全局配置。提示词通过文件/stdin 传入；核退出码、final 事件和实际审阅结论。已成功通道在配置未变时沿用，增量任务限制输入范围，避免重复全仓评审；通道变化再做最小核验。不要读取或复制账号凭据。候选代码的评审通过不代表产品优先级、完整 pr-ready 或最终行为兼容。
