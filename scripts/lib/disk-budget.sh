#!/usr/bin/env bash
# disk-budget.sh — `_build`+`_tmp` 磁盘预算的**唯一**阈值与测量实现。
#
# 三档（2026-11-05 人授权，正本 state/decisions-20261105-worktree-cap-60.md）：
#   ok     ≤ 160 GiB   放行
#   warn   > 160 GiB   放行 + WARN（先回收空闲树）；`scripts/pdrs-loop.sh` 在这一线就停派新活
#   hard   > 180 GiB   拒开新树 / 停新增写入（200 GiB 总量上限留 20 GiB 缓冲）
# 判断一律用 `du -sk` 的 **KiB 原值**，不做 GiB 四舍五入（180.4 GiB 必须判成 > 180）。
#
# 用法：
#   . scripts/lib/disk-budget.sh
#   pdrs_disk_used_kib [root]        # 打印 _build+_tmp 的 KiB 求和；测量失败返回 2，不把失败算成 0
#   pdrs_disk_level_of_kib <kib>     # 打印 ok|warn|hard；纯判定、无 I/O——**生产调用方用这个**
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
    real=$(cd "$d" 2>/dev/null && pwd -P) || return 2
    # fail-closed：du 非 0 或输出不是数字就返回 2，绝不把「没量到」当 0 放行。
    # 但 `_build` 是 cargo target 树，编译并发删改文件时 du 会以非 0 退出（两家评审均指出），
    # 瞬时竞态不该让整条循环自停 → 失败先重试一次；仍失败则 fail-closed（偏小的部分和绝不采信）
    if ! out=$(du -sk "$real" 2>/dev/null); then
      sleep 1
      if ! out=$(du -sk "$real" 2>/dev/null); then
        printf 'disk-budget: du 测量失败（已重试一次）：%s\n' "$real" >&2
        return 2
      fi
    fi
    out=${out%%[!0-9]*}
    [ -n "$out" ] || return 2
    sum=$((sum + out))
  done
  printf '%s\n' "$sum"
}

pdrs_disk_level_of_kib() { # $1=KiB 原值；三档判定只有这一处，调用方（pdrs-loop / new-tree）都用它
  if [ "$1" -gt "$((PDRS_DISK_HARD_GIB * 1048576))" ]; then printf 'hard\n'
  elif [ "$1" -gt "$((PDRS_DISK_WARN_GIB * 1048576))" ]; then printf 'warn\n'
  else printf 'ok\n'; fi
}

pdrs_disk_level() { # 测量 + 判档一步到位；失败打印 unknown 并返回 2（空串会被调用方误当「不是 hard」放行）
  local kib
  kib=$(pdrs_disk_used_kib "${1:-$PWD}") || { printf 'unknown\n'; return 2; }
  pdrs_disk_level_of_kib "$kib"
}

# 直接执行：打印三档阈值、当前测量值与级别（自测 / 排障用）
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  kib=$(pdrs_disk_used_kib "$PWD") || { echo "disk-budget: 测量失败" >&2; exit 2; }
  echo "disk-budget: 总量上限 ${PDRS_DISK_TOTAL_GIB} GiB / 硬闸 ${PDRS_DISK_HARD_GIB} GiB / WARN ${PDRS_DISK_WARN_GIB} GiB"
  echo "disk-budget: 当前 $((kib / 1048576)) GiB（${kib} KiB）→ $(pdrs_disk_level_of_kib "$kib")"
fi
