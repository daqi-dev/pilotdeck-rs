#!/usr/bin/env bash
# test-worktree-count.sh — scripts/lib/worktree-count.sh 的规则测试。
# 口径：本仓全部 linked worktree，排除主仓工作区；等价 `git worktree list | wc -l` − 1。
# 用法：bash scripts/test-worktree-count.sh     退出码 0 全过 · 1 有失败 · 2 前置错误
# 自足：在临时目录自建仓库，不读写真实工作树。

set -u
# 环境里残留 PDRS_WORKTREE_CAP 会让下面「默认 40」等用例假失败：先清掉，本脚本自设自清。
unset PDRS_WORKTREE_CAP
LIB="$(cd "$(dirname "$0")" && pwd -P)/lib/worktree-count.sh"
[ -r "$LIB" ] || { echo "缺 $LIB" >&2; exit 2; }

T=$(mktemp -d "${TMPDIR:-/tmp}/pdrs-wtcount.XXXXXX") || exit 2
cleanup() {
  for w in "$T"/wt*; do [ -d "$w" ] && git -C "$T/repo" worktree remove --force "$w" >/dev/null 2>&1; done
  rm -rf "$T"
}
trap cleanup EXIT

fail=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }
eq() { [ "$2" = "$3" ] && ok "$1（$2）" || bad "$1：期望 $3，实得 $2"; }

git init -q "$T/repo" || exit 2
git -C "$T/repo" -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m init || exit 2
git init -q "$T/other" || exit 2
git -C "$T/other" -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m init || exit 2

# shellcheck source=lib/worktree-count.sh
. "$LIB" || exit 2

# 1. 只有主仓 → 0
eq "空仓 0 棵" "$(pdrs_worktree_count "$T/repo")" 0

# 2. 加树 → +1，且与 git 自己的口径一致
git -C "$T/repo" worktree add -q --detach "$T/wt1" || exit 2
eq "加 1 棵" "$(pdrs_worktree_count "$T/repo")" 1
eq "与 list-1 一致" "$(pdrs_worktree_count "$T/repo")" "$(( $(git -C "$T/repo" worktree list | wc -l) - 1 ))"

git -C "$T/repo" worktree add -q --detach "$T/wt2" || exit 2
eq "加第 2 棵" "$(pdrs_worktree_count "$T/repo")" 2

# 3. 删树 → −1
git -C "$T/repo" worktree remove --force "$T/wt1" || exit 2
eq "删 1 棵后" "$(pdrs_worktree_count "$T/repo")" 1

# 3b. 明细必须是「除主仓之外的那几棵」：行数与集合都对，主仓路径不得出现
#     （只砍首条 / 少砍一条都会在这里露馅，计数相同但集合不同）
pl=$(pdrs_worktree_paths "$T/repo")
eq "明细行数" "$(printf '%s\n' "$pl" | awk 'END { print NR + 0 }')" 1
case "$pl" in
  */wt2) ok "明细是 wt2" ;;
  *) bad "明细是 wt2：实得 [$pl]" ;;
esac
case "$pl" in
  *"$T/repo") bad "明细混进主仓 [$pl]" ;;
  *) ok "明细不含主仓" ;;
esac

# 4. 不依赖调用者 cwd：在别的仓库里、在仓外，传参都必须数对
cq=$(cd "$T/other" && pdrs_worktree_count "$T/repo")
eq "cwd 在别的仓库" "$cq" 1
cq=$(cd / && pdrs_worktree_count "$T/repo")
eq "cwd 在仓外" "$cq" 1

# 5. 不传参且 cwd 不在仓库内 → 必须报错退出 2，绝不静默算成 0
out=$(cd / && pdrs_worktree_count 2>/dev/null); rc=$?
eq "仓外不传参 rc" "$rc" 2
eq "仓外不传参无输出" "[$out]" "[]"

# 6. 默认 = 人定上限 40（放宽到 60 待人批）；60 只能显式开；非数字 / 0 / >60 一律回落 40
cap() { ( . "$LIB" >/dev/null 2>&1; echo "$PDRS_WORKTREE_CAP" ); }
eq "默认上限（人定 40）" "$(cap)" 40
eq "PDRS_WORKTREE_CAP=60 显式生效（待人批能力上限）" "$(PDRS_WORKTREE_CAP=60 cap)" 60
eq "PDRS_WORKTREE_CAP=999 回落 40（笔误不当放宽）" "$(PDRS_WORKTREE_CAP=999 cap)" 40
eq "PDRS_WORKTREE_CAP=abc 回落 40" "$(PDRS_WORKTREE_CAP=abc cap)" 40
eq "PDRS_WORKTREE_CAP=0 回落 40" "$(PDRS_WORKTREE_CAP=0 cap)" 40
eq "PDRS_WORKTREE_CAP=1 允许调低" "$(PDRS_WORKTREE_CAP=1 cap)" 1

# 7. bare 主仓：porcelain 首条仍是 worktree 行，只该砍掉主仓 1 条
git init -q --bare "$T/bare.git" || exit 2
git -C "$T/bare.git" fetch -q "$T/repo" HEAD || exit 2
git -C "$T/bare.git" worktree add -q --detach "$T/wt3" FETCH_HEAD || exit 2
eq "bare 仓 1 棵 linked" "$(pdrs_worktree_count "$T/bare.git")" 1

if [ "$fail" = 0 ]; then echo "PASS test-worktree-count"; else echo "FAILED test-worktree-count"; fi
exit "$fail"
