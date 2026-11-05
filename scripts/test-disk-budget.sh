#!/usr/bin/env bash
# test-disk-budget.sh — scripts/lib/disk-budget.sh 的规则测试。
# 三档：ok ≤160 GiB · warn (160,180] · hard >180 GiB，判断用 KiB 原值。
# 用法：bash scripts/test-disk-budget.sh     退出码 0 全过 · 1 有失败 · 2 前置错误
# 自足：在临时目录里放 du 桩（stub），不读真实磁盘、不依赖仓内 _build/_tmp。

set -u
# 环境里残留的阈值会让「默认值」用例假失败：先清掉，本脚本自设自清。
unset PDRS_DISK_TOTAL_GIB PDRS_DISK_HARD_GIB PDRS_DISK_WARN_GIB
LIB="$(cd "$(dirname "$0")" && pwd -P)/lib/disk-budget.sh"
[ -r "$LIB" ] || { echo "缺 $LIB" >&2; exit 2; }

GIB=1048576 # KiB

T=$(mktemp -d "${TMPDIR:-/tmp}/pdrs-diskbudget.XXXXXX") || exit 2
cleanup() { rm -rf "$T"; }
trap cleanup EXIT

fail=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }
eq() { [ "$2" = "$3" ] && ok "$1（$2）" || bad "$1：期望 $3，实得 $2"; }

# du 桩：按路径后缀返回 DU_BUILD_KIB / DU_TMP_KIB；DU_FAIL=1 模拟 du 失败（fail-closed 用例）
mkdir -p "$T/bin" "$T/root/_build" "$T/root/_tmp"
cat >"$T/bin/du" <<'STUB'
#!/usr/bin/env bash
# DU_FIRST_FAIL=1：第一次调用失败、之后正常（钉「失败先重试一次」这条路径本身）
if [ "${DU_FIRST_FAIL:-0}" = 1 ]; then
  n=$(cat "${DU_COUNT_FILE:-/dev/null}" 2>/dev/null || echo 0); n=$((n + 1))
  echo "$n" > "${DU_COUNT_FILE:-/dev/null}" 2>/dev/null || true
  [ "$n" = 1 ] && { echo "du: transient stat failure" >&2; exit 1; }
fi
[ "${DU_FAIL:-0}" = 1 ] && { echo "du: permission denied" >&2; exit 1; }
for a in "$@"; do last=$a; done
case $last in
  */_build) v=${DU_BUILD_KIB:-0} ;;
  */_tmp) v=${DU_TMP_KIB:-0} ;;
  *) echo "du stub: 未知路径 $last" >&2; exit 1 ;;
esac
printf '%s\t%s\n' "$v" "$last"
STUB
chmod +x "$T/bin/du"
PATH="$T/bin:$PATH"
export PATH

# shellcheck source=lib/disk-budget.sh
. "$LIB" || exit 2

# 1. 阈值默认值与「只许调低」
th() { ( . "$LIB" >/dev/null 2>&1; echo "$PDRS_DISK_TOTAL_GIB/$PDRS_DISK_HARD_GIB/$PDRS_DISK_WARN_GIB" ); }
eq "默认阈值" "$(th)" "200/180/160"
eq "HARD 调高被夹回" "$(PDRS_DISK_HARD_GIB=999 th)" "200/180/160"
eq "HARD 非数字回落" "$(PDRS_DISK_HARD_GIB=abc th)" "200/180/160"
eq "HARD 允许调低" "$(PDRS_DISK_HARD_GIB=100 th)" "200/100/100"

# 2. 求和：两个目录相加（首行数字，忽略路径）
eq "两目录求和" "$(DU_BUILD_KIB=$((3 * GIB)) DU_TMP_KIB=$((2 * GIB)) pdrs_disk_used_kib "$T/root")" "$((5 * GIB))"

