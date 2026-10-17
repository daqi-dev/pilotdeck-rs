#!/usr/bin/env bash
# pdrs-loop.sh — 外层循环：读 READY 队首 → 写本迭代任务书 → 起一个**全新**协调会话做这一单 → 记账 → 下一单。
#
# 连续性不再依赖工具通道的事件回调（A 时代由 Claude Code 后台通知隐式提供）。每迭代一个冷启动会话：
# CLAUDE.md/board 每次重新注入（规则改动下一迭代即生效），迭代时限即两小时交接点。
#
# 由人在终端（建议 tmux）前台启动；不用 launchd/cron——定时器不知道上一迭代是否还在跑。
#   scripts/pdrs-loop.sh [--runner opencode|claude] [--max-iter N] [--iter-timeout SEC] [--dry-run]
# 停：touch /tmp/pdrs-loop.stop（本迭代结束后停）；Ctrl-C（立即，杀本迭代进程组）。
# 停机条件＝CLAUDE.md「连续推进模式」的机器化：队列无 ready（④）· 同一单两迭代未推进（②挂起后仍不动）·
#           磁盘超 46 GiB · stop 文件 · max-iter。「暂不执行」档由协调会话把条目标 human（①）。
# 退出码：0 正常停 · 1 前置/守卫失败 · 2 锁被占 · 130 中断

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOCK_DIR=/tmp/pdrs-loop.lock
STOP_FILE=/tmp/pdrs-loop.stop
# 测试注入：只在没有 .git 的隔离 fixture 根里允许改锁/stop 路径；真实仓库与 worktree 一律拒绝（防误删、防绕开单实例锁）
if [ -n "${PDRS_LOOP_LOCK:-}${PDRS_LOOP_STOP:-}" ]; then
  if [ -e "$ROOT/.git" ]; then echo "pdrs-loop: PDRS_LOOP_LOCK/STOP 只许测试 fixture 使用，真实仓库拒绝" >&2; exit 1; fi
  LOCK_DIR="${PDRS_LOOP_LOCK:-$LOCK_DIR}"; STOP_FILE="${PDRS_LOOP_STOP:-$STOP_FILE}"
fi
RUNNER=opencode
MAX_ITER=50
ITER_TIMEOUT=7200
DRY=0
DISK_CAP_KIB=48234496   # 46 GiB，口径见 CLAUDE.md「资源上限」

while [ $# -gt 0 ]; do
  case "$1" in
    --runner) RUNNER="${2:-}"; shift ;;
    --max-iter) MAX_ITER="${2:-}"; shift ;;
    --iter-timeout) ITER_TIMEOUT="${2:-}"; shift ;;
    --dry-run) DRY=1 ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "pdrs-loop: 未知参数 $1" >&2; exit 1 ;;
  esac
  shift
done
case "$RUNNER" in opencode|claude) ;; *) echo "pdrs-loop: --runner 只许 opencode|claude" >&2; exit 1 ;; esac
case "$MAX_ITER" in ''|*[!0-9]*) echo "pdrs-loop: --max-iter 须为正整数" >&2; exit 1 ;; esac
case "$ITER_TIMEOUT" in ''|*[!0-9]*) echo "pdrs-loop: --iter-timeout 须为正整数（秒）" >&2; exit 1 ;; esac
[ "$ITER_TIMEOUT" -gt 0 ] && [ "$ITER_TIMEOUT" -le 7200 ] || { echo "pdrs-loop: --iter-timeout 须在 (0,7200]（两小时交接点不许放宽）" >&2; exit 1; }
command -v timeout >/dev/null || { echo "pdrs-loop: 缺 timeout（brew coreutils）" >&2; exit 1; }

# dry-run：锁之前的只读短路（不建锁、不写任务书、不派活）
if [ "$DRY" = 1 ]; then
  echo "pdrs-loop: dry-run——runner=$RUNNER max-iter=$MAX_ITER iter-timeout=${ITER_TIMEOUT}s"
  bash "$ROOT/scripts/next-task.sh" || true
  echo "pdrs-loop: dry-run 结束"
  exit 0
fi

