#!/usr/bin/env bash
# test-pdrs-loop.sh — pdrs-loop.sh 的行为测试：临时 fixture 根（无 .git）+ 假 opencode（PATH 桩，记调用次数），不派真会话、不碰真锁。
#   bash scripts/test-pdrs-loop.sh
# 覆盖：① 推进 → 账本 done、队列空 exit 0；② 再起一次（board 复位）→ 新 run-* 目录、前一跑 iter 原件不被覆盖；
#       ③ 不推进 → 两行 no-progress、守卫 exit 1、第二迭代带「先处置」；④ 原样重起 → 守卫立即停、runner 不再被调用；
#       ⑤ v1 账本已有两次未推进 → 首次 v2 启动不派发（⑤b 数字 ref 按字符串比）；⑥ 有 .git 的根拒绝锁/stop 覆盖；
#       ⑦ board 无 updated 行 → board-unparsed；⑧ 同分钟推进记 done；⑨ runner 失败也计入、重起不派发；
#       ⑩ 同分钟连续推进不触发守卫（落账与守卫同一判据）；⑪ 迭代超时记 timeout 并带交接提示。
set -euo pipefail

SRC="$(cd "$(dirname "$0")" && pwd)"
BASE="$(mktemp -d)"
trap 'rm -rf "$BASE"' EXIT
T="$BASE/loop fixture"   # 路径含空格：守卫遍历历史账本不能按空白拆词
mkdir -p "$T"
fails=0
check() { if eval "$2"; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi; }

board() {  # $1 = updated 行（空串 = 不写 updated 行）
  { echo '# test board'; echo; echo '<!-- READY:BEGIN -->'
    [ -n "$1" ] && echo "updated: $1 · by test"
    echo '- t1 | docs | ready | 测试条目'; echo '<!-- READY:END -->'; echo; echo '> ## 历史段'; } > "$T/root/state/board.md"
}
setup() {  # 全新 fixture；$1 = progress|stall|wipe
  rm -rf "$T/root" "$T/bin" "$T/calls"
  mkdir -p "$T/root/scripts" "$T/root/state" "$T/root/_build" "$T/root/_tmp" "$T/bin"
  cp "$SRC/pdrs-loop.sh" "$SRC/next-task.sh" "$T/root/scripts/"
  board "2026-10-17 10:00"
  stub "$1"
}
stub() {
  cat > "$T/bin/opencode" <<EOF
#!/usr/bin/env bash
cat > /dev/null
echo x >> "$T/calls"
case "$1" in
  progress) sed -i.bak -e 's/^updated: .*/updated: 2026-10-17 11:00 · by stub/' -e 's/| ready |/| human |/' "$T/root/state/board.md" ;;
  wipe) sed -i.bak -e '/^updated: /d' -e 's/| ready |/| human |/' "$T/root/state/board.md" ;;
  same-minute) sed -i.bak -e 's/| ready |/| human |/' "$T/root/state/board.md" ;;
  touch-only) echo "> 进展 \$(cat "$T/calls" | wc -l)" >> "$T/root/state/board.md" ;;
  hang) sleep 20 ;;
  fail) exit 1 ;;
esac
exit 0
EOF
  chmod +x "$T/bin/opencode"
  # 固定时钟：日期目录与 run-HHMMSS 恒定，两次启动必然同秒、只靠 PID 区分；不受跨午夜影响
  printf '%s\n' '#!/usr/bin/env bash' 'case "${1:-}" in' '  +%Y%m%d) echo 20261017 ;;' '  +%H%M%S) echo 120000 ;;' '  *) exec /bin/date "$@" ;;' 'esac' > "$T/bin/date"
  chmod +x "$T/bin/date"
}
calls() { [ -f "$T/calls" ] && wc -l < "$T/calls" | tr -d ' ' || echo 0; }
run_loop() {
  set +e
  PATH="$T/bin:$PATH" PDRS_LOOP_LOCK="$T/lock" PDRS_LOOP_STOP="$T/stop" \
    bash "$T/root/scripts/pdrs-loop.sh" --runner opencode --max-iter 5 --iter-timeout "${ITO:-30}" > "$T/out.log" 2>&1
  rc=$?
  set -e
}
ledger() { ls "$T"/root/_tmp/*-pdrs-loop/ledger-v2.tsv; }

# ① 推进
setup progress; run_loop
check "① 推进：exit 0" '[ "$rc" = 0 ]'
check "① 账本 done 且 updated_after 已变" '[ "$(sed -n 2p "$(ledger)" | cut -f5)" = done ] && [ "$(sed -n 2p "$(ledger)" | cut -f7)" = "2026-10-17 11:00" ]'

# ② 再起：board 复位为 ready、updated 变新值，前一跑原件不能被覆盖
first="$(ls -d "$T"/root/_tmp/*-pdrs-loop/run-*/iter-001)"
echo ORIGINAL >> "$first/prompt.md"
board "2026-10-17 12:00"
run_loop
check "② 再起：exit 0 且第二个 run-* 目录里有 iter-001" '[ "$rc" = 0 ] && [ "$(ls -d "$T"/root/_tmp/*-pdrs-loop/run-*/iter-001 | wc -l | tr -d " ")" = 2 ]'
check "② 前一跑 iter-001 原件未被覆盖" 'tail -n 1 "$first/prompt.md" | grep -q ORIGINAL'
check "② 账本两行 done，runner 共调用 2 次" '[ "$(cut -f5 "$(ledger)" | grep -c "^done$")" = 2 ] && [ "$(calls)" = 2 ]'

