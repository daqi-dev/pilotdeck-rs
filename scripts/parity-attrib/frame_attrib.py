#!/usr/bin/env python3
"""#1811 选项 A：帧级归因 + 反证判据（只读，不改任何冻结件、不改尺子）。

用法（必须 -I）：
  python3 -I frame_attrib.py <rounds 目录> <场景>
  python3 -I frame_attrib.py <rounds 目录> <场景> --negctl-out <独占新建目录>
  rounds 目录下同时含 ts-r1..ts-rN 与 rust-r1..rust-rN（parity-full 的落盘布局）。

判据（这就是「#6 归因」的可机检形式，逐 pair 判）：
  C0 冻结比较器必须**真的比成了**：进程级（超时 / 输出不可解析 / 输出里没有该场景）或
     进程内自报（`triggered_rules` 含 `trace_input` / `trace_identity`）都算「未运行」
     —— 一律判 UNKNOWN，**不**折成 NOT_6 反证。
  C1 场景 ∈ {deadline, deadline_during_tool}（accepted-diffs #6 的登记范围）
  C2 冻结比较器 G03 对该 pair 报出的语义差异集 **恰好** 是 1 条：
       left = null，right.kind = durable.status，right.event = gateway_submit_failed，
       right.statusKind = error，right.text = TS 宿主常量
     —— 多一条、少一条、换成别的 event、或 left 非 null（替换而非新增）都不算 #6。
  C3 该 gsf 记录在右侧 trace 里恰好 1 条、在左侧 0 条，且它的序号严格晚于
     harness.proof{state=result_unknown_fail_closed}。
  C4 右侧 sidecar 是 Rust 侧（identity 帧自证），且 sidecar crate 内无 status / 持久化写路径
     （由本脚本外部的源码 grep 提供，见 README 与 attr-grep.txt）。
  C5 删掉右侧那条 gsf 后，两侧帧集合（多重集）必须完全相同。
     **豁免 `requestEvidence`**：对齐冻结口径 `specs/parity/normalization.md:32`
     「G03 与原版严格 comparator 都去掉 requestEvidence 后再比语义」与
     `scripts/parity/g03_compare.py` 的 `pop("requestEvidence")`（两处）。
     残余（README 已写明）：C5 因此**不再**覆盖「单侧多出 requestEvidence」这一类差异。
  C6 两侧同名帧**都存在** `requestEvidence` 时，规范化（sort_keys + 紧凑）后必须逐字节相同。
     单侧存在不在 C6 覆盖范围（那是 C5 的残余，见 README）；两侧都不存在则不适用。

四种结论（`judge()`）：`ATTRIB_6` · `PASS` · `NOT_6`（反证命中）· `UNKNOWN`（比较器没比成 /
未产出该场景 / C0 自报没比成 —— **未判定**，既不是通过也不是反证）。
反证（「不是 #6 会长什么样」）：C1–C6 任一不满足即判 NOT_6。
negctl 子命令用真实 trace 的变异副本证明这些判据不是空话；其中
`L-single-side-requestEvidence` 期望 ATTRIB_6 —— 它证明 C5 的豁免**确实生效**，
是已登记的残余而不是漏判（见 README「C5 残余」）；`N` / `O` 期望 UNKNOWN —— 它们证明
C0 的自报路径确实把「没比成」判成未判定，而不是折回 NOT_6 反证。

退出码（`run`）：0 归因成功 · 2 反证命中 · 5 有效轮次不足 · 6 批次不完整 · 7 全 PASS 无 FAIL · 8 未判定 ·
9 输出写失败（ENOSPC / EIO，日志被截断，不算完整证据）· 64 **用法/环境错误**（缺参数 / 场景不在
`SCENARIOS` / 没带 `-I` / 找不到仓库根：不在册场景会被 C1 判成 NOT_6，用法错误不得冒充「判据被证伪」）。
退出码（`--negctl-out`）：0 全部按期望命中 · 1 证据目录不可用（已存在不覆盖 / 无法创建，
不覆盖证据）· 64 **用法/环境错误**（场景不在 `SCENARIOS`：负控期望表以在册场景为前提，未登记场景会让
A/L/N/O 全落成 C1-NOT_6 → 假 `MISMATCH`/rc=4；同码也含没带 `-I` / 找不到仓库根）· 3 基线不可用
（无有效轮次 / 轮次目录名不合规 / 基线缺本装置要改的变异点，**数据问题，不下判据结论**）·
4 判据被证伪 · 8 有负控未判定（4 优先于 8，汇总行报真正的退出码）· 9 装置自身崩溃（已删半成品
证据目录；与 `1` 区分开，免得把崩溃读成「目录名已占用」）。
进程被 SIGTERM 打断时也会删掉半成品证据目录（入口即装 handler + atexit 兜底），退出码是 143
而不是 `3`「基线不可用」：中断不是数据问题（`rounds()` 抛 `SystemExit(str)`，handler 抛
`SystemExit(143)`，按 code 类型分开）。
stdout **与 stderr** 统一套 `_PipeSafe`：输出接 `| head` 这类短读者时退 120（缓冲）或 1（`-u`）的
老毛病不再出现（`run()` 的无效轮次清单走 stderr，`2>&1 | head -1` 下同样会撞）。
只吞断管道（`BrokenPipeError`）；其余写失败（ENOSPC / EIO → 日志被截断）**不静默**，
统一退 rc=9：截断的日志不能被当成完整证据。
完整语义见 README「退出码」。
"""
import argparse
import atexit
import glob
import json
import math
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import traceback
from collections import Counter

# 命令行**用法错误**的统一退出码：缺参数 / 场景不在册 / 没带 `-I` / 找不到仓库根。
# 不能沿用 2：`run` 的 2 是「反证命中（已证伪）」，用法错误落 2 会被按 rc 判停的复查读成
# 「判据被证伪」；也不能沿用 1：negctl 的 1 是「证据目录不可用」，会被读成「换个目录名重跑」
# （#2031 r2 sibling/boundary P2）。64 = sysexits 的 EX_USAGE，不进任何结论表。
EXIT_USAGE = 64


def _silence_stream(stream):
    """把 stream 的 fd 接到 /dev/null。**先取 fd、再开 /dev/null**：`fileno()` 抛
    `io.UnsupportedOperation`（`redirect_stdout(StringIO())` 形状，OSError 子类）时不会
    先开出一个没人关的 fd；无论后面哪步失败，`os.close(devnull)` 都在 `finally` 里
    （`#2031` 第 5 条：旧实现先 open 再 fileno，异常时漏 fd）。

    定义在文件顶部：module 级守卫（`-I` / `_repo_root()`）在 `_PipeSafe` 之前就要用它
    兜住断管道（`#2031` r4）。
    """
    try:
        fd = stream.fileno()
    except (OSError, ValueError):
        return
    try:
        devnull = os.open(os.devnull, os.O_WRONLY)
    except OSError:
        return
    try:
        os.dup2(devnull, fd)
    except OSError:
        pass
    finally:
        try:
            os.close(devnull)
        except OSError:
            pass


def _usage_exit(msg=None, emit=None):
    """用法 / 环境错误的**唯一**出口：只退 `EXIT_USAGE`，消息写不出去也照退。

    module 级守卫（非 `-I`、`_repo_root()` 找不到根）跑在 `main()` 装配 `_PipeSafe`
    **之前**，裸 `print` 撞断管道就是另一条假码：默认缓冲下 120（解释器退出 flush 失败）、
    `-u` 下 1（`BrokenPipeError` 逃出 → traceback，在 negctl 表里等于「证据目录不可用」）。
    实测（读端预先关闭）改前 120 / 1，改后 64（`#2031` r4 concurrency + boundary 同点 P2）。
    所以**只包发送这一段**：发送失败一律吞掉——一句诊断丢了不改变「这是用法错误」，
    换个退出码才会；吞掉之前先把该流接到 `/dev/null`，否则解释器退出时的最后一次 flush
    会再把 64 改写成 120。`UnicodeEncodeError`（`PYTHONIOENCODING=ascii`）同理。
    """
    try:
        if emit is not None:
            emit()
        if msg is not None:
            print(msg, file=sys.stderr)
        sys.stderr.flush()
    except Exception:  # 仅覆盖发送；SystemExit 是 BaseException，不受影响
        _silence_stream(sys.stderr)
    raise SystemExit(EXIT_USAGE)


