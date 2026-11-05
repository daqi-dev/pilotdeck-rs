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
#   4   统一闸存在但给不出名额（--status 非 0 / 输出解不出名额 / 冷启动超时且本机无名额缓存 /
#       HOME 未设或不是目录导致闸路径无法确定 / 本机有 pdrs-ops 或共享池迹象但闸不可执行 /
#       名额缓存超过 24 h 未从闸刷新）→ fail-closed。冷启动超时的恢复路径是人工跑
#       `--refresh-cap`（预算 240 s ≥ 闸自己的单次 du 上限 180 s）：写完缓存后消费方只读缓存。
#   64  用法错误（传了旧的上限参数）
#   69  本机确实没有统一闸（EX_UNAVAILABLE：CI / 普通开发机：$HOME 下既无 pdrs-ops 也无共享池目录），
#       或调用方用 PDRS_CARGO_SLOT_BIN 显式声明「本机不用共享名额」→ 无共享名额可等，可以继续
#
# 为什么要有名额缓存（11-06，#1556 r1 P1-2）：`cargo-slot --status` 除名额行外还会算磁盘余量，
# 闸内 disk.cache（TTL 300 s）过期时会起 8 路 du 扫 _build+_tmp。本机实测冷缓存 31.8–38.6 s、
# 结构上限是闸自己的单次 du 超时 180 s；而消费方单次调用只给 10 s → 冷路径必超时。
# 名额是「按主机静态配置」，磁盘余量与本脚本的判定无关，所以把解析出的名额按 TTL 缓存（行格式
# `cap used ts lastok`，lastok = 最近一次真的从闸拿到名额的时间，只有成功刷新才推进）：
#   · 缓存新鲜（≤ PDRS_CARGO_SLOTS_CAP_TTL，默认 300 s）→ 直接读，毫秒级，不碰闸；
#   · 缓存过期 → 限时（默认 5 s；若 lastok 已超过 900 s 则改用冷启动预算，让慢闸有机会答完）问一次闸：
#     答了就用新值并推进 lastok；闸非 0 / 输出不可解析 → 退出 4（错误必须暴露）；
#     只是慢（限时内没答完，典型就是冷 du）→ 沿用旧缓存并盖 ts、**不推进 lastok**（#1556 r2：旧实现
#     无条件连时间戳一起回写，既能把 lastok 推进到「永远不用再问闸」，也能覆盖并发方刚刷到的新值）；
#   · lastok 超过 24 h → 退出 4（名额缓存陈旧到不可信，fail-closed，跑 --refresh-cap 恢复）；
#   · 本机没有缓存（首次）→ 给足冷启动预算（默认 240 s）问一次闸。预算必须盖过闸自己的单次
#     du 上限 180 s（cargo_gate.py 的 `timeout=180`），否则「闸本来答得出」的调用会被我们判超时
#     → exit 4 → 消费方当硬错（#1556 r4 boundary P1）。消费方 deadline 更短（多数手写站点 180 s）
#     时会先超时，那走的是 `TimeoutExpired`，属于「环境未取得判定」；预热用 `--refresh-cap`。
# 判定口径（#1556 r3，r4 修正）：ps 数出的进程树 `n` 与闸的 flock 占用 `used` 都要有余量才放行。
# 只看 n 会假放行：shim 先取槽位、后做磁盘 du，冷 du 那几十秒里 ps 只看到 python，n=0 而 used 已满。
# 但 `used` 是**快变量**（秒级变化），只能信「刚从闸拿到的」那一份：缓存里的 used 是写缓存那一刻的
# 快照，最长可到上一轮成功刷新（稳态下 refresh 常常答不进预算，见下），拿它判会两个方向都错——
# 旧的 used=0 假放行、旧的 used=cap 让整段 TTL 恒判「满」（车道空转，#1556 r4 两家评审）。
# 所以 used 只在「闸最近一次真的答复距今 ≤ USED_TTL（默认 60 s）」时参与判定，否则只按 n 判并
# 在输出里标注「快照不计入判定」。两个口径都只是观测：本脚本不预留名额（租约见 #1581）。
# 慢路径回写前重读缓存做 CAS：读到的那一行变了（别的进程刚刷新过）就不再回写，本次直接用它的新值。
# 缓存键含闸路径 / 主机名 / 名额相关环境变量，负控用桩闸时不会互相污染。
set -uo pipefail
CAP_TTL=${PDRS_CARGO_SLOTS_CAP_TTL:-300}
REFRESH_TIMEOUT=${PDRS_CARGO_SLOTS_REFRESH_TIMEOUT:-5}
BOOTSTRAP_TIMEOUT=${PDRS_CARGO_SLOTS_BOOTSTRAP_TIMEOUT:-240}
# `--refresh-cap` 是人工恢复路径，没有消费方 deadline，预算要盖过闸自己的单次 du 上限 180 s
# （cargo_gate.py 的 `timeout=180`）——跟 bootstrap 共用同一个数会让慢盘上「恢复」也救不回来
# （#1556 r3 boundary P1）；bootstrap 在 r4 也提到 240 s，两条路径同口径（#1556 r4 boundary P1）。
REFRESH_CAP_TIMEOUT=${PDRS_CARGO_SLOTS_REFRESH_CAP_TIMEOUT:-240}
# lastok 超过这个岁数就不再问 5 s，改用冷启动预算（#1556 r2：慢闸永远答不进 5 s 时，
# 靠这条把「永久冻结在首次 bootstrap 值」变成「15 min 后会有一次真的刷新」）。
FAT_REFRESH_AFTER=${PDRS_CARGO_SLOTS_FAT_REFRESH_AFTER:-900}
MAX_STALE=${PDRS_CARGO_SLOTS_MAX_STALE:-86400}
# 闸的 used 只有这么新才参与判定（见头注释「判定口径」）：更旧的一律当未知，只按 ps 判。
USED_TTL=${PDRS_CARGO_SLOTS_USED_TTL:-60}
REFRESH_ONLY=0
if [ "${1:-}" = "--refresh-cap" ]; then
  REFRESH_ONLY=1
  shift
