#!/usr/bin/env bash
# worktree-count.sh — 「worktree 名额」口径与上限常量的唯一实现。
#
# 口径（2026-11-05 定，决策卡 state/decisions-20261105-worktree-cap.md「口径」节，PR #1548）：
#   计入本仓全部 linked worktree，排除主仓工作区本身。等价 `git worktree list | wc -l` − 1。
#   codex 工作树（~/.codex/worktrees/…）与 /tmp 下的树同样是本仓注册的 linked worktree，
#   `git worktree list` 会列出它们，因而与 `_build/wt-*` 同一口径计入——它们同样吃磁盘、占席位。
#   主仓不是副本、不可回收，不计入。
#
# 为什么是全量口径（实测见 _tmp/20261105-tree-hyg/TREES.md）：只看 `_build/wt-*` 会漏掉
#   codex 6 棵与 /tmp 1 棵——子集 35 < 40 放行时，真值已 43。上限依据与磁盘三档
#   （scripts/lib/disk-budget.sh）见 state/decisions-20261105-worktree-cap-60.md。
#
# 用法：
#   . scripts/lib/worktree-count.sh
#   pdrs_worktree_paths [repo]    # 打印每个 linked worktree 的绝对路径，每行一个
#   pdrs_worktree_count [repo]    # 打印一个整数；失败返回 2，不会把失败静默算成 0
#   repo 省略时取当前目录所属仓库；调用方**应显式传入自己的仓根**，不要依赖调用者 cwd。
#   PDRS_WORKTREE_CAP             # 生效上限，缺省 60（2026-11-05 23:0x 人令「放宽、按硬盘算」，
#                                   # AI 代批待人复核，见 #1157）；非数字 / 0 / >60 一律回落 60，
#                                   # 笔误不会把闸门关掉；调低只应出现在自测里。
# 直接执行：打印「数 + 上限」与明细（自测用）。

PDRS_WORKTREE_CAP_MAX=60          # 能力上限（2026-11-05 23:0x 人令放宽，AI 代批待人复核；回退只改这一处）
PDRS_WORKTREE_CAP_DEFAULT=$PDRS_WORKTREE_CAP_MAX   # 生效默认 = 能力上限
PDRS_WORKTREE_CAP=${PDRS_WORKTREE_CAP:-$PDRS_WORKTREE_CAP_DEFAULT}
case "$PDRS_WORKTREE_CAP" in
  '' | *[!0-9]*) PDRS_WORKTREE_CAP=$PDRS_WORKTREE_CAP_DEFAULT ;;
esac
# 超过能力上限或 0：回落默认，不把笔误（999）当成「放宽」
[ "$PDRS_WORKTREE_CAP" -ge 1 ] && [ "$PDRS_WORKTREE_CAP" -le "$PDRS_WORKTREE_CAP_MAX" ] ||
  PDRS_WORKTREE_CAP=$PDRS_WORKTREE_CAP_DEFAULT
# 偏离默认就嚷一声：调低只应出现在自测里（调高超过 60 上面已回落）
[ "$PDRS_WORKTREE_CAP" -eq "$PDRS_WORKTREE_CAP_DEFAULT" ] ||
  echo "worktree-count: 注意生效上限 ${PDRS_WORKTREE_CAP} 低于默认 ${PDRS_WORKTREE_CAP_DEFAULT}（只应出现在自测里）" >&2

_pdrs_worktree_root() {
  local here=${1:-$PWD} top
  top=$(git -C "$here" rev-parse --show-toplevel 2>/dev/null) && { printf '%s\n' "$top"; return 0; }
  # bare 仓没有工作树，--show-toplevel 必失败：退回 git 目录本身（worktree list 同样可用）
  git -C "$here" rev-parse --absolute-git-dir 2>/dev/null
}

pdrs_worktree_paths() {
  local root out
  root=$(_pdrs_worktree_root "${1:-}") || {
    echo "worktree-count: 取不到仓库根（参数='${1:-}'，cwd=$(pwd)）" >&2
    return 2
  }
  # 先收输出再判退出码：直接管道时退出码取末段命令，git 失败会被吞成「0 棵」
  out=$(git -C "$root" worktree list --porcelain) || {
    echo "worktree-count: git worktree list 失败（$root）" >&2
    return 2
  }
  # porcelain 每条以 `worktree <绝对路径>` 开头，主仓工作区恒为第一条（git 文档；
  # bare 主仓同样有这一行，只多一条 `bare` 属性行）→ 去掉第一条即全部 linked worktree。
  printf '%s\n' "$out" | sed -n 's/^worktree //p' | tail -n +2
}

pdrs_worktree_count() {
  local paths
  paths=$(pdrs_worktree_paths "${1:-}") || return 2
  printf '%s' "$paths" | awk 'END { print NR + 0 }'
}

# 直接执行（非 source）时打印明细，方便人工核对与自测
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  n=$(pdrs_worktree_count) || exit 2
  echo "linked worktree: $n 棵（上限 ${PDRS_WORKTREE_CAP}；不含主仓工作区）"
  pdrs_worktree_paths | sed 's/^/  /'
fi