if not sys.flags.isolated:
    _usage_exit(f'用法错误：本装置必须用 `python3 -I` 运行（隔离模式；rc={EXIT_USAGE}）')


def _repo_root():
    """从脚本所在目录向上找含 scripts/parity/g03_compare.py 的仓库根。

    装置同时存在于 `_tmp/`（历史副本）与 `scripts/parity-attrib/`（入仓副本），
    两份副本都在仓库内，向上必能命中。**找不到就报错**：绝不回退到硬编码的机器路径，
    否则被审树与比较器可能来自不同 HEAD，归因结论不可信（#1926 评审 P1）。
    """
    here = os.path.dirname(os.path.abspath(__file__))
    cur = here
    while True:
        if os.path.isfile(os.path.join(cur, 'scripts', 'parity', 'g03_compare.py')):
            return cur
        parent = os.path.dirname(cur)
        if parent == cur:
            # 环境/用法错误，不是「证据目录不可用」：必须与 negctl 的 rc=1 分开（#2031 r2），
            # 且这一次 print 发生在 `_PipeSafe` 装配之前，必须走不受断管道影响的出口（r4）。
            _usage_exit(f'用法错误：repo root not found（从 {here} 向上没有 scripts/parity/g03_compare.py；'
                        f'rc={EXIT_USAGE}）')
        cur = parent


REPO = _repo_root()
G03 = f'{REPO}/scripts/parity/g03_compare.py'
SCENARIOS = ('deadline', 'deadline_during_tool')
FAIL_CLOSED = 'result_unknown_fail_closed'
GSF_TEXT = ('AgentLoop terminal outcome is result_unknown and host reconciliation '
            'found no final result.')
# #6 产物那条 gsf 在冻结比较器 channel 差异里的**投影形状**（真实数据实测：G03 的
# `_semantic_record` 只留这几个键，q/scenarioId/invocationId/sequence 都在投影时就被丢掉）。
# C5/C6 都跳过这一帧，所以它的字段集必须单独钉死：多带或少带字段同样不是 #6。
GSF_FIELDS = frozenset({'kind', 'agentScope', 'event', 'statusKind', 'text'})
G03_TIMEOUT = 900  # 秒；冻结比较器单次要跑全部场景，病态输入不许把装置挂死
# C0「G03 自己说没比成」的规则名。正本在冻结比较器与我们只读的评分器里：
#   scripts/parity/g03_compare.py:491（trace_identity）/ :494（trace_input）、
#   scripts/parity/score.py:179（同一对名字判 invalid）。
# 冻结件不可改，这里只能跟随；`test_frame_attrib.py` 会读那两份源文本断言两个字面量仍在，
# 冻结侧改名而这里漏改会立刻红（否则 C0 静默失效，「没比成」又会被折成 NOT_6 反证）。
# trace_health **不在此列**：它是可解析 trace 的真实异常，属产品行为差异（负控 C/D 依赖它判 NOT_6）。
G03_UNRUN_RULES = frozenset({'trace_input', 'trace_identity'})
_G03_CACHE = {}


_LOAD_CACHE = {}


def load(path):
    """读 jsonl trace。同一路径在一个进程里只解析一次（20 轮 → 400 pair 时会重复读同 40 个文件）。"""
    if path not in _LOAD_CACHE:
        with open(path) as fh:
            _LOAD_CACHE[path] = [json.loads(l) for l in fh if l.strip()]
    return _LOAD_CACHE[path]


def is_gsf(rec):
    return rec.get('kind') == 'durable.status' and rec.get('event') == 'gateway_submit_failed'


def is_identity(rec):
    return rec.get('kind') == 'harness.proof' and rec.get('state') == 'sidecar_identity'


_G03_TMPDIR = None
# 已建好、还没跑完的 `--negctl-out` 证据目录：进程被 SIGTERM 打断时由 atexit 删掉，
# 免得留下形貌与完整证据无异、却没跑完的残缺目录（r2 concurrency 评审 P2）。
_PENDING_OUTDIRS = set()


def install_sigterm():
    """把 SIGTERM 转成 `SystemExit`，让 `atexit` / `finally` 的清理有机会运行。

    CPython 默认对 SIGTERM 走 SIG_DFL（进程直接终止，atexit 不跑）：CI 用 `timeout` 包装置时
    正好是 SIGTERM。只在主线程装（非主线程会抛 ValueError，退回「异常终止则每进程残留」）。
    独立成函数是为了**在入口就装**：懒装在第一次 spawn 比较器时才生效，那之前被打断的窗口
    里 atexit 也拦不住。
    """
    try:
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    except (ValueError, OSError):
        pass


def _block_sigterm():
    """在临界区内屏蔽 SIGTERM，返回旧掩码（平台不支持时返回 None）。

    用途：`os.makedirs(outdir)` 成功到 `_PENDING_OUTDIRS.add(outdir)` 之间的窗口一旦被
    SIGTERM 命中，SystemExit 会在登记之前逃逸，atexit 不认账 → 空证据目录残留，
    同目录名重跑得 rc=1「已存在」（#2031 第 3 条）。屏蔽后信号在恢复点才送达，
    那时目录已登记，atexit 能删干净。
    """
    try:
        return signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM})
    except (AttributeError, ValueError, OSError):
        # 非主线程 / 平台不支持：退回无屏蔽（窗口仍在，但比把信号永久吞掉安全）。
        return None


def _restore_sigmask(old):
    if old is None:
        return
    try:
        # 被屏蔽期间挂起的 SIGTERM 在这一行之后才送达：此时 add() 已经执行完。
        signal.pthread_sigmask(signal.SIG_SETMASK, old)
    except (ValueError, OSError):
        pass


def _drop_pending_outdir(outdir):
    """删掉半成品证据目录并销账。清理期间屏蔽 SIGTERM：删到一半再被信号打断会留半删残骸。"""
    try:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    except (ValueError, OSError):
        pass
    _PENDING_OUTDIRS.discard(outdir)
    shutil.rmtree(outdir, ignore_errors=True)


def _cleanup_pending_outdirs():
    """atexit：删掉本轮没跑完的证据目录（崩溃 / 被信号打断都不会走到显式清理）。"""
    for d in list(_PENDING_OUTDIRS):
        _drop_pending_outdir(d)


atexit.register(_cleanup_pending_outdirs)


def g03_tmpdir():
    """冻结比较器在 import 期 `tempfile.mkdtemp(prefix='pdrs-parity-pyc-')` 建目录且从不删除
    （`sys.dont_write_bytecode = True`，所以它始终是空目录）。它的扇出是每次 spawn 一个，
    一次全量归因 400 次、negctl 15 次 → 往宿主 TMPDIR 里漏 400 个空目录（#1969 评审 P3）。

    冻结件不可改，就在调用侧兜住：给子进程一个我们自己建、进程退出时整棵删掉的 TMPDIR。
    """
    global _G03_TMPDIR
    if _G03_TMPDIR is None:
        _G03_TMPDIR = tempfile.mkdtemp(prefix='frame-attrib-g03-')
        atexit.register(shutil.rmtree, _G03_TMPDIR, True)
        # atexit 不覆盖 SIGTERM：那棵 TMPDIR 会残留，转成 SystemExit 让 atexit 得以运行。
        install_sigterm()
    return _G03_TMPDIR


def g03(left_dir, right_dir, scenario):
    """调冻结比较器。同一对目录只跑一次（run() 每 pair 要用 G03 列两次，缓存免重复 spawn）。"""
    key = (left_dir, right_dir, scenario)
    if key in _G03_CACHE:
        return _G03_CACHE[key]
    try:
        out = subprocess.run([sys.executable, '-I', G03, left_dir, right_dir],
                             capture_output=True, text=True, timeout=G03_TIMEOUT,
                             env={**os.environ, 'TMPDIR': g03_tmpdir()})
    except subprocess.TimeoutExpired:
        result = (None, None, f'g03 超时（>{G03_TIMEOUT}s）')
        _G03_CACHE[key] = result
        return result
    except OSError as exc:
        # 起不来（解释器缺失 / EMFILE / fork 失败）也是「没比成」，不能当装置崩溃上抛。
        result = (None, None, f'g03 无法执行：{exc}')
        _G03_CACHE[key] = result
        return result
    try:
        data = json.loads(out.stdout)
    except ValueError:
        result = (None, None, f'g03 输出不可解析 rc={out.returncode} {out.stderr[:200]}')
        _G03_CACHE[key] = result
        return result
    result = (data['per_scenario'].get(scenario, {}), data, None)
    _G03_CACHE[key] = result
    return result