# 单实例锁：持有人进程已死视为陈旧锁并接管；活着则拒绝（也挡住会话里误起嵌套循环）。
if ! mkdir "$LOCK_DIR" 2>/dev/null; then
  old_pid="$(sed -n 's/^pid=//p' "$LOCK_DIR/owner" 2>/dev/null || true)"
  if [ -n "$old_pid" ] && kill -0 "$old_pid" 2>/dev/null; then
    echo "pdrs-loop: 已有循环在跑：$(tr '\n' ' ' 2>/dev/null < "$LOCK_DIR/owner")" >&2
    exit 2
  fi
  echo "pdrs-loop: 接管陈旧锁（pid ${old_pid:-?} 已不在）" >&2
fi
printf 'pid=%s\nstarted=%s\nrunner=%s\n' "$$" "$(date '+%F %T')" "$RUNNER" > "$LOCK_DIR/owner"
rm -f "$STOP_FILE"
child=""
# GNU timeout 默认自成进程组组长，杀 -pgid 即连同会话派生的子进程一起收掉。
cleanup() { if [ -n "$child" ]; then kill -- "-$child" 2>/dev/null || true; fi; rm -rf "$LOCK_DIR"; }
trap cleanup EXIT
trap 'echo "pdrs-loop: 中断" >&2; exit 130' INT TERM

RUN_DIR="$ROOT/_tmp/$(date +%Y%m%d)-pdrs-loop"
# 每次启动一个独立目录：同日再起循环不会覆盖前一跑的 iter-NNN 原件
LAUNCH_DIR="$RUN_DIR/run-$(date +%H%M%S)-$$"
mkdir -p "$RUN_DIR"
mkdir "$LAUNCH_DIR"
# v2 账本：status 以 board updated 是否变化判推进（exit 0 但 updated 未变 = no-progress），不只看退出码
LEDGER="$RUN_DIR/ledger-v2.tsv"
[ -f "$LEDGER" ] || printf 'iter\tstart\tref\tupdated\tstatus\texit\tupdated_after\tlaunch\n' > "$LEDGER"

