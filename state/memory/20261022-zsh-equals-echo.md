---
name: zsh-equals-echo
description: 本机 shell 是 zsh，命令里不加引号的 `===` / `==` 开头词会被当作 =命令 展开报「== not found」并让整条命令非 0 退出、后续输出丢失；分隔线一律写 echo '---' 或加引号。
metadata:
  type: feedback
---

**为什么**：2026-10-22 /loop 协调轮里 `echo ===` 至少 4 次触发 `(eval):1: == not found`，导致同一条命令后半段的检查结果没打印、需要重跑。zsh 的 `=cmd` 展开（EQUALS 选项）把 `===` 解析成查找命令 `==`。

**怎么用**：分隔线写 `echo '---'` 或 `echo "== x"`（不以 `=` 开头的未加引号词）；不要依赖 `setopt noequals`。