def is_the_gsf_insertion(diff):
    if diff.get('left') is not None:
        return False, 'left 非 null（是替换/改动，不是新增）'
    right = diff.get('right')
    if not isinstance(right, dict):
        return False, 'right 不是记录'
    if not is_gsf(right):
        return False, (f'新增记录不是 gsf（kind={right.get("kind")} '
                       f'event={right.get("event")}）')
    if right.get('statusKind') != 'error':
        return False, f'新增记录 statusKind={right.get("statusKind")}'
    if right.get('text') != GSF_TEXT:
        return False, f'新增记录 text 不是 TS 宿主常量：{right.get("text")!r}'
    if right.get('agentScope') != 'parent':
        return False, f'新增记录 agentScope={right.get("agentScope")}（#6 只在 parent 流）'
    # 投影形状必须恰好是 #6 那 5 个键：C5/C6 都会跳过这条 gsf，多带/少带字段只有这里能抓。
    keys = set(right)
    extra, missing = keys - GSF_FIELDS, GSF_FIELDS - keys
    if extra or missing:
        return False, (f'新增记录字段集不是 #6 形状（多 {sorted(extra)} / 少 {sorted(missing)}）')
    return True, 'ok'


def canon(value):
    """与冻结尺子 `g03_compare.py` 的 `canon()` 同口径：sort_keys + 紧凑 + ensure_ascii=False。

    紧凑分隔符是**口径**不是格式偏好：装置若与尺子不同，「逐字节相同」的结论不可比（#1926 评审）。
    """
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':'))


def norm(rec):
    """C5 的比较键：抹掉每轮随机的 invocationId、插入导致的 sequence 右移，
    以及 requestEvidence（冻结口径两处都 pop 掉它，见模块 docstring）。"""
    return canon({k: v for k, v in rec.items()
                  if k not in ('invocationId', 'sequence', 'requestEvidence')})


def ev_norm(value):
    """C6 的比较键：规范化后逐字节比较（键序不算差异，值算）。"""
    return canon(value)


def identity_ok(recs, side):
    """C4 / 计数表共用：sidecar 身份帧自证成立（恰 1 条、side 对、sha 自洽、spawn 观测到）。"""
    ident = [r for r in recs if is_identity(r)]
    if len(ident) != 1:
        return False, None
    rec = ident[0]
    ok = (rec.get('side') == side
          and isinstance(rec.get('sha256'), str) and rec.get('sha256')
          and rec.get('sha256') == rec.get('expectedSha256')
          and rec.get('spawnObserved') is True)
    return ok, rec


def request_evidence_index(recs, skip=frozenset()):
    """按 C5 比较键归组，记录每帧的 requestEvidence（可能为 None）。"""
    idx = {}
    for i, rec in enumerate(recs):
        if is_identity(rec) or i in skip:
            continue
        idx.setdefault(norm(rec), []).append(rec.get('requestEvidence'))
    return idx


def check_request_evidence(ts_recs, rust_recs, skip_rust=frozenset()):
    """C6：两侧**都存在** requestEvidence 的同名帧，规范化后必须逐字节相同。

    返回 (ok, 证据行)。单侧存在属于 C5 的已登记残余（README），C6 不覆盖、也不算失败。
    同名帧以 C5 的比较键（`norm()`，已抹掉 requestEvidence）归组；同组内按规范化内容排序后
    一一配对，因此同键多帧时也能比较到每一个 requestEvidence。
    """
    ts_idx = request_evidence_index(ts_recs)
    ru_idx = request_evidence_index(rust_recs, skip=skip_rust)
    compared = residual = 0
    for key in ts_idx.keys() & ru_idx.keys():
        # 同键两侧帧数不等说明 C5 的归组前提已被破坏：静默 zip 截断会漏比尾部（#1926 评审）。
        if len(ts_idx[key]) != len(ru_idx[key]):
            return False, (f'C6 失败：同键帧数两侧不等（TS {len(ts_idx[key])} / '
                           f'Rust {len(ru_idx[key])}）：' + key[:300])
        # 只把**两侧都存在**的 requestEvidence 放进多重集比内容；单侧存在计入残余。
        # 不能按「有无」排序后 zip：None 的排序键与内容串不可比，None 位置不同就会错位，
        # 把单侧存在误判成内容不同（#1926 评审 P1）。
        left = Counter(ev_norm(v) for v in ts_idx[key] if v is not None)
        right = Counter(ev_norm(v) for v in ru_idx[key] if v is not None)
        common = left & right
        compared += sum(common.values())
        only_left, only_right = left - common, right - common
        if only_left and only_right:
            return False, ('C6 失败：两侧共有的 requestEvidence 内容不同：'
                           + (sorted(only_left)[0][:300] + ' || ' + sorted(only_right)[0][:300]))
        residual += sum(only_left.values()) + sum(only_right.values())
    return True, (f'C6 ok：{compared} 条两侧共有的 requestEvidence 规范化后逐字节相同'
                  f'（单侧存在 {residual} 条，属 C5 残余，C6 不覆盖）')