# 3. 三档边界：等于阈值为低一档，超 1 KiB 才升级（容不得四舍五入）
lv() { DU_BUILD_KIB=$1 DU_TMP_KIB=0 pdrs_disk_level "$T/root"; }
lvk() { pdrs_disk_level_of_kib "$1"; }   # 生产调用方（pdrs-loop / new-tree）直接用的纯判定函数
eq "159 GiB → ok" "$(lv $((159 * GIB)))" ok
eq "160 GiB 整 → ok" "$(lv $((160 * GIB)))" ok
eq "160 GiB + 1 KiB → warn" "$(lv $((160 * GIB + 1)))" warn
eq "180 GiB 整 → warn" "$(lv $((180 * GIB)))" warn
eq "180 GiB + 1 KiB → hard" "$(lv $((180 * GIB + 1)))" hard
eq "level_of_kib 160 整 → ok（调用方入口）" "$(lvk $((160 * GIB)))" ok
eq "level_of_kib 160 + 1 KiB → warn（调用方入口）" "$(lvk $((160 * GIB + 1)))" warn
# 入参校验：空 / 非数字绝不能落到 else 打印 ok（`[ "" -gt N ]` 报错后两个分支都为假）
eq "level_of_kib 空串 → unknown" "$(lvk '')" "unknown"
eq "level_of_kib 空串 rc" "$(lvk '' >/dev/null 2>&1; echo $?)" 2
eq "level_of_kib 非数字 → unknown" "$(lvk abc)" "unknown"
eq "level_of_kib 非数字 rc" "$(lvk abc >/dev/null 2>&1; echo $?)" 2

# 4. 目录不存在按 0（远端宿主的 target 不在 _build/_tmp）
mkdir -p "$T/empty"
eq "无 _build/_tmp 计 0" "$(pdrs_disk_used_kib "$T/empty")" 0
eq "无 _build/_tmp → ok" "$(pdrs_disk_level "$T/empty")" ok

# 5. fail-closed：du 失败必须 rc=2，且不把「没量到」当成 0 放行
out=$(DU_FAIL=1 pdrs_disk_used_kib "$T/root" 2>/dev/null); rc=$?
eq "du 失败 rc" "$rc" 2
eq "du 失败无输出" "[$out]" "[]"
out=$(DU_FAIL=1 pdrs_disk_level "$T/root" 2>/dev/null); rc=$?
eq "du 失败 level rc" "$rc" 2
# 失败必须给出显式值：空串等于「不是 hard」，`[ "$lvl" = hard ]` 形式的调用方会静默放行
eq "du 失败 level 打印 unknown" "$out" "unknown"

# 4b. du 输出畸形（"12x345"）必须整条拒绝，不能前缀截断读成 12 KiB（偏小 = 假 PASS）
mkdir -p "$T/bin-bad"
printf '%s\n' '#!/usr/bin/env bash' 'echo -e "12x345\t$2"' > "$T/bin-bad/du"; chmod +x "$T/bin-bad/du"
out=$(PATH="$T/bin-bad:$PATH" pdrs_disk_used_kib "$T/root" 2>/dev/null); rc=$?
eq "du 畸形输出 rc" "$rc" 2

# 5d. 瞬时失败的重试路径本身：第一次 du 失败、重试成功 → 正常放行（把重试块删掉/不重新赋值时这条会红）
eq "首次 du 失败、重试成功 → 求和正确" \
  "$(DU_FIRST_FAIL=1 DU_COUNT_FILE="$T/cnt" DU_BUILD_KIB=$((3 * GIB)) DU_TMP_KIB=$((2 * GIB)) pdrs_disk_used_kib "$T/root")" "$((5 * GIB))"
eq "du 畸形输出无值" "[$out]" "[]"

# 5a. 仓根不存在 = 前置错误（拼错路径不能读成 0 GiB 放行）；仓根在、但没有 _build/_tmp 仍按 0
eq "仓根不存在 used rc" "$(pdrs_disk_used_kib /nonexistent/pdrs 2>/dev/null; echo "rc=$?")" "rc=2"
eq "仓根不存在 level rc" "$(pdrs_disk_level /nonexistent/pdrs 2>/dev/null; echo "rc=$?")" "unknown
rc=2"

# 5b. 符号链接：`du -sk` 对命令行 symlink 不跟随（BSD 实测 0 KiB），lib 必须先 cd + pwd -P。
#     本仓库的 runner 树确有 `_build` 指向外接卷的配置，量成 0 会让闸门静默失效。
mkdir -p "$T/data/_build" "$T/rootlink"
#     `bs` 用字节数而不是 `1m`：GNU dd（CI 的 ubuntu runner）不认小写 `m`，会直接失败，
#     blob 不落地 → 下一步量到空目录 4 KiB（11-05 CI a51-coverage 实测）
dd if=/dev/zero of="$T/data/_build/blob" bs=1048576 count=3 2>/dev/null || bad "fixture 写入 3 MiB blob 失败"
ln -s "$T/data/_build" "$T/rootlink/_build"
# 阈值取 2 MiB 而不是 1 MiB：空目录本身约 4 KiB、ext4 快符号链接可能占 ≥4 KiB，
# 门槛太低的话「漏掉 cd+pwd -P 的回归」在某些文件系统上仍会发绿（评审 P2）
k=$(PATH=/usr/bin:/bin pdrs_disk_used_kib "$T/rootlink")
[ "${k:-0}" -ge 2048 ] && ok "symlink 的 _build 量到实体（${k} KiB）" || bad "symlink 的 _build 量到 ${k:-?} KiB，应 ≥2048"

