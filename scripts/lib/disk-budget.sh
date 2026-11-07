#!/usr/bin/env bash
# disk-budget.sh — `_build`+`_tmp` 磁盘预算的**唯一**阈值与测量实现。
#
# 三档（2026-11-05 定，AI 代批待人复核；正本 state/decisions-20261105-worktree-cap-60.md）：
#   ok     ≤ 160 GiB   放行
#   warn   > 160 GiB   放行 + WARN（先回收空闲树）；`scripts/pdrs-loop.sh` 在这一线就停派新活
#   hard   > 180 GiB   拒开新树 / 停新增写入（200 GiB 总量上限留 20 GiB 缓冲）
# 判断一律用 `du -sk` 的 **KiB 原值**，不做 GiB 四舍五入（180.4 GiB 必须判成 > 180）。
#
# 用法：
#   . scripts/lib/disk-budget.sh
#   pdrs_disk_used_kib [root]        # 打印 _build+_tmp 的 KiB 求和；测量失败返回 2，不把失败算成 0
#   pdrs_disk_level_of_kib <kib>     # 打印 ok|warn|hard；纯判定、无 I/O——**生产调用方用这个**
#                                    # 入参不是非负整数 → 打印 unknown 并返回 2（绝不落到 ok）
#   pdrs_disk_level [root]           # 测量 + 判档一步到位；失败打印 unknown 并返回 2（不打印空串）
#   root 不存在按前置错误返回 2（不能与「仓根在但没有 _build/_tmp」的 0 混为一谈）
#   root 省略时取当前目录；调用方**应显式传入仓根**，不要依赖 cwd。
#   `_build` / `_tmp` 都不存在时按 0 处理（远端宿主的 target 在别处，见 remote-worker/README §4）。
#   PDRS_DISK_TOTAL_GIB / PDRS_DISK_HARD_GIB / PDRS_DISK_WARN_GIB  只允许调低（自测用），
#   > 上限或非数字一律回落上限。调高不是「放宽」——要放宽走决策卡改这里的 _MAX。
#
# 调用方（都 source 这一份，不另写数字）：
#   scripts/pdrs-loop.sh          —— 主仓根的 _build+_tmp
#   pdrs-ops/scripts/loop/new-tree.sh —— 开树前（绝对路径 source，可用 PDRS_DISK_LIB 指向同内容副本自测）

PDRS_DISK_TOTAL_GIB_MAX=200
PDRS_DISK_HARD_GIB_MAX=180
PDRS_DISK_WARN_GIB_MAX=160

_pdrs_disk_clamp() { # $1=变量当前值 $2=上限；只允许调低
  case "${1:-}" in '' | *[!0-9]*) printf '%s' "$2" ;; *)
    if [ "$1" -le "$2" ] && [ "$1" -gt 0 ]; then printf '%s' "$1"; else printf '%s' "$2"; fi ;;
  esac
}
PDRS_DISK_TOTAL_GIB=$(_pdrs_disk_clamp "${PDRS_DISK_TOTAL_GIB:-}" "$PDRS_DISK_TOTAL_GIB_MAX")
PDRS_DISK_HARD_GIB=$(_pdrs_disk_clamp "${PDRS_DISK_HARD_GIB:-}" "$PDRS_DISK_HARD_GIB_MAX")
PDRS_DISK_WARN_GIB=$(_pdrs_disk_clamp "${PDRS_DISK_WARN_GIB:-}" "$PDRS_DISK_WARN_GIB_MAX")
# WARN 不得高于 HARD：自测把 HARD 调低时 WARN 跟着降，否则会出现「全 warn 不 hard」的空档
[ "$PDRS_DISK_WARN_GIB" -le "$PDRS_DISK_HARD_GIB" ] || PDRS_DISK_WARN_GIB=$PDRS_DISK_HARD_GIB

_pdrs_tmo() { # $1=秒，其余为命令；有 GNU timeout 就限时，没有则直跑
  # 闸门自己不能挂住：`_build` 可能指向一个挂死的外接卷，`cd`/`du` 都会无限阻塞
  if command -v timeout >/dev/null 2>&1; then timeout "$@"; else shift; "$@"; fi
}

pdrs_disk_used_kib() {
  local root=${1:-$PWD} d real out sum=0
  # 仓根不存在 = 前置错误（路径拼错 / 仓被搬走），不能与「仓根在但没有 _build/_tmp」混为一谈：
  # 后者按 0 计（远端宿主的 target 在别处），前者若也按 0 计，磁盘闸就整体静默失效
  [ -d "$root" ] || return 2
  for d in "$root/_build" "$root/_tmp"; do
    # 既不是实体也不是链接 → 目录本就不存在（远端宿主的 target 在别处），计 0
    [ -e "$d" ] || [ -L "$d" ] || continue
    # 断链：`du` 对它是 0 KiB 且 rc=0（11-05 实测），静默放行等于把闸关掉 → fail-closed
    [ -e "$d" ] || return 2
    # 命令行操作数是符号链接时 `du -sk` 不跟随（11-05 BSD 实测 0 KiB；本仓 runner 树
    # 有 `_build` 指向外接卷的配置）→ 先 cd + pwd -P 取物理路径再量
    real=$(_pdrs_tmo 10 bash -c 'cd "$1" 2>/dev/null && pwd -P' _ "$d")
    [ -n "$real" ] || return 2
    # fail-closed：du 非 0 或输出不是数字就返回 2，绝不把「没量到」当 0 放行。
    # 但 `_build` 是 cargo target 树，编译并发删改文件时 du 会以非 0 退出（两家评审均指出），
    # 瞬时竞态不该让整条循环自停 → 失败先重试一次；仍失败则 fail-closed（偏小的部分和绝不采信）
    if ! out=$(_pdrs_tmo 180 du -sk "$real" 2>/dev/null); then
      sleep 1
      if ! out=$(_pdrs_tmo 180 du -sk "$real" 2>/dev/null); then
        printf 'disk-budget: du 测量失败（已重试一次）：%s\n' "$real" >&2
        return 2
      fi
    fi
    # 只认第一字段且必须全是数字：前缀截断会把 "12x345" 读成 12（偏小 = 假 PASS）
    out=${out%%[ 	]*}
    case "$out" in '' | *[!0-9]*) return 2 ;; esac
    sum=$((sum + out))
  done
  printf '%s\n' "$sum"
}