def judge(scenario, ts_dir, rust_dir, ts_recs, rust_recs, per=None):
    """返回 (结论, 证据行列表, 自报规则元组)。结论 ∈ {PASS, ATTRIB_6, NOT_6, UNKNOWN}。

    `UNKNOWN` = 比较器没比成 —— 进程级（超时 / 输出不可解析 / 输出里没有该场景）**或**
    C0 进程内自报（`triggered_rules` 含 `trace_input` / `trace_identity`），**不是**「不是 #6」。
    装置/比较器故障与「反证命中」必须分开：把前者折成 `NOT_6` 会把 rc=2「反证命中」这个最强
    的否定结论发给工具故障（#1926 评审 P1）。

    第三个返回值是**结构化**的自报规则元组：只有 C0 进程内自报路径非空，其余一律空元组。
    负控靠它区分「C0 按期望命中」与「装置故障」，不再对证据行做子串匹配（#1969 r1 两家评审：
    进程级路径会把 G03 的 stderr 拼进证据行，子串匹配可能被 traceback 回显的源码行污染）。

    `per` 可注入该场景的 G03 结果（`per_scenario[scenario]`）：自测用桩数据覆盖 C1–C6
    的编排层，不必真跑冻结比较器；生产调用传 None，由 `g03()` 现算。
    """
    ev = []
    if scenario not in SCENARIOS:
        return 'NOT_6', [f'C1 失败：场景 {scenario} 不在 #6 登记范围 {SCENARIOS}'], ()
    if per is None:
        per, _, err = g03(ts_dir, rust_dir, scenario)
        if per is None:
            return 'UNKNOWN', [f'C0 未运行：{err}（装置/比较器故障，未判定，不当作反证）'], ()
    if not per:
        return 'UNKNOWN', [f'C0 未运行：G03 输出里没有场景 {scenario} 的结果（未判定，不当作反证）'], ()
    diffs = per.get('channel', {}).get('differences') or []
    verdict = per.get('verdict')
    rules = sorted(per.get('triggered_rules') or [])
    # C0（进程内自报）：G03 自己说「这轮没比成」时同样不能折成 NOT_6。
    #   trace_input：compare_records 里抛异常（记录缺必需键 / 形状非法）→ 比较器根本没比成；
    #   trace_identity：sidecar 身份自证不成立 → 该轮不可信。轮次闸门只看 run.json，
    #   挡不住 identity 帧本身被 G03 判否（#1926 r4 两家非作者评审 P1，独立撞上同一条）。
    #   trace_health **不在此列**：它是可解析 trace 的真实异常（重复/乱序终态），属产品行为
    #   差异，negctl C/D 依赖它判 NOT_6；一律折成 UNKNOWN 会把这些真差异放走。
    not_run = [r for r in sorted(G03_UNRUN_RULES) if r in rules]
    if not_run:
        return 'UNKNOWN', [f'C0 未运行：G03 自报 {not_run}（比较器没比成 / 轮次身份不成立；'
                           f'未判定，不当作反证）{per.get("error") or ""}'], tuple(not_run)
    if verdict == 'PASS':
        return 'PASS', [f'C2：G03 判 PASS，语义差异 0 条'], ()
    # C2a：规则集必须恰好是 #6 的那三条。#6 只会多一条 durable.status，
    #      因此任何别的规则（E6 终态不符、trace_health、E9 settlement…）都是另一种缺陷。
    if rules != ['E10', 'E11', 'G03_channels']:
        return 'NOT_6', [f'C2a 失败：triggered_rules={rules}，不是 #6 的 [E10,E11,G03_channels]'], ()
    if len(diffs) != 1:
        return 'NOT_6', [f'C2b 失败：G03 语义差异 {len(diffs)} 条（#6 只允许 1 条）：'
                         + json.dumps(diffs, ensure_ascii=False)[:600]], ()
    ok, why = is_the_gsf_insertion(diffs[0])
    if not ok:
        return 'NOT_6', [f'C2b 失败：唯一差异不是 gsf 新增 —— {why}',
                         json.dumps(diffs[0], ensure_ascii=False)], ()
    ev.append('C2a ok：triggered_rules 恰好 [E10,E11,G03_channels]')
    ev.append('C2b ok：G03 语义差异恰好 1 条 = 右侧新增 gateway_submit_failed（left=null）')
    ts_gsf = [i for i, r in enumerate(ts_recs) if is_gsf(r)]
    ru_gsf = [i for i, r in enumerate(rust_recs) if is_gsf(r)]
    if len(ru_gsf) != 1 or len(ts_gsf) != 0:
        return 'NOT_6', [f'C3 失败：gsf 计数 ts={len(ts_gsf)} rust={len(ru_gsf)}（要求 0/1）'], ()
    fc = next((i for i, r in enumerate(rust_recs)
               if r.get('kind') == 'harness.proof' and r.get('state') == FAIL_CLOSED), None)
    if fc is None:
        return 'NOT_6', ['C3 失败：右侧 trace 里没有 result_unknown_fail_closed 帧'], ()
    if ru_gsf[0] <= fc:
        return 'NOT_6', [f'C3 失败：gsf 序号 {ru_gsf[0]} 不晚于 fail_closed 帧 {fc}（分叉前移）'], ()
    ev.append(f'C3 ok：gsf 计数 ts=0 rust=1，位于第 {ru_gsf[0]} 帧，晚于 fail_closed 帧 {fc}')
    ident_ok, ident = identity_ok(rust_recs, 'rust')
    if not ident_ok:
        return 'NOT_6', ['C4 失败：右侧 sidecar 身份自证不成立'], ()
    ev.append(f'C4 ok：右侧 sidecar 身份自证 sha256={str(ident.get("sha256"))[:12]}…，spawnObserved=true')
    # C5：把右侧那一条 gsf 删掉后，两侧的帧**集合**必须完全相同（逐条多重集相等）。
    #     抹掉 invocationId（每轮随机 UUID，G03 自己也归一化）、sequence（插入后整段右移，
    #     顺序不变性已由 C2a 的 G03 规则与 C3 的插入位置覆盖），以及 requestEvidence
    #     （冻结口径 normalization.md:32 与 g03_compare.py 两处都 pop 掉它，见 #1926）。
    #     用集合而不是逐位，是因为 #1841 的 context.budget 相对顺序偏离已被 G03 判为可接受；
    #     逐位比对会把它误报成「不是 #6」，那是把装置口径问题混进产品归因。
    ts_cmp = sorted(norm(r) for r in ts_recs if not is_identity(r))
    ru_cmp = sorted(norm(r) for i, r in enumerate(rust_recs) if not is_identity(r) and i != ru_gsf[0])
    if ts_cmp != ru_cmp:
        only_ts = [x for x in ts_cmp if x not in ru_cmp]
        only_ru = [x for x in ru_cmp if x not in ts_cmp]
        return 'NOT_6', [f'C5 失败：删掉那条 gsf 后两侧帧集合仍不等 '
                         f'(仅 TS {len(only_ts)} 条 / 仅 Rust {len(only_ru)} 条)：'
                         + (only_ts[:1] + only_ru[:1]).__str__()[:500]], ()
    ev.append(f'C5 ok：删掉右侧第 {ru_gsf[0]} 帧后，两侧各 {len(ts_cmp)} 帧的集合完全相同'
              '（口径已豁免 requestEvidence）')
    # C6：两侧**都存在** requestEvidence 的同名帧，规范化后必须逐字节相同。
    #     单侧存在属于 C5 的已登记残余（README），C6 不覆盖、也不算失败。
    c6_ok, c6_msg = check_request_evidence(ts_recs, rust_recs, skip_rust=frozenset({ru_gsf[0]}))
    if not c6_ok:
        return 'NOT_6', [c6_msg], ()
    ev.append(c6_msg)
    return 'ATTRIB_6', ev, ()


def rounds(root, side):
    """`<root>/<side>-r<N>` 轮次目录，按 N 升序。目录名不合规即报错（不静默跳过）。"""
    dirs = [p for p in glob.glob(f'{root}/{side}-r*') if os.path.isdir(p)]
    bad = [p for p in dirs if not os.path.basename(p).rsplit('-r', 1)[1].isdigit()]
    if bad:
        raise SystemExit(f'轮次目录名不合规（应为 {side}-r<N>）：{sorted(bad)}')
    return sorted(dirs, key=lambda p: int(os.path.basename(p).rsplit('-r', 1)[1]))


def round_problem(d, side):
    """轮次有效性（与冻结 `scripts/parity/score.py:146-158` 同口径）。

    返回 None 表示有效，否则返回原因。没有这道闸门时：被 BLOCKED / 只跑了半截的轮次会被
    G03 追加 `trace_identity` 规则 → 判成 NOT_6（把「轮次无效」误报成「不是 #6」），或者
    直接因 trace 缺失/截断抛异常中断整个分析（#1926 评审 P1）。
    """
    try:
        with open(os.path.join(d, 'run.json')) as fh:
            ledger = json.load(fh)
    except (OSError, ValueError) as exc:
        return f'run.json 不可读：{exc}'
    if ledger.get('side') != side:
        return f'run.json side={ledger.get("side")!r}，不是 {side!r}'
    if ledger.get('valid_round') is not True:
        return 'run.json valid_round 不为 true'
    if ledger.get('identity_ok') is not True:
        return 'run.json identity_ok 不为 true'
    started = ledger.get('started_wall')
    if type(started) not in (int, float):
        return 'run.json missing/invalid started_wall'
    try:
        finite = math.isfinite(started)
    except (OverflowError, ValueError):
        # 任意精度 int 超出 double 范围时 isfinite 抛 OverflowError，不能让它逃出闸门。
        finite = False
    if not finite:
        return 'run.json missing/invalid started_wall'
    # summary.json **缺失即无效**：冻结 `score.py:154-156` 是无条件 read，缺失会抛异常把轮次判无效；
    # 这里若放行，「blocked 未知」就会被当成「未 blocked」，静默参与判定（#1926 评审 P1）。
    sp = os.path.join(d, 'artifacts', 'summary.json')
    try:
        with open(sp) as fh:
            summary = json.load(fh)
    except (OSError, ValueError) as exc:
        return f'summary.json 不可读（缺失即无效）：{exc}'
    if summary.get('blocked'):
        return f'summary.json blocked={summary.get("blocked")!r}'
    return None


def collect_rounds(root, side, trace):
    """筛出有效轮次；无效轮次逐条列原因，不静默参与判定。"""
    good, bad = [], []
    for d in rounds(root, side):
        why = round_problem(d, side)
        if why is None:
            try:
                load(os.path.join(d, 'artifacts', trace))
            except (OSError, ValueError) as exc:
                why = f'trace 不可读：{exc}'
        (bad.append((os.path.basename(d), why)) if why else good.append(d))
    return good, bad


def print_bad_rounds(ts_bad, ru_bad, file=None):
    """逐条列出无效轮次的原因（`run()` 走 stderr、`negctl()` 走 stdout，故 `file` 可传）。"""
    for side, bad in (('ts', ts_bad), ('rust', ru_bad)):
        for name, why in bad:
            print(f'   {side}/{name}: {why}', file=file)


