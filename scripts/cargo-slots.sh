#!/usr/bin/env bash
# cargo-slots.sh — 开编译/测试前看一眼：在跑的 cargo 进程是否已到统一闸的名额。
# 名额唯一来源：统一闸 `cargo-slot --status` 输出里的「名额 used/N」分母（cap 配置在
# ~/ws/pdrs-ops/scripts/cargo-slot/cargo_gate.py 的 SLOT_DEFAULTS，按主机：本机 2 / cb8 4 /
# macmini 2 / mbp2 2，11-05 21:4x 人定）。本脚本不再自带数字——旧默认 4 与真闸 2 是两套口径，
# 人照着跑会把「满」看成「有空位」，放行到超名额后白排队。
# 可用 PDRS_CARGO_SLOT_BIN 指定闸的位置（默认 ~/ws/pdrs-ops/bin/cargo-slot），供换机 / 负控使用。
# 计数口径：命令以 cargo 可执行文件开头的进程（含 build/test/check/clippy/run/bench/doc，及
# cargo-clippy 等子命令二进制），不数 rustc（它是 cargo 的子进程，数了会重复）。
# 同一进程树只算一次：`cargo clippy` 会派生 `cargo-clippy`，其下再派生 `cargo check`，三层首词都匹配 cargo(-xxx)，
# 逐个 grep 会把一次 clippy 记成 2–3 次（11-05 巡检据此误判「7–8 棵树绕过闸门」，实际一条都没绕过；
# fleet.py 已按祖先去重，本脚本对齐）。
#
# 用法：scripts/cargo-slots.sh             （不再接受上限参数：名额由统一闸决定）
#       scripts/cargo-slots.sh --refresh-cap（只刷新名额缓存，供换机 / 首次预热；不数进程）
#
# 退出码（调用方按此分流；2 / 3 / 4 / 64 一律 fail-closed，不得当成「有名额」）：
#   0   有空位
#   1   已满（调用方等待后重试）
#   2   保留：本脚本从不主动使用。bash 自身语法错误会返回 2，调用方见到就是「脚本坏了」，不得放行。
#   3   取样失败（ps 失败 / 输出为空），无法判断名额
#   4   统一闸存在但给不出名额（--status 非 0 / 输出解不出名额 / 冷启动超时且本机无名额缓存）→ fail-closed
#   64  用法错误（传了旧的上限参数）
#   69  本机没有统一闸（EX_UNAVAILABLE：CI / 普通开发机）→ 无共享名额可等，可以继续
#
# 为什么要有名额缓存（11-06，#1556 r1 P1-2）：`cargo-slot --status` 除名额行外还会算磁盘余量，
# 闸内 disk.cache（TTL 300 s）过期时会起 8 路 du 扫 _build+_tmp。本机实测冷缓存 38.6 s、
# 结构上限是闸自己的单次 du 超时 180 s；而消费方单次调用只给 10 s → 冷路径必超时。
# 名额是「按主机静态配置」，磁盘余量与本脚本的判定无关，所以把解析出的名额按 TTL 缓存：
#   · 缓存新鲜（≤ PDRS_CARGO_SLOTS_CAP_TTL，默认 300 s）→ 直接读，毫秒级，不碰闸；
#   · 缓存过期 → 限时（默认 5 s）问一次闸：答了就用新值；闸非 0 / 输出不可解析 → 退出 4（错误必须暴露）；
#     只是慢（限时内没答完，典型就是冷 du）→ 沿用旧缓存并盖时间戳，不再每次都白等；
#   · 本机没有缓存（首次）→ 给足冷启动预算（默认 120 s）问一次闸。
# 缓存键含闸路径 / 主机名 / 名额相关环境变量，负控用桩闸时不会互相污染。
set -uo pipefail
GATE=${PDRS_CARGO_SLOT_BIN:-$HOME/ws/pdrs-ops/bin/cargo-slot}
CAP_TTL=${PDRS_CARGO_SLOTS_CAP_TTL:-300}
REFRESH_TIMEOUT=${PDRS_CARGO_SLOTS_REFRESH_TIMEOUT:-5}
BOOTSTRAP_TIMEOUT=${PDRS_CARGO_SLOTS_BOOTSTRAP_TIMEOUT:-120}
REFRESH_ONLY=0
if [ "${1:-}" = "--refresh-cap" ]; then
  REFRESH_ONLY=1
  shift