fi
if [ $# -gt 0 ]; then
  echo "cargo-slots: 不再接受上限参数（收到「${1}」）；名额以统一闸 --status 为准" >&2
  exit 64
fi
# 时间参数只接受正整数（#1556 r2 P2、r3 P2）：非数字会让 `$((secs * 10))` 把 "abc" 当变量名，
# `set -u` 下脚本以 1（=已满）退出，调用方会空等到 deadline；前导零（08 / 09）能过 `-lt` 却在
# `$(( ))` 里按八进制解析（09 非法 → 算术错误、非交互 shell 退 1）；0 则让 MAX_STALE / FAT_REFRESH_AFTER
# 变成恒真。一律 fail-closed 到 4，别伪装成「已满」。
for knob in "$CAP_TTL" "$REFRESH_TIMEOUT" "$BOOTSTRAP_TIMEOUT" "$REFRESH_CAP_TIMEOUT" \
            "$FAT_REFRESH_AFTER" "$MAX_STALE" "$USED_TTL"; do
  case "$knob" in
    ''|*[!0-9]*|0*)
      echo "cargo-slots: 时间参数必须是正整数（收到「${knob}」；0 与前导零都不接受）→ fail-closed" >&2
      exit 4
      ;;
  esac
done
# 闸定位（#1556 r2 P1）：别把「闸路径解析不出来」当成「本机没有闸」——那会让 env -u HOME、
# 服务账号 HOME、掉 +x 的闸都静默放行到超名额。三分：
#   · PDRS_CARGO_SLOT_BIN 显式指定 = 调用方声明闸在哪（负控桩闸 / 换机）；不可执行按「显式 opt-in
#     的不用共享名额」处理 → 69（沿用 r1 契约）。
#   · 未指定且 HOME 未设 → 无法定位，fail-closed 4。
#   · 未指定、HOME 有但闸不可执行 → 本机有 pdrs-ops 目录或共享池目录就是「装了但坏了」→ 4；
#     两者都没有才是真的没有闸 → 69。
if [ -n "${PDRS_CARGO_SLOT_BIN:-}" ]; then
  GATE=$PDRS_CARGO_SLOT_BIN
  if [ ! -x "$GATE" ]; then
    echo "cargo-slots: PDRS_CARGO_SLOT_BIN=${GATE} 不可执行 → 按显式声明「本机不用共享名额」继续" >&2
    exit 69
  fi