def verdict_exit(npass, n6, not6, invalid_rounds, unjudged=0):
    """把计数折成退出码（纯函数，自测可覆盖）。

    rc=0 只在「确实归因到了 #6 且没有 NOT_6 / 未判定、批次完整」时给出：全 PASS 的批次虽然没出错，
    但它**没有复现任何 FAIL**，不能当作「#6 已归因」的验收证据（#1926 评审 P1）。
    `unjudged`（比较器故障未判定）优先于 `invalid_rounds`：批次不完整时也不能给 rc=0；
    但真正的 `NOT_6` 比它更硬——已经证伪就不受未判定影响。
    """
    if not6:
        return 2
    if unjudged:
        return 8
    if invalid_rounds:
        return 6
    if n6 == 0:
        return 7
    return 0


def run(root, scenario):
    # 与 negctl 同一把用法闸（#2031 r2 boundary P2）：不在册场景会让 C1 把每条 pair 判成
    # NOT_6 → rc=2「已证伪」，用法错误被报成最强的否定结论。`judge()` 的 C1 语义**不动**
    # （负控 F-scenario-out-of-scope 靠 C1 判 NOT_6 证明豁免边界），只在命令行入口拒绝。
    if scenario not in SCENARIOS:
        print(f'run 用法错误：场景 {scenario} 不在 #6 登记范围 {SCENARIOS}。'
              f'不在册场景会被 C1 判成 NOT_6，不能当作判据被证伪（rc={EXIT_USAGE}：用法错误，不下判据结论）。',
              file=sys.stderr)
        return EXIT_USAGE
    trace = f'{scenario}.pilotdeck-current-sidecar.jsonl'
    ts_all, ts_bad = collect_rounds(root, 'ts', trace)
    ru_all, ru_bad = collect_rounds(root, 'rust', trace)
    # 空对拍必须**硬失败**：0 对时循环不执行、total=0，旧版会打印「所有 FAIL 都可归因」
    # 并以 rc=0 退出 —— 那是尺子假 PASS（#1926 评审 P0）。
    if not ts_all or not ru_all:
        print(f'## 失败：有效轮次不足（ts {len(ts_all)} 轮 / rust {len(ru_all)} 轮），无法归因：{root}',
              file=sys.stderr)
        print_bad_rounds(ts_bad, ru_bad, file=sys.stderr)
        return 5
    print(f'# 场景 {scenario}')
    print(f'# rounds = {root}')
    print()
    print('## 每轮 gsf / requestEvidence 计数（原件 trace 直读）')
    print(f'{"round":10s} {"n_frames":>8s} {"gsf":>4s} {"ev":>4s} {"ident":>6s}  durable.status events')
    for side, dirs in (('ts', ts_all), ('rust', ru_all)):
        for d in dirs:
            recs = load(os.path.join(d, 'artifacts', trace))
            st = [f'{r.get("event")}/{r.get("statusKind")}' for r in recs if r.get('kind') == 'durable.status']
            iok = identity_ok(recs, side)[0]
            nev = sum(1 for r in recs if r.get('requestEvidence') is not None)
            print(f'{os.path.basename(d):10s} {len(recs):8d} {sum(1 for r in recs if is_gsf(r)):4d} '
                  f'{nev:4d} {str(iok):>6s}  {st}')
    print()
    print('## 逐 pair 判定')
    print(f'{"pair":16s} {"G03":6s} {"判定":9s} 证据')
    npass = n6 = not6 = nun = 0
    bad = []
    unjudged = []
    for td in ts_all:
        for rd in ru_all:
            ts_recs = load(os.path.join(td, 'artifacts', trace))
            ru_recs = load(os.path.join(rd, 'artifacts', trace))
            verdict, ev, _ = judge(scenario, td, rd, ts_recs, ru_recs)
            name = f'{os.path.basename(td)} v {os.path.basename(rd)}'
            per, _, _ = g03(td, rd, scenario)
            g = (per or {}).get('verdict')
            print(f'{name:16s} {str(g):6s} {verdict:9s} {ev[0]}')
            for line in ev[1:]:
                print(f'{"":16s} {"":6s} {"":9s} {line}')
            if verdict == 'PASS':
                npass += 1
            elif verdict == 'ATTRIB_6':
                n6 += 1
            elif verdict == 'UNKNOWN':
                nun += 1
                unjudged.append((name, ev))
            else:
                not6 += 1
                bad.append((name, ev))
    total = npass + n6 + not6 + nun
    print()
    print(f'## 汇总：{total} pair 中 PASS={npass} · 归因 #6={n6} · 不能归因={not6}'
          + (f' · 未判定={nun}' if nun else ''))
    if total == 0:  # 兜底：对拍为空时绝不打印「全部可归因」（尺子假 PASS）
        print('## 失败：0 pair 被判，拒绝给出结论', file=sys.stderr)
        return 5
    if nun:
        print('## 未判定（比较器故障 / 未产出该场景，已排除在结论外）：')
        for name, ev in unjudged:
            print(' ', name, ev)
    if not6:
        print('## 反证命中（不能归因 #6）：')
        for name, ev in bad:
            print(' ', name, ev)
        return verdict_exit(npass, n6, not6, bool(ts_bad or ru_bad), nun)
    if ts_bad or ru_bad:
        # 批次不完整：结论只覆盖有效轮次，不得作为验收证据（fail-closed，不静默通过）。
        print(f'## 警告：{len(ts_bad) + len(ru_bad)} 个轮次无效，已排除：')
        for side, bad_rounds in (('ts', ts_bad), ('rust', ru_bad)):
            for name, why in bad_rounds:
                print(f'   {side}/{name}: {why}')
        print('## 结论仅覆盖上面列出的有效轮次；含无效轮次的批次不算验收证据。')
        return verdict_exit(npass, n6, not6, True, nun)
    if nun:
        print(f'## 结论：{nun} 个 pair 因装置/比较器故障**未判定**（rc=8），'
              '不得据此断言「全部是 #6」。')
        return verdict_exit(npass, n6, not6, False, nun)
    if n6 == 0:
        print(f'## 结论：本批次 {npass} 个 pair 全部 PASS，没有任何 FAIL 可归因；'
              '该批次不能作为 #6 归因证据。')
        return verdict_exit(npass, n6, not6, False)
    print('## 结论：该场景所有 FAIL 的语义差异集恰好是 #6 那一条，无第二种差异。')
    return verdict_exit(npass, n6, not6, False)


# ---------- negctl：用真实 trace 的变异副本证明判据不是空话 ----------

class BaselineUnusable(Exception):
    """negctl 基线不满足某条用例的前置条件（缺变异点 / 空 trace / 缺 identity 帧）。

    与「判据被证伪」严格分开：基线不可用按 rc=3 退出，绝不折成 MISMATCH（rc=4）。
    """


def clone(recs):
    """记录列表的浅拷贝（每条记录本身也拷一层）：变异只改副本，绝不改基线（#1969 评审 P3）。"""
    return [dict(r) for r in recs]


def need_index(recs, pred, what, valid):
    """取第一条满足 `pred` 的记录下标；取不到就按「基线不可用」退出。

    `valid` 是给使用者看的前置条件描述（例：`需要含 gsf 的 FAIL 基线`）。
    与 `mutate_field` / `put_ev` / N / O 同口径：变异点缺失一律 `BaselineUnusable`（rc=3），
    绝不让裸 `StopIteration` / `IndexError` 把整个 negctl 打成未文档化的 rc=1（#1969 评审 P2）。
    """
    for i, r in enumerate(recs):
        if pred(r):
            return i
    raise BaselineUnusable(f'negctl 变异点不存在：{what}（{valid}）')



def case_mark(got, want, cause=()):
    """负控结果归类（纯函数，自测可覆盖）。

    `UNJUDGED` = 装置/比较器故障，不是判据被证伪；MISMATCH 只留给「真的判错」。
    期望 `UNKNOWN` 的用例（N/O）额外要求 `cause`（`judge()` 的第三个返回值，结构化自报规则
    元组）非空：只比结论值的话，任何一条装置故障（超时 / 缺场景）都会记成 OK，C0 就被静默
    放过 —— 那是 r4 错向的镜像（把「装置故障」报成「判据命中」，#1969 评审 P2）。

    判 `cause` 的结构，不判证据行文案：进程级故障的证据行会把 G03 的 stderr 拼进来，若按子串
    匹配 `trace_input` / `trace_identity`，traceback 回显的那行源码就可能把它污染成 OK
    （#1969 r1 两家评审各自独立撞上同一条）。
    """
    if got == want:
        if want == 'UNKNOWN' and not cause:
            return 'UNJUDGED'
        return 'OK'
    if got == 'UNKNOWN':
        return 'UNJUDGED'
    return 'MISMATCH'