fi
if [ $# -gt 0 ]; then
  echo "cargo-slots: 不再接受上限参数（收到「${1}」）；名额以统一闸 ${GATE} --status 为准" >&2
  exit 64
fi
if [ ! -x "$GATE" ]; then
  echo "cargo-slots: 本机没有统一闸（${GATE} 不存在或不可执行）→ 无共享名额可等" >&2
  exit 69
fi
if [ -n "${PDRS_CARGO_SLOTS_CACHE_DIR:-}" ]; then
  CACHE_DIR=$PDRS_CARGO_SLOTS_CACHE_DIR
elif [ -n "${PDRS_CARGO_SLOT_DIR:-}" ]; then
  CACHE_DIR=$PDRS_CARGO_SLOT_DIR
elif [ "$(uname -s)" = Darwin ]; then
  CACHE_DIR=/private/tmp/pdrs-cargo-gate
else
  CACHE_DIR=/tmp/pdrs-cargo-gate
fi
cache_key=$(printf '%s\n' "$GATE" "${PDRS_CARGO_SLOT_N-}" "${PDRS_CARGO_SLOTS-}" \
  "${PDRS_WORKER_CARGO_SLOTS-}" "$(uname -n)" | cksum | awk '{print $1}')
CACHE_FILE=$CACHE_DIR/cap-cache.${cache_key:-nocache}

# 从 `--status` 输出解出 cap / used；解不出返回 1。
parse_cap() {
  local pair
  pair=$(printf '%s\n' "$1" | sed -n 's/.*名额[[:space:]]*\([0-9][0-9]*\)\/\([0-9][0-9]*\).*/\1 \2/p' | head -1)
  [ -n "$pair" ] || return 1
  USED=${pair%% *}
  CAP=${pair#* }
  CAP="${CAP//[[:space:]]/}"
  USED="${USED//[[:space:]]/}"
  # 只接受正整数；0、负数、非数字一律报错，不静默变成「永远满」（#1287 r1 P2-1）
  case "$CAP" in ''|*[!0-9]*|0*) return 1;; esac
  case "$USED" in ''|*[!0-9]*) USED=0;; esac
  return 0
}

# 读缓存；命中且格式正确返回 0（CAP_CACHE_CAP/_USED/_TS）。
read_cap_cache() {
  CAP_CACHE_CAP=; CAP_CACHE_USED=; CAP_CACHE_TS=
  [ -f "$CACHE_FILE" ] || return 1
  { read -r c u t _ <"$CACHE_FILE"; } 2>/dev/null || return 1
  case "${c:-}" in ''|*[!0-9]*|0*) return 1;; esac
  CAP_CACHE_CAP=$c
  CAP_CACHE_USED=${u:-0}
  { case "${CAP_CACHE_USED:-}" in ''|*[!0-9]*) CAP_CACHE_USED=0;; esac; }
  CAP_CACHE_TS=${t:-0}
  { case "${CAP_CACHE_TS:-}" in ''|*[!0-9]*) CAP_CACHE_TS=0;; esac; }
  return 0
}

# 原子写缓存（memory「signal-file-atomic」：tmp + rename，读者不会读到半行）。
write_cap_cache() {
  local tmp
  mkdir -p "$CACHE_DIR" 2>/dev/null || return 0
  tmp=$CACHE_FILE.$$
  if printf '%s %s %s\n' "$1" "${2:-0}" "$(date +%s)" >"$tmp" 2>/dev/null; then
    mv -f "$tmp" "$CACHE_FILE" 2>/dev/null || rm -f "$tmp" 2>/dev/null
  else
    rm -f "$tmp" 2>/dev/null
  fi
  return 0
}

# 限时跑一次闸：结果在 GATE_OUT / GATE_RC；超时返回 1（并把闸连同它的 du 孙进程整组杀掉，
# 不把孤儿留给 init 去等 180 s）。超时用 job control 给子进程单独建进程组，`kill -- -pid` 整组清。
run_gate() {
  local secs=$1 out rc pid waited timed_out=0
  out=$(mktemp "${TMPDIR:-/tmp}/cargo-slots-gate.XXXXXX") || return 1
  GATE_OUT=; GATE_RC=
  # 整段 stderr 丢弃：job control 在作业被杀时会打印「Terminated」通知，那不是本脚本的诊断信息，
  # 混进调用方的 slots.log 会把 stderr 干净的断言弄脏。闸自己的输出已经重定向进 $out。
  {
    set -m 2>/dev/null || true
    "$GATE" --status >"$out" 2>&1 &
    pid=$!
    set +m 2>/dev/null || true
    waited=0
    while kill -0 "$pid" 2>/dev/null; do
      if [ "$waited" -ge "$((secs * 10))" ]; then
        timed_out=1
        kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
        sleep 1
        kill -KILL -- "-$pid" 2>/dev/null || true
        break
      fi
      sleep 0.1
      waited=$((waited + 1))
    done
    wait "$pid" 2>/dev/null
    rc=$?
  } 2>/dev/null
  GATE_OUT=$(cat "$out" 2>/dev/null)
  rm -f "$out" 2>/dev/null
  [ "$timed_out" = 0 ] || return 1
  GATE_RC=$rc
  return 0
}