elif [ -z "${HOME:-}" ] || [ ! -d "${HOME}" ]; then
  echo "cargo-slots: HOME 未设或不是目录（「${HOME:-}」），无法定位统一闸（\$HOME/ws/pdrs-ops/bin/cargo-slot）→ fail-closed" >&2
  exit 4
else
  GATE=$HOME/ws/pdrs-ops/bin/cargo-slot
  if [ ! -x "$GATE" ]; then
    POOL_DIR=/private/tmp/pdrs-cargo-gate
    [ "$(uname -s)" = Darwin ] || POOL_DIR=/tmp/pdrs-cargo-gate
    if [ -d "$HOME/ws/pdrs-ops" ] || [ -d "$POOL_DIR" ]; then
      echo "cargo-slots: 本机有 pdrs-ops 或共享池（${POOL_DIR}）迹象，但闸 ${GATE} 不可执行 → fail-closed" >&2
      exit 4
    fi
    echo "cargo-slots: 本机没有统一闸（${GATE} 不存在，$HOME 下也无 pdrs-ops / 共享池）→ 无共享名额可等" >&2
    exit 69
  fi
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

# 读缓存；命中且格式正确返回 0（CAP_CACHE_CAP/_USED/_TS/_LASTOK）。
# 行格式 `cap used ts lastok`；旧的三字段文件把 lastok 当成 ts（下限保护，不会因此永久免刷新）。
read_cap_cache() {
  CAP_CACHE_CAP=; CAP_CACHE_USED=; CAP_CACHE_TS=; CAP_CACHE_LASTOK=
  [ -f "$CACHE_FILE" ] || return 1
  { read -r c u t k _ <"$CACHE_FILE"; } 2>/dev/null || return 1
  case "${c:-}" in ''|*[!0-9]*|0*) return 1;; esac
  CAP_CACHE_CAP=$c
  CAP_CACHE_USED=${u:-0}
  { case "${CAP_CACHE_USED:-}" in ''|*[!0-9]*) CAP_CACHE_USED=0;; esac; }
  CAP_CACHE_TS=${t:-0}
  { case "${CAP_CACHE_TS:-}" in ''|*[!0-9]*) CAP_CACHE_TS=0;; esac; }
  CAP_CACHE_LASTOK=${k:-$CAP_CACHE_TS}
  { case "${CAP_CACHE_LASTOK:-}" in ''|*[!0-9]*) CAP_CACHE_LASTOK=$CAP_CACHE_TS;; esac; }
  return 0
}

# 原子写缓存（memory「signal-file-atomic」：tmp + rename，读者不会读到半行）。
# $3 = lastok：只有「真的从闸拿到名额」才推进它；沿用旧值时不推进（#1556 r2）。
write_cap_cache() {
  local tmp lastok
  lastok=${3:-$(date +%s)}
  mkdir -p "$CACHE_DIR" 2>/dev/null || return 0
  tmp=$CACHE_FILE.$$
  if printf '%s %s %s %s\n' "$1" "${2:-0}" "$(date +%s)" "$lastok" >"$tmp" 2>/dev/null; then
    mv -f "$tmp" "$CACHE_FILE" 2>/dev/null || rm -f "$tmp" 2>/dev/null
  else
    rm -f "$tmp" 2>/dev/null
  fi
  return 0
}

# 重新读缓存，判断「我读到的那一行」是否还是文件里的那一行（CAS）。
# 返回 0 = 没变（可以回写）；1 = 变了（别的进程刚刷新过，别覆盖）。
cache_unchanged() { # $1=cap $2=used $3=ts $4=lastok
  read_cap_cache || return 1
  [ "$CAP_CACHE_CAP" = "$1" ] && [ "$CAP_CACHE_USED" = "$2" ] \
    && [ "$CAP_CACHE_TS" = "$3" ] && [ "$CAP_CACHE_LASTOK" = "$4" ]
}

