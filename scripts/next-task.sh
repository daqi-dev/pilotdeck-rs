#!/usr/bin/env bash
# next-task.sh — 读 state/board.md 顶部的 READY 队列块，输出队首（第一条 state=ready 的项）。
# 格式正本：specs/README.md「board READY 队列块」。路由表（kind → 通道）只在本文件 route_for() 维护。
#
# 用法：
#   scripts/next-task.sh              队首一行：ref<TAB>kind<TAB>summary<TAB>route
#   scripts/next-task.sh --all        全部条目（含 blocked / human）
#   scripts/next-task.sh --check      只校验格式
#   scripts/next-task.sh --route KIND 只输出该 kind 的路由
#   --board FILE                      换一份 board（测试用）
# 退出码：0 有队首 · 2 board 或 READY 块缺失 · 3 无 ready 项（队列空或只剩 doing/human/blocked）· 4 格式错误 · 64 参数错
# 副作用：无（只读）。stderr 提醒 human 项与外层循环锁状态。
# board 固定读**主工作树**（定位复用 lib/disk-budget.sh 的 pdrs_disk_main_root）：$0 相对路径解析到的
# 树 ≠ 主树、或主树 board 与 origin/main 不一致时 stderr 提示并自动纠正，不硬失败（2026-11-08 人令修）。

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BOARD=""
MODE=head
ROUTE_KIND=""
LOCK_DIR="/tmp/pdrs-loop.lock"

route_for() {
  case "$1" in
    impl)       echo "codex exec（astra-ultra）在 _build/wt-<码>/ 实现；协调者提交/推送" ;;
    gate)       echo "python3 scripts/run-review-roles.py（三角色+四视角，claude -p 只读）→ 结果块贴 PR → pr-gate CI" ;;
    review)     echo "双谱系非作者评审：GLM（claude -p 接智谱，见 state/memory/20261015-model-runners.md）优先，另一家取 codex / DeepSeek / Claude 中非作者者" ;;
    research)   echo "codex exec 或 claude -p opus 调研，产物写 _tmp/<YYYYMMDD>-<名>/" ;;
    docs|coord) echo "协调者本会话直接做" ;;
    *) return 1 ;;
  esac
}

while [ $# -gt 0 ]; do
  case "$1" in
    --all) MODE=all ;;
    --check) MODE=check ;;
    --route) MODE=route; ROUTE_KIND="${2:-}"; shift ;;
    --board) BOARD="${2:-}"; [ -n "$BOARD" ] || { echo "next-task: --board 须带非空文件路径" >&2; exit 64; }; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "next-task: 未知参数 $1" >&2; exit 64 ;;
  esac
  shift
done

if [ "$MODE" = route ]; then
  route_for "$ROUTE_KIND" || { echo "next-task: 未知 kind「$ROUTE_KIND」" >&2; exit 64; }
  exit 0
fi

# ── board 定位（2026-11-08 人令修）：固定读主工作树的 state/board.md ──
# 协调会话 cwd 常被派单/收货带进 _build/wt-*，$0 相对路径解析到的 scripts/ 也在该树上，
# 会读到树基点上的陈旧 READY/待人清单（2026-11-08 00:5x 事故：wt-1344 报出 6 张早已拍板的
# 待人单，污染派单判断）。主树定位复用 lib 的 pdrs_disk_main_root（与 pdrs-loop 磁盘闸、
# new-tree 同一策略）；解析失败按本树继续并提示，不硬失败（协调者脚本常在树里跑）。
# --board 显式指定时整段跳过。BOARD_ROOT 留给下面的 origin/main 一致性提示复用。
if [ -z "$BOARD" ]; then
  BOARD="$ROOT/state/board.md"
  BOARD_ROOT="$ROOT"
  if [ -r "$ROOT/scripts/lib/disk-budget.sh" ]; then
    # shellcheck source=lib/disk-budget.sh
    . "$ROOT/scripts/lib/disk-budget.sh"
    if main_root="$(pdrs_disk_main_root "$ROOT")"; then
      # 主树路径来自 git worktree list（物理路径），ROOT 来自 cd+pwd（逻辑路径）；/var→/private/var
      # 一类符号链接别名会让字符串比对把主仓误判成「非主树」，比较前两边都解析成物理路径。
      if main_real="$(cd "$main_root" 2>/dev/null && pwd -P)"; then :; else main_real="$main_root"; fi
      if root_real="$(cd "$ROOT" 2>/dev/null && pwd -P)"; then :; else root_real="$ROOT"; fi
      if [ "$main_real" != "$root_real" ]; then
        echo "[next-task] cwd 非主工作树，改读 ${main_real}/state/board.md" >&2
        BOARD="$main_real/state/board.md"
        BOARD_ROOT="$main_real"
      else
        BOARD_ROOT="$main_real"
      fi
    else
      echo "next-task: 无法定位主工作树（git worktree list 失败），按本树 $ROOT/state/board.md 读" >&2
    fi
  else
    echo "next-task: 缺 $ROOT/scripts/lib/disk-budget.sh，无法定位主工作树，按本树 $ROOT/state/board.md 读" >&2
  fi
fi

[ -f "$BOARD" ] || { echo "next-task: 找不到 $BOARD" >&2; exit 2; }

