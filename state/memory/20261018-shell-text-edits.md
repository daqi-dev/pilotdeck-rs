---
name: shell-text-edits
description: 含中文的文本编辑一律用 python 不用 sed；停进程按 PID 不用宽模式 pkill
metadata:
  type: feedback
---
**规则 1：含中文（或其他多字节字符）的文件编辑、模板替换，一律用 python，不用 sed。** macOS 上的 BSD sed 处理多字节字符会报 `bad flag in substitute command` 或吞掉内容。10-18 已犯三次，最近一次让两家评审只拿到一行提示词。

**规则 2：停后台进程要精确定位：按 PID，或者用 `lsof -t <任务书文件>` 找到读这个文件的进程，再按 PID kill。不用 `pkill -f "<通用命令片段>"`。** 10-18 有两次，宽模式 pkill 可能误杀同时在跑的其他评审：多个 codex/opencode 进程的命令行一模一样，任务书是经 stdin 传入的，不出现在命令行里。

**为什么**：同一类错误犯到第三次，按 CLAUDE.md 要求沉淀成规则。

**怎么用**：
- 写任务书、改 PR 描述等文本，用 `python3 - <<'PYEOF' … PYEOF`，或者用 Write/Edit 工具。
- 发出评审前，先用 `head` 看一眼任务书是否完整。
- 要停某个后台进程：先 `lsof -t`，或从状态文件里拿到 PID，再 kill。