def write_trace(path, recs):
    """原子写证据 trace：先**全量**序列化（坏记录在这里就抛，不落半截文件），再 tmp + `os.replace`。

    复核者会拿这个目录重跑 G03；半截 jsonl 会让 G03 报 `trace_input`、证据目录本身作废。
    与 `scripts/parity/round.py` 的 temp+rename 发布同口径（#1969 评审 P2）。
    """
    payload = ''.join(json.dumps(rec, ensure_ascii=False) + '\n' for rec in recs)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix='.trace-', suffix='.tmp')
    try:
        with os.fdopen(fd, 'w') as fh:
            fh.write(payload)
            fh.flush()
            os.fsync(fh.fileno())
        os.chmod(tmp, 0o644)  # mkstemp 默认 0600；证据要能被别的账号复核（r2 评审 P3）
        os.replace(tmp, path)
    except BaseException:
        # fix-r15（DS r14 sibling#1 P2a，与 check_frozen write_manifest r14 同族）：
        # 清理 remove 是唯一吞掉的错——失败路径清理错不得顶掉原发布错。
        from contextlib import suppress
        with suppress(OSError):
            if os.path.exists(tmp):
                os.remove(tmp)
        raise


def build_cases(base_ts, base_ru):
    """由一对真实基线 trace 造出全部负控用例：`(名字, ts 记录, rust 记录, 期望结论, 场景)`。

    变异只作用于 `clone()` 出来的副本，基线本身不被修改。任一变异点在前置条件上不成立时
    抛 `BaselineUnusable`（负控的判据依赖「这条记录确实存在」，缺了就不该判，更不能崩）。
    场景位为 `None` 表示沿用命令行给的场景。
    """
    cases = []
    # A 基线：原样 → 应判 ATTRIB_6
    cases.append(('A-baseline-attrib6', base_ts, base_ru, 'ATTRIB_6', None))
    # B 换成别的 event 名 → NOT_6
    #   与 E/H 同一类：基线没有 gsf 时旧写法循环空转，ru_b ≡ 基线，B 退化成第二个 A
    #   → 实得 ATTRIB_6 vs 期望 NOT_6 → 假 MISMATCH → rc=4（r2 simplify 评审 P2）。
    ru_b = clone(base_ru)
    need_index(ru_b, is_gsf, 'gsf 帧', '需要含 gsf 的 FAIL 基线')
    for r in ru_b:
        if is_gsf(r):
            r['event'] = 'gateway_submit_aborted'
    cases.append(('B-event-renamed', base_ts, ru_b, 'NOT_6', None))
    # C 多写一条 → NOT_6
    ru_c = list(base_ru)
    idx = need_index(ru_c, is_gsf, 'gsf 帧', '需要含 gsf 的 FAIL 基线')
    ru_c.insert(idx + 1, ru_c[idx])
    cases.append(('C-duplicated', base_ts, ru_c, 'NOT_6', None))
    # D 分叉前移：把 gsf 挪到 fail_closed 之前 → NOT_6
    ru_d = clone(base_ru)
    g = need_index(ru_d, is_gsf, 'gsf 帧', '需要含 gsf 的 FAIL 基线')
    fc = need_index(ru_d,
                    lambda r: r.get('kind') == 'harness.proof' and r.get('state') == FAIL_CLOSED,
                    'result_unknown_fail_closed 帧', '需要 fail-closed 基线')
    rec = ru_d.pop(g)
    ru_d.insert(fc, rec)
    cases.append(('D-moved-before-fail-closed', base_ts, ru_d, 'NOT_6', None))
    # E 替换型：把右侧终态 outcome 改掉（left 非 null）→ NOT_6
    #   C1–C6 都不要求这帧存在，所以变异点缺失时旧写法是**静默空操作**（ru_e ≡ 基线）：
    #   实得 ATTRIB_6 vs 期望 NOT_6 → 假 MISMATCH → rc=4「判据被证伪」。
    #   与 #1968 同向（数据问题被报成判据被证伪），必须前置成 BaselineUnusable（rc=3）。
    ru_e = clone(base_ru)
    e = need_index(ru_e,
                   lambda r: r.get('kind') == 'operation.terminal' and r.get('outcome') == 'result_unknown',
                   'operation.terminal(outcome=result_unknown) 帧', '需要含该终态的基线')
    ru_e[e]['outcome'] = 'failed'
    cases.append(('E-terminal-substituted', base_ts, ru_e, 'NOT_6', None))
    # F 场景越界：把场景名换成未登记场景 → NOT_6
    cases.append(('F-scenario-out-of-scope', base_ts, base_ru, 'NOT_6', 'sidecar_budget_limit'))
    # G 载荷篡改：event 名对但 text 换成别的话 → NOT_6
    ru_g = clone(base_ru)
    need_index(ru_g, is_gsf, 'gsf 帧', '需要含 gsf 的 FAIL 基线')  # 同上：缺 gsf 时是空操作
    for r in ru_g:
        if is_gsf(r):
            r['text'] = 'AgentLoop terminal outcome is result_unknown.'
    cases.append(('G-text-tampered', base_ts, ru_g, 'NOT_6', None))
    # H 静默少一帧（G03 的偏序比较器看不见的那种）→ 只有 C5 能抓 → NOT_6
    #   H 的全部价值在「确实删掉了帧」：基线没有该帧时 ru_h ≡ 基线，H 退化成第二个 A，
    #   同样得到假 MISMATCH → rc=4。只探测、不变异，缺失即 BaselineUnusable（rc=3）。
    need_index(base_ru, lambda r: r.get('kind') == 'side_effect.state', 'side_effect.state 帧',
               '需要含该帧的基线')
    ru_h = [r for r in base_ru if r.get('kind') != 'side_effect.state']
    cases.append(('H-frame-dropped', base_ts, ru_h, 'NOT_6', None))
    # I 与 #6 无关地多一帧（重复一条 model.request）→ C5 抓 → NOT_6
    ru_i = list(base_ru)
    mi = need_index(ru_i, lambda r: r.get('kind') == 'model.request', 'model.request 帧',
                    '需要含 model.request 的基线')
    ru_i.insert(mi + 1, dict(ru_i[mi]))
    cases.append(('I-unrelated-extra-frame', base_ts, ru_i, 'NOT_6', None))
    # J/K 冻结比较器看不见的字段改动（偏序规则不覆盖），只能靠 C5 的集合比对抓
    def mutate_field(recs, kind, field, val):
        out = clone(recs)
        i = need_index(out, lambda r: r.get('kind') == kind and field in r, f'{kind}.{field}',
                       '需要含该字段的基线')
        out[i][field] = val
        return out

    cases.append(('J-field-q-changed', base_ts,
                  mutate_field(base_ru, 'terminal', 'q', 'X'), 'NOT_6', None))
    cases.append(('K-lastApplied-changed', base_ts,
                  mutate_field(base_ru, 'operation.terminal', 'lastAppliedSequence', 99), 'NOT_6', None))

    # L/M 绑定 #1926 的新增 C5 豁免与 C6。
    def put_ev(recs, value):
        out = clone(recs)
        i = need_index(out, lambda r: r.get('kind') == 'context.budget', 'context.budget 帧',
                       '需要带该帧的基线')
        out[i]['requestEvidence'] = value
        return out

    # L 单侧多出 requestEvidence：C5 已豁免 → 期望 ATTRIB_6。
    #   这是**故意保留的残余**（README「C5 残余」），负控的价值在于盯住豁免真的生效，
    #   而不是证明装置能抓它；若哪天它变回 NOT_6，说明口径又被改严了，需同步文档。
    cases.append(('L-single-side-requestEvidence', base_ts,
                  put_ev(base_ru, {'accountingContract': 'negctl/v1', 'total': 1}), 'ATTRIB_6', None))
    # M 双侧都有但内容不同：C6 必须抓到 → NOT_6
    cases.append(('M-requestEvidence-content-differs',
                  put_ev(base_ts, {'accountingContract': 'negctl/v1', 'total': 1}),
                  put_ev(base_ru, {'accountingContract': 'negctl/v1', 'total': 2}), 'NOT_6', None))

    # N/O 绑定 #1926 r4 的 C0（进程内自报故障）：G03 **自己**说「没比成」时必须判 UNKNOWN，
    #   不能折成 NOT_6 反证（rc=2 / MISMATCH 都会把装置故障报成「判据被证伪」）。
    # N identity 自证被改 → G03 追加 trace_identity → UNKNOWN
    def break_identity(recs):
        out = clone(recs)
        proof = out[need_index(out, is_identity, 'ts 侧 sidecar_identity 帧',
                               'G03 允许「TS 捕获早于 identity 包装」，但本用例需要一帧可改')]
        proof['sha256'] = '0' * 64
        return out

    cases.append(('N-identity-broken-g03-selfreport', break_identity(base_ts), base_ru, 'UNKNOWN', None))

    # O 记录缺必需键（G03 load_aligned 抛 ValueError）→ trace_input → UNKNOWN
    def drop_sequence(recs):
        out = clone(recs)
        if not out:
            raise BaselineUnusable('negctl 变异点不存在：ts 基线 trace 为空（需要至少 1 条记录）')
        if 'sequence' not in out[0]:
            raise BaselineUnusable(
                f'negctl 变异点不存在：ts 基线首条记录没有 sequence（kind={out[0].get("kind")}）')
        del out[0]['sequence']
        return out

    cases.append(('O-record-missing-sequence-g03-input-error', drop_sequence(base_ts), base_ru, 'UNKNOWN', None))
    return cases


