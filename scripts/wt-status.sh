#!/usr/bin/env bash
# wt-status.sh — 列出 _build/ 下每棵 worktree 的分支、对应 PR 状态与脏文件数，判定能否回收。
# 名额以本脚本为准，不抄 board 上的数字（2026-10-22：7 棵已合入的树占名额，board 照抄 12/12 一整天）。
# 名额口径（2026-11-05）：全部 linked worktree，排除主仓工作区；实现见 scripts/lib/worktree-count.sh（与 new-tree 共用）。
#
# 用法：scripts/wt-status.sh          每树一行：路径<TAB>分支<TAB>PR<TAB>状态<TAB>dirty<TAB>判定
# 判定：在做（OPEN/无 PR/有改动）· 可回收（PR MERGED|CLOSED 且无改动）
# 退出码：0 无可回收 · 1 有可回收（循环据此先回收再派单）· 2 前置错误
# 副作用：无（只读；回收由协调者按 CLAUDE.md §五 先归档 _tmp 再 git worktree remove）。

set -uo pipefail
# pwd -P：git 报物理路径，$ROOT 取逻辑路径时经 symlink（/tmp→/private/tmp）会全不匹配
ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
command -v gh >/dev/null || { echo "wt-status: 缺 gh" >&2; exit 2; }

# 名额口径唯一实现（与 pdrs-ops 的 new-tree.sh 共用，2026-11-05）：全部 linked worktree，排除主仓。
# shellcheck source=lib/worktree-count.sh
. "$ROOT/scripts/lib/worktree-count.sh" || { echo "wt-status: 缺 scripts/lib/worktree-count.sh" >&2; exit 2; }
# 只枚举一次：计数与逐树明细必须同一快照，否则两行数字会自相矛盾
paths=$(pdrs_worktree_paths "$ROOT") || { echo "wt-status: worktree 枚举失败" >&2; exit 2; }
linked=$(printf '%s' "$paths" | awk 'END { print NR + 0 }')

total=0; free=0
while read -r path; do
  case "$path" in "$ROOT/_build/"*) ;; *) continue ;; esac
  total=$((total + 1))
  br=$(git -C "$path" branch --show-current 2>/dev/null)
  dirty=$(git -C "$path" status --short 2>/dev/null | wc -l | tr -d ' ')
  pr=$(gh pr list --state all --head "$br" --json number,state --jq '.[0] | "\(.number)\t\(.state)"' 2>/dev/null)
  [ -n "$pr" ] || pr=$'-\t-'
  state=${pr#*$'\t'}
  verdict=在做
  if [ "$dirty" = 0 ] && { [ "$state" = MERGED ] || [ "$state" = CLOSED ]; }; then
    verdict=可回收; free=$((free + 1))
  fi
  printf '%s\t%s\t%s\t%s\t%s\n' "${path#"$ROOT/"}" "$br" "$pr" "$dirty" "$verdict"
done <<< "$paths"

echo "wt-status: 共 ${linked} 棵 linked worktree（上限 ${PDRS_WORKTREE_CAP}；不含主仓），_build 下 ${total} 棵，可回收 ${free}" >&2
if [ "$linked" -ge "$PDRS_WORKTREE_CAP" ]; then
  echo "wt-status: 名额已满（${linked}/${PDRS_WORKTREE_CAP}），new-tree 会拒绝开树；先回收上面标「可回收」的树" >&2
fi
[ "$free" = 0 ]