# 限时跑一次闸：结果在 GATE_OUT / GATE_RC；超时返回 1（并把闸连同它的 du 孙进程整组杀掉，
# 不把孤儿留给 init 去等 180 s）。超时用 job control 给子进程单独建进程组，`kill -- -pid` 整组清。
run_gate() {
  local secs=$1 out rc pid start timed_out=0
  out=$(mktemp "${TMPDIR:-/tmp}/cargo-slots-gate.XXXXXX") || return 1
  GATE_OUT=; GATE_RC=
  # 整段 stderr 丢弃：job control 在作业被杀时会打印「Terminated」通知，那不是本脚本的诊断信息，
  # 混进调用方的 slots.log 会把 stderr 干净的断言弄脏。闸自己的输出已经重定向进 $out。
  {
    set -m 2>/dev/null || true
    "$GATE" --status >"$out" 2>&1 &
    pid=$!
    set +m 2>/dev/null || true
    # 墙钟判定（bash 内建 $SECONDS），不用迭代计数：每轮还含一次 kill -0 的 fork，
    # 负载高时单轮 > 0.1 s，名义预算会被放大成 1.1–1.2 倍并顶穿消费方的硬 deadline
    # （#1556 r3 boundary P2）。
    start=$SECONDS
    while kill -0 "$pid" 2>/dev/null; do
      if [ $((SECONDS - start)) -ge "$secs" ]; then
        timed_out=1
        kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
        sleep 1
        kill -KILL -- "-$pid" 2>/dev/null || true
        break
      fi
      sleep 0.1
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
  query_cap_or_die "$REFRESH_CAP_TIMEOUT" "无法预热名额缓存"
  write_cap_cache "$CAP" "$USED" "$(date +%s)"
  echo "cargo-slots: 已缓存统一闸名额 ${USED}/${CAP}（${CACHE_FILE}）"
  exit 0
fi

CAP_SOURCE=
# 这份 USED 是「什么时候从闸拿到的」（cache 里记的 lastok / 本次现问的 now）；判定时按它算旧。
USED_TS=
if read_cap_cache; then
  now=$(date +%s)
  age=$((now - CAP_CACHE_TS))
  # 时钟回拨 / 未来时间戳不能夹成 0——那会把这份缓存无限期当成「刚写的」，闸再也不被问（#1556 r2）。
  [ "$age" -ge 0 ] 2>/dev/null || age=$((CAP_TTL + 1))
  stale=$((now - CAP_CACHE_LASTOK))
  # lastok 落到未来（写缓存后时钟回拨）不能夹成 0——那会把「24 h 没从闸刷新」整段回拨窗口
  # 变成永不触发，闸若同时一直答不进 5 s，缓存就永久冻结（#1556 r3 P2）。
  [ "$stale" -ge 0 ] 2>/dev/null || stale=$((MAX_STALE + 1))
  if [ "$stale" -gt "$MAX_STALE" ]; then
    echo "cargo-slots: 名额缓存已 ${stale}s 未从统一闸刷新（上限 ${MAX_STALE}s）→ fail-closed；跑 scripts/cargo-slots.sh --refresh-cap 恢复" >&2
    exit 4
  fi
  if [ "$age" -le "$CAP_TTL" ]; then
    CAP=$CAP_CACHE_CAP
    USED=$CAP_CACHE_USED
    CAP_SOURCE="缓存 ${age}s（闸内 ${stale}s 前刷新）"
    USED_TS=$CAP_CACHE_LASTOK
  else
    # lastok 太久没推进说明闸每次都答不进 5 s（典型：每次都在跑冷 du），改用冷启动预算问一次。
    budget=$REFRESH_TIMEOUT
    [ "$stale" -le "$FAT_REFRESH_AFTER" ] || budget=$BOOTSTRAP_TIMEOUT
    old_cap=$CAP_CACHE_CAP; old_used=$CAP_CACHE_USED
    old_ts=$CAP_CACHE_TS; old_lastok=$CAP_CACHE_LASTOK
    if run_gate "$budget"; then
      if [ "$GATE_RC" -ne 0 ]; then
        echo "cargo-slots: 统一闸 ${GATE} --status 失败（rc=${GATE_RC}）：${GATE_OUT} → fail-closed" >&2
        exit 4
      fi
      if ! parse_cap "$GATE_OUT"; then
        echo "cargo-slots: 解不出统一闸的名额（${GATE} --status 输出：${GATE_OUT}）→ fail-closed" >&2
        exit 4
      fi
      # 时间戳要在闸答完之后取：`now` 是问闸**之前**的时间，用它当 lastok/USED_TS 会让
      # used_age ≈ 闸本次应答耗时——闸慢过 USED_TTL 时，刚拿到的 used 立刻被判「陈旧」而弃用，
      # 等于白问一次（#1556 r5 boundary P1）。
      now=$(date +%s)
      write_cap_cache "$CAP" "$USED" "$now"
      CAP_SOURCE="闸刷新"
      USED_TS=$now
    else
      # 闸只是慢（典型是冷 du）：沿用旧名额继续本次判定，但 lastok 不推进，并且回写前做 CAS——
      # 别的进程若刚刷新过，就直接用它的新值，绝不把旧 cap 盖回去（#1556 r2 concurrency P2）。
      CAP=$old_cap
      USED=$old_used
      if cache_unchanged "$old_cap" "$old_used" "$old_ts" "$old_lastok"; then
        write_cap_cache "$old_cap" "$old_used" "$old_lastok"
        CAP_SOURCE="闸 ${budget}s 内无答复，沿用 ${age}s 前的缓存名额（闸内 ${stale}s 前刷新）"
        USED_TS=$old_lastok
      else
        CAP=$CAP_CACHE_CAP
        USED=$CAP_CACHE_USED
        if [ -z "$CAP" ]; then
          # 缓存文件在刷新窗口内被删（tmp 清理 / 他人 rm）：read_cap_cache 返回前已把
          # CAP_CACHE_* 清空，回落到本次读到的旧值，别让空 CAP 走到末端的整数比较（bash 报
          # integer expression expected → rc=2「脚本坏了」，#1556 r3 P3）。
          CAP=$old_cap
          USED=$old_used
          CAP_SOURCE="闸 ${budget}s 内无答复，沿用 ${age}s 前的缓存名额（缓存文件已被并发方删除）"
          USED_TS=$old_lastok
        else
          CAP_SOURCE="闸 ${budget}s 内无答复，改用并发方刚刷新的名额（闸内 $(( $(date +%s) - CAP_CACHE_LASTOK ))s 前刷新）"
          USED_TS=$CAP_CACHE_LASTOK
        fi
      fi
    fi
  fi
else
  query_cap_or_die "$BOOTSTRAP_TIMEOUT" "本机还没有名额缓存（可先跑 scripts/cargo-slots.sh --refresh-cap 预热）"
  write_cap_cache "$CAP" "$USED" "$(date +%s)"
  CAP_SOURCE="闸冷启动"
  USED_TS=$(date +%s)
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
# 判定（见头注释「判定口径」）：n 抓得到不走闸的直接 cargo，USED 抓得到「shim 已取槽、还没 exec
# 出 cargo」的冷 du 窗口。但 USED 只在它确实是最近从闸拿到的时候才算数——缓存里的快照既可能把
# 「已满」看成空（旧 used=0），也可能把「空」看成满（旧 used=cap 卡住整段 TTL），两个方向都会错。
now_end=$(date +%s)
used_age=$((now_end - USED_TS))
[ "$used_age" -ge 0 ] 2>/dev/null || used_age=$((USED_TTL + 1))
if [ "$used_age" -le "$USED_TTL" ]; then
  echo "cargo-slots: 在跑 ${n} / 上限 ${CAP}（名额来自统一闸 cargo-slot，${CAP_SOURCE}，闸内已占 ${USED}，${used_age}s 前取自闸）"
  [ "$n" -lt "$CAP" ] && [ "$USED" -lt "$CAP" ]
else
  echo "cargo-slots: 在跑 ${n} / 上限 ${CAP}（名额来自统一闸 cargo-slot，${CAP_SOURCE}，闸内占用快照 ${USED} 已 ${used_age}s 未从闸刷新，不计入判定）"
  [ "$n" -lt "$CAP" ]
fi