# ③ 不推进
setup stall; run_loop
check "③ 不推进：守卫停 exit 1" '[ "$rc" = 1 ]'
check "③ 两行 no-progress，runner 调用 2 次" '[ "$(cut -f5 "$(ledger)" | grep -c "^no-progress$")" = 2 ] && [ "$(calls)" = 2 ]'
check "③ 第二迭代任务书带「先处置」" 'grep -q "先处置" "$T"/root/_tmp/*-pdrs-loop/run-*/iter-002/prompt.md'

# ④ 原样重起：守卫从账本恢复计数，立即停，不再派发
run_loop
check "④ 原样重起：exit 1 且 runner 仍是 2 次" '[ "$rc" = 1 ] && [ "$(calls)" = 2 ]'

# ⑤ v1 账本已有两次未推进（另一日目录），首次 v2 启动不派发
setup stall
mkdir -p "$T/root/_tmp/20261016-pdrs-loop"
printf 'iter\tstart\tref\tupdated\tstatus\texit\n1\tx\tt1\t2026-10-17 10:00\tdone\t0\n2\tx\tt1\t2026-10-17 10:00\tdone\t0\n' > "$T/root/_tmp/20261016-pdrs-loop/ledger.tsv"
run_loop
check "⑤ v1 历史两次未推进：exit 1、runner 0 次" '[ "$rc" = 1 ] && [ "$(calls)" = 0 ]'

# ⑤b 数字形 ref 按字符串比较：历史是 001、01，当前队首 1 从未执行，不能被误判为已两次未推进
setup stall
sed -i.bak 's/- t1 |/- 1 |/' "$T/root/state/board.md"
mkdir -p "$T/root/_tmp/20261016-pdrs-loop"
printf 'iter\tstart\tref\tupdated\tstatus\texit\n1\tx\t001\t2026-10-17 10:00\tdone\t0\n2\tx\t01\t2026-10-17 10:00\tdone\t0\n' > "$T/root/_tmp/20261016-pdrs-loop/ledger.tsv"
run_loop
check "⑤b ref 1 与历史 001/01 不相等：runner 被调用" '[ "$(calls)" -ge 1 ]'

# ⑥ 有 .git 的根拒绝覆盖锁/stop
setup progress; mkdir "$T/root/.git"; run_loop
check "⑥ 有 .git 时拒绝 PDRS_LOOP_LOCK/STOP：exit 1、runner 0 次、不建锁" '[ "$rc" = 1 ] && [ "$(calls)" = 0 ] && [ ! -e "$T/lock" ] && grep -q "只许测试 fixture" "$T/out.log"'

# ⑦ 会话删掉了 updated 行
setup wipe; run_loop
check "⑦ updated 行丢失记 board-unparsed" '[ "$(sed -n 2p "$(ledger)" | cut -f5)" = board-unparsed ]'

# ⑧ 同一分钟内推进（board 变了、updated 没变）记 done
setup same-minute; run_loop
check "⑧ 同分钟推进：exit 0 且记 done" '[ "$rc" = 0 ] && [ "$(sed -n 2p "$(ledger)" | cut -f5)" = done ]'

# ⑨ runner 失败（exit 1）也计入未推进：两次 error 后守卫停，原样重起不再派发
setup fail; run_loop
check "⑨ 两行 error、守卫停 exit 1、runner 2 次" '[ "$rc" = 1 ] && [ "$(cut -f5 "$(ledger)" | grep -c "^error$")" = 2 ] && [ "$(calls)" = 2 ]'
run_loop
check "⑨ 原样重起：exit 1、runner 仍 2 次、停机消息含恢复方法" '[ "$rc" = 1 ] && [ "$(calls)" = 2 ] && grep -q "恢复" "$T/out.log"'

# ⑩ 同一单多迭代、每次只改 board 内容不改 updated：都记 done，守卫不误停，跑满 max-iter
setup touch-only; run_loop
check "⑩ 同分钟连续推进：5 行 done、exit 0（max-iter）" '[ "$rc" = 0 ] && [ "$(cut -f5 "$(ledger)" | grep -c "^done$")" = 5 ] && [ "$(calls)" = 5 ]'

# ⑪ 迭代超时：记 timeout，下一迭代任务书带超时交接提示，两次后守卫停
setup hang; ITO=2 run_loop
check "⑪ 超时：两行 timeout、守卫停 exit 1" '[ "$rc" = 1 ] && [ "$(cut -f5 "$(ledger)" | grep -c "^timeout$")" = 2 ]'
check "⑪ 第二迭代任务书带超时交接提示" 'grep -q "超时终止" "$T"/root/_tmp/*-pdrs-loop/run-*/iter-002/prompt.md'

check "锁已释放" '[ ! -e "$T/lock" ]'
echo "test-pdrs-loop: $([ "$fails" = 0 ] && echo OK || echo "$fails FAIL")"
[ "$fails" = 0 ]
