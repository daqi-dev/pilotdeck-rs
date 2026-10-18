#!/usr/bin/env bash
# cargo-slots.sh — 开编译/测试前看一眼：同时在跑的 cargo 进程是否已到人定上限（CLAUDE.md「资源上限」：≤ 4）。
# 计数口径：命令以 cargo 可执行文件开头的进程（含 build/test/check/clippy/run/bench/doc，及 cargo-clippy 等子命令二进制），
# 不数 rustc（它是 cargo 的子进程，数了会重复）。
# 用法：scripts/cargo-slots.sh [上限，默认 4]   退出码：0 有空位 · 1 已满（调用方应等待后重试）
set -uo pipefail
CAP="${1:-4}"
n=$(ps -axo command= | awk '{print $1}' | grep -cE '(^|/)cargo(-[a-z]+)?$' || true)
echo "cargo-slots: 在跑 $n / 上限 $CAP"
[ "$n" -lt "$CAP" ]
