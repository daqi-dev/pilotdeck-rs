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
#   pdrs_disk_used_kib [root]   # 打印 _build+_tmp 的 KiB 求和；测量失败返回 2，不把失败算成 0
#   pdrs_disk_level [root]      # 打印 ok|warn|hard；测量失败返回 2（不打印级别）
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
  local root=${1:-$PWD} d out sum=0
  for d in "$root/_build" "$root/_tmp"; do
    [ -d "$d" ] || continue
    # fail-closed：du 非 0 或输出不是数字就返回 2，绝不把「没量到」当 0 放行
    out=$(du -sk "$d" 2>/dev/null) || return 2
    out=${out%%[!0-9]*}
    [ -n "$out" ] || return 2
    sum=$((sum + out))
  done
  printf '%s\n' "$sum"
}

pdrs_disk_level() {
  local root=${1:-$PWD} kib
  kib=$(pdrs_disk_used_kib "$root") || return 2
  if [ "$kib" -gt "$((PDRS_DISK_HARD_GIB * 1048576))" ]; then printf 'hard\n'
  elif [ "$kib" -gt "$((PDRS_DISK_WARN_GIB * 1048576))" ]; then printf 'warn\n'
  else printf 'ok\n'; fi
}

# 直接执行：打印三档阈值、当前测量值与级别（自测 / 排障用）
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  kib=$(pdrs_disk_used_kib "$PWD") || { echo "disk-budget: 测量失败" >&2; exit 2; }
  echo "disk-budget: 总量上限 ${PDRS_DISK_TOTAL_GIB} GiB / 硬闸 ${PDRS_DISK_HARD_GIB} GiB / WARN ${PDRS_DISK_WARN_GIB} GiB"
  echo "disk-budget: 当前 $((kib / 1048576)) GiB（${kib} KiB）→ $(pdrs_disk_level "$PWD")"
fi
