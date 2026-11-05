#!/usr/bin/env bash
# cargo-slots.sh — 开编译/测试前看一眼：同时在跑的 cargo 进程是否已到人定上限（CLAUDE.md「资源上限」：≤ 4）。
# 计数口径：命令以 cargo 可执行文件开头的进程（含 build/test/check/clippy/run/bench/doc，及 cargo-clippy 等子命令二进制），
# 不数 rustc（它是 cargo 的子进程，数了会重复）。
# 同一进程树只算一次：`cargo clippy` 会派生 `cargo-clippy`，其下再派生 `cargo check`，三层首词都匹配 cargo(-xxx)，
# 逐个 grep 会把一次 clippy 记成 2–3 次（11-05 巡检据此误判「7–8 棵树绕过闸门」，实际一条都没绕过；
# fleet.py 已按祖先去重，本脚本对齐）。
# 用法：scripts/cargo-slots.sh [上限]   退出码：0 有空位 · 1 已满（调用方应等待后重试）· 2 上限非法 · 3 无法取样
# 上限取值：命令行参数优先，其次环境变量 PDRS_CARGO_SLOTS，默认 4。
# 来由：宿主链按主机核数配置名额（cb8 10、macmini 6、mbp2 4、本机仍 4），见决策卡
# decisions-20261101-host-throughput.md（_tmp/20261101-1140-speedup/）。
set -uo pipefail
CAP="${1:-${PDRS_CARGO_SLOTS:-4}}"
# 只接受正整数（去掉首尾空白）；0、负数、非数字一律报错退出 2，不静默变成「永远满」（#1287 r1 P2-1）
CAP="${CAP//[[:space:]]/}"
case "$CAP" in ''|*[!0-9]*|0*) echo "cargo-slots: 上限必须是正整数（参数或 PDRS_CARGO_SLOTS），实际「${CAP}」" >&2; exit 2;; esac
# 一次 ps 取全 pid/ppid/args，再从叶子往上归并：沿父指针上溯，祖先链里已经有 cargo 的不再另计，
# 只剩「祖先里没有 cargo」的 cargo 作为每棵进程树的根，数根即数并发槽位。按整条祖先链而不是只看直接父进程，
# 是为了覆盖 cargo → rustc → build-script → cargo（build.rs 里再起 cargo）这类中间夹了非 cargo 进程的同一条树。
# 单次 ps 取样，避免多次取样之间进程生灭造成的数字不一致。
# 匹配 args 的首词而不是 comm：macOS 同时输出 pid,ppid,comm,args 时 comm 会被压到 16 列（/Users/da/.rustu），
# args 才是完整命令行。
# 加 -ww 不限宽：BSD ps 接终端时按终端宽截断末列，rustup 的 cargo 全路径约 66 列，80 列终端减去 pid/ppid 两列
# 正好会把它切成 `carg` 而漏计（两个角色独立提出，cb8 上 -Aww 同样可用）。
# ps 失败不能当成「在跑 0」放行：`set -uo pipefail` 没有 -e，命令替换失败不会中断，
# awk 对空输入照样 `print c+0`，闸门就变成假 PASS（与 delivery-orphan「探测失败不再被当成没人」同类）。
snap=$(ps -Aww -o pid=,ppid=,args=) || { echo "cargo-slots: ps 取样失败，无法判断名额（不按 0 放行）" >&2; exit 3; }
[ -n "$snap" ] || { echo "cargo-slots: ps 输出为空，无法判断名额（不按 0 放行）" >&2; exit 3; }
n=$(printf '%s\n' "$snap" | awk '
  { par[$1]=$2; exe=$3; sub(/.*\//,"",exe)
    if (exe ~ /^cargo(-[a-z]+)?$/) isc[$1]=1 }
  END { for (p in isc) {
          q=par[p]; d=0
          while ((q in par) && !(q in isc) && d++ < 4096) q=par[q]
          if (!(q in isc)) c++
        }
        print c+0 }')
echo "cargo-slots: 在跑 $n / 上限 $CAP"
[ "$n" -lt "$CAP" ]