class OutputFailure(Exception):
    """装置自身**输出**写失败（ENOSPC / EIO / EBADF …）：日志已被截断，rc 不再代表判定结论。

    与断管道严格分开：读者提前退出（`… | head -1`）是正常用法，吞掉并静音即可；写不进去的盘
    不是「读者走了」，而是**证据不可信**。混为一谈就是「截断日志当完整证据」（#2031 第 6 条）。
    """


def _silence_stdout():
    _silence_stream(sys.stdout)


def _silence_stderr():
    _silence_stream(sys.stderr)


class _PipeSafe:
    """stdout / stderr 包装：读者提前退出时不改写退出码，写失败不静默。

    只靠 Python 层的 `print` 会在两处翻车，都会把真实 rc（0/2/3/4/8）盖掉 —— 而 #1157 的常驻
    复查是按 rc 判停的：`| head` 读错 rc 会被读成「换名重跑」，甚至把完整证据当没跑完：
      · 无缓冲（`python3 -u` / `PYTHONUNBUFFERED`）时 write 抛 BrokenPipeError，未捕获 →
        traceback → **rc=1**，与 README 登记的「证据目录不可用」撞码；
      · 有缓冲时异常留到解释器退出 flush，CPython 把出口改写成 **120**（实测见 README）。
    实测（2000 个无效轮次，`… | head -1`）：包装前 120 / `-u` 下 1，包装后一律是真实 rc。
    同一层也套在 **stderr** 上：`run()` 的无效轮次清单走 stderr，`2>&1 | head -1` 同样会撞
    （#2031 第 1 条）。

    只吞 `BrokenPipeError`。其它 `OSError`（ENOSPC / EIO）说明输出**写不进去**：静音后抛
    `OutputFailure`，由 `main()` 折成 rc=9 —— 否则日志被截断、rc 照旧，截断的日志会被当成
    完整证据（#2031 第 6 条）。
    """

    def __init__(self, inner, silence=None):
        self._inner = inner
        self._silence = silence

    def _trip(self):
        (self._silence or _silence_stdout)()

    def write(self, s):
        try:
            return self._inner.write(s)
        except BrokenPipeError:
            self._trip()
            return len(s)
        except OSError as exc:
            self._trip()
            raise OutputFailure(f'输出写失败（{exc}）') from exc

    def flush(self):
        try:
            self._inner.flush()
        except BrokenPipeError:
            self._trip()
        except OSError as exc:
            self._trip()
            raise OutputFailure(f'输出刷新失败（{exc}）') from exc

    def __getattr__(self, name):
        return getattr(self._inner, name)


def _report_crash(exc, outdir):
    """rc=9 的收尾输出。管道已断（`… | head -1`）或磁盘满时输出本身会失败：
    那时**绝不能**让异常逃出 except 块把 rc 改写成 1（又撞回「目录已存在」的码）。"""
    try:
        traceback.print_exc()
        print(f'## negctl 中断：{exc!r}；已删除半成品证据目录 {outdir}（rc=9，非「目录已存在」rc=1）')
        return
    except (OSError, OutputFailure):
        pass
    _silence_stdout()
    _silence_stderr()


