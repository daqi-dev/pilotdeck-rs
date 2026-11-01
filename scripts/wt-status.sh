#!/usr/bin/env bash
# wt-status.sh — 列出 _build/ 下每棵 worktree 的分支、对应 PR 状态与脏文件数，判定能否回收。
# 名额以本脚本为准，不抄 board 上的数字（2026-10-22：7 棵已合入的树占名额，board 照抄 12/12 一整天）。
#
# 用法：scripts/wt-status.sh          每树一行：路径<TAB>分支<TAB>PR<TAB>状态<TAB>dirty<TAB>判定
# 判定：在做（OPEN/无 PR/有改动）· 可回收（PR MERGED|CLOSED 且无改动）
# 退出码：0 无可回收 · 1 有可回收（循环据此先回收再派单）· 2 前置错误
# 副作用：无（只读；回收由协调者按 CLAUDE.md §五 先归档 _tmp 再 git worktree remove）。

set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
command -v gh >/dev/null || { echo "wt-status: 缺 gh" >&2; exit 2; }

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
done < <(git -C "$ROOT" worktree list --porcelain | sed -n 's/^worktree //p')

echo "wt-status: 共 $total 棵（上限 40），可回收 $free" >&2
[ "$free" = 0 ]
