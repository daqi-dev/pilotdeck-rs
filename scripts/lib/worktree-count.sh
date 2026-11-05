#!/usr/bin/env bash
# worktree-count.sh — 「worktree 上限 60」名额口径与上限常量的唯一实现。
#
# 口径（2026-11-05 定，决策卡 state/decisions-20261105-worktree-cap.md「口径」节，PR #1548）：
#   计入本仓全部 linked worktree，排除主仓工作区本身。等价 `git worktree list | wc -l` − 1。
#   codex 工作树（~/.codex/worktrees/…）与 /tmp 下的树同样是本仓注册的 linked worktree，
#   `git worktree list` 会列出它们，因而与 `_build/wt-*` 同一口径计入——它们同样吃磁盘、占席位。
#   主仓不是副本、不可回收，不计入。
#
# 为什么是全量口径（实测见 _tmp/20261105-tree-hyg/TREES.md）：只看 `_build/wt-*` 会漏掉
#   codex 6 棵与 /tmp 1 棵——子集 35 < 40 放行时，真值已 43。上限为什么是 60、真闸为什么
#   换成磁盘，见 state/decisions-20261105-worktree-cap-60.md；磁盘三档在 scripts/lib/disk-budget.sh。
#
# 用法：
#   . scripts/lib/worktree-count.sh
#   pdrs_worktree_paths [repo]    # 打印每个 linked worktree 的绝对路径，每行一个
#   pdrs_worktree_count [repo]    # 打印一个整数；失败返回 2，不会把失败静默算成 0
#   repo 省略时取当前目录所属仓库；调用方**应显式传入自己的仓根**，不要依赖调用者 cwd。
#   PDRS_WORKTREE_CAP             # 上限，缺省 60；只允许调低（自测拒绝路径用），>60 或非数字一律按 60
# 直接执行：打印「数 + 上限」与明细（自测用）。

PDRS_WORKTREE_CAP_MAX=60
PDRS_WORKTREE_CAP=${PDRS_WORKTREE_CAP:-$PDRS_WORKTREE_CAP_MAX}
case "$PDRS_WORKTREE_CAP" in
  '' | *[!0-9]*) PDRS_WORKTREE_CAP=$PDRS_WORKTREE_CAP_MAX ;;
esac
[ "$PDRS_WORKTREE_CAP" -le "$PDRS_WORKTREE_CAP_MAX" ] || PDRS_WORKTREE_CAP=$PDRS_WORKTREE_CAP_MAX
# 调低只在自测里合法：生效值不是 60 就嚷一声，免得 shell 里残留的变量静默收紧闸门
[ "$PDRS_WORKTREE_CAP" -eq "$PDRS_WORKTREE_CAP_MAX" ] ||
  echo "worktree-count: 注意上限被环境变量改成 ${PDRS_WORKTREE_CAP}（默认 ${PDRS_WORKTREE_CAP_MAX}，只应出现在自测里）" >&2

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
