#!/usr/bin/env bash
# test-pdrs-loop.sh — pdrs-loop.sh 的行为测试：临时仓根 + 假 opencode（PATH 桩），不派真会话、不碰真锁。
#   bash scripts/test-pdrs-loop.sh
# 覆盖：① 推进（桩改 board updated 并把条目标 human）→ 账本 done、队列空正常停；
#       ② 不推进（桩什么都不改、exit 0）→ 账本 no-progress，两迭代后守卫停（exit 1），第二迭代任务书带「先处置」；
#       ③ 同日再起一次 → 新的 run-* 目录，前一跑的 iter-001 原件不被覆盖。
set -euo pipefail

SRC="$(cd "$(dirname "$0")" && pwd)"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fails=0
check() { if eval "$2"; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi; }

setup() {
  rm -rf "$T/root" "$T/bin"
  mkdir -p "$T/root/scripts" "$T/root/state" "$T/root/_build" "$T/root/_tmp" "$T/bin"
  cp "$SRC/pdrs-loop.sh" "$SRC/next-task.sh" "$T/root/scripts/"
  cat > "$T/root/state/board.md" <<'EOF'
# test board

<!-- READY:BEGIN -->
updated: 2026-10-17 10:00 · by test
- t1 | docs | ready | 测试条目
<!-- READY:END -->

> ## 历史段
EOF
  # $1 = progress|stall：progress 时改 updated 行并把 t1 标 human
  cat > "$T/bin/opencode" <<EOF
#!/usr/bin/env bash
cat > /dev/null
if [ "$1" = progress ]; then
  sed -i.bak -e 's/^updated: .*/updated: 2026-10-17 11:00 · by stub/' -e 's/| ready |/| human |/' "$T/root/state/board.md"
fi
exit 0
EOF
  chmod +x "$T/bin/opencode"
}

run_loop() {
  PATH="$T/bin:$PATH" PDRS_LOOP_LOCK="$T/lock" PDRS_LOOP_STOP="$T/stop" \
    bash "$T/root/scripts/pdrs-loop.sh" --runner opencode --max-iter 5 --iter-timeout 30 > "$T/out.log" 2>&1
}

# ① 推进
setup progress
set +e; run_loop; rc=$?; set -e
L="$(ls "$T"/root/_tmp/*-pdrs-loop/ledger-v2.tsv)"
check "推进：正常停 exit 0" '[ "$rc" = 0 ]'
check "推进：账本 status=done 且 updated_after 已变" '[ "$(sed -n 2p "$L" | cut -f5)" = done ] && [ "$(sed -n 2p "$L" | cut -f7)" = "2026-10-17 11:00" ]'

# ② 不推进
setup stall
set +e; run_loop; rc=$?; set -e
L="$(ls "$T"/root/_tmp/*-pdrs-loop/ledger-v2.tsv)"
check "不推进：守卫停 exit 1" '[ "$rc" = 1 ]'
check "不推进：两行 no-progress" '[ "$(cut -f5 "$L" | grep -c "^no-progress$")" = 2 ]'
check "不推进：第二迭代任务书带「先处置」" 'grep -q "先处置" "$T"/root/_tmp/*-pdrs-loop/run-*/iter-002/prompt.md'

# ③ 再起一次：前一跑原件不被覆盖
first="$(ls -d "$T"/root/_tmp/*-pdrs-loop/run-*/iter-001)"
echo ORIGINAL >> "$first/prompt.md"
sleep 1
set +e; run_loop; set -e
check "再起：出现第二个 run-* 目录" '[ "$(ls -d "$T"/root/_tmp/*-pdrs-loop/run-* | wc -l | tr -d " ")" = 2 ]'
check "再起：前一跑 iter-001 原件未被覆盖" 'tail -n 1 "$first/prompt.md" | grep -q ORIGINAL'
check "锁已释放" '[ ! -e "$T/lock" ]'

echo "test-pdrs-loop: $([ "$fails" = 0 ] && echo OK || echo "$fails FAIL")"
[ "$fails" = 0 ]