def negctl(root, scenario, outdir):
    # 场景名必须先校验：负控的**期望表**以「在册场景」为前提（A/L 期望 ATTRIB_6、N/O 期望
    # UNKNOWN）。传一个真实存在但不在 SCENARIOS 的场景 id 时，`judge()` 的 C1 会把每条用例
    # 都判成 NOT_6，于是 A/L/N/O 记 MISMATCH → rc=4「判据被证伪」。那是**用法错误**被报成
    # 最强的否定结论（按 rc 判停的复查会去查产品缺陷）。必须在动证据目录之前拒绝（#2031 第 4 条）。
    if scenario not in SCENARIOS:
        print(f'negctl 用法错误：场景 {scenario} 不在 #6 登记范围 {SCENARIOS}。'
              '负控期望表只对在册场景成立（未登记场景会让 A/L/N/O 全落成 C1-NOT_6 → 假 MISMATCH）。'
              f'（rc={EXIT_USAGE}：用法错误，不进负控、不创建证据目录）', file=sys.stderr)
        return EXIT_USAGE
    trace = f'{scenario}.pilotdeck-current-sidecar.jsonl'
    # 基线必须过**轮次有效性闸门**（与 run() 同一把）：拿被 BLOCKED / 只跑半截的轮次当基线，
    # A-baseline 会实得 NOT_6 → 记 MISMATCH → rc=4「判据被证伪」，而同一批数据 run() 明明
    # 会 rc=5 硬失败。装置故障不得报成判据被证伪（#1968 P1）。
    install_sigterm()
    try:
        ts_dirs, ts_bad = collect_rounds(root, 'ts', trace)
        ru_dirs, ru_bad = collect_rounds(root, 'rust', trace)
    except SystemExit as exc:
        # 这里会撞上两种 SystemExit，必须按 code **类型**分开（#2031 第 2 条）：
        #   · `rounds()` 对不合规目录名 `raise SystemExit(str)`：数据不合格 = 基线不可用 → rc=3
        #     （r2 sibling 评审 P2：stdlib 语义的进程退 1 与「证据目录不可用」撞码）；
        #   · `install_sigterm()` 的 handler `sys.exit(143)`（int）：CI `timeout` 杀进程时正好
        #     落在这个窗口（collect_rounds 是大批次里最长的阶段）。把它吞成 rc=3「基线不可用」
        #     会让按 rc 判停的复查去换数据重跑，而真实原因是自己被杀。
        if not isinstance(exc.code, str):
            raise
        print(f'negctl 基线不可用：{exc}（rc=3，不判 MISMATCH）')
        return 3
    if not ts_dirs or not ru_dirs:
        print(f'negctl 基线不可用：{root} 下有效轮次 ts={len(ts_dirs)} rust={len(ru_dirs)}（rc=3，不判 MISMATCH）')
        print_bad_rounds(ts_bad, ru_bad)
        return 3
    if ts_bad or ru_bad:
        print(f'# 注意：{len(ts_bad) + len(ru_bad)} 个无效轮次已排除，基线与 A/B/C… 一律取首个有效轮')
    ts_dir = ts_dirs[0]
    ru_dir = ru_dirs[0]
    base_ts = load(os.path.join(ts_dir, 'artifacts', trace))
    base_ru = load(os.path.join(ru_dir, 'artifacts', trace))
    # 基线前置检查**只有一处**：build_cases 里的 need_index / 显式 raise（含 A/B/C/G 依赖的
    # gsf）。这里不再重复检查 gsf —— 同一条件两条 rc=3 路径、两套文案，将来改口径要改两处。
    # 先把用例造出来（纯内存，必要时 rc=3 退出），再落证据目录：否则基线不可用的失败会
    # 留下一个空目录，把「独占新建」变成「跑一次就废一个名字」。
    try:
        cases = build_cases(base_ts, base_ru)
    except BaselineUnusable as exc:
        # 基线缺变异点 → rc=3（同 run() 的「有效轮次不足」口径），不是 MISMATCH。
        print(f'negctl 基线不可用（{ru_dir}）：{exc}（rc=3，不判 MISMATCH）')
        return 3

    # 证据目录独占新建：`os.makedirs` 本身就是检查（O_EXCL 语义），不是 check-then-act。
    # 旧的 `os.path.exists` 再写有窗口：两个并发进程都通过检查，随后 `write_case` 的
    # `rmtree` 会删掉对方正在被 G03 读的用例目录（假 UNKNOWN，甚至假 MISMATCH）。
    # 因此也删掉了 `write_case` 里的 `rmtree` —— 独占新建后用例目录不可能预先存在（#1969 P2）。
    # 非「已存在」的创建失败（只读父目录 / 路径中段是文件）同样归 1：两者都是「证据目录不可用」，
    # 消息点名具体原因，免得按 rc 判停的自动化把它读成同一种情形。
    # 创建成功到登记之间的窗口屏蔽 SIGTERM：否则信号落在这里 → SystemExit 在下面的 try 之外
    # 逃逸 → atexit 不认账 → 空证据目录残留，同目录名重跑得 rc=1「已存在」（#2031 第 3 条）。
    old_mask = _block_sigterm()
    try:
        try:
            os.makedirs(outdir)
        except OSError as exc:
            why = '已存在（不覆盖证据）' if isinstance(exc, FileExistsError) else f'无法创建：{exc}'
            raise SystemExit(f'--negctl-out 必须是可独占新建的目录：{outdir} {why}')
        # 从这一刻到跑完，目录归本进程所有：中途被打断就由 atexit 删掉（不留残缺证据）。
        _PENDING_OUTDIRS.add(outdir)
    finally:
        _restore_sigmask(old_mask)

    def write_case(name, ts_recs, ru_recs):
        d = os.path.join(outdir, name)
        for side, recs, src in (('ts', ts_recs, ts_dir), ('rust', ru_recs, ru_dir)):
            dst = os.path.join(d, f'{side}-r1', 'artifacts')
            shutil.copytree(src, os.path.join(d, f'{side}-r1'))
            write_trace(os.path.join(dst, trace), recs)
        return os.path.join(d, 'ts-r1'), os.path.join(d, 'rust-r1')

    ok = True
    unjudged = 0
    try:
        for name, ts_recs, ru_recs, want, sc in cases:
            td, rd = write_case(name, ts_recs, ru_recs)
            got, ev, cause = judge(sc or scenario, td, rd, ts_recs, ru_recs)
            mark = case_mark(got, want, cause)
            if mark == 'UNJUDGED':
                # 装置/比较器故障 ≠ 判据被证伪：单独计，不置 MISMATCH（#1926 r4 评审 P1）。
                unjudged += 1
            elif mark == 'MISMATCH':
                ok = False
            print(f'negctl {name:32s} 期望={want:9s} 实得={got:9s} {mark}  {ev[0][:110]}')
    except (KeyboardInterrupt, SystemExit):
        # 半成品证据不许留下：独占新建保证 outdir 归本进程所有，删掉是安全的。
        _drop_pending_outdir(outdir)
        raise
    except BaseException as exc:
        # 循环中途崩溃（ENOSPC / EIO / 意外形状）若以 traceback 退 1，会与本 README 登记为
        # 「证据目录已存在」的 rc=1 撞码：按 rc 判停的复查会把它读成命名冲突换名重跑，还可能
        # 把上一次崩溃留下的残缺目录（形貌与完整证据无异）当成已完成结果。因此独立 rc=9，
        # 并删掉半成品（#1969 r1 concurrency 评审 P2）。
        _drop_pending_outdir(outdir)
        _report_crash(exc, outdir)
        return 9
    _PENDING_OUTDIRS.discard(outdir)
    # 未判定的汇总必须无条件打印：MISMATCH 优先只影响退出码，不该把「其中若干条其实是装置
    # 故障」这条信息吞掉（#1969 评审 P3）。行内 rc 必须报**真正的**退出码：MISMATCH 与未判定
    # 并存时进程退 4，写成 rc=8 会让只读日志的审计者把「判据被证伪」读成「装置故障」
    # （r2 boundary 评审 P2）。
    if not ok:
        rc = 4
    elif unjudged:
        rc = 8
    else:
        rc = 0
    if unjudged:
        tail = 'rc=4：另有判据被证伪，优先于未判定' if rc == 4 else 'rc=8'
        print(f'## 未判定：{unjudged} 条负控因装置/比较器故障未判定（{tail}），不构成 MISMATCH')
    return rc


def _flush_output():
    """显式 flush 两条流。

    stdout 是块缓冲（本机 `DEFAULT_BUFFER_SIZE=131072`）：真实批次 20 多 KB 的输出会整个留到
    解释器退出才写。那时写失败（ENOSPC）既超出 `_PipeSafe` 的捕获点、又会把 rc 改写成 120。
    在这里主动 flush，失败由 `main()` 统一折成 rc=9（#2031 第 6 条）。
    """
    sys.stdout.flush()
    sys.stderr.flush()


def _note(msg):
    """输出失败之后的收尾提示：写不进去就算了，绝不让异常逃出 main 改掉 rc。"""
    try:
        print(msg, file=sys.stderr)
        sys.stderr.flush()
    except (OSError, OutputFailure):
        pass


class _UsageParser(argparse.ArgumentParser):
    """命令行用法错误统一退 `EXIT_USAGE`。

    默认 argparse 退 2，与 `run` 的 2「反证命中（已证伪）」撞码：`frame_attrib.py <rounds>`
    少给 scenario 时 stderr 只有 usage，按 rc 判停的复查会把用法错误读成「判据被证伪」
    （#2031 r2 boundary P2）。
    """

    def error(self, message):
        def emit():
            self.print_usage(sys.stderr)
            print(f'{self.prog}: 用法错误：{message}（rc={EXIT_USAGE}）', file=sys.stderr)

        # 与 module 级守卫同一个出口：这里虽然已装配 `_PipeSafe`，但用法提示写不出去
        # （断管道 / stderr 满）不该把「用法错误」改写成 9 或 1（#2031 r4）。
        _usage_exit(emit=emit)


def main():
    # 输出接 `| head` 之类的短读者时，退出码必须是真实 rc：不加这层，缓冲/无缓冲两条路径分别
    # 退 120 / 1（实测见 README），按 rc 判停的复查会读错结论。run() 与 negctl() 都由它兜住。
    # **stderr 同样要套**：`run()` 的「有效轮次不足 + 逐轮原因」整段走 stderr，
    # `2>&1 | head -1` 下一样会撞断管道（#2031 第 1 条）。
    if not isinstance(sys.stdout, _PipeSafe):
        sys.stdout = _PipeSafe(sys.stdout)
    if not isinstance(sys.stderr, _PipeSafe):
        sys.stderr = _PipeSafe(sys.stderr, _silence_stderr)
    ap = _UsageParser()
    ap.add_argument('root')
    ap.add_argument('scenario')
    ap.add_argument('--negctl-out', default=None)
    # `is not None` 而不是真值判断：`--negctl-out ''`（未加引号的空 shell 变量）若落进 run()，
    # 调用方会按 negctl 的 rc 表解读 run 的 rc（rc=0「归因成功」被读成「负控全命中」）。
    # 空串交给下面的 makedirs 显式报错。解析也在 try 里：用法提示本身写不进去时（stderr 满）
    # 同样折 rc=9，不让 OutputFailure 逃成 traceback（rc=1）。
    try:
        args = ap.parse_args()
        if args.negctl_out is not None:
            rc = negctl(args.root, args.scenario, args.negctl_out)
        else:
            rc = run(args.root, args.scenario)
        _flush_output()
        return rc
    except OutputFailure as exc:
        # 输出（stdout/stderr）写不进去：日志已被截断，rc 不能再说「归因成功 / 判据命中」。
        # 统一 rc=9，与 negctl 的「装置自身崩溃」同码，与「读者提前退出」严格分开。
        _note(f'## 装置输出失败：{exc}（rc=9：日志已被截断，不得当作完整证据）')
        return 9


if __name__ == '__main__':
    sys.exit(main())