# 问闸并校验；成功则填 CAP/USED，失败按退出码语义退出。
query_cap_or_die() { # $1 = 限时秒数，$2 = 失败提示
  if ! run_gate "$1"; then
    echo "cargo-slots: 统一闸 ${GATE} ${1}s 内没有答复，$2 → fail-closed" >&2
    exit 4
  fi
  if [ "$GATE_RC" -ne 0 ]; then
    echo "cargo-slots: 统一闸 ${GATE} --status 失败（rc=${GATE_RC}）：${GATE_OUT} → fail-closed" >&2
    exit 4
  fi
  if ! parse_cap "$GATE_OUT"; then
    echo "cargo-slots: 解不出统一闸的名额（${GATE} --status 输出：${GATE_OUT}）→ fail-closed" >&2
    exit 4
  fi
}

if [ "$REFRESH_ONLY" = 1 ]; then
  query_cap_or_die "$BOOTSTRAP_TIMEOUT" "无法预热名额缓存"
  write_cap_cache "$CAP" "$USED"
  echo "cargo-slots: 已缓存统一闸名额 ${USED}/${CAP}（${CACHE_FILE}）"
  exit 0
fi

CAP_SOURCE=
if read_cap_cache; then
  now=$(date +%s)
  age=$((now - CAP_CACHE_TS))
  [ "$age" -ge 0 ] 2>/dev/null || age=0
  if [ "$age" -le "$CAP_TTL" ]; then
    CAP=$CAP_CACHE_CAP
    USED=$CAP_CACHE_USED
    CAP_SOURCE="缓存 ${age}s"
  else
    if run_gate "$REFRESH_TIMEOUT"; then
      if [ "$GATE_RC" -ne 0 ]; then
        echo "cargo-slots: 统一闸 ${GATE} --status 失败（rc=${GATE_RC}）：${GATE_OUT} → fail-closed" >&2
        exit 4
      fi
      if ! parse_cap "$GATE_OUT"; then
        echo "cargo-slots: 解不出统一闸的名额（${GATE} --status 输出：${GATE_OUT}）→ fail-closed" >&2
        exit 4
      fi
      write_cap_cache "$CAP" "$USED"
      CAP_SOURCE="闸刷新"
    else
      # 闸只是慢（典型是冷 du）：名额是主机静态配置，沿用旧缓存并盖时间戳，避免每次预检都白等。
      CAP=$CAP_CACHE_CAP
      USED=$CAP_CACHE_USED
      CAP_SOURCE="闸 ${REFRESH_TIMEOUT}s 内无答复，沿用 ${age}s 前的缓存名额"
      write_cap_cache "$CAP" "$USED"
    fi
  fi
else
  query_cap_or_die "$BOOTSTRAP_TIMEOUT" "本机还没有名额缓存（可先跑 scripts/cargo-slots.sh --refresh-cap 预热）"
  write_cap_cache "$CAP" "$USED"
  CAP_SOURCE="闸冷启动"
fi

# 一次 ps 取全 pid/ppid/args，再从叶子往上归并：沿父指针上溯，祖先链里已经有 cargo 的不再另计，
# 只剩「祖先里没有 cargo」的 cargo 作为每棵进程树的根，数根即数并发槽位。按整条祖先链而不是只看直接父进程，
# 是为了覆盖 cargo → rustc → build-script → cargo（build.rs 里再起 cargo）这类中间夹了非 cargo 进程的同一条树。
# 单次 ps 取样，避免多次取样之间进程生灭造成的数字不一致。
# 匹配 args 的首词而不是 comm：macOS 同时输出 pid,ppid,comm,args 时 comm 会被压到 16 列（/Users/da/.rustu），
# args 才是完整命令行。
# 加 -ww 不限宽：BSD ps 接终端时按终端宽截断末列，rustup 的 cargo 全路径约 66 列，80 列终端减去 pid/ppid 两列
# 正好会把它切成 `carg` 而漏计（两个角色独立提出，cb8 上 -Aww 同样可用）。
# ps 失败不能当成「在跑 0」放行：`set -uo pipefail` 没有 -e，命令替换失败不会中断，
# awk 对空输入照样 `print c+0`，闸门就变成假 PASS（与 delivery-orphan「探测失败不再被当成没人」同类）。
snap=$(ps -Aww -o pid=,ppid=,args=) || { echo "cargo-slots: ps 取样失败，无法判断名额（不按 0 放行）" >&2; exit 3; }
[ -n "$snap" ] || { echo "cargo-slots: ps 输出为空，无法判断名额（不按 0 放行）" >&2; exit 3; }
n=$(printf '%s\n' "$snap" | awk '
  { par[$1]=$2; exe=$3; sub(/.*\//,"",exe)
    if (exe ~ /^cargo(-[a-z]+)?$/) isc[$1]=1 }
  END { for (p in isc) {
          q=par[p]; d=0
          while ((q in par) && !(q in isc) && d++ < 4096) q=par[q]
          if (!(q in isc)) c++
        }
        print c+0 }')
echo "cargo-slots: 在跑 ${n} / 上限 ${CAP}（名额来自统一闸 cargo-slot，${CAP_SOURCE}，闸内已占 ${USED}）"
[ "$n" -lt "$CAP" ]