# 第二道保险：主树 board 与 origin/main 不一致（可能落后，也可能本地草稿未推）时提示，
# 不阻断——board 段子代理会在本地草稿上跑。git 不可用、无 origin/main、路径缺失都静默跳过。
# origin/main 是本地引用，不 fetch 拿不到远端最新，提示只是提醒核分支。
# 比较经 $() 吃掉两侧尾部换行：仅差尾部空行视为一致（提示性保险，不做字节级比较）。
if [ -n "${BOARD_ROOT:-}" ] \
   && origin_board="$(git -C "$BOARD_ROOT" show "origin/main:state/board.md" 2>/dev/null)" \
   && [ "$origin_board" != "$(cat "$BOARD")" ]; then
  echo "[next-task] board 与 origin/main 不一致（可能落后或有本地未推草稿），建议先 git fetch + 核分支" >&2
fi

# 解析：BEGIN/END 各恰好一个、BEGIN 在第一个历史段（行首「> ## 」）之前、有 updated 行、条目四段齐全。
# 只用 POSIX awk 语法（macOS 自带 awk 不保证支持 {n} 区间）；错误逐行输出为「ERR<TAB>说明」。
parsed="$(awk '
  function bad(msg) { errs[++nerr] = msg }
  BEGIN { inb = 0; nb = 0; ne = 0; first_hist = 0; begin_line = 0; upd = 0; nerr = 0 }
  /^<!-- READY:BEGIN -->[ \t]*$/ { nb++; begin_line = NR; inb = 1; next }
  /^<!-- READY:END -->[ \t]*$/   { ne++; inb = 0; next }
  /^> ## / { if (!first_hist) first_hist = NR }
  inb && /^updated: [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]/ { upd = 1; next }
  inb && /^- / {
    n = split(substr($0, 3), f, / [|] /)
    if (n != 4) { bad("第" NR "行：条目须为 4 段「ref | kind | state | summary」（summary 内不得含「 | 」）"); next }
    ref = f[1]; kind = f[2]; st = f[3]; sum = f[4]
    if (ref !~ /^[#A-Za-z0-9][A-Za-z0-9._#\/-]*$/) bad("第" NR "行：ref「" ref "」含空格或非法字符")
    if (kind !~ /^(impl|gate|review|research|docs|coord)$/) bad("第" NR "行：kind「" kind "」不在 impl/gate/review/research/docs/coord")
    if (st !~ /^(ready|doing|blocked|human)$/) bad("第" NR "行：state「" st "」不在 ready/doing/blocked/human")
    if (sum ~ /^[ \t]*$/) bad("第" NR "行：summary 为空")
    printf "ITEM\t%s\t%s\t%s\t%s\n", ref, kind, st, sum
    next
  }
  inb && /[^ \t]/ { bad("第" NR "行：READY 块内只许 updated 行与「- 」条目") }
  END {
    if (nb == 0 && ne == 0) { print "MISSING"; exit }
    if (nb != 1 || ne != 1) bad("READY:BEGIN/END 须各恰好一个（现 " nb "/" ne "）")
    if (first_hist && begin_line > first_hist) bad("READY 块须在第一个历史段（第" first_hist "行）之前")
    if (!upd) bad("缺「updated: YYYY-MM-DD HH:MM」行")
    for (i = 1; i <= nerr; i++) printf "ERR\t%s\n", errs[i]
  }
' "$BOARD")"

if [ "$parsed" = MISSING ]; then
  echo "next-task: $BOARD 没有 READY 块（<!-- READY:BEGIN --> … <!-- READY:END -->）；格式见 specs/README.md「board READY 队列块」" >&2
  exit 2
fi
if printf '%s\n' "$parsed" | grep -q '^ERR'; then
  printf '%s\n' "$parsed" | awk -F'\t' '$1 == "ERR" { print "next-task: " $2 }' >&2
  exit 4
fi

items="$(printf '%s\n' "$parsed" | grep '^ITEM' || true)"

# 提醒：human 项（待人拍板）与外层循环锁。不影响退出码。
printf '%s\n' "$items" | awk -F'\t' '$4 == "human" { print "next-task: 待人 " $2 " — " $5 }' >&2
if [ -d "$LOCK_DIR" ]; then
  echo "next-task: 外层循环在跑（$(tr '\n' ' ' 2>/dev/null < "$LOCK_DIR/owner")）——交互会话不并行做协调写入；要接手先 touch /tmp/pdrs-loop.stop，等本迭代结束" >&2
fi

case "$MODE" in
  check) echo "next-task: READY 块格式 OK（$(printf '%s\n' "$items" | grep -c . || true) 条）"; exit 0 ;;
  all)   printf '%s\n' "$items" | cut -f2-5; exit 0 ;;
esac

head_line="$(printf '%s\n' "$items" | awk -F'\t' '$4 == "ready" { print; exit }')"
if [ -z "$head_line" ]; then
  echo "next-task: 无 ready 项（队列空或只剩 human/blocked）" >&2
  exit 3
fi
ref="$(printf '%s' "$head_line" | cut -f2)"
kind="$(printf '%s' "$head_line" | cut -f3)"
summary="$(printf '%s' "$head_line" | cut -f5)"
printf '%s\t%s\t%s\t%s\n' "$ref" "$kind" "$summary" "$(route_for "$kind")"
