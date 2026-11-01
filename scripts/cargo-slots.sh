#!/usr/bin/env bash
# cargo-slots.sh — 开编译/测试前看一眼：同时在跑的 cargo 进程是否已到人定上限（CLAUDE.md「资源上限」：≤ 4）。
# 计数口径：命令以 cargo 可执行文件开头的进程（含 build/test/check/clippy/run/bench/doc，及 cargo-clippy 等子命令二进制），
# 不数 rustc（它是 cargo 的子进程，数了会重复）。
# 用法：scripts/cargo-slots.sh [上限]   退出码：0 有空位 · 1 已满（调用方应等待后重试）
# 上限取值：命令行参数优先，其次环境变量 PDRS_CARGO_SLOTS，默认 4。
# 来由：宿主链按主机核数配置名额（cb8 10、macmini 6、mbp2 4、本机仍 4），见决策卡
# decisions-20261101-host-throughput.md（_tmp/20261101-1140-speedup/）。
set -uo pipefail
CAP="${1:-${PDRS_CARGO_SLOTS:-4}}"
# 只接受正整数（去掉首尾空白）；0、负数、非数字一律报错退出 2，不静默变成「永远满」（#1287 r1 P2-1）
CAP="${CAP//[[:space:]]/}"
case "$CAP" in ''|*[!0-9]*|0*) echo "cargo-slots: 上限必须是正整数（参数或 PDRS_CARGO_SLOTS），实际「${CAP}」" >&2; exit 2;; esac
n=$(ps -axo command= | awk '{print $1}' | grep -cE '(^|/)cargo(-[a-z]+)?$' || true)
echo "cargo-slots: 在跑 $n / 上限 $CAP"
[ "$n" -lt "$CAP" ]
