#!/usr/bin/env bash
# test-worktree-count.sh — scripts/lib/worktree-count.sh 的规则测试。
# 口径：本仓全部 linked worktree，排除主仓工作区；等价 `git worktree list | wc -l` − 1。
# 用法：bash scripts/test-worktree-count.sh     退出码 0 全过 · 1 有失败 · 2 前置错误
# 自足：在临时目录自建仓库，不读写真实工作树。

set -u
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

# 4. 不依赖调用者 cwd：在别的仓库里、在仓外，传参都必须数对
cq=$(cd "$T/other" && pdrs_worktree_count "$T/repo")
eq "cwd 在别的仓库" "$cq" 1
cq=$(cd / && pdrs_worktree_count "$T/repo")
eq "cwd 在仓外" "$cq" 1

# 5. 不传参且 cwd 不在仓库内 → 必须报错退出 2，绝不静默算成 0
out=$(cd / && pdrs_worktree_count 2>/dev/null); rc=$?
eq "仓外不传参 rc" "$rc" 2
eq "仓外不传参无输出" "[$out]" "[]"

# 6. 上限只能调低：非数字 / >40 一律回落 40
cap() { ( . "$LIB" >/dev/null 2>&1; echo "$PDRS_WORKTREE_CAP" ); }
eq "默认上限" "$(cap)" 40
eq "PDRS_WORKTREE_CAP=999 被夹回 40" "$(PDRS_WORKTREE_CAP=999 cap)" 40
eq "PDRS_WORKTREE_CAP=abc 回落 40" "$(PDRS_WORKTREE_CAP=abc cap)" 40
eq "PDRS_WORKTREE_CAP=1 允许调低" "$(PDRS_WORKTREE_CAP=1 cap)" 1

# 7. bare 主仓：porcelain 首条仍是 worktree 行，只该砍掉主仓 1 条
git init -q --bare "$T/bare.git" || exit 2
git -C "$T/bare.git" fetch -q "$T/repo" HEAD || exit 2
git -C "$T/bare.git" worktree add -q --detach "$T/wt3" FETCH_HEAD || exit 2
eq "bare 仓 1 棵 linked" "$(pdrs_worktree_count "$T/bare.git")" 1

if [ "$fail" = 0 ]; then echo "PASS test-worktree-count"; else echo "FAILED test-worktree-count"; fi
exit "$fail"