board_updated() { sed -n 's/^updated: \([0-9-]* [0-9:]*\).*/\1/p' "$ROOT/state/board.md" | head -1; }
# 所有历史账本（按日期；同日先 v1 再 v2）末尾连续几行的 (ref, updated) 等于当前 = 连续未推进次数。
# 跨启动、跨日、v1→v2 都持续计数（有意为之：人不改 board 就重起，不会白得新机会）。
stale_count() {
  local f files=()
  for d in $(ls -d "$ROOT"/_tmp/*-pdrs-loop 2>/dev/null | sort); do
    for f in "$d/ledger.tsv" "$d/ledger-v2.tsv"; do [ -f "$f" ] && files+=("$f"); done
  done
  [ "${#files[@]}" -gt 0 ] || { echo 0; return; }
  for f in "${files[@]}"; do tail -n +2 "$f"; done | awk -F'\t' -v r="$1" -v u="$2" \
    '{ if ($3 == r && $4 == u) n++; else n = 0 } END { print n + 0 }'
}

iter=0
carry=""   # 留给下一迭代任务书的「先处置」指令
while :; do
  iter=$((iter + 1))
  [ "$iter" -le "$MAX_ITER" ] || { echo "pdrs-loop: 到 max-iter=${MAX_ITER}，停"; exit 0; }
  if [ -e "$STOP_FILE" ]; then echo "pdrs-loop: 见 stop 文件，停"; rm -f "$STOP_FILE"; exit 0; fi

  used_kib="$({ du -sk "$ROOT/_build" "$ROOT/_tmp" 2>/dev/null || true; } | awk '{s+=$1} END {print s+0}')"
  if [ "$used_kib" -gt "$DISK_CAP_KIB" ]; then
    echo "pdrs-loop: _build+_tmp=${used_kib}KiB 超 46GiB 上限，停（CLAUDE.md 资源上限）" >&2; exit 1
  fi

  set +e
  head_line="$(bash "$ROOT/scripts/next-task.sh")"; rc=$?
  set -e
  case "$rc" in
    0) ;;
    3) echo "pdrs-loop: 队列无 ready 项，停（连续推进停机条件④）"; exit 0 ;;
    *) echo "pdrs-loop: next-task 退出码 ${rc}，停；先修 board READY 块" >&2; exit 1 ;;
  esac
  ref="$(printf '%s' "$head_line" | cut -f1)"
  kind="$(printf '%s' "$head_line" | cut -f2)"
  summary="$(printf '%s' "$head_line" | cut -f3)"
  route="$(printf '%s' "$head_line" | cut -f4)"
  upd_before="$(board_updated)"

  # 守卫：队首与 updated 都和此前迭代开始时相同 = 没推进。累计一次：本迭代先挂起它；两次：停，不再派发。
  stale="$(stale_count "$ref" "$upd_before")"
  if [ "$stale" -ge 2 ]; then
    echo "pdrs-loop: $ref 连续两迭代未推进且未被挂起，停（交人看 ${RUN_DIR}）" >&2; exit 1
  fi
  if [ "$stale" = 1 ]; then
    carry="${carry:+$carry }上一迭代结束时没有更新 board，队首仍是 ${ref}。先核现场（worktree、在途进程、_tmp 证据），把 $ref 在 READY 块标为 blocked 并在本轮段写明原因（单线挂起），再取下一条 ready 继续。"
  fi

  idir="$LAUNCH_DIR/iter-$(printf '%03d' "$iter")"
  mkdir "$idir"
  cat > "$idir/prompt.md" <<EOF
# 外层循环第 ${iter} 迭代 · 协调者任务书（scripts/pdrs-loop.sh 生成）

你是 PilotDeck-RS 本迭代的协调者。按 CLAUDE.md 开工协议读 .42cog/、board、memory 后，**只做这一单**：

- 队首：${ref}（kind=${kind}）— ${summary}
- 路由（正本在 scripts/next-task.sh route_for）：${route}
${carry:+- **先处置**：${carry}}

规则：
1. 技能停点按 CLAUDE.md「技能停点覆盖表」处理，不等人；需人拍板的写决策卡，把该条目在 READY 块改为 human，然后结束本迭代。
2. 本迭代时限 $((ITER_TIMEOUT / 60)) 分钟（两小时交接点）。到点前收尾：改 board 顶部 READY 块（含 updated 行）+ 追加本轮段（做了什么 / 下一步）；跑 scripts/next-task.sh --check 确认格式。
3. 本单做完或挂起后**结束会话**，下一单由外层循环接；不要自己起 scripts/pdrs-loop.sh。
4. 放权边界、红线、合并闸门照 CLAUDE.md；本任务书不扩大任何授权。
EOF
  carry=""

  case "$RUNNER" in
    opencode) cmd=(opencode run -m zai-coding-plan/glm-5.3) ;;
    # claude -p 无人应答权限提示：以项目 settings 的 allow 列表为准，未列者拒绝（dontAsk）；不用 bypass。
    claude)   cmd=(claude -p --model claude-opus-5-5 --permission-mode dontAsk --output-format stream-json --verbose) ;;
  esac
  printf '%s ' "${cmd[@]}" > "$idir/command.txt"; echo "< $idir/prompt.md  # cwd=$ROOT timeout=${ITER_TIMEOUT}s" >> "$idir/command.txt"
  echo "pdrs-loop: 第 $iter 迭代 ${ref}（${kind}）→ ${RUNNER}；证据 $idir"
  if [ "$DRY" = 1 ]; then echo "pdrs-loop: dry-run，任务书与命令已写，停"; exit 0; fi

  start="$(date '+%F %T')"
  set +e
  (cd "$ROOT" && exec timeout --kill-after=60 "$ITER_TIMEOUT" "${cmd[@]}" < "$idir/prompt.md" > "$idir/out.log" 2> "$idir/err.log") &
  child=$!
  wait "$child"; ec=$?
  child=""
  set -e
  upd_after="$(board_updated)"
  case "$ec" in
    0) if [ -z "$upd_after" ]; then status=board-unparsed
       elif [ "$upd_after" != "$upd_before" ]; then status=done; else status=no-progress; fi ;;
    124|137) status=timeout ;;
    *) status=error ;;
  esac
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$iter" "$start" "$ref" "$upd_before" "$status" "$ec" "$upd_after" "${LAUNCH_DIR##*/}" >> "$LEDGER"
  if [ "$status" = timeout ]; then
    carry="上一迭代（${ref}）在两小时交接点被超时终止。先核现场与 board 是否已写交接，缺则补写，再继续。"
  fi
done