# 5c. 断链：du 对它返回 0 KiB 且 rc=0，必须 fail-closed 而不是静默放行
mkdir -p "$T/rootbroken"; ln -s /nonexistent/pdrs "$T/rootbroken/_build"
out=$(pdrs_disk_used_kib "$T/rootbroken" 2>/dev/null); rc=$?
eq "断链 _build rc" "$rc" 2
eq "断链 _build 无输出" "[$out]" "[]"

# 6. 判档入口拒绝「bash 解析不了的数字」：`[ 08 -gt N ]` / 超 int64 字面量都报错、两个分支都为假 → 落 else 打 ok
for v in "" abc 08 016 0x10 1e9 99999999999999999999; do
  o=$(pdrs_disk_level_of_kib "$v"); rc=$?
  [ "$o" = unknown ] && [ "$rc" = 2 ] && ok "判档拒绝『${v}』（unknown/2）" || bad "判档『${v}』→ [${o}] rc=${rc}，期望 unknown/2"
done

# 7. 主仓根策略（pdrs_disk_main_root）：从 linked worktree 里跑必须量主仓，量自己会恒判 ok（评审 r5 P1 的正控）
mkdir -p "$T/git/main"
if git -C "$T/git/main" init -q 2>/dev/null &&
  git -C "$T/git/main" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init 2>/dev/null &&
  git -C "$T/git/main" worktree add -q "$T/git/wt" 2>/dev/null; then
  eq "linked 树 → 主仓根" "$(cd "$T/git/wt" && pdrs_disk_main_root "$PWD")" "$(cd "$T/git/main" && pwd -P)"
  # 排障常在子目录里执行：`cd src && bash ../scripts/lib/disk-budget.sh` 不得把 src 当仓根（会打印 0 GiB → ok）
  mkdir -p "$T/git/main/src" "$T/git/wt/src"
  eq "主仓子目录 → 主仓根" "$(cd "$T/git/main/src" && pdrs_disk_main_root "$PWD")" "$(cd "$T/git/main" && pwd -P)"
  eq "linked 树子目录 → 主仓根" "$(cd "$T/git/wt/src" && pdrs_disk_main_root "$PWD")" "$(cd "$T/git/main" && pwd -P)"
  # 裸主仓没有工作区：首条 porcelain 是仓目录本身，量它必得 0 → 必须 fail-closed 而不是打 ok
  if git clone -q --bare "$T/git/main" "$T/git/bare.git" 2>/dev/null &&
    git -C "$T/git/bare.git" worktree add -q "$T/git/bwt" 2>/dev/null; then
    o=$(cd "$T/git/bwt" && pdrs_disk_main_root "$PWD"); rc=$?
    [ "$rc" = 2 ] && ok "裸主仓 → rc 2（${o}）" || bad "裸主仓 → rc=${rc} [${o}]，期望 2"
  else
    bad "fixture：裸主仓 + worktree 造不出来"
  fi
else
  bad "fixture：主仓 + linked 树造不出来"
fi
eq "子目录（非 git）→ 原样返回" "$(mkdir -p "$T/root/sub" && pdrs_disk_main_root "$T/root/sub")" "$T/root/sub"
eq "普通目录（非 git）→ 原样返回" "$(pdrs_disk_main_root "$T/root")" "$T/root"

# 8. 直接执行：打印三档与当前级别，并打出实测的仓根（在 linked 树里排障不会误读成「闸显示 ok」）
o=$(cd "$T/root" && DU_BUILD_KIB=$((170 * GIB)) bash "$LIB" 2>&1); rc=$?
eq "直接执行 rc" "$rc" 0
case $o in *"180 GiB"*"170 GiB"*warn*) ok "直接执行打印阈值与级别" ;; *) bad "直接执行输出：[$o]" ;; esac
case $o in *"实测根"*"/root"*) ok "直接执行打印实测根" ;; *) bad "直接执行未打印实测根：[$o]" ;; esac

echo "----"
[ "$fail" = 0 ] && echo "PASS test-disk-budget" || echo "FAILED test-disk-budget"
exit "$fail"