pdrs_disk_main_root() { # $1=起点目录；输出「该量的仓根」
  # 「量哪个根」是策略，不是调用方的私事：从 linked worktree 里跑时量自己那棵（没有 _build/_tmp）
  # 会恒判 ok，闸门静默失效。策略放这里，pdrs-loop.sh / new-tree.sh / 直接执行块都调它；
  # next-task.sh / pdrs-loop.sh 的 board 读取也调它定位主树（2026-11-08 人令修，
  # 与磁盘闸同一策略，防 worktree 里读到陈旧 READY）。
  # 非 git 树（隔离测试 fixture）原样返回；解析不出主仓根、或主仓是裸仓（没有工作区）→ 返回 2（fail-closed）。
  local d=${1:-$PWD} start out first
  start=$d   # 不能写进上一条 local：local 会先把 $d 展开再赋值，set -u 下直接 unbound
  if [ ! -e "$d/.git" ]; then
    # 子目录也要能定位：`cd src && bash ../scripts/lib/disk-budget.sh` 若把 src 当仓根，
    # 会量到不存在的 _build/_tmp → 打印「0 GiB → ok」，正是这段要防的误读。
    # --show-toplevel 在 linked worktree 里给的是**本树**根，主仓还要再走下面的 worktree list。
    start=$(git -C "$d" rev-parse --show-toplevel 2>/dev/null) || { printf '%s\n' "$d"; return 0; }
    [ -n "$start" ] && [ -e "$start/.git" ] || { printf '%s\n' "$d"; return 0; }   # 非 git 树（隔离 fixture）原样返回
  fi
  out=$(git -C "$start" worktree list --porcelain 2>/dev/null) || return 2
  first=$(printf '%s\n' "$out" | sed -n 's/^worktree //p' | head -n 1)
  [ -n "$first" ] || return 2
  # 裸主仓的首条是仓目录本身，其下没有工作区 → 量它必得 0（=闸门静默放行），宁可停机
  printf '%s\n' "$out" | awk '/^$/{exit} $0=="bare"{f=1} END{exit !f}' && return 2
  [ -d "$first" ] || return 2
  printf '%s\n' "$first"
}

pdrs_disk_level_of_kib() { # $1=KiB 原值；三档判定只有这一处，调用方（pdrs-loop / new-tree）都用它
  # 入参必须是非负整数：`[ "" -gt N ]` 在 bash 里报错且两个分支都为假 → 落到 else 打印 ok，
  # 那会让「量失败但漏查 rc」的调用方静默放行（本文件其余路径全是 fail-closed，唯独判档放行）
  case "${1:-}" in '' | *[!0-9]*) printf 'unknown\n'; return 2 ;; esac
  # 光「全是数字」不够：bash 的 [ ] 按 base0 解析，"08"/"09" 报语法错、"0x.." 与超 int64 的字面量
  # 同样能让两个分支都为假 → 又落到 else 打印 ok。这里只认规范十进制且不超 int64 位宽。
  case "$1" in 0?*) printf 'unknown\n'; return 2 ;; esac
  [ "${#1}" -le 18 ] || { printf 'unknown\n'; return 2; }
  if [ "$1" -gt "$((PDRS_DISK_HARD_GIB * 1048576))" ]; then printf 'hard\n'
  elif [ "$1" -gt "$((PDRS_DISK_WARN_GIB * 1048576))" ]; then printf 'warn\n'
  else printf 'ok\n'; fi
}

pdrs_disk_level() { # 测量 + 判档一步到位；失败打印 unknown 并返回 2（空串会被调用方误当「不是 hard」放行）
  local kib
  kib=$(pdrs_disk_used_kib "${1:-$PWD}") || { printf 'unknown\n'; return 2; }
  pdrs_disk_level_of_kib "$kib"
}

# 直接执行：打印三档阈值、实测的仓根与测量值/级别（自测 / 排障用）
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  # 排障时最常犯的错：在 linked worktree 里执行，量到自己那棵没有 _build/_tmp 的树 → 打 ok。
  # 走与生产同一条 pdrs_disk_main_root，并把实测的根打出来，读数才和真闸一致。
  root=$(pdrs_disk_main_root "$PWD") || { echo "disk-budget: 无法确定主仓根（fail-closed）" >&2; exit 2; }
  kib=$(pdrs_disk_used_kib "$root") || { echo "disk-budget: 测量失败（${root}）" >&2; exit 2; }
  echo "disk-budget: 总量上限 ${PDRS_DISK_TOTAL_GIB} GiB / 硬闸 ${PDRS_DISK_HARD_GIB} GiB / WARN ${PDRS_DISK_WARN_GIB} GiB"
  echo "disk-budget: 实测根 ${root}"
  echo "disk-budget: 当前 $((kib / 1048576)) GiB（${kib} KiB）→ $(pdrs_disk_level_of_kib "$kib")"
fi
