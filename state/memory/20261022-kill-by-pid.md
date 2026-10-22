---
name: kill-by-pid
description: 停后台 worker 或闸门只按记下的 pid 杀，禁止 pkill -n 和宽模式 pkill -f；实验与正式任务的输出目录不得同名
metadata:
  type: feedback
---

**规则**：要停后台 worker（dsh、codex）或闸门进程，只按派发时记下的 pid 杀（启动时 `echo $! > <任务目录>/<名>.pid`）。禁止用 `pkill -n`（「最新的那个」不一定是你想停的那个），也禁止用能同时匹配多个任务的宽模式 `pkill -f`。实验跑的输出目录必须与正式任务不同名。

**Why:** 2026-10-22 一小时内连续出了两次事故，都是协调者自己造成的：
1. 闸门并行实测和闸门链里排着的正式闸门用了同一个输出目录 `20261022-issue-414-gate2`。链启动时 `rm -rf` 删掉了实验目录，我写的 `pkill -f '…414-gate2'` 又把实验进程一起杀了（exit 143）。实验作废，结论被污染，差点被当成「并行会挂起」。
2. 想停 #458 修复轮的 dsh，用了 `pkill -n 'dsh --profile'`，结果误杀了正在写夹具生成器的 EdgeClaw pr10a worker，只能续跑。

**How to apply:** 派发时记 pid；停任务前用 `ps -o pid,lstart,command -p <pid>` 核对是不是目标再 kill。核对 worker 是否异常结束，看 status 文件的时间戳和 out 的末尾，不要只看 exit=0（被信号打断的 dsh 也可能写出 exit=0）。关联 [[macos-exec-sigkill]]、[[worker-evidence-pullback]]。

**补充（17:40）**：归档失败产物时，只挪输出文件（`.out`、`.err`、`.status`、`.try*`，评审输出 `rv-*.md`），任务书（`fix*.md`、`impl.md`、`*-resume.md`）一律不动。17:15 后缀表里带了 `md`，把 `fix1-resume.md` 也挪走了，之后两次重派都立刻 exit=1，空转 20 分钟。
