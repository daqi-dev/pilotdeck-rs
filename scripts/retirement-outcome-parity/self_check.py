#!/usr/bin/env python3
"""retirement-outcome-parity 装置的自动闸门（#1809）。

把「装置自己还能不能用」变成可机检的一条命令，供 CI 与本地共用：

  budget   切片夹具的体积预算（单文件 ≤ 1 MiB、总量 ≤ 5 MiB，铁律 8 的入库上限）
  emit     `emit` 从入库夹具复算金样，必须与已提交的 `ts-oracle.json` **逐字节**相同
  check    `check` 5 轮全 PASS，且 `p7Evaluated` 为真（跨轮稳定性真的核过）
  negctl   负控 N1–N17 全 KILLED（任一非 KILLED 即红；正控注册数现 0，PC1–PC2 已随 A8.6-R2 撤回）
  lock     输出目录互斥：第二个进程必须**等**第一个（不是静默并行）
  wiring   `.github/workflows/ci.yml` 里这四件事都接着，且 `check` 聚合 job 依赖它；
           pr-gate.yml（合并闸门第 2、3 步的宿主）同一巡走：根键白名单 + job `if`
           逐字 + step 顺序/键集/env/run 标量逐字 + 每个 run step 钉 `shell: bash` +
           每个 uses step 的 `with` 逐键钉死；retirement job 的外层 timeout-minutes
           必须大于本装置内部超时的最坏和

用法：
  python3 -I scripts/retirement-outcome-parity/self_check.py            # 全部
  python3 -I scripts/retirement-outcome-parity/self_check.py --only lock --only wiring
  python3 -I scripts/retirement-outcome-parity/self_check.py --self-check-controls
      # 变异自检：给每个「守卫」造一个违反样本，断言对应检查**变红**。
      # 守卫被拆掉（体积上限删了 / flock 删了 / CI 步骤删了）时，本模式必须红——
      # 这正是「拆掉上限 / 锁 / CI 步骤 → 相应用例变红」的机器证据。

退出码：0 全通过 / 1 有检查失败 / 2 装置本身跑不了（输入不可用，fail-closed）。
"""
from __future__ import annotations

import sys

if not sys.flags.isolated:
    print(f'run with -I: python3 -I {__file__}', file=sys.stderr)
    raise SystemExit(2)

import argparse
import atexit
import functools
import hashlib
import importlib.util
import json
import os
import re
import select
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# 同族装置口径（scripts/gateway-parity/*、scripts/parity/* 均成对设置）：不写也**不读**
# 缓存字节码。本文件自己的 `check_lock` 会用 `spec_from_file_location` 把
# `check_outcome_parity.py` 在子进程里再加载一次（本目录的第三个再加载点），子进程
# 的守卫见 LOCK_HOLDER；`-I` 只是 isolated，**不等于** `-B`，所以入口这一侧也要设。
# #2452：自建前缀注册 atexit 清理（rmtree 容错）——此前三个装置入口 + 三个子进程
# 探针各自 mkdtemp 一份、全仓无任何 rmtree，`pdrs-retirement-pyc-*` 在 $TMPDIR
# 无界累积（本机开工实测 2052 个）。目录由 mkdtemp 独占创建（mode 700）且
# `dont_write_bytecode` 下恒为空目录，退出时 rmtree 只会删掉自己的空目录；OSError
# 回退路径核实不存在，不注册（rmtree 别人后建的目录不是清理自己的产物）。
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
    atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
except OSError:
    base = os.path.dirname(os.path.abspath(__file__))
    for _ in range(8):
        candidate = os.path.join(
            base, f'.no-pycache-{os.getpid()}-{os.urandom(8).hex()}')
        if not os.path.lexists(candidate):
            break
    else:
        print('self_check: cannot reserve a fresh pycache prefix', file=sys.stderr)
        raise SystemExit(2)
    sys.pycache_prefix = candidate
sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
CHECKER = HERE / 'check_outcome_parity.py'
CONTROLS = HERE / 'negative_controls.py'
FIXTURES = HERE / 'fixtures'
ORACLE = HERE / 'ts-oracle.json'
# 本文件自己的源码路径：_assert_pass_counting 的同源互证读它（数调用点）。
SELF = HERE / 'self_check.py'
WORKFLOW = REPO_ROOT / '.github' / 'workflows' / 'ci.yml'
# pr-gate.yml 的两个 run step（git fetch 基线步、check-pr-gate.py 步）是合并闸门第 2、3
# 步的宿主，此前不在任何守卫覆盖内（评审 r8 P1：self_check 与兄弟守卫都只读 ci.yml）。
PR_GATE = REPO_ROOT / '.github' / 'workflows' / 'pr-gate.yml'


def _load_checker():
    """按 spec 再加载一份 check_outcome_parity（pycache_prefix 已在模块头设好）。

    只为读 `EXPECTED_CONTROLS` / `EXPECTED_CONTROL_IDS` 等登记值：名册与条数的
    单一登记点在 check_outcome_parity.py，negative_controls 的执行器与这里的
    check_negctl 同源读，不各抄一份（评审 r7 P1——控件集被删到 0 条时执行器
    空真判 PASS；#2378——条数拦不住改名/重复，登记值升级为 ID 全集）。
    #2452：checker 模块体第一件事是自建一个新的 `sys.pycache_prefix`——进程内
    exec 会把宿主的前缀顶掉（复写进程级状态）。这里保存/还原：加载期间用 checker
    自建的隔离前缀（它自己的 atexit 负责清理），加载完宿主前缀原样恢复。
    """
    previous = sys.pycache_prefix
    try:
        spec = importlib.util.spec_from_file_location('check_outcome_parity', CHECKER)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        sys.pycache_prefix = previous


CHECK = _load_checker()
EXPECTED_CONTROLS = CHECK.EXPECTED_CONTROLS
EXPECTED_POSITIVES = CHECK.EXPECTED_POSITIVES
# #2378：ID 名册（条数拦不住改名/重复），check_negctl 与执行器同源读这里。
EXPECTED_CONTROL_IDS = CHECK.EXPECTED_CONTROL_IDS
EXPECTED_POSITIVE_IDS = CHECK.EXPECTED_POSITIVE_IDS
# 判决词表（r4 sibling#1）：期望 counts 的键集也从登记点同源取，不做第四份
# 字面量副本——执行器词表扩/改后 self_check 按旧键集比较，闸门要跑完最长
# 1800 s 的负控轮才暴露。
NEGATIVE_VERDICTS = CHECK.NEGATIVE_VERDICTS
POSITIVE_VERDICTS = CHECK.POSITIVE_VERDICTS
# 各检查子进程的内部超时（评审 r9 P2：外层 timeout-minutes 的预算算式要用它们，
# 之前是散在调用点的字面量——抬哪个都改不动派生断言）。最坏和见
# INTERNAL_WORST_CASE_SECONDS。
EMIT_TIMEOUT_SECONDS = 300
CHECK_TIMEOUT_SECONDS = 300
NEGCTL_TIMEOUT_SECONDS = 1800

# 铁律 8 的入库上限：单文件超 10 MB 要先想清楚。这里收得更紧，给未来重采留余量。
FIXTURE_FILE_LIMIT_BYTES = 1 << 20          # 1 MiB
FIXTURE_TOTAL_LIMIT_BYTES = 5 << 20         # 5 MiB
EXPECTED_ROUNDS = 5
# 持锁方**等释放信号**（不是睡固定秒数），因此这里只是「父进程失联」时的兜底上限，
# 不参与任何判定；判定用的事件是第二个进程打出「等锁」提示。
LOCK_PROBE_HOLD_SECONDS = 60.0
LOCK_PROBE_WAIT_TIMEOUT_SECONDS = 60.0
# 预置敌意锁文件（FIFO / 符号链接 / 目录）时，持锁方必须**很快**失败；这个上限只用来
# 把「挂起」判成红，正常路径远低于它。
LOCK_HOSTILE_TIMEOUT_SECONDS = 20.0
# 敌意样本超时路径上的清理 wait（kill 后收尸）：计入外层预算的最坏和。
HOSTILE_CLEANUP_WAIT_SECONDS = 30.0
# 与 check_outcome_parity.LOCK_BASENAME 同步的**字面**副本：故意不 import，
# 这样改了常量却忘了改控件会当场红（下面还有一条源码断言兜住重命名）。
LOCK_BASENAME_LITERAL = '.retirement-outcome-parity.lock'

# 子进程里持有输出目录锁的最小驱动（与 negative_controls 用**同一个** output_lock）。
# 取到锁后打 `ACQUIRED <monotonic>`、放锁前取时戳、放锁后打 `RELEASED <时戳>`：
# 父进程用**两个进程的时间戳序**证明互斥真的成立（评审 r7 P2——「等锁」那行字符串
# 只是「在等」的自述，吞掉 BlockingIOError 继续跑的弱化同样能打出它）。
LOCK_HOLDER = r'''
import atexit, importlib.util, os, select, shutil, sys, tempfile, time
# 同族口径：加载 checker **之前**先把字节码缓存关掉。`-I` 不等于 `-B`，
# `spec_from_file_location` 在 exec 前会读本目录的 __pycache__/*.pyc——
# 预置的 unchecked-hash pyc 可以只打印 LOCKED 而不真持锁，锁守卫就假 PASS 了。
# #2452：自建前缀注册 atexit 清理（同三个装置入口），探针退出即收。
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
    atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
except OSError:
    print('lock holder: cannot reserve a fresh pycache prefix', file=sys.stderr)
    raise SystemExit(2)
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('check_outcome_parity', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
try:
    with module.output_lock(sys.argv[2]):
        # 取锁信号与时戳合并为**单次** print（评审 r8 P0）：分两次 flush 时，父进程的
        # `_read_until(b'LOCKED')` 恰好在两次 write 之间读到块边界即返回，`ACQUIRED t1`
        # 留在管道里、被后来的 `_read_until(b'RELEASED')` 一并读入——时戳锚若不区分
        # ACQUIRED / RELEASED 就会锚到取锁时刻。单次写入让「信号 + 时戳」原子到达；
        # 父进程的锚正则仍按锚名取值（见 _monotonic_stamp），两道修法互为冗余。
        print(f'LOCKED ACQUIRED {time.monotonic():.9f}', flush=True)
        # 持到父进程写一行（或 stdin 关闭返回 EOF）为止；argv[3] 只是兜底上限。
        readable, _, _ = select.select([sys.stdin], [], [], float(sys.argv[3]))
        if readable:
            sys.stdin.readline()
        # RELEASED 的时戳必须**在退出 with 块（真正放锁）之前**取：时戳取晚了的
        # 话，第二个进程的 ACQUIRED 可能先于我们打印（解锁→对方取锁→对方打印
        # 都发生在我们取时戳之前），把真互斥误判成假红。取早了只是把 RELEASED
        # 往前挪，断言方向（第二个 ACQUIRED >= 第一个 RELEASED）仍然成立。
        released_at = time.monotonic()
except module.InputError as error:
    print('INPUT_ERROR', error, file=sys.stderr)
    raise SystemExit(2)
print(f'RELEASED {released_at:.9f}', flush=True)
'''


class CheckFailure(Exception):
    """一条检查不通过（rc 1）。"""


# —— 各检查 ——

def check_budget(fixtures=None):
    """切片夹具体积预算。超限说明有人把重采的全量数据又塞回库里。"""
    root = Path(fixtures or FIXTURES)
    files = sorted(path for path in root.rglob('*') if path.is_file())
    if not files:
        raise CheckFailure(f'{root}: no fixture files found')
    per_file = {str(path.relative_to(root)): path.stat().st_size for path in files}
    total = sum(per_file.values())
    oversized = {name: size for name, size in per_file.items()
                 if size > FIXTURE_FILE_LIMIT_BYTES}
    if oversized:
        raise CheckFailure(
            f'single fixture file over {FIXTURE_FILE_LIMIT_BYTES} bytes: '
            f'{ {name: size for name, size in oversized.items()} }')
    if total > FIXTURE_TOTAL_LIMIT_BYTES:
        raise CheckFailure(
            f'fixtures total {total} bytes over {FIXTURE_TOTAL_LIMIT_BYTES} bytes')
    return f'{len(per_file)} files, total {total} B, max {max(per_file.values())} B'


def fixture_round_dirs(fixtures=None):
    root = Path(fixtures or FIXTURES)
    rounds = sorted(path for path in root.iterdir()
                    if (path / 'driver' / 'methods.json').is_file())
    if len(rounds) != EXPECTED_ROUNDS:
        raise CheckFailure(f'expected {EXPECTED_ROUNDS} fixture rounds, '
                           f'found {[path.name for path in rounds]}')
    return rounds


def _sha256_file(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def check_emit():
    """`emit` 复算必须与已提交金样逐字节相同（金样来源可复现）。"""
    rounds = fixture_round_dirs()
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-emit-') as tmp:
        target = Path(tmp) / 'ts-oracle.json'
        command = [sys.executable, '-I', str(CHECKER), 'emit', '--oracle', str(target)]
        for round_dir in rounds:
            command += ['--round', str(round_dir)]
        result = subprocess.run(command, capture_output=True, text=True,
                                timeout=EMIT_TIMEOUT_SECONDS, check=False)
        if result.returncode != 0:
            raise CheckFailure(f'emit rc={result.returncode}: '
                               f'{result.stderr.strip()[:800]}')
        produced = target.read_bytes()
    committed = ORACLE.read_bytes()
    if produced != committed:
        raise CheckFailure('emit output differs from the committed ts-oracle.json; '
                           'regenerate the oracle (see fixtures/PROVENANCE.md)')
    _assert_emit_publish_failure_surface()
    return f'byte-identical to {ORACLE.name} ({len(committed)} B)'


def _assert_emit_publish_failure_surface(checker=None):
    """emit 发布失败孪生（fix-r15，DS r14 sibling#3 / MM sibling#1 / MM
    boundary#1 三撞 P2a）：失败路径清理错不得顶掉原发布错。

    三份 check_frozen 的同款不变量已有 frozen-manifest 夹具的孪生+负控；emit 侧
    此前只在成功路径上比金样字节，`publish_json` 的失败分支是裸的。进程内加载
    真实 checker 源（同 `_load_negative_controls` 的 `pycache_prefix` 保存/还原
    纪律），直接驱动 `publish_json` 的两半：
    - 发布成功 + 清理会失败：金样照常落地（成功路径不得被清理反咬——将来有人
      给成功路径加清理也必须吞掉清理错，与 check_frozen 的孪生同款）；
    - 发布失败 + 清理也失败：冒出来的必须是发布自己的错（裸 unlink 会把清理错
      顶上去＝r14 修掉的归因掩蔽形态）。
    牙齿由 control_emit 的「裸化 unlink」变异提供（--self-check-controls）。
    """
    saved_prefix = sys.pycache_prefix
    try:
        spec = importlib.util.spec_from_file_location(
            'checker_publish_probe', checker or CHECKER)
        module = importlib.util.module_from_spec(spec)
        try:
            spec.loader.exec_module(module)
        except SystemExit as error:
            raise CheckFailure(
                'cannot load the checker for the publish-failure twin: '
                f'{CHECKER.name} exited during import with rc={error.code!r}'
            ) from error
    finally:
        sys.pycache_prefix = saved_prefix
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-emit-twin-') as tmp:
        target = Path(tmp) / 'oracle.json'
        original_unlink = Path.unlink
        original_replace = os.replace

        def guarded_unlink(self, missing_ok=False):
            raise OSError('#emit-twin cleanup boom')

        try:
            # ① 成功路径：清理失败不得拖红发布。
            Path.unlink = guarded_unlink
            module.publish_json(target, {'probe': 'success'})
        except OSError as error:
            raise CheckFailure('the emit publish is undone by a cleanup failure '
                               f'on the success path: {error!r}') from error
        finally:
            Path.unlink = original_unlink
        expected = json.dumps({'probe': 'success'}, ensure_ascii=False,
                              indent=2) + '\n'
        if not target.is_file() or target.read_text(encoding='utf-8') != expected:
            raise CheckFailure('the emit publish did not land the oracle bytes '
                               'while the cleanup path is hostile')

        def failing_replace(src, dst):
            raise OSError('#emit-twin publish boom')

        try:
            # ② 失败路径：发布错必须冒自己的错——裸 unlink 的清理错会顶掉它。
            os.replace = failing_replace
            Path.unlink = guarded_unlink
            module.publish_json(target, {'probe': 'failure'})
        except OSError as error:
            if '#emit-twin publish boom' not in str(error):
                raise CheckFailure(
                    'the emit publish failure surfaced the wrong error (a '
                    f'cleanup failure must not mask it): {error!r}') from error
        else:
            raise CheckFailure('the emit publish failure was swallowed entirely')
        finally:
            os.replace = original_replace
            Path.unlink = original_unlink


def _provenance_original_digests(fixtures=None):
    """解析 `fixtures/PROVENANCE.md` 登记的**原始采集** sha256 表（methods + frames）。

    评审 r9 P2（boundary#4）：r8 的「5 轮 sha256 互异」只挡**字节级同片**——保留
    `(outcome, errorCode)` 不变、每轮塞一个判据不消费的字节即可骗过机检（解析值
    集合断言在 P7 稳定判据下恒真空洞：5 轮解析值必须逐轮相同，否则 check 本身就
    FAIL——见 RESULT 的实测）。真正钉住「5 份独立采集」的外部锚是 PROVENANCE 表
    登记的**原始采集哈希**（对照 #1477 仓外原始数据可审计）；这里把入库件的复算
    哈希与该表交叉核对：改任何一份入库 methods.json（哪怕只加一个不消费字节）
    都当场红。r10 P1（评审 MM boundary#2 = DS boundary#3）：frames.jsonl 此前是
    自指行（引用由入库夹具 emit 出的金样 `rounds.files[].framesSha256`），没有
    外部锚——给任一轮 frames.jsonl 塞一个判据不消费的字节照样全绿；r10 起两个
    文件都逐轮登记原始哈希（三列表，见 PROVENANCE.md），与 methods 同口径交叉
    核对。表格式漂移（行缺失 / 哈希列不再是 64 hex）fail-closed 判红，不猜。
    """
    provenance = Path(fixtures or FIXTURES) / 'PROVENANCE.md'
    try:
        text = provenance.read_text(encoding='utf-8')
    except OSError as error:
        raise CheckFailure(f'cannot read {provenance}: {error}') from error
    methods, frames = {}, {}
    for match in re.finditer(
            r'^\| (round-\d{2}) \| `([0-9a-f]{64})` \| `([0-9a-f]{64})` \|$',
            text, re.M):
        if match.group(1) in methods:
            raise CheckFailure(
                f'{provenance}: round {match.group(1)} is registered twice in the '
                'original-collection digest table')
        methods[match.group(1)] = match.group(2)
        frames[match.group(1)] = match.group(3)
    if not methods:
        raise CheckFailure(
            f'{provenance}: no original-collection digest rows parsed; the '
            'provenance cross-check refuses to pass on an unparsable table')
    return {'methods': methods, 'frames': frames}


def check_check(fixtures=None):
    """`check` 必须对入库的 5 轮全 PASS，并且 P7 真的跨轮核过。"""
    rounds = fixture_round_dirs(fixtures)
    # P7 非退化的机检（评审 r8 P2）：5 轮 methods.json / frames.jsonl 的 sha256 必须
    # 各自互异。`check` 报告不携带文件哈希（P7 判据只读解析后的 (outcome, errorCode)，
    # 文件哈希不进判据），所以这里直接对入库夹具文件复算——切回「去掉容差字段」的
    # 切片会让 5 轮 methods.json 字节级相同、审计链（5 份独立采集）失效，当场红。
    methods_digests = {_sha256_file(round_dir / 'driver' / 'methods.json')
                      for round_dir in rounds}
    frames_digests = {_sha256_file(round_dir / 'driver' / 'frames.jsonl')
                      for round_dir in rounds}
    if len(methods_digests) != EXPECTED_ROUNDS or len(frames_digests) != EXPECTED_ROUNDS:
        raise CheckFailure(
            f'fixture rounds are not {EXPECTED_ROUNDS} independent collections: '
            f'{len(methods_digests)} distinct methods.json and {len(frames_digests)} '
            'distinct frames.jsonl sha256; re-slicing that drops the tolerance fields '
            'makes the rounds byte-identical and collapses the provenance audit chain')
    # r9 P2（boundary#4）：互异只挡「字节级同片」，挡不住「同源重切片 + 每轮塞一个
    # 不消费字节」。入库 methods.json 的复算哈希还必须与 PROVENANCE 表登记的
    # **原始采集**哈希逐轮全等（原始数据在仓外可审计，攻击者改了入库件却改不了
    # #1477 的采集记录；同步改表即改审计链，属可见伪造而非静默漂移）。
    # r10 P1：frames.jsonl 同口径——此前它只有自指锚（金样本身从入库件 emit 出），
    # 改一个判据不消费的字节全绿；现在与 methods 一样逐轮对表。
    recomputed_methods = {
        round_dir.name: _sha256_file(round_dir / 'driver' / 'methods.json')
        for round_dir in rounds}
    recomputed_frames = {
        round_dir.name: _sha256_file(round_dir / 'driver' / 'frames.jsonl')
        for round_dir in rounds}
    provenance = _provenance_original_digests(fixtures)
    if provenance['methods'] != recomputed_methods:
        raise CheckFailure(
            'fixture methods.json digests no longer match the registered '
            'original-collection digests in fixtures/PROVENANCE.md: '
            f'provenance {sorted(provenance["methods"])}, '
            f'recomputed {sorted(recomputed_methods)}; '
            'a byte added to (or re-sliced from) any round breaks the independent-'
            'collection audit chain even when every (outcome, errorCode) stays '
            'golden-identical')
    if provenance['frames'] != recomputed_frames:
        raise CheckFailure(
            'fixture frames.jsonl digests no longer match the registered '
            'original-collection digests in fixtures/PROVENANCE.md: '
            f'provenance {sorted(provenance["frames"])}, '
            f'recomputed {sorted(recomputed_frames)}; '
            'a byte added to (or re-sliced from) any round breaks the independent-'
            'collection audit chain even when every (outcome, errorCode) and the '
            'wire frames stay golden-identical (the r10 P1 fix has rotted)')
    command = [sys.executable, '-I', str(CHECKER), 'check', '--oracle', str(ORACLE)]
    for round_dir in rounds:
        command += ['--round', str(round_dir)]
    result = subprocess.run(command, capture_output=True, text=True,
                            timeout=CHECK_TIMEOUT_SECONDS, check=False)
    if result.returncode != 0:
        raise CheckFailure(f'check rc={result.returncode}: '
                           f'{(result.stdout or result.stderr).strip()[:800]}')
    try:
        report = json.loads(result.stdout)
    except ValueError as error:
        raise CheckFailure(f'check stdout is not JSON: {error}') from error
    if report.get('status') != 'PASS':
        raise CheckFailure(f"check status {report.get('status')!r}")
    if not report.get('p7Evaluated'):
        raise CheckFailure('p7Evaluated is false: cross-round stability was not checked')
    if len(report.get('rounds') or []) != EXPECTED_ROUNDS:
        raise CheckFailure(f"check covered {len(report.get('rounds') or [])} rounds, "
                           f'expected {EXPECTED_ROUNDS}')
    return f'{EXPECTED_ROUNDS} rounds PASS, violations {report.get("violationsCount")}'


def _assert_run_positive_tail(source):
    """#2397 rebase 解评审（DS delta/anchor 两轮）源码锚守卫。

    run_positive 尾部的 HELD/BROKEN 落判 + ``return record`` 曾被 rebase 重放静默
    丢行——origin/main 有、重放后无，当时全部机检绿（正控注册数 0，该路径不可达，
    「HELD==EXPECTED_POSITIVES」空真）。本函数用 ``ast.parse`` 断言这两条语句是
    ``run_positive`` 函数体的**最后两条**：只证「出现在区间里」的裸子串锚抓不住
    「两行被挪进 rc==1 分支 / 只丢 return / 整块被删」的形态，也会被合法重排假红；
    ast.unparse 归一化后比对，改格式不红、改语义（挪分支、丢行、改名）必红。
    第三面是**可达性**（anthropic p1fix / p1fix2 boundary P1）：末两行前若有必然执行的
    return/raise（含必执行复合语句体内的——with / async with 体、try / except* 语句的
    body 与 finalbody、常量真值 if / while 体），锚两行成死代码——形状逐字匹配但永不
    执行，同样是「校验的不是生效的那份」；条件路径（if / except handler / for / match /
    非常量 while / 循环 else）豁免，不误伤正常源。常量折叠病态（如 ``while 1+1:``）
    不折叠、超出本面（见函数体内声明边界）。
    """
    import ast  # 局部导入：仅本守卫需要，模块头保持原导入面
    try:
        tree = ast.parse(source)
    except SyntaxError as error:
        raise CheckFailure(f'negative_controls.py 解析失败：{error!r}') from None
    # 同名 def 只认一份：Python 模块执行时绑定**最后**一份 def，坏合并「保留双方」
    # 产出两份 run_positive 时，只校验第一份会拿死代码当真身（DS anchor2 boundary P1）。
    defs = [node for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == 'run_positive']
    if len(defs) != 1:
        raise CheckFailure(f'negative_controls.py 有 {len(defs)} 份顶层 def run_positive（应为恰 1 份；'
                           '重复定义即坏合并形态，#2397 源码锚点拒收）')
    node = defs[0]
    if not (len(node.body) >= 2
            and ast.unparse(node.body[-2]) ==
            "record.update(verdict='HELD' if ok else 'BROKEN', reason=reason)"
            and ast.unparse(node.body[-1]) == 'return record'):
        raise CheckFailure('run_positive 尾部 HELD/BROKEN 落判缺失（#2397 rebase 丢行类源码锚点：'
                            '函数体末两条语句应为 record.update(verdict=...) 与 return record，'
                            f'实际 {ast.unparse(node.body[-2:])!r}）')
    # 可达性：锚两行必须是函数体控制流的**必经出口**。之前的语句里若有必然执行的
    # return/raise，锚两行即死代码——形状逐字匹配但永不执行。扫描不只看函数体直接
    # 子语句，还下钻「必执行」复合语句体：with / async with 体、try / except* 语句的
    # body 与 finalbody、常量真值 if / while 体（anthropic p1fix2 boundary P1）；条件
    # 路径（if / except handler / for / match / 非常量 while / 循环 else）豁免，不误伤
    # 正常源（现有 return 全在 if / except 条件分支内）。
    # 声明边界：`while 1+1:` 类常量折叠病态不折叠（test 非字面常量即不视为必执行）；
    # for / while 的 else 仅无 break 时必执行，同样不视为必经——两类形态超出本面。
    try_types = tuple(t for t in (getattr(ast, 'Try', None),
                                  getattr(ast, 'TryStar', None)) if t is not None)

    def const_true(test):
        return isinstance(test, ast.Constant) and bool(test.value)

    def must_execute(stmts):
        for stmt in stmts:
            yield stmt
            if isinstance(stmt, (ast.With, ast.AsyncWith)):
                yield from must_execute(stmt.body)
            elif isinstance(stmt, try_types):
                yield from must_execute(stmt.body)
                yield from must_execute(stmt.finalbody)
            elif isinstance(stmt, (ast.If, ast.While)) and const_true(stmt.test):
                yield from must_execute(stmt.body)

    early = next((stmt for stmt in must_execute(node.body[:-2])
                  if isinstance(stmt, (ast.Return, ast.Raise))), None)
    if early is not None:
        raise CheckFailure(f'run_positive 末尾锚两行不可达：第 {early.lineno} 行的必执行路径上 '
                           f'有 {type(early).__name__} 截断控制流，锚两行成死代码'
                           '（形状匹配但永不执行，#2397 源码锚点拒收）')
    return


def check_negctl():
    """负控必须全部 KILLED、正控必须全部 HELD（正控注册数现 0；A8.6-R1 的 PC1–PC2 与 N18 已随 #2294 按反转条款撤回）。"""
    _assert_run_positive_tail(CONTROLS.read_text(encoding='utf-8'))
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-negctl-') as tmp:
        result = subprocess.run(
            [sys.executable, '-I', str(CONTROLS), '--output', tmp],
            capture_output=True, text=True, timeout=NEGCTL_TIMEOUT_SECONDS,
            check=False)
        summary_path = Path(tmp) / 'summary.json'
        if not summary_path.is_file():
            raise CheckFailure(f'negative controls produced no summary '
                               f'(rc={result.returncode}): {result.stderr.strip()[:800]}')
        summary = json.loads(summary_path.read_text(encoding='utf-8'))
    counts = summary.get('counts') or {}
    positive_counts = summary.get('positiveCounts') or {}
    if result.returncode != 0 or summary.get('status') != 'PASS':
        raise CheckFailure(f'negative controls rc={result.returncode} '
                           f'status={summary.get("status")!r} counts={counts} '
                           f'positiveCounts={positive_counts}')
    # #2378：计数与名册都钉死。计数只对 KILLED / HELD 逐键核过条数，词表外键
    # （SURVIVED==0 且 INVALID==0 的「两个零」）拦不住记录侧缩水；这里直接要求
    # counts **整字典**等于期望值。名册从 summary 的 controls / positives 条目
    # 里读 id（执行器侧已拒绝不等名册，这里再核一遍「self_check 拿到的 summary
    # 确实覆盖了登记全集」，防两端各改一边后仍然互相自洽地变绿）。
    # r4 sibling#1：期望 counts 的键集从登记点的词表同源推导（不再写字面量），
    # 整字典比较保留——执行器词表多出词表外键时这里照样不等。
    expected_counts = dict.fromkeys(NEGATIVE_VERDICTS, 0)
    expected_counts['KILLED'] = EXPECTED_CONTROLS
    if counts != expected_counts:
        raise CheckFailure(f'expected counts {{KILLED: {EXPECTED_CONTROLS}, '
                           f'SURVIVED: 0, INVALID: 0}}, got {counts}')
    # 正控条数同样钉死（r9 合并补全）：build_positives 被删空时 positiveCounts 全 0、
    # status 仍 PASS——「应当 PASS 的性质」静默失保；#2378 同样升级为整字典 + ID 名册。
    expected_positive_counts = dict.fromkeys(POSITIVE_VERDICTS, 0)
    expected_positive_counts['HELD'] = EXPECTED_POSITIVES
    if positive_counts != expected_positive_counts:
        raise CheckFailure(f'expected positiveCounts {{HELD: {EXPECTED_POSITIVES}, '
                           f'BROKEN: 0, INVALID: 0}}, got {positive_counts}')
    control_ids = sorted(str(entry.get('id')) for entry
                         in summary.get('controls') or [])
    if control_ids != sorted(EXPECTED_CONTROL_IDS):
        raise CheckFailure(f'summary control ids {control_ids} do not match the '
                           f'registered roster {sorted(EXPECTED_CONTROL_IDS)} (#2378)')
    positive_ids = sorted(str(entry.get('id')) for entry
                         in summary.get('positives') or [])
    if positive_ids != sorted(EXPECTED_POSITIVE_IDS):
        raise CheckFailure(f'summary positive ids {positive_ids} do not match the '
                           f'registered roster {sorted(EXPECTED_POSITIVE_IDS)} (#2378)')
    return (f'{counts["KILLED"]}/{EXPECTED_CONTROLS} KILLED, '
            f'{positive_counts["HELD"]}/{EXPECTED_POSITIVES} HELD, '
            '0 SURVIVED/INVALID/BROKEN')


def _spawn_lock_holder(root, hold_seconds, stdin, checker=None):
    return subprocess.Popen(
        [sys.executable, '-I', '-c', LOCK_HOLDER, str(checker or CHECKER), str(root),
         str(hold_seconds)],
        stdin=stdin, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def _read_until(stream, marker, timeout, label):
    """在子进程输出上等到 ``marker`` 出现，超时即 CheckFailure。

    用 ``select`` + ``os.read`` 而不是 ``readline()``：子进程若在 exec / 取锁前挂死，
    裸 ``readline()`` 会把整个自检无限挂起（只能靠 CI job 的 20 min 收红）。
    """
    descriptor = stream.fileno()
    deadline = time.monotonic() + timeout
    data = b''
    while marker not in data:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise CheckFailure(f'{label}: timed out after {timeout:.0f}s waiting for '
                               f'{marker!r}; got {data[-400:]!r}')
        ready, _, _ = select.select([descriptor], [], [], remaining)
        if not ready:
            continue
        chunk = os.read(descriptor, 4096)
        if not chunk:  # EOF：不再等，由调用方按 marker 是否出现判断
            break
        data += chunk
    return data


# 时戳锚必须**逐名**指定（评审 r8 P0）：合并正则 `(?:ACQUIRED|RELEASED)` 取首个
# 匹配，在「父进程等 LOCKED 时恰好读到块边界、`ACQUIRED t1` 留在管道里被后来的
# RELEASED 读取一并收入」的交错下会锚到 ACQUIRED——RELEASED 锚错成**取锁**时刻，
# 断言退化成「second 的取锁 ≥ first 的取锁」恒真，被弱化的 flock 恰从这条守卫下
# SURVIVED；且错取方向**永不假红**，只静默丢失检测力。
RELEASED_STAMP = re.compile(rb'RELEASED (\d+\.\d+)')
ACQUIRED_STAMP = re.compile(rb'ACQUIRED (\d+\.\d+)')


def _monotonic_stamp(data, anchor, label):
    """从持锁子进程的输出里取**指定锚**（ACQUIRED / RELEASED）后面的时戳。

    `time.monotonic()` 在同一台主机上跨进程可比（单调钟是系统范围的），所以
    「第二个的 ACQUIRED ≥ 第一个的 RELEASED」是互斥的时间戳序证明。锚错方向的
    锚（如合并正则取首匹配）由 control_lock 的时戳锚控件与弱化 flock 变异兜住。
    """
    match = anchor.search(data)
    if match is None:
        raise CheckFailure(f'{label}: no {anchor.pattern!r} timestamp found '
                           f'in {data[-200:]!r}')
    return float(match.group(1))


# 敌意锁文件样本的登记簿（评审 r8 P2，对齐 SAMPLE_FAILURE_ANCHORS 先例）：check_lock
# 消费这份登记并断言条数与登记值全等——删掉一条（少一种敌意形态）当场红，而不是
# 少测一种还全绿。数量同步登记在装置 README。
LOCK_HOSTILE_PLANTS = (
    ('planted FIFO', os.mkfifo),
    ('planted directory', os.mkdir),
    ('planted symlink', lambda path: os.symlink('/etc/hosts', path)),
)
EXPECTED_LOCK_HOSTILE_PLANTS = 3


def _assert_lock_path_rejected(root, label, checker=None):
    """预置敌意锁文件时，持锁方必须**快速**非零退出，绝不能挂起或假装持锁。

    这是 P0 回归钉：锁文件用 `O_RDONLY|O_CREAT` 打开，若不带 `O_NONBLOCK` 且不核
    `S_ISREG`，预置一个同名 FIFO 就会让 `open` 永久阻塞（Codex r2 实测），整个装置
    连同 CI job 一起挂死；预置目录 / 符号链接则分别是静默成功与 ELOOP。
    """
    process = _spawn_lock_holder(root, 0.0, subprocess.DEVNULL, checker)
    try:
        out, err = process.communicate(timeout=LOCK_HOSTILE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=HOSTILE_CLEANUP_WAIT_SECONDS)
        raise CheckFailure(
            f'{label}: `output_lock` hung on a planted non-regular lock file '
            f'(no exit within {LOCK_HOSTILE_TIMEOUT_SECONDS:.0f}s)')
    if b'LOCKED' in out:
        raise CheckFailure(f'{label}: `output_lock` accepted a planted lock file '
                           f'(printed LOCKED, rc={process.returncode})')
    if process.returncode == 0:
        raise CheckFailure(f'{label}: `output_lock` exited 0 on a planted lock file '
                           f'(out={out[-200:]!r})')
    if b'INPUT_ERROR' not in err and b'InputError' not in err:
        raise CheckFailure(f'{label}: rejection was not the documented InputError '
                           f'(rc={process.returncode}, err={err[-300:]!r})')
    return process.returncode


# 负控执行器的**守卫 fd**（`_output_lock_guard_fd`）在敌意锁文件上的上限：正常路径
# 立即返回（打开失败 / S_ISREG 拒 → 当被占），这个上限只把「挂起」判成红。
GUARD_FD_TIMEOUT_SECONDS = 20.0
# 子进程里直接调 negative_controls.invalidate_stale_summary(respect_output_lock=True)
# 的最小驱动（与 LOCK_HOLDER / WRITE_PROBE 同族口径：先设 pycache_prefix 再加载模块
# ——预置的 unchecked-hash pyc 不能替我们说 KEPT）。
GUARD_PROBE = r'''
import atexit, importlib.util, shutil, sys, tempfile
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
    atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
except OSError:
    print('guard probe: cannot reserve a fresh pycache prefix', file=sys.stderr)
    raise SystemExit(2)
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('negative_controls', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
kept = module.invalidate_stale_summary(sys.argv[2], respect_output_lock=True)
print(f'GUARD {"KEPT" if kept else "DROPPED"}', flush=True)
'''


# 守卫 fd 三变异的登记簿（r9 P2 落地、r10 P0 提为模块级常量）：control_lock 逐条
# 把临时 negative_controls 副本退回 r8 之前的口径，check_lock 的守卫 fd 探针必须红
# 在各自的专属判定上（锚串取自 `_assert_guard_fd_fail_closed` 的专属文案）。外层
# 预算的锁趟数算式（LOCK_CHECK_PASSES，见下）从这份登记簿**推导**——新增 / 删除
# 一条变异时趟数自动跟上，不再手抄。
GUARD_FD_MUTATIONS = (
    ('guard fd without O_NONBLOCK',
     'os.O_RDONLY | os.O_CREAT | nofollow | nonblock',
     'os.O_RDONLY | os.O_CREAT | nofollow',
     'the summary guard fd hung on a planted non-regular lock'),
    ('guard fd without the S_ISREG check',
     'if not stat_module.S_ISREG(info.st_mode):',
     'if False:',
     'the summary guard deleted a published summary'),
    ('guard fd without O_NOFOLLOW',
     'os.O_RDONLY | os.O_CREAT | nofollow | nonblock',
     'os.O_RDONLY | os.O_CREAT | nonblock',
     'the summary guard deleted a published summary'),
)
# 条数钉死（#2436，对齐 EXPECTED_LOCK_HOSTILE_PLANTS 口径）：锁趟数算式
# `LOCK_CHECK_PASSES − len(GUARD_FD_MUTATIONS)` 在登记簿缩水时代数上仍成立——
# LOCK_CHECK_PASSES 本身从 len(...) 推导，删一条变异只会让两边一起降、不等式照绿，
# 「r9 的『跟随符号链接删除已发布 summary』形态不再被覆盖」这件事没有任何变红。
# 条数断言把删一条变成当场红，而不是少测一种还全绿。
EXPECTED_GUARD_FD_MUTATIONS = 3


def _assert_guard_fd_fail_closed(controls, root, label):
    """预置敌意锁文件时，负控的守卫 fd 必须把目录当**被占**处理：不挂死、不删已
    发布的 summary.json（评审 r9 P2，README「同一口径」承诺的机器证据）。

    - 守卫丢了 `O_NONBLOCK`：预置无读者端的 FIFO 上 `open(O_RDONLY)` 永久阻塞——
      整个执行器连同 CI job 一起挂死（超时判红）；
    - 守卫丢了 `S_ISREG` 判定：目录 / FIFO 被当成「锁」探到空闲 → 已发布的
      summary.json 被删（哨兵内容变没）；
    - 守卫丢了 `O_NOFOLLOW`：符号链接被跟随到普通文件 → 同上删 summary。
    """
    # #2452：超时路径上探针被 kill（SIGKILL 下 atexit 不运行），它自建/加载期建的
    # pycache 前缀会泄漏——把探针的 TMPDIR 圈进一个专属沙盒目录，父进程收尾统一
    # rmtree（kill 与正常退出都收；正常退出时探针与被加载模块的 atexit 已各自清
    # 空，rmtree 只是收空目录）。沙盒名不带 pdrs-retirement-pyc- 前缀，不混入
    # 泄漏计数的对照口径。
    sandbox = Path(tempfile.mkdtemp(prefix='pdrs-retirement-guard-probe-'))
    try:
        process = subprocess.Popen(
            [sys.executable, '-I', '-c', GUARD_PROBE, str(controls), str(root)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env={**os.environ, 'TMPDIR': str(sandbox)})
        try:
            out, err = process.communicate(timeout=GUARD_FD_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=HOSTILE_CLEANUP_WAIT_SECONDS)
            raise CheckFailure(
                f'{label}: the summary guard fd hung on a planted non-regular lock '
                f'file (no exit within {GUARD_FD_TIMEOUT_SECONDS:.0f}s); the guard '
                'must open with O_NONBLOCK and treat the directory as occupied')
        if process.returncode != 0 or b'GUARD KEPT' not in out:
            raise CheckFailure(
                f'{label}: the summary guard fd failed on a planted hostile lock '
                f'file (rc={process.returncode}, out={out[-200:]!r}, '
                f'err={err[-200:]!r})')
        sentinel = root / 'summary.json'
        if not sentinel.is_file() \
                or sentinel.read_text(encoding='utf-8') != '{"sentinel": "must-survive"}':
            raise CheckFailure(
                f'{label}: the summary guard deleted a published summary.json while '
                'the lock path was hostile; the guard must fail closed (treat the '
                'directory as occupied) instead of trusting a non-regular lock file')
        return process.returncode
    finally:
        shutil.rmtree(sandbox, ignore_errors=True)


def check_lock(checker=None, controls=None):
    """两个进程抢同一输出目录：第二个必须等（不是静默并行写）。

    时序**事件驱动**，不靠固定秒数：第一个取锁后停在那里等释放信号，第二个启动后
    必须打出「等锁」提示——那行只有在第一个真的抱着锁时才可能出现——父进程看到它
    才放开第一个。因此慢机、冷启动、runner 抖动都不会把这个守卫推成假红。
    **互斥状态可观测（评审 r7 P2 / r8 P0）**：两个持锁进程各打 `LOCKED ACQUIRED <t>`
    与 `RELEASED <t>`（`time.monotonic()`），父进程用**专属锚正则**取第二个的
    ACQUIRED 与第一个的 RELEASED，断言前者**不早于**后者——「吞掉 BlockingIOError
    继续跑」这类弱化会把自己的 ACQUIRED 提前到第一个释放之前，时间戳序当场红。
    锚必须逐名（r8 P0：合并正则取首匹配在管道分块交错下锚错方向、恒真不红），
    取锁信号与时戳合并为单次 print 消除切分窗口，两道修法互为冗余。
    之后再验一遍**敌意锁文件**路径：预置 FIFO / 目录 / 符号链接时必须快速失败。
    ``checker`` 只给变异自检（control_lock 的弱化 flock 样本）用：默认加载本装置的
    check_outcome_parity.py；``controls`` 同理（r9：守卫 fd 样本的变异副本），默认
    加载本装置的 negative_controls.py。
    """
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-lock-') as tmp:
        root = Path(tmp) / 'out'
        first = _spawn_lock_holder(root, LOCK_PROBE_HOLD_SECONDS, subprocess.PIPE,
                                   checker)
        second = None
        try:
            _read_until(first.stdout, b'LOCKED', LOCK_PROBE_WAIT_TIMEOUT_SECONDS,
                        'first lock holder')
            started = time.monotonic()
            second = _spawn_lock_holder(root, 0.0, subprocess.PIPE, checker)
            seen = _read_until(second.stderr, b'waiting for output lock',
                               LOCK_PROBE_WAIT_TIMEOUT_SECONDS, 'second lock holder')
            waited = time.monotonic() - started
            if b'waiting for output lock' not in seen:
                raise CheckFailure(
                    'second holder never reported waiting for the output lock '
                    f'(got {seen[-400:]!r}); the waiting report is the observable '
                    'half of mutual exclusion')
            # 看到「等锁」才放开第一个：第二个随后取到锁、读 stdin 见 EOF 退出。
            first.stdin.write(b'\n')
            first.stdin.flush()
            first.stdin.close()
            first_released = _read_until(first.stdout, b'RELEASED',
                                         LOCK_PROBE_WAIT_TIMEOUT_SECONDS,
                                         'first lock holder release')
            second_acquired = _read_until(second.stdout, b'ACQUIRED',
                                          LOCK_PROBE_WAIT_TIMEOUT_SECONDS,
                                          'second lock holder acquisition')
            first_rc = first.wait(timeout=LOCK_PROBE_WAIT_TIMEOUT_SECONDS)
            second_rc = second.wait(timeout=LOCK_PROBE_WAIT_TIMEOUT_SECONDS)
        finally:
            for process in (first, second):
                if process is not None and process.poll() is None:
                    process.kill()
                    process.wait(timeout=HOSTILE_CLEANUP_WAIT_SECONDS)
        if first_rc != 0:
            raise CheckFailure(f'first lock holder rc={first_rc}: '
                               f'{first.stderr.read()[:400]!r}')
        if second_rc != 0:
            raise CheckFailure(f'second lock holder rc={second_rc}: '
                               f'{second.stderr.read()[:400]!r}')
        released_at = _monotonic_stamp(first_released, RELEASED_STAMP,
                                       'first holder RELEASED')
        acquired_at = _monotonic_stamp(second_acquired, ACQUIRED_STAMP,
                                       'second holder ACQUIRED')
        if acquired_at < released_at:
            raise CheckFailure(
                f'second holder ACQUIRED at {acquired_at:.9f} but the first only '
                f'RELEASED at {released_at:.9f}: the output lock is not actually '
                'mutually exclusive (a weakened flock would let the second proceed '
                'while the first still holds the lock)')

        # 锁文件名的三份字面副本都要在源码里（评审 r8 P2）：CHECKER 之外，负控的
        # `_output_lock_guard_fd` 也有一份裸字面量——改了常量却只改两处时，这里当场红。
        if LOCK_BASENAME_LITERAL not in CHECKER.read_text(encoding='utf-8'):
            raise CheckFailure(
                f'{CHECKER.name} no longer uses the lock basename '
                f'{LOCK_BASENAME_LITERAL!r}; update this control with it')
        if LOCK_BASENAME_LITERAL not in CONTROLS.read_text(encoding='utf-8'):
            raise CheckFailure(
                f'{CONTROLS.name} no longer uses the lock basename '
                f'{LOCK_BASENAME_LITERAL!r}; update this control with it')
        if len(LOCK_HOSTILE_PLANTS) != EXPECTED_LOCK_HOSTILE_PLANTS:
            raise CheckFailure(
                f'the lock hostile-plant registry carries '
                f'{len(LOCK_HOSTILE_PLANTS)} samples but {EXPECTED_LOCK_HOSTILE_PLANTS} '
                'are registered; deleting a hostile shape must go red here, not '
                'silently narrow the guard')
        hostile_rcs = []
        for label, plant in LOCK_HOSTILE_PLANTS:
            hostile = Path(tempfile.mkdtemp(prefix='pdrs-retirement-lock-bad-', dir=tmp))
            plant(hostile / LOCK_BASENAME_LITERAL)
            hostile_rcs.append(_assert_lock_path_rejected(hostile, label, checker))
        # 负控执行器自己的**守卫 fd**（`_output_lock_guard_fd`，模块体早退路径用）也要
        # 在同一批敌意锁文件上守住（评审 r9 P2 = r8 sibling#5/boundary#5）：README 承诺
        # 它与真锁「同一口径」打开（O_NONBLOCK + S_ISREG + O_NOFOLLOW），此前没有
        # 机器证据——退回 `O_RDWR`/无 `O_NONBLOCK` 无任何断言抓。探针预置已发布的
        # summary.json：守卫必须把目录当**被占**处理（保住已发布结果），且不挂死。
        guard_rcs = []
        for label, plant in LOCK_HOSTILE_PLANTS:
            hostile = Path(tempfile.mkdtemp(prefix='pdrs-retirement-lock-guard-', dir=tmp))
            plant(hostile / LOCK_BASENAME_LITERAL)
            (hostile / 'summary.json').write_text('{"sentinel": "must-survive"}',
                                                  encoding='utf-8')
            guard_rcs.append(_assert_guard_fd_fail_closed(
                Path(controls or CONTROLS), hostile, label))
    return (f'second waited {waited:.2f}s for the output lock '
            f'(ACQUIRED {acquired_at:.6f} >= first RELEASED {released_at:.6f}); '
            f'non-regular lock paths rejected with rc={hostile_rcs}; '
            f'summary guard fd fail-closed with rc={guard_rcs}')


# —— r7（评审 MM sibling#1，b 类）：`write_at` 的敌意写目标。——
# 预置敌意锁文件时，预置 FIFO 会让 `output_lock` 永久阻塞；同一招打在**写目标**
# 上（`N1/round-01/driver/methods.json` 是 FIFO）同样让 `write_at` 的
# `O_WRONLY` open 挂死。写目标还多一个变体：**带读者端的 FIFO**——`O_NONBLOCK`
# 挡不住它（open 立即成功），只有 `fstat` 后的 `S_ISREG` 判定能拒。
WRITE_HOSTILE_TIMEOUT_SECONDS = 20.0
# 敌意写目标的登记簿（评审 r8 P2，对齐 SAMPLE_FAILURE_ANCHORS 先例）：check_writes
# 消费这份登记并断言条数与登记值全等——删掉一条（少一种敌意形态）当场红。数量同步
# 登记在装置 README。
WRITE_HOSTILE_TARGETS = (
    ('planted FIFO (no reader)', os.mkfifo, None),
    ('planted FIFO (reader held open)', os.mkfifo,
     lambda path: os.open(path, os.O_RDONLY | os.O_NONBLOCK)),
    ('planted directory', os.mkdir, None),
    ('planted symlink', lambda path: os.symlink('/etc/hosts', path), None),
)
EXPECTED_WRITE_HOSTILE_TARGETS = 4
# 子进程里直接调 negative_controls.write_at 的最小驱动（与 LOCK_HOLDER 同族口径：
# 先设 pycache_prefix 再加载模块——预置的 unchecked-hash pyc 不能替我们说 REJECTED）。
WRITE_PROBE = r'''
import atexit, importlib.util, os, shutil, sys, tempfile
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
    atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
except OSError:
    print('write probe: cannot reserve a fresh pycache prefix', file=sys.stderr)
    raise SystemExit(2)
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('negative_controls', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
fd = os.open(sys.argv[2], os.O_RDONLY)
try:
    module.write_at(fd, 'methods.json', 'probe')
except module.OutputError as error:
    print(f'REJECTED {error}', flush=True)
    raise SystemExit(0)
finally:
    os.close(fd)
print('ACCEPTED a hostile write target', flush=True)
raise SystemExit(1)
'''


def _assert_write_target_rejected(plant, label, keep_reader_open=None):
    """预置敌意写目标时，`write_at` 必须快速拒绝，绝不能挂起或照写。"""
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-write-bad-') as tmp:
        target = Path(tmp) / 'methods.json'
        plant(target)
        reader_fd = keep_reader_open(target) if keep_reader_open is not None else None
        process = subprocess.Popen(
            [sys.executable, '-I', '-c', WRITE_PROBE, str(CONTROLS), str(tmp)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            out, err = process.communicate(timeout=WRITE_HOSTILE_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=HOSTILE_CLEANUP_WAIT_SECONDS)
            raise CheckFailure(
                f'{label}: `write_at` hung on a planted non-regular write target '
                f'(no exit within {WRITE_HOSTILE_TIMEOUT_SECONDS:.0f}s)')
        finally:
            if reader_fd is not None:
                os.close(reader_fd)
        if b'REJECTED' not in out or process.returncode != 0:
            raise CheckFailure(
                f'{label}: `write_at` accepted a planted hostile write target '
                f'(rc={process.returncode}, out={out[-200:]!r}, err={err[-200:]!r})')
    return process.returncode


def check_writes():
    """负控写盘族的敌意目标：`write_at` 对非常规文件必须快速拒绝。

    敌意样本本身就在本检查里（与 `check_lock` 的敌意锁文件段同构）：FIFO / 目录 /
    符号链接，外加**带读者端的 FIFO**——把 `O_NONBLOCK` 拿掉只挡得住无读者端的
    FIFO，带读者端的只有 `S_ISREG` 判定能拒；反过来删掉 `S_ISREG` 而 `O_NONBLOCK`
    还在，无读者端样本照红。因此这组样本对「删掉其中任一守卫」都有各自的违反路径。
    敌意样本登记在 WRITE_HOSTILE_TARGETS（评审 r8 P2：删掉一条不红的日子到此为止
    ——条数与登记值全等断言当场红，数量同步登记在装置 README）。
    """
    if len(WRITE_HOSTILE_TARGETS) != EXPECTED_WRITE_HOSTILE_TARGETS:
        raise CheckFailure(
            f'the write hostile-target registry carries {len(WRITE_HOSTILE_TARGETS)} '
            f'samples but {EXPECTED_WRITE_HOSTILE_TARGETS} are registered; deleting a '
            'hostile shape must go red here, not silently narrow the guard')
    hostile_rcs = []
    for label, plant, reader in WRITE_HOSTILE_TARGETS:
        hostile_rcs.append(
            _assert_write_target_rejected(plant, label, keep_reader_open=reader))
    return f'non-regular write targets rejected with rc={hostile_rcs}'


# —— 外层预算的派生算式（评审 r9 P2 = r10 P0 复算）——
# r8 注释自设的判据是「外层 timeout-minutes > 装置内部超时最坏和」，r9 把算式落成
# 常量，但 check_lock 按「三趟」计、control_check 的两次 check_check 各自再起一个
# 300 s checker 子进程也不计——内部和虚低成 4580 s，85 min 下派生不等式恒真（尺子
# 假通过）。r10 起趟数与子进程数都从**调用结构**推导（乘数漂移由
# _assert_pass_counting 同源互证钉住），算式只修数、判据本身不动。
# check_lock 单趟最坏等待：4×等锁读（LOCKED / 等锁提示 / RELEASED / ACQUIRED，
# 各 LOCK_PROBE_WAIT_TIMEOUT_SECONDS）+ 2×进程 wait（同上限）+ 敌意锁
# 3×(20 + 清理 30 s) + 守卫 fd 探针 3×(20 + 清理 30 s)。
LOCK_WORST_CASE_SECONDS = (
    4 * LOCK_PROBE_WAIT_TIMEOUT_SECONDS
    + 2 * LOCK_PROBE_WAIT_TIMEOUT_SECONDS
    + EXPECTED_LOCK_HOSTILE_PLANTS
    * (LOCK_HOSTILE_TIMEOUT_SECONDS + HOSTILE_CLEANUP_WAIT_SECONDS)
    + EXPECTED_LOCK_HOSTILE_PLANTS
    * (GUARD_FD_TIMEOUT_SECONDS + HOSTILE_CLEANUP_WAIT_SECONDS))
WRITES_WORST_CASE_SECONDS = EXPECTED_WRITE_HOSTILE_TARGETS * (
    WRITE_HOSTILE_TIMEOUT_SECONDS + HOSTILE_CLEANUP_WAIT_SECONDS)
# 全 job（七项自检与变异自检是同一个 job 的两个 step，共用同一个 timeout-minutes
# 兜底）：check_lock 共六趟——七项自检 1 趟（CHECKS/ORDER 注册的 'lock' 项）+
# control_lock 五趟（detail 1 + 弱化 flock 样本 1 + 守卫 fd 变异循环
# len(GUARD_FD_MUTATIONS) = 3 趟）。
LOCK_CHECK_PASSES = (
    1  # 七项自检：run_checks 按 ORDER 逐名调 CHECKS 的 'lock' 项
    + 1  # control_lock 开头的 detail 重跑一趟
    + 1  # control_lock 的弱化 flock 样本一趟
    + len(GUARD_FD_MUTATIONS))  # control_lock 的守卫 fd 变异循环，每条一趟
# control_check 里 check_check 的调用次数（r10 P0：此前 r9 的两次调用各起一个
# 300 s 上限的 checker 子进程，完全不在预算算式里；r10 P1 的 frames 变异方向再加
# 一次——三个变异方向各一趟，由 _assert_pass_counting 与调用点互证）。
CONTROL_CHECK_SUBPROCESSES = 3
# runner 侧非守卫开销（checkout / venv 装 PyYAML / 上传证据）的余量。
JOB_OVERHEAD_MARGIN_SECONDS = 300
RETIREMENT_JOB_TIMEOUT_MINUTES = 170


def _assert_pass_counting():
    """同源互证（r10 P0）：预算算式里的「数」必须与本文件源码里的调用点对得上。

    趟数 / 子进程数只写在常量里时，新增 / 删除一个 `check_lock` 调用点而忘了改
    `LOCK_CHECK_PASSES`，内部和只会朝「算小」方向漂——派生不等式恒真、尺子假
    通过（r9 被评掉的形态，乘数压回旧错值不会让任何检查变红）。这里按源码文本
    数调用点并与派生式互证（先例：control_lock 的 `source.count(mutation) != 1`
    锚点计数；调用点按「函数名紧跟左括号」的字面形态数，def 行除外）：

    - CHECKS 里 `'lock'` 恰注册一次、ORDER 里 `'lock'` 恰出现一次（七项自检那趟
      的唯一入口；换个名字注册会让 ORDER 计数当场红）；
    - check_lock 的调用点（def 之外）恰为 `LOCK_CHECK_PASSES -
      len(GUARD_FD_MUTATIONS)`——detail 1 + 弱化 flock 1 + 守卫 fd 循环体 1
      （循环体一处文本、按表展开 len(...) 趟）；
    - check_check 的调用点（def 之外）恰为 `CONTROL_CHECK_SUBPROCESSES`；
    - ORDER 不得重名（每个检查恰跑一趟是「每项超时只计一次」的前提）。

    计数一律用 ``\\s*`` 拼接的正则而不是整串字面量：本函数自己的匹配文本也是
    源码的一部分，整串字面量会把自己数进去（自指）。同理，注释 / 文档串里写出
    调用形态（函数名紧跟左括号）也会数进来（fail-closed），改注释须同步这里
    的登记。
    """
    source = SELF.read_text(encoding='utf-8')
    if len(re.findall(r"^\s*'lock':\s*check_lock,\s*$", source, re.M)) != 1:
        raise CheckFailure(
            'self_check.py does not register exactly one `lock` entry in CHECKS; '
            'the budget formula counts that registry pass as one lock pass '
            '(update _assert_pass_counting alongside the registry)')
    if ORDER.count('lock') != 1:
        raise CheckFailure(
            'ORDER does not run the lock check exactly once; the budget formula '
            'counts one seven-item pass of check_lock')
    if len(ORDER) != len(set(ORDER)):
        raise CheckFailure(
            'ORDER repeats a check name; the per-check subprocess budget assumes '
            'each check runs exactly once in the seven-item pass')
    lock_calls = len(re.findall(r'(?<!def )\bcheck_lock\s*\(', source))
    expected_lock_calls = LOCK_CHECK_PASSES - len(GUARD_FD_MUTATIONS)
    if lock_calls != expected_lock_calls:
        raise CheckFailure(
            f'self_check.py carries {lock_calls} check_lock call sites but '
            f'LOCK_CHECK_PASSES={LOCK_CHECK_PASSES} with '
            f'{len(GUARD_FD_MUTATIONS)} guard-fd mutations implies '
            f'{expected_lock_calls}; a call site was added or removed without '
            'updating the derived pass count, so the outer-budget inequality '
            'would rot back to always-true (the r9 failure shape)')
    if len(re.findall(r"^\s*'check':\s*check_check,\s*$", source, re.M)) != 1:
        raise CheckFailure(
            'self_check.py does not register exactly one `check` entry in '
            'CHECKS; the budget formula counts that registry pass as one '
            'checker subprocess')
    check_calls = len(re.findall(r'(?<!def )\bcheck_check\s*\(', source))
    if check_calls != CONTROL_CHECK_SUBPROCESSES:
        raise CheckFailure(
            f'self_check.py carries {check_calls} check_check call sites but '
            f'CONTROL_CHECK_SUBPROCESSES={CONTROL_CHECK_SUBPROCESSES}; the '
            'control_check subprocesses must be counted in the internal '
            'worst-case sum or the outer budget silently over-covers them '
            '(the r9 failure shape)')
    if len(GUARD_FD_MUTATIONS) != EXPECTED_GUARD_FD_MUTATIONS:
        raise CheckFailure(
            '#2436 the guard-fd mutation registry carries '
            f'{len(GUARD_FD_MUTATIONS)} samples but {EXPECTED_GUARD_FD_MUTATIONS} '
            'are registered; the lock-pass algebra (LOCK_CHECK_PASSES - len(...)) '
            'still holds when an entry is deleted, so nothing else would go red '
            'here — deleting a guard-fd mutation must go red here, not silently '
            'narrow the lock budget passes')
    return (f'{lock_calls} direct call sites + 1 registry pass + '
            f'{len(GUARD_FD_MUTATIONS)} guard-fd loop passes = '
            f'{LOCK_CHECK_PASSES} lock passes; {check_calls} control_check '
            'subprocesses')


# #2421（评审 r1 同类跟改）：negative_controls 执行器里 CHECK 常量的 checker 子进程
# 超时消费点条数（preflight / 控件循环 / 正控循环各 1）。预算负控行的 limit 就是这个
# 常量——任一处改回字面量 = #2421 形态在执行面复发，当场红。
EXPECTED_CONTROLS_CHECK_TIMEOUT_SITES = 3


def _assert_subprocess_timeout_wiring():
    """#2421：内部超时常量必须被**真实调用点**消费（源码级互证）。

    check_check 的 checker 子进程超时曾写字面量 ``timeout=300``（r9 引入
    ``CHECK_TIMEOUT_SECONDS`` 后的漂移）：抬高常量时预算不等式（内部最坏和）跟着
    变长、README/ci.yml 口径同步，而实际子进程超时不动——预算算的与执行的不再是
    同一个数（同族于 r10 P0 的「预算假 PASS」形态）。这里按源码文本数每个
    ``timeout=<常量>`` 形态的消费点并要求恰为 1：改回字面量（0 个）或新增一个
    未入账的消费点（2+ 个）都当场红，逼着改动者同步预算算式。三个常量
    （EMIT / CHECK / NEGCTL）一起钉：同族正确写法是 check_emit / check_negctl，
    check_check 漂移过一次，其余两个没有理由豁免（同类跟改）。

    计数用**拼接正则**（同 _assert_pass_counting 的自指规避）：本函数源码里不出现
    连续的 ``timeout=<常量名>`` 文本，数不进自己。已知边界（同 _assert_pass_counting
    的文本互证空档，评审 r1 登记跟进）：只认裸标识符形态，括号包裹或经中转变量的
    消费点不进计数；表达式改写（如 ``timeout=<常量> * 2``）仍按 1 个入账。词边界
    （count 正则结尾的 word-char 否定前瞻，评审 r2 boundary#1）：以常量为前缀的
    新标识符（如 ``CHECK_TIMEOUT_SECONDS_V2``）不算登记常量的消费点——否则消费点
    改指新常量后计数仍绿、预算行仍读旧常量入算，「预算算 600 / 执行 900」无人红，
    恰是本守卫要堵的 #2421 形态在改名边界上的复发。

    同类跟改（评审 r1 P2，zai/DS 两家撞上）：预算负控行 ``limit`` 的**执行面**在
    negative_controls 自己的三个 checker 子进程（preflight / 控件 / 正控）——本
    断言原先只扫 SELF，任一处改回字面量时「预算算常量、执行是字面量」的 #2421
    形态在那边复发而这边照绿。CONTROLS 侧同款计数：CHECK 常量的消费点恰为
    ``EXPECTED_CONTROLS_CHECK_TIMEOUT_SITES``（3 处，其余 EMIT/NEGCTL 在那边
    无调用点，不计）。

    别名拒绝（评审 r3，concurrency 与 boundary 两家独立撞上）：词边界堵住「原名
    改指 ``*_V2``」的同时，把「原登记消费点保留 + 旁边新增一个指向别名常量的
    消费点」从旧版前缀入账的计数 4≠3 当场红变成不可见——与本 docstring 上段
    「新增一个未入账的消费点（2+ 个）都当场红」相反。故凡
    ``timeout=<常量><词字符>`` 形态（以登记常量为前缀的标识符消费点）一律拒绝：
    预算按登记常量求和，别名消费点跑的是预算从未入算的数。
    """
    source = SELF.read_text(encoding='utf-8')
    for prefix in ('EMIT', 'CHECK', 'NEGCTL'):
        constant = prefix + '_TIMEOUT_' + 'SECONDS'
        uses = len(re.findall(
            r'timeout\s*=\s*' + re.escape(constant) + r'(?!\w)', source))
        if uses != 1:
            raise CheckFailure(
                '#2421 self_check.py carries '
                f'{uses} `timeout={constant}` call sites but exactly 1 is '
                'registered; the internal worst-case budget counts these '
                'constants (a literal timeout means the budget inequality '
                'covers a number nothing executes, an uncounted extra '
                'consumer means it under-covers what does run)')
        if re.search(
                r'timeout\s*=\s*' + re.escape(constant) + r'\w+', source):
            raise CheckFailure(
                '#2421 self_check.py consumes '
                f'`{constant}` through an alias-named constant (a prefixed '
                'identifier such as one ending in `_V2`); the internal '
                'worst-case budget counts the registered constant itself, so '
                'an aliased consumer runs on a number the budget never sums')
    checker_constant = 'CHECK' + '_TIMEOUT_' + 'SECONDS'
    controls_text = CONTROLS.read_text(encoding='utf-8')
    controls_uses = len(re.findall(
        r'timeout\s*=\s*' + re.escape(checker_constant) + r'(?!\w)',
        controls_text))
    if controls_uses != EXPECTED_CONTROLS_CHECK_TIMEOUT_SITES:
        raise CheckFailure(
            '#2421 negative_controls.py carries '
            f'{controls_uses} `{checker_constant}` call sites but '
            f'{EXPECTED_CONTROLS_CHECK_TIMEOUT_SITES} are registered; the '
            'negctl budget row derives its limit from this constant (a '
            'literal timeout there recreates the #2421 drift on the '
            'execution surface the budget claims to cover)')
    if re.search(r'timeout\s*=\s*' + re.escape(checker_constant) + r'\w+',
                 controls_text):
        raise CheckFailure(
            '#2421 negative_controls.py consumes '
            f'`{checker_constant}` through an alias-named constant (a '
            'prefixed identifier such as one ending in `_V2`); the negctl '
            'budget row derives its limit from the registered constant, so '
            'an aliased consumer runs on a number the budget never sums')
    return ('EMIT / CHECK / NEGCTL subprocess timeouts consume their '
            'constants; the controls executor consumes its checker '
            'constant at every checker call site')


# —— 行定位正则，**只**给 wiring_samples() 构造变异样本用：把真实 ci.yml 的文本切成
# 可改写的锚点。守卫本身（check_wiring）不做任何行式 / 缩进式解析——判断一律走
# PyYAML 解析后的完整标量。 ——
JOB_HEADER = re.compile(r'^  ([\w-]+):\s*$')
STEP_ITEM = re.compile(r'^      - +(\S.*)$')
RETIREMENT_JOB = 'retirement-outcome-parity'
AGGREGATE_JOB = 'check'
REQUIRE_STEP_NAME = 'Require retirement outcome-parity success'
REQUIRE_RUN = 'exit 1'
# 两个 job 的直属键白名单：出现 `continue-on-error`（job 级，4 空格缩进，此前完全
# 扫不到）、`env`、`defaults` 等任何未登记键即红。job 级 `continue-on-error: true`
# 会让六个闸门 step 全红而 job 仍 success。
RETIREMENT_JOB_KEYS = frozenset({'needs', 'if', 'runs-on', 'timeout-minutes', 'steps'})
AGGREGATE_JOB_KEYS = frozenset({'needs', 'if', 'runs-on', 'steps'})
# 根文档直属键白名单（评审 r6 P0）：workflow 级 `defaults.run.shell` 会把**每个** run
# step 的 shell 换掉（如 `/bin/true {0}`——全部闸门无条件 rc=0），根级 `env` 给所有
# job 注入变量；两者都不动任何 job 内的键，只看 `jobs:` 的守卫逐字全等仍假 PASS。
# 与 job 级 `defaults` 的既有判法同一口径：不在白名单即红。
ROOT_KEYS = frozenset({'name', 'on', 'jobs', 'permissions', 'concurrency', 'run-name'})
RETIREMENT_JOB_CONDITION = "${{ needs.scope.outputs.docs_only != 'true' }}"
# 聚合 `check` job 自己是**必需检查**。GitHub 把被跳过的 job 报成 success，所以
# `check` 带上 `if: false` 会让必需检查整体「成功」——这一侧没有人再兜底。
AGGREGATE_JOB_CONDITION = '${{ always() }}'
# 聚合闸门的 Require step 本来就该带 `always()` 条件，所以不能像 job 内 step 那样
# 一律拒 `if:`；要求它**恰好**是这一条（归一化空白后逐字相等）——只核前缀 + 子串
# 挡不住在末尾追加 `&& false`（条件恒假 → step 从不执行，job 仍 success）。
REQUIRE_CONDITION = (
    f"always() && needs.{RETIREMENT_JOB}.result != 'success' && "
    f"!(needs.{RETIREMENT_JOB}.result == 'skipped' && "
    "needs.scope.outputs.docs_only == 'true')")
# 闸门 step 的 `run` 必须是 **literal 块标量（`|`）**，且解析后的**完整标量值**与
# 这里逐字相等（含结尾换行）。只按源码行 / 行首值比对证明不了「执行的就是这段」：
# plain 多行标量把换行折叠成空格（续行拼接）、`>` 折叠块同样改字，而 `echo` 前缀、
# `|| true` 后缀、外层包一层 `if false; then … fi`、多一行 `set +e` 都会改掉标量值。
# 完整标量逐字相等把这些形态一次性挡掉，且不会误拒（不靠猜哪些 token 危险）。
GATE_SCRIPTS = {
    'emit 复算与金样逐字节比对':
        'F=scripts/retirement-outcome-parity/fixtures\n'
        'python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py emit \\\n'
        '  --oracle "$RUNNER_TEMP/ts-oracle.json" \\\n'
        '  --round "$F/round-01" --round "$F/round-02" --round "$F/round-03" \\\n'
        '  --round "$F/round-04" --round "$F/round-05"\n'
        'cmp "$RUNNER_TEMP/ts-oracle.json" '
        'scripts/retirement-outcome-parity/ts-oracle.json\n',
    'check 5 轮':
        'F=scripts/retirement-outcome-parity/fixtures\n'
        'python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py check \\\n'
        '  --oracle scripts/retirement-outcome-parity/ts-oracle.json \\\n'
        '  --round "$F/round-01" --round "$F/round-02" --round "$F/round-03" \\\n'
        '  --round "$F/round-04" --round "$F/round-05"\n',
    '负控 N1–N17 全 KILLED':
        'python3 -I scripts/retirement-outcome-parity/negative_controls.py \\\n'
        '  --output "$RUNNER_TEMP/retirement-outcome-parity-negctl"\n',
    '装置守卫自检（七项）':
        'python3 -I scripts/retirement-outcome-parity/self_check.py\n',
    '接线守卫变异自检':
        'python3 -I scripts/retirement-outcome-parity/self_check.py '
        '--self-check-controls\n',
}
# retirement job 的 step 列表按**顺序**登记：删一个闸门 step、插一个 `run: exit 0`
# 的 step、或把两个 step 调换，都会让「登记的命令」与实际执行的不再对应。
RETIREMENT_STEP_ORDER = (
    ('uses', 'actions/checkout@v4'),
    ('setup', '安装钉死 PyYAML（#2017-A）'),
    ('gate', 'emit 复算与金样逐字节比对'),
    ('gate', 'check 5 轮'),
    ('gate', '负控 N1–N17 全 KILLED'),
    ('gate', '装置守卫自检（七项）'),
    ('gate', '接线守卫变异自检'),
    ('uses', '上传负控证据'),
)
# r11 P0：直跑 gate 没有内部包裹超时，以 step 声明作预算上限。
# check_wiring 钉住声明值；预算遍历读取 YAML 声明，不再手列全 job 的加数。
GATE_STEP_TIMEOUT_MINUTES = {
    'emit 复算与金样逐字节比对': 5,
    'check 5 轮': 5,
    '负控 N1–N17 全 KILLED': NEGCTL_TIMEOUT_SECONDS // 60,
}
# 这三步共享 JOB_OVERHEAD_MARGIN_SECONDS，不另计入 internal。桶也双向核覆盖，
# 新 uses/setup 不能静默当成零秒；新增形态必须明确登记或给出上限。
JOB_OVERHEAD_STEPS = frozenset({
    ('uses', 'actions/checkout@v4'),
    ('setup', '安装钉死 PyYAML（#2017-A）'),
    ('uses', '上传负控证据'),
})
# #2453：直跑负控的 step 声明上限吸收的**整** checker 超时数（cap 1800 s = 3 ×
# limit 600 s）。前 3 次 checker 超时被声明限「吃掉」（该控件判为 KILLED、照常出
# summary）；第 4 次起挂住的行为落进 step 硬杀——summary.json 只在控件循环全部
# 结束后才写，硬杀下没有 summary，哪一控挂了不可见。cap 与 limit 的整除关系被
# 破坏（改声明 30 min / 改负控 checker 上限 600 s）时，「第几次超时开始硬杀」就
# 变了，`_assert_step_budget_coverage` 据此当场红。零余量口径（评审 r1）：机器钉
# 的是声明配置的整除关系；真实硬杀边界受 step 时钟先于 checker 启动、进程启动
# 开销影响，会**略早于**名义上的第 4 次（第 3 次的记录也可能赶不上 summary）。
NEGCTL_STEP_CAP_ABSORBED_CHECKER_TIMEOUTS = 3


def _retirement_run_step_keys(kind, label):
    keys = {'name', 'run', 'shell'}
    if kind == 'gate' and label in GATE_STEP_TIMEOUT_MINUTES:
        keys.add('timeout-minutes')
    return keys


# 非闸门 step 必须是 action：action step 不执行 shell，吞不掉闸门结果。
# `with` 只允许出现在登记过的 uses step 上，且内容逐键钉死（USE_STEP_WITH_KEYS）：
# 给 `actions/checkout@v4` 加 `with: {ref: …}` 会让闸门执行**另一棵树**的脚本，
# 而登记的 `run` 标量逐字不变（评审 r7 P1）。
USE_STEP_KEYS = frozenset({'name', 'uses', 'if', 'with'})
# uses step 的 `with` 白名单（键集合必须**恰为**登记值）：retirement job 只有两个
# action step——checkout 不带任何 `with`，upload-artifact 只带 name / path /
# if-no-files-found。
USE_STEP_WITH_KEYS = {
    'actions/checkout@v4': frozenset(),
    'actions/upload-artifact@v4': frozenset({'name', 'path', 'if-no-files-found'}),
}
# 全 workflow（ci.yml + pr-gate.yml）每个 uses step 的 `with` 键集合按 action 钉死
# （评审 r8 P1）：r7 只钉了 retirement job 的两个 uses step，同一攻击形态（checkout
# 加 `with: {ref: …}`，全部登记 `run` 标量逐字不变、job 级键逐字不变）在
# metadata-observer / negative-controls / gateway-schema-baseline 三个 job 上无人拦截。
# checkout 有两种合法形态（带 `fetch-depth: 0` 的深克隆与不带 `with` 的默认检出），
# 登记为「允许的键集合族」；出现族外键（如 `ref` / `repository` / `path`）即红。
WORKFLOW_USE_WITH_KEYS = {
    'actions/checkout@v4': {frozenset(), frozenset({'fetch-depth'})},
    'actions/setup-node@v4': {frozenset({'node-version'})},
    'actions/cache@v4': {frozenset({'path', 'key'})},
    'Swatinem/rust-cache@v2.9.2': {frozenset({'workspaces', 'cache-bin'})},
    'actions/upload-artifact@v4': {frozenset({'name', 'path', 'if-no-files-found'})},
}
# pr-gate.yml 的 job 直属键白名单（评审 r8 P1-2）：`pr-gate` job 现只有 if / runs-on /
# steps；job 级 `continue-on-error` 会让 check-pr-gate 步全红而 job 仍 success。
PR_GATE_JOB_KEYS = frozenset({'if', 'runs-on', 'steps'})
# —— r9 P0（boundary#1 = sibling#1 = concurrency#1，三形态全绿）——
# r8 的 pr-gate 巡走只钉外壳（根键 / job 键 / shell / with），不钉 job `if` 值、
# step 直属键集合与 `run` 标量：step 加 `continue-on-error: true`、job `if: false`
# （被跳过的必需检查按 README invariant 报 success）、`run: exit 0` 三个静默废闸门
# 的形态都全绿。对照 ci.yml 闸门 step 同口径（GATE_SCRIPTS 逐字 + step 键白名单）：
PR_GATE_JOB_CONDITION = 'github.event.pull_request.draft == false'
# step 列表按**顺序**登记（对照 RETIREMENT_STEP_ORDER）：删一个 step、插一个
# `run: exit 0` 的 step、或调换两步，都会让合并闸门跑的不是登记的样子。
PR_GATE_STEP_ORDER = (
    ('uses', 'actions/checkout@v4'),
    ('run', '显式取 base 分支与 PR head（不依赖 checkout 的默认 refspec）'),
    ('run', '角色审计结果块与 issue 关联'),
)
# 两个 run step 的直属键白名单：fetch 基线步带 `continue-on-error`（登记值恰为
# `true`——取不到 base 只影响「沿用」，不挡 head 一致的 PR）与 env；check-pr-gate 步
# 只有 name / env / shell / run——给它加 `continue-on-error: true` 即步全红而 job 仍
# success（P0 形态①）。键集**恰等**（不是子集）：多一键少一键都是漂移。
PR_GATE_STEP_KEYS = {
    '显式取 base 分支与 PR head（不依赖 checkout 的默认 refspec）':
        frozenset({'name', 'continue-on-error', 'env', 'shell', 'run'}),
    '角色审计结果块与 issue 关联': frozenset({'name', 'env', 'shell', 'run'}),
}
# env 键集合同样逐字登记：PR_HEAD / PR_BASE 换名或被删，check-pr-gate.py 拿到的
# 就是另一个世界（静默错判），而 run 标量与 job 键逐字不变。
PR_GATE_STEP_ENV_KEYS = {
    '显式取 base 分支与 PR head（不依赖 checkout 的默认 refspec）':
        frozenset({'BASE_REF', 'PR_HEAD'}),
    '角色审计结果块与 issue 关联': frozenset({'PR_BODY', 'PR_HEAD', 'PR_BASE'}),
}
# 两个 run step 的脚本逐字钉死（GATE_SCRIPTS 同口径，按解析后的完整标量比对）：
# fetch 步是 literal 块；check 步是单行 plain 标量（解析值就是源码行）——`exit 0`
# 换壳（P0 形态③）、`|| true` 后缀、折叠块改字一律逐字不等即红。
PR_GATE_SCRIPTS = {
    '显式取 base 分支与 PR head（不依赖 checkout 的默认 refspec）':
        'git fetch --no-tags origin '
        '"+refs/heads/${BASE_REF}:refs/remotes/origin/${BASE_REF}" "${PR_HEAD}"\n'
        'git rev-parse --verify "origin/${BASE_REF}^{commit}"\n'
        'git rev-parse --verify "${PR_HEAD}^{commit}"\n',
    '角色审计结果块与 issue 关联': 'python3 -I scripts/check-pr-gate.py',
}
# checkout 步的 `with` 键集合恰为 {fetch-depth}（补丁指纹要算 git diff base...head）。
PR_GATE_CHECKOUT_WITH_KEYS = frozenset({'fetch-depth'})
# 每个 run step 都必须显式 `shell: bash`（评审 r7 P0）：step 级 `shell:` 优先于
# workflow / job 级 `defaults.run.shell`（GitHub workflow syntax："You can override
# the default shell settings … using the `shell` keyword"），因此这一行是
# `defaults.run.shell: /bin/true {0}` 假绿攻击在**执行层**的解——根键白名单挡
# 「defaults 出现」，这一条挡「就算 defaults 已经在（或守卫被绕过），检测步照样跑」。
PINNED_STEP_SHELL = 'bash'
# PyYAML 版本钉死（#2017-A，评审 r6 P1）：接线守卫按「PyYAML 解析后的完整标量」判，
# 不同版本可能解析出不同形状（假 PASS / 假 FAIL）。CI 里由「安装钉死 PyYAML（#2017-A）」
# 步骤装进 venv 并前置到 PATH（`python3 -I` 不读 user site，但 venv 的 site-packages 属
# 解释器自身配置、`-I` 下仍可见——本机实测）；版本同步登记在装置 README。
PYYAML_PINNED_VERSION = '6.0.3'
# 装置自身的准备 step（#2017-A）：与闸门同一口径——`name` + 一个 literal 块标量
# `run`，脚本逐字登记。准备步被改（版本号被换 / venv 被指向别处）同样改变判定环境。
SETUP_SCRIPTS = {
    '安装钉死 PyYAML（#2017-A）':
        'python3 -m venv "$RUNNER_TEMP/pyyaml-pinned"\n'
        '"$RUNNER_TEMP/pyyaml-pinned/bin/pip" install --disable-pip-version-check '
        f'pyyaml=={PYYAML_PINNED_VERSION}\n'
        'echo "$RUNNER_TEMP/pyyaml-pinned/bin" >> "$GITHUB_PATH"\n',
}


def _indent_of(line):
    return len(line) - len(line.lstrip(' '))


def _normalised_expression(condition):
    return ' '.join(condition.removeprefix('${{').removesuffix('}}').split())


def _yaml_module():
    """PyYAML 必须可用且版本钉死：否则接线守卫 fail-closed 判红，绝不降级回行式解析。"""
    try:
        import yaml
    except ImportError as error:
        raise CheckFailure(
            f'PyYAML is not importable ({error}); the wiring guard fails closed '
            'instead of falling back to line-based parsing') from error
    if yaml.__version__ != PYYAML_PINNED_VERSION:
        raise CheckFailure(
            f'PyYAML version mismatch: installed {yaml.__version__!r} but '
            f'{PYYAML_PINNED_VERSION!r} is pinned (CI installs it into a venv; the '
            'device README registers the pin) — a different YAML build may parse the '
            'workflow differently, so the guard fails closed')
    return yaml


_STYLE_NAMES = {
    None: 'plain (continuation-folding) scalar',
    '|': 'literal block',
    '>': 'folded block',
    "'": 'single-quoted scalar',
    '"': 'double-quoted scalar',
}


def _style_name(style):
    return _STYLE_NAMES.get(style, f'`{style}` block scalar')


def _workflow_root(path):
    """唯一文档的根节点；多文档 / 解析失败 / PyYAML 缺失一律红。"""
    yaml = _yaml_module()
    try:
        text = path.read_text(encoding='utf-8')
    except OSError as error:
        raise CheckFailure(f'cannot read {path}: {error}') from error
    try:
        documents = list(yaml.compose_all(text))
    except yaml.YAMLError as error:
        raise CheckFailure(f'{path}: not parsable as YAML: {error}') from error
    if len(documents) != 1:
        raise CheckFailure(
            f'{path}: expected exactly one YAML document, found {len(documents)}; '
            'the runner rejects a multi-document workflow, and any single-document '
            'reader would silently ignore the rest')
    return yaml, documents[0]


def _pairs(yaml, path, node, what):
    """YAML 映射节点 → [(键文本, 值节点)]；非映射 / 非标量键 / 重复键即红。"""
    if not isinstance(node, yaml.MappingNode):
        raise CheckFailure(f'{path}: {what} must be a YAML mapping')
    pairs = []
    seen = set()
    for key, value in node.value:
        if not isinstance(key, yaml.ScalarNode):
            raise CheckFailure(f'{path}: {what} has a non-scalar key {key!r}')
        if key.value in seen:
            raise CheckFailure(
                f'{path}: {what} repeats the key `{key.value}`; with duplicate keys '
                'the runner and the guard can disagree about which one is live')
        seen.add(key.value)
        pairs.append((key.value, value))
    return pairs


def _scalar_text(yaml, path, node, what):
    if node is None:
        raise CheckFailure(f'{path}: {what} is missing')
    if not isinstance(node, yaml.ScalarNode):
        raise CheckFailure(
            f'{path}: {what} must be a scalar, found a {type(node).__name__}; an '
            'alias or nested structure is not the value the runner would use')
    return node.value


def _gate_run_text(yaml, path, node, label):
    """闸门 step 的 `run`：必须是 literal 块标量（`|`），返回其完整标量值。

    非 literal 形态**显式判红并写明形态**：plain 多行标量是续行拼接产物（换行折叠成
    空格），`>` 折叠块同样改字，引号标量是另一套转义——三者执行到的文本都不是源码行
    的逐字内容。
    """
    if not isinstance(node, yaml.ScalarNode) or node.style != '|':
        found = ('a non-scalar node'
                 if not isinstance(node, yaml.ScalarNode)
                 else f'a {_style_name(node.style)}')
        raise CheckFailure(
            f'{path}: gate step `{label}` `run` is {found}, not a literal block '
            'scalar (`|`); plain and quoted scalars fold continuation lines into '
            'spaces, `>` folds every newline — the executed text is not the source '
            'lines verbatim')
    return node.value


def _first_line_diff(got, want):
    got_lines, want_lines = got.split('\n'), want.split('\n')
    for number, (line_got, line_want) in enumerate(zip(got_lines, want_lines), 1):
        if line_got != line_want:
            return f'line {number}: {line_got!r} != {line_want!r}'
    if len(got_lines) != len(want_lines):
        return f'{len(got_lines)} lines vs {len(want_lines)} lines'
    return 'identical'


def _needs_names(yaml, path, node, job):
    if isinstance(node, yaml.SequenceNode):
        return [_scalar_text(yaml, path, item, f'`{job}` needs entry')
                for item in node.value]
    if isinstance(node, yaml.ScalarNode):
        return [node.value] if node.value else []
    raise CheckFailure(f'{path}: `{job}` needs must be a list or a job name')


def _job_map(yaml, path, jobs, name, allowed):
    """只在 `jobs:` 映射里找 job，并核 job 直属键白名单。"""
    if name not in jobs:
        raise CheckFailure(
            f'{path}: no `{name}` job under `jobs:`; a block by that name anywhere '
            'else (e.g. under `on:` as an unknown event name) is silently ignored '
            'by the runner')
    pairs = _pairs(yaml, path, jobs[name], f'`{name}` job')
    for key, _ in pairs:
        if key not in allowed:
            raise CheckFailure(
                f'{path}: `{name}` job carries the unregistered key `{key}`; a '
                'job-level `continue-on-error` / `env` / `defaults` can change or '
                'swallow the gate without touching any step')
    return dict(pairs)


def _step_list(yaml, path, node, job):
    if not isinstance(node, yaml.SequenceNode):
        raise CheckFailure(f'{path}: `{job}` has no step list')
    return [dict(_pairs(yaml, path, item, f'`{job}` step'))
            for item in node.value]


@functools.lru_cache(maxsize=1)
def _load_negative_controls():
    """预算求值专用的 negative_controls 模块——进程级单例（#2452）。

    r11 起预算行从 negative_controls 同源读参考轮集合、控件登记与 checker 超时；
    此前每次 `retirement_budget_rows` 都在进程内**重新 exec** 模块体（连带经其
    `load_checker` 再 exec 一遍 checker）：`--self-check-controls` 一轮约 45 次
    预算调用 × 2 个模块体 ≈ 90 个常驻空临时目录，且每次都把进程级
    `sys.pycache_prefix` 复写成刚建的目录。选型：**进程级单例缓存**
    （`functools.lru_cache`）而不是 ast/正则从源码读登记值——预算的语义是
    「同一份源码的同源推导」（r11 评审批掉的就是手抄第二份计数/秒数），ast/正则
    会成为登记值的第二个解析器（锚点一漂就读错对象）；单例缓存保住同源语义，同时
    把 exec 从每次调用降到每进程一次。加载期间新建的 pycache 前缀由各模块自己的
    atexit 清理；这里保存/还原进程级前缀，宿主的前缀不被顶掉。
    """
    previous = sys.pycache_prefix
    try:
        spec = importlib.util.spec_from_file_location('negative_controls', CONTROLS)
        module = importlib.util.module_from_spec(spec)
        try:
            spec.loader.exec_module(module)
        except SystemExit as error:
            # 负控模块体把入口失败收口成 SystemExit(2)（`-I` 断言 / checker 加载
            # 失败）；进程内加载时它不是退出码，是裸异常穿透预算求值——包成守卫
            # 判词（CheckFailure），不被下游读成装置崩溃。
            raise CheckFailure(
                f'#2452 cannot load {CONTROLS} for the budget rows: '
                f'negative_controls exited during import with rc={error.code!r} '
                '(the module wraps its own entry-level failures in SystemExit(2))'
            ) from error
        return module
    finally:
        sys.pycache_prefix = previous


def retirement_budget_rows(workflow=None):
    """逐个遍历真实 job steps，返回实际入算记录（再由守卫独立核覆盖）。

    七项/变异自检沿用 r10 内部公式及 _assert_pass_counting 互证；直跑负控从
    自己的模块读参考轮与控件登记、子进程超时，不抄第二份计数或秒数。

    #2452 P3（双重解析，**保留并写明理由**）：本函数与 check_wiring 各自
    `_workflow_root` 解析同一份 ci.yml——不合并。`retirement_budget_rows` 是独立
    导出面（模块级 `INTERNAL_WORST_CASE_SECONDS` 求值与兄弟测试的 subprocess 都
    **不带解析上下文**单独调它），把解析结果穿进调用面要改两层签名，收益只是省
    一次毫秒级 parse；两个解析入口共用同一 fail-closed 判词（多文档 / 解析失败
    即红），语义不受影响。
    """
    path = Path(workflow or WORKFLOW)
    yaml, root = _workflow_root(path)
    jobs = dict(_pairs(yaml, path, dict(_pairs(
        yaml, path, root, 'the workflow')).get('jobs'), 'jobs'))
    job = _job_map(yaml, path, jobs, RETIREMENT_JOB, RETIREMENT_JOB_KEYS)
    steps = _step_list(yaml, path, job.get('steps'), RETIREMENT_JOB)
    kinds = {label: kind for kind, label in RETIREMENT_STEP_ORDER}
    rows = []
    for step in steps:
        label = _scalar_text(yaml, path, step.get('name', step.get('uses')),
                             'a retirement budget step identity')
        kind = kinds.get(label)
        identity = (kind, label)
        cap_seconds = None
        if kind == 'gate' and label in GATE_STEP_TIMEOUT_MINUTES:
            timeout = _scalar_text(yaml, path, step.get('timeout-minutes'),
                                   f'`{label}` step `timeout-minutes`')
            expected = GATE_STEP_TIMEOUT_MINUTES[label]
            if timeout != str(expected):
                raise CheckFailure(
                    f'#1809 r11 step timeout: `{label}` expected `{expected}`, '
                    f'got `{timeout}`; the declared cap must stay pinned')
            cap_seconds = int(timeout) * 60
        if identity in JOB_OVERHEAD_STEPS:
            seconds, passes, limit = 0, 1, JOB_OVERHEAD_MARGIN_SECONDS
            source = 'shared JOB_OVERHEAD_MARGIN_SECONDS'
        elif kind == 'gate' and label in ('emit 复算与金样逐字节比对', 'check 5 轮'):
            if cap_seconds is None:
                raise CheckFailure(f'#1809 r11 budget rule missing: `{label}` step cap')
            seconds, passes, limit = cap_seconds, 1, cap_seconds
            source = 'declared step timeout-minutes'
        elif kind == 'gate' and label == '负控 N1–N17 全 KILLED':
            module = _load_negative_controls()
            try:
                names = module.reference_rounds(FIXTURES)
            except module.CHECK.InputError as error:
                # #2452：reference_rounds 抛的是负控模块**自己那份** checker 的
                # InputError（每次 exec 产生新的类对象，self_check 的 CHECK.InputError
                # 接不住）——裸异常穿透预算求值会被读成装置崩溃，包成守卫判词。
                raise CheckFailure(
                    f'#2452 the negative-controls budget row cannot derive the '
                    f'reference-round set from {FIXTURES}: {error}') from error
            passes = (len(names) + 1 + module.EXPECTED_CONTROLS
                      + module.EXPECTED_POSITIVES)
            limit = module.CHECK_TIMEOUT_SECONDS
            seconds = passes * limit
            source = (f'negative_controls: preflight ({len(names)}+1) + '
                      f'controls {module.EXPECTED_CONTROLS} + '
                      f'positives {module.EXPECTED_POSITIVES}')
        elif kind == 'gate' and label == '装置守卫自检（七项）':
            seconds = (EMIT_TIMEOUT_SECONDS + CHECK_TIMEOUT_SECONDS
                       + NEGCTL_TIMEOUT_SECONDS + LOCK_WORST_CASE_SECONDS
                       + WRITES_WORST_CASE_SECONDS)
            passes, limit = 1, seconds
            source = 'EMIT + CHECK + NEGCTL + one LOCK + WRITES'
        elif kind == 'gate' and label == '接线守卫变异自检':
            seconds = ((LOCK_CHECK_PASSES - 1) * LOCK_WORST_CASE_SECONDS
                       + CONTROL_CHECK_SUBPROCESSES * CHECK_TIMEOUT_SECONDS)
            passes, limit = 1, seconds
            source = ('(LOCK_CHECK_PASSES - 1) * LOCK + '
                      'CONTROL_CHECK_SUBPROCESSES * CHECK')
        else:
            raise CheckFailure(
                f'#1809 r11 budget rule missing: {identity!r}; every declared '
                'step needs a derived contribution or a registered overhead bucket')
        # 保留底层原始推导供复算，再应用声明的硬上限。直跑负控的原始贡献
        # （passes × CHECK_TIMEOUT_SECONDS，量级 ≈ 3.8 h）不能只靠外层兜底：
        # ubuntu-latest 的 job 平台上限是 6 小时，须由 step 声明限压低。
        raw_seconds = seconds
        if cap_seconds is not None:
            seconds = min(seconds, cap_seconds)
        rows.append(dict(identity=identity, seconds=seconds, raw_seconds=raw_seconds,
                         passes=passes, limit=limit, cap_seconds=cap_seconds,
                         source=source))
    return rows


def _assert_step_budget_coverage(rows):
    """实际贡献记录与步骤/桶登记双向互证，漏算与幽灵加数都当场红。"""
    counted = tuple(row['identity'] for row in rows)
    if counted != RETIREMENT_STEP_ORDER:
        raise CheckFailure(
            f'#1809 r11 budget coverage: expected {RETIREMENT_STEP_ORDER!r}, '
            f'got {counted!r}; declared and counted steps must match both ways')
    overhead = frozenset(row['identity'] for row in rows
                         if row['source'] == 'shared JOB_OVERHEAD_MARGIN_SECONDS')
    if overhead != JOB_OVERHEAD_STEPS:
        raise CheckFailure(
            f'#1809 r11 overhead coverage: expected {sorted(JOB_OVERHEAD_STEPS)!r}, '
            f'got {sorted(overhead)!r}')
    capped = {row['identity'][1] for row in rows
              if row['cap_seconds'] is not None}
    if capped != set(GATE_STEP_TIMEOUT_MINUTES):
        raise CheckFailure(
            f'#1809 r11 step cap coverage: expected '
            f'{sorted(GATE_STEP_TIMEOUT_MINUTES)!r}, got {sorted(capped)!r}')
    # #2453：消费 raw_seconds / passes / limit 三个此前只写不读的推导字段，把
    # 「raw > cap」的降级语义机器化——被声明限压到 cap 的必须恰是直跑负控这一行
    # （emit/check 的声明值与入算值相等，raw 不超 cap），且 cap 吸收的整 checker
    # 超时数是登记值：raw = passes × limit（23 次 600 s），cap = 3 × limit，
    # 名义上第 4 次 checker 超时落进 step 硬杀（summary 只在控件循环后写，硬杀
    # 无迹；真实边界受启动开销影响略早，见 NEGCTL_STEP_CAP_ABSORBED_CHECKER_TIMEOUTS
    # 的注释）。
    negctl_label = '负控 N1–N17 全 KILLED'
    over_cap = [row for row in rows if row['cap_seconds'] is not None
                and row['raw_seconds'] > row['cap_seconds']]
    if [row['identity'][1] for row in over_cap] != [negctl_label]:
        raise CheckFailure(
            f'#2453 cap absorption: expected exactly the `{negctl_label}` row to '
            f'contribute a raw value above its declared cap, got '
            f'{[row["identity"] for row in over_cap]!r}; the raw-above-cap '
            'degradation is what keeps the 4th-checker-timeout step hard-kill '
            'semantics machine-checked')
    negctl_row = over_cap[0]
    cap, limit = negctl_row['cap_seconds'], negctl_row['limit']
    if negctl_row['passes'] * limit != negctl_row['raw_seconds']:
        raise CheckFailure(
            f'#2453 cap absorption: the `{negctl_label}` raw contribution must '
            f'stay passes ({negctl_row["passes"]}) × limit ({limit}s) = '
            f'{negctl_row["passes"] * limit:.0f}s, got '
            f'{negctl_row["raw_seconds"]:.0f}s')
    if cap % limit or cap // limit != NEGCTL_STEP_CAP_ABSORBED_CHECKER_TIMEOUTS:
        raise CheckFailure(
            f'#2453 cap absorption: the `{negctl_label}` cap ({cap}s) must absorb '
            f'exactly {NEGCTL_STEP_CAP_ABSORBED_CHECKER_TIMEOUTS} whole checker '
            f'timeouts (limit {limit}s; got cap//limit = {cap // limit} with '
            f'remainder {cap % limit}s); at the registered value it is nominally '
            f'the {NEGCTL_STEP_CAP_ABSORBED_CHECKER_TIMEOUTS + 1}th checker '
            'timeout that degrades into the step hard-kill (cancelled, no '
            'summary of which control hung; the real boundary lands slightly '
            'earlier — the step clock starts before the first checker and '
            'startup overheads eat into the margin)')


def _step_with_keys(yaml, path, step, what):
    """step 的 `with` 键集合（缺 `with` 为空集）；非映射 `with` 即红。"""
    with_node = step.get('with')
    if with_node is None:
        return frozenset()
    if not isinstance(with_node, yaml.MappingNode):
        raise CheckFailure(f'{path}: {what} has a non-mapping `with`')
    return frozenset(key for key, _ in _pairs(yaml, path, with_node, f'{what} `with`'))


def _sweep_workflow_steps(yaml, path, jobs):
    """全 workflow 每个 job 巡走：run step 必须显式 `shell: bash`（r7 P0），uses
    step 的 `with` 键集合必须落在该 action 的登记族内（r8 P1）。

    ci.yml 与 pr-gate.yml 共用。根键 / job 键白名单挡 `defaults` 的**出现**，这里挡
    的是某个 run step **裸奔**（裸奔步才会继承 defaults）；`with` 越族挡的是「checkout
    加 `ref:` 跑另一棵树」——登记的 `run` 标量与 job 级键都逐字不变。
    """
    for job_name, job_node in jobs.items():
        job_map = dict(_pairs(yaml, path, job_node, f'`{job_name}`'))
        steps_node = job_map.get('steps')
        if steps_node is None:
            raise CheckFailure(f'{path}: job `{job_name}` has no step list')
        for step in _step_list(yaml, path, steps_node, job_name):
            if 'run' in step:
                shell = step.get('shell')
                if shell is None or _scalar_text(
                        yaml, path, shell, f'`{job_name}` step `shell`') != PINNED_STEP_SHELL:
                    raise CheckFailure(
                        f'{path}: job `{job_name}` has a run step without the pinned '
                        f'`shell: {PINNED_STEP_SHELL}`; a bare run step inherits any '
                        'workflow/job-level `defaults.run.shell` (e.g. `/bin/true {0}`), '
                        'which would silently disable that detection point')
            elif 'uses' in step:
                uses = _scalar_text(yaml, path, step['uses'],
                                    f'`{job_name}` step `uses`')
                allowed = WORKFLOW_USE_WITH_KEYS.get(uses)
                if allowed is None:
                    raise CheckFailure(
                        f'{path}: job `{job_name}` runs the unregistered action '
                        f'`{uses}`; `with` content is only pinned for registered actions')
                with_keys = _step_with_keys(yaml, path, step,
                                           f'job `{job_name}` uses step')
                if with_keys not in allowed:
                    raise CheckFailure(
                        f'{path}: job `{job_name}` uses step `{uses}` carries the '
                        f'unregistered `with` keys {sorted(with_keys)} (registered '
                        f'families: {sorted(sorted(family) for family in allowed)}); '
                        'an added `with: ref/repository` on a checkout makes the job '
                        'run scripts from a different tree while every pinned run '
                        'scalar and job-level key stays verbatim-identical')


def _check_pr_gate_wiring(path):
    """pr-gate.yml 同一口径巡走（评审 r8 P1-2 + r9 P0 step 级钉死）。

    合并闸门第 2、3 步（git fetch 基线步、check-pr-gate.py 步）住在这份文件里，而
    此前 self_check 与兄弟守卫都只读 ci.yml：对它加根级 `defaults.run.shell:
    /bin/true {0}` 即让 check-pr-gate 步无条件 rc=0，**合并闸门假 PASS** 且无守卫
    变红。口径与 ci.yml 同一巡走：唯一文档、根键白名单、job 直属键白名单、每个
    run step 显式 `shell: bash`、每个 uses step 的 `with` 键集按 action 钉死。
    r9 P0 起（对照 retirement job 的 job `if` 逐字 + 闸门 step 键白名单 + run 标量
    逐字）再钉四层：job `if` 逐字、step 顺序与条数、每个 run step 的直属键集合
    （含 `continue-on-error` 登记值）与 env 键集合、每个 run step 的 `run` 标量
    逐字——`run: exit 0` 换壳 / step `continue-on-error: true` / job `if: false`
    三个静默废闸门的形态必须全红。
    """
    yaml, root = _workflow_root(path)
    gate_pairs = _pairs(yaml, path, root, 'the pr-gate workflow')
    for key, _ in gate_pairs:
        if key not in ROOT_KEYS:
            raise CheckFailure(
                f'{path}: the pr-gate workflow carries the unregistered root key '
                f'`{key}`; a workflow-level `defaults.run.shell` replaces the shell of '
                'every run step (the check-pr-gate step unconditionally rc=0) and '
                'neither the ci.yml guards nor a job-level scan would see it')
    jobs_node = dict(gate_pairs).get('jobs')
    if jobs_node is None:
        raise CheckFailure(f'{path}: the pr-gate workflow has no `jobs:` mapping')
    jobs = dict(_pairs(yaml, path, jobs_node, 'jobs'))
    if set(jobs) != {'pr-gate'}:
        raise CheckFailure(f'{path}: the pr-gate workflow must carry exactly the '
                           f'`pr-gate` job, found {sorted(jobs)}')
    gate_job = _job_map(yaml, path, jobs, 'pr-gate', PR_GATE_JOB_KEYS)

    # job `if` 逐字（r9 P0 形态②）：被跳过的必需检查被 GitHub 报成 success，
    # `if: false` 之外任何漂移（如 draft 反转）都让合并闸门换了一个世界。
    gate_condition = _normalised_expression(_scalar_text(
        yaml, path, gate_job.get('if'), 'the `pr-gate` job `if`'))
    expected_gate_condition = _normalised_expression(PR_GATE_JOB_CONDITION)
    if gate_condition != expected_gate_condition:
        raise CheckFailure(
            f'{path}: the `pr-gate` job condition is `{gate_condition}` but must be '
            f'exactly `{expected_gate_condition}`; a skipped required job is '
            'reported as success by GitHub, so nothing else would block the merge '
            'on draft PRs / reopened PRs the gate must cover')

    steps = _step_list(yaml, path, gate_job.get('steps'), 'pr-gate')
    if len(steps) != len(PR_GATE_STEP_ORDER):
        raise CheckFailure(
            f'{path}: the `pr-gate` job must keep exactly {len(PR_GATE_STEP_ORDER)} '
            f'steps (found {len(steps)}); adding, dropping or reordering steps '
            'changes which script actually runs')
    for step, (kind, wanted) in zip(steps, PR_GATE_STEP_ORDER):
        if kind == 'uses':
            unknown = set(step) - {'name', 'uses', 'with'}
            if unknown or 'uses' not in step:
                raise CheckFailure(
                    f'{path}: the pr-gate checkout step must stay an action '
                    f'(`uses:`), found {sorted(step)}; an inserted `run:` step is '
                    'not the registered fetch')
            uses = _scalar_text(yaml, path, step['uses'], 'the pr-gate checkout `uses`')
            if uses != wanted:
                raise CheckFailure(
                    f'{path}: the pr-gate checkout step uses `{uses}` but must use '
                    f'exactly `{wanted}`; the patch fingerprint needs this action')
            with_keys = _step_with_keys(yaml, path, step, 'the pr-gate checkout step')
            if with_keys != PR_GATE_CHECKOUT_WITH_KEYS:
                raise CheckFailure(
                    f'{path}: the pr-gate checkout step must carry exactly the '
                    f'registered `with` keys {sorted(PR_GATE_CHECKOUT_WITH_KEYS)}, '
                    f'found {sorted(with_keys)}; the merge fingerprint needs a full '
                    'clone (fetch-depth: 0), and any other key changes which tree '
                    'the gate reads')
            continue
        label = _scalar_text(yaml, path, step['name'], f'`{wanted}` `name`')
        if label != wanted:
            raise CheckFailure(
                f'{path}: expected the `{wanted}` step at this position, '
                f'found `{label}`')
        allowed = PR_GATE_STEP_KEYS[wanted]
        if set(step) != allowed:
            raise CheckFailure(
                f'{path}: pr-gate run step `{wanted}` may only carry '
                f'{sorted(allowed)}, found {sorted(step)}; a step-level '
                '`continue-on-error` swallows a red check-pr-gate while the job '
                'stays success, and any stray key can replace what runs')
        if 'continue-on-error' in step and _scalar_text(
                yaml, path, step['continue-on-error'],
                f'`{wanted}` `continue-on-error`') != 'true':
            raise CheckFailure(
                f'{path}: pr-gate run step `{wanted}` `continue-on-error` must '
                'stay exactly `true`; it is a registered design decision (a missing '
                'base only downgrades fingerprinting, it must not gate the job)')
        env_keys = frozenset(
            key for key, _ in _pairs(yaml, path, step['env'], f'`{wanted}` `env`'))
        if env_keys != PR_GATE_STEP_ENV_KEYS[wanted]:
            raise CheckFailure(
                f'{path}: pr-gate run step `{wanted}` env must carry exactly the '
                f'registered keys {sorted(PR_GATE_STEP_ENV_KEYS[wanted])}, found '
                f'{sorted(env_keys)}; a dropped `PR_HEAD`/`PR_BASE`/`BASE_REF` '
                'silently hands check-pr-gate.py a different world while every '
                'pinned `run` scalar stays verbatim-identical')
        if _scalar_text(yaml, path, step['shell'],
                        f'`{wanted}` `shell`') != PINNED_STEP_SHELL:
            raise CheckFailure(
                f'{path}: pr-gate run step `{wanted}` must pin `shell: '
                f'{PINNED_STEP_SHELL}`; any other value (e.g. `/bin/true {{0}}`) '
                'replaces what actually executes')
        script_node = step['run']
        if '\n' in PR_GATE_SCRIPTS[wanted]:
            # 多行脚本必须是 literal 块（`|`）：plain 续行折叠 / `>` 折叠都会改字。
            script = _gate_run_text(yaml, path, script_node, wanted)
        else:
            script = _scalar_text(yaml, path, script_node, f'`{wanted}` `run`')
        if script != PR_GATE_SCRIPTS[wanted]:
            raise CheckFailure(
                f'{path}: pr-gate run step `{wanted}` does not run the registered '
                f'script ({_first_line_diff(script, PR_GATE_SCRIPTS[wanted])}); '
                'the parsed scalar is pinned verbatim, so an `exit 0` swap, an '
                '`|| true` suffix or a folded block all change the text')
    _sweep_workflow_steps(yaml, path, jobs)


def check_wiring(workflow=None, pr_gate=None):
    """CI 里的闸门必须**按登记的样子**接着（PyYAML 完整标量逐字比对）。

    用 YAML 解析器取**解析后的完整标量值**比对，不做任何行式 / 缩进式文本切块：

    - 根文档直属键必须在白名单内（workflow 级 `defaults.run.shell` 会替换每个 run
      step 的 shell——全部闸门无条件 rc=0；根级 `env` 给所有 job 注入变量；评审 r6
      P0）；重复键即红——两个解析器可以对「哪个键活着」各执一词；
    - 只在 `jobs:` 映射里找两个 job（挂在 `on:` 等别处的同名块不是 job）；
    - 两个 job 的直属键必须在白名单内（job 级 `continue-on-error` 会让全红的 job 仍
      success）；重复键即红——两个解析器可以对「哪个键活着」各执一词；
    - retirement job 的 step 顺序、闸门 step 的名字、直属键集合（只有 `name` + `run` +
      `shell`）必须与登记一致——`env: {run: …}` / `env.UNUSED: |` / `if:` /
      `continue-on-error` 一律判红；
    - 每个闸门 step 的 `run` 必须是 literal 块标量（`|`）且**完整标量值**与
      `GATE_SCRIPTS` 逐字相等——续行拼接（plain 多行标量）、`>` 折叠块、引号标量
      **显式判红并写明形态**；`echo` 前缀、`|| true` 后缀、外层 `if false; then … fi`、
      多一行 `set +e` 全部逐字不等即红；
    - 每个 run step（闸门 / Require / **全 workflow 任何 job**）都必须显式
      `shell: bash`——step 级 shell 优先于任何 `defaults.run.shell`，这一行是
      r7 P0 的执行层解；uses step 的 `with` 内容按 action 逐键钉死（r8 P1：
      **全 workflow** 巡走——同一手法换一个 job 的 checkout 照样让闸门跑
      另一棵树，而登记的 `run` 标量逐字不变）；
    - pr-gate.yml（合并闸门第 2、3 步的宿主，r8 P1）同一巡走：根键白名单 +
      每个 run step 钉 `shell: bash` + 每个 uses step 的 `with` 逐键钉死——
      check-pr-gate step 被 workflow 级 `defaults.run.shell` 静默 rc=0 时
      **没有任何**守卫会看见（此前两个守卫都只读 ci.yml）；r9 P0 起再钉
      job `if` 逐字、step 顺序 / 直属键集（含 `continue-on-error` 登记值）/
      env 键集 / `run` 标量逐字（`exit 0` 换壳、step `continue-on-error`、
      job `if: false` 三形态必须全红）；
    - retirement job 的外层 `timeout-minutes` 值钉死且必须大于装置内部超时的
      最坏和 + 开销余量（r9 P2 concurrency#2，派生算式
      INTERNAL_WORST_CASE_SECONDS——外层不够时挂住的检查退化为平台硬杀）；
    - 聚合 `check` job 依赖 retirement job，`if` 归一化后恰为 `always()`（GitHub 把
      被跳过的 job 报成 success，而 `check` 自己是必需检查）；Require step 只有
      `name` / `if` / `run` / `shell` 四个键，`run` 恰为 `exit 1`，`if` 归一化后逐字
      等于登记的恒执行条件。

    任一条不满足，闸门要么不执行、要么失败了不报，job 结果仍是 success，聚合闸门就放
    行了。PyYAML 不可用时 fail-closed 判红，不降级回行式解析。
    """
    path = Path(workflow or WORKFLOW)
    yaml, root = _workflow_root(path)
    root_pairs = _pairs(yaml, path, root, 'the workflow')
    for key, _ in root_pairs:
        if key not in ROOT_KEYS:
            raise CheckFailure(
                f'{path}: the workflow carries the unregistered root key `{key}`; a '
                'workflow-level `defaults.run.shell` replaces the shell of every `run` '
                'step (all gates unconditionally rc=0) and `env` shadows variables for '
                'every job — neither touches any job-level key the rest of this guard '
                'checks (same reasoning as the job-level `defaults` ban)')
    jobs_node = dict(root_pairs).get('jobs')
    if jobs_node is None:
        raise CheckFailure(f'{path}: the workflow has no `jobs:` mapping')
    jobs = dict(_pairs(yaml, path, jobs_node, 'jobs'))

    retirement = _job_map(yaml, path, jobs, RETIREMENT_JOB, RETIREMENT_JOB_KEYS)
    if 'scope' not in _needs_names(yaml, path, retirement.get('needs'),
                                    RETIREMENT_JOB):
        raise CheckFailure(
            f'{path}: `{RETIREMENT_JOB}` must still need `scope` (docs-only classifier)')
    job_condition = _normalised_expression(_scalar_text(
        yaml, path, retirement.get('if'), f'`{RETIREMENT_JOB}` job `if`'))
    expected_job_condition = _normalised_expression(RETIREMENT_JOB_CONDITION)
    if job_condition != expected_job_condition:
        raise CheckFailure(
            f'{path}: `{RETIREMENT_JOB}` job condition is `{job_condition}` but must be '
            f'exactly `{expected_job_condition}`; any other condition can skip the job '
            'on PRs the classifier does not mark docs-only')

    # 外层预算（评审 r9 P2 concurrency#2，r10 P0 复算）：值钉死 + 派生不等式。
    # 判据是 r8 注释自设的「外层 > 内部最坏和」——内部超时打不满时失败由装置
    # 自己收口成红，外层不够则失败退化为平台硬杀（job 记 cancelled，哪个检查
    # 红了都看不见）。算式输入（锁趟数 / control_check 子进程数）由
    # _assert_pass_counting 与本文件源码的调用点同源互证，乘数漂移当场红。
    _assert_pass_counting()
    _assert_subprocess_timeout_wiring()
    timeout_text = _scalar_text(
        yaml, path, retirement.get('timeout-minutes'),
        f'`{RETIREMENT_JOB}` job `timeout-minutes`')
    if timeout_text != str(RETIREMENT_JOB_TIMEOUT_MINUTES):
        raise CheckFailure(
            f'{path}: `{RETIREMENT_JOB}` job timeout-minutes is `{timeout_text}` '
            f'but must stay exactly `{RETIREMENT_JOB_TIMEOUT_MINUTES}`; the outer '
            f'budget must exceed the internal worst-case sum '
            f'{INTERNAL_WORST_CASE_SECONDS:.0f}s (see INTERNAL_WORST_CASE_SECONDS), '
            'or a hanging check degrades into a runner hard-kill (cancelled, no '
            'summary of which check failed)')
    if (RETIREMENT_JOB_TIMEOUT_MINUTES * 60
            <= INTERNAL_WORST_CASE_SECONDS + JOB_OVERHEAD_MARGIN_SECONDS):
        raise CheckFailure(
            f'the registered outer budget {RETIREMENT_JOB_TIMEOUT_MINUTES} min '
            f'(= {RETIREMENT_JOB_TIMEOUT_MINUTES * 60}s) no longer exceeds the '
            f'internal worst-case sum {INTERNAL_WORST_CASE_SECONDS:.0f}s + '
            f'{JOB_OVERHEAD_MARGIN_SECONDS}s overhead; raise the outer budget or '
            'tighten the internal wait caps (this inequality is the r8 comment\'s '
            'self-imposed criterion, now machine-checked)')

    steps = _step_list(yaml, path, retirement.get('steps'), RETIREMENT_JOB)
    if len(steps) != len(RETIREMENT_STEP_ORDER):
        raise CheckFailure(
            f'{path}: `{RETIREMENT_JOB}` must keep exactly {len(RETIREMENT_STEP_ORDER)} '
            f'steps (found {len(steps)}); adding, dropping or reordering steps changes '
            'which script actually runs')
    for step, (kind, wanted) in zip(steps, RETIREMENT_STEP_ORDER):
        label = (_scalar_text(yaml, path, step['name'], f'`{wanted}` `name`')
                 if 'name' in step else
                 _scalar_text(yaml, path, step.get('uses'), f'`{wanted}` `uses`')
                 if 'uses' in step else None)
        if label != wanted:
            raise CheckFailure(
                f'{path}: expected the `{wanted}` step at this position, '
                f'found `{label}`')
        if kind in ('gate', 'setup'):
            registry = GATE_SCRIPTS if kind == 'gate' else SETUP_SCRIPTS
            allowed = _retirement_run_step_keys(kind, label)
            if set(step) != allowed:
                raise CheckFailure(
                    f'{path}: {kind} step `{label}` may only carry `name`, `run` and '
                    f'`shell` (plus its registered timeout-minutes), found '
                    f'{sorted(step)}; `env` / `with` / `if` / '
                    '`continue-on-error` can disable the step or replace what it executes')
            if _scalar_text(yaml, path, step['shell'],
                            f'`{label}` `shell`') != PINNED_STEP_SHELL:
                raise CheckFailure(
                    f'{path}: {kind} step `{label}` must pin `shell: '
                    f'{PINNED_STEP_SHELL}`; any other value (e.g. `/bin/true {{0}}`) '
                    'replaces what actually executes')
            script = _gate_run_text(yaml, path, step['run'], label)
            if script != registry[label]:
                raise CheckFailure(
                    f'{path}: {kind} step `{label}` does not run the registered script '
                    f'({_first_line_diff(script, registry[label])}); the parsed '
                    'scalar is pinned verbatim, so an `echo` prefix, a `|| true` / '
                    '`! ` suffix, an `if false; then … fi` wrapper or an extra line '
                    'all change the text')
        else:
            unknown = set(step) - USE_STEP_KEYS
            if unknown or 'uses' not in step:
                raise CheckFailure(
                    f'{path}: step `{label}` must stay an action (`uses:`), '
                    f'found {sorted(step)}')
            uses = _scalar_text(yaml, path, step['uses'], f'`{label}` `uses`')
            expected_with = USE_STEP_WITH_KEYS.get(uses)
            if expected_with is None:
                raise CheckFailure(
                    f'{path}: action step `{label}` uses unregistered action '
                    f'`{uses}`; `with` content is only pinned for registered actions')
            with_keys = _step_with_keys(
                yaml, path, step, f'action step `{label}`')
            if with_keys != expected_with:
                raise CheckFailure(
                    f'{path}: action step `{label}` (`{uses}`) must carry exactly the '
                    f'registered `with` keys {sorted(expected_with)}, found '
                    f'{sorted(with_keys)}; an added `with: ref/repository` on the '
                    'checkout makes the gates run scripts from a different tree while '
                    'every pinned `run` scalar stays verbatim-identical')

    rows = retirement_budget_rows(path)
    _assert_step_budget_coverage(rows)
    internal = sum(row['seconds'] for row in rows)
    if internal != INTERNAL_WORST_CASE_SECONDS:
        raise CheckFailure(
            f'#1809 r11 budget total: expected {INTERNAL_WORST_CASE_SECONDS}s, '
            f'got {internal}s from actual job steps')

    # 聚合闸门那一侧同样要守：Require step 被 `if: false` 挡掉、条件被改弱、或
    # `run: exit 0` 时，retirement job 照跑照过，job 结果仍是 success。
    aggregate = _job_map(yaml, path, jobs, AGGREGATE_JOB, AGGREGATE_JOB_KEYS)
    if RETIREMENT_JOB not in _needs_names(yaml, path, aggregate.get('needs'),
                                          AGGREGATE_JOB):
        raise CheckFailure(
            f'{path}: `{AGGREGATE_JOB}` no longer needs `{RETIREMENT_JOB}`; without the '
            'dependency the Require step cannot see the gate result')
    aggregate_condition = _normalised_expression(_scalar_text(
        yaml, path, aggregate.get('if'), f'`{AGGREGATE_JOB}` job `if`'))
    expected_aggregate_condition = _normalised_expression(AGGREGATE_JOB_CONDITION)
    if aggregate_condition != expected_aggregate_condition:
        raise CheckFailure(
            f'{path}: `{AGGREGATE_JOB}` job condition is `{aggregate_condition}` but '
            f'must be exactly `{expected_aggregate_condition}`; a skipped required job '
            'is reported as success by GitHub, so nothing else would block the merge')
    require = None
    for step in _step_list(yaml, path, aggregate.get('steps'), AGGREGATE_JOB):
        name_node = step.get('name')
        if name_node is not None and _scalar_text(
                yaml, path, name_node, 'an aggregate step `name`') == REQUIRE_STEP_NAME:
            require = step
            break
    if require is None:
        raise CheckFailure(f'{path}: `{AGGREGATE_JOB}` has no retirement Require step')
    if set(require) != {'name', 'if', 'run', 'shell'}:
        raise CheckFailure(
            f'{path}: the aggregate Require step may only carry `name`, `if`, `run` '
            f'and `shell`, found {sorted(require)}; an extra key can swallow the failure')
    if _scalar_text(yaml, path, require['shell'],
                    'the aggregate Require step `shell`') != PINNED_STEP_SHELL:
        raise CheckFailure(
            f'{path}: the aggregate Require step must pin `shell: {PINNED_STEP_SHELL}`; '
            'any other value (e.g. `/bin/true {0}`) would make the required check '
            '`check` falsely green')
    if _scalar_text(yaml, path, require['run'],
                    'the aggregate Require step `run`') != REQUIRE_RUN:
        raise CheckFailure(
            f'{path}: the aggregate Require step must run exactly `{REQUIRE_RUN}`; '
            'anything else lets a failed retirement job pass')
    expression = _normalised_expression(_scalar_text(
        yaml, path, require['if'], 'the aggregate Require step `if`'))
    if expression != REQUIRE_CONDITION:
        raise CheckFailure(
            f'{path}: the aggregate Require step condition is `{expression}` but must '
            f'be exactly `{REQUIRE_CONDITION}`; a weakened condition (e.g. a trailing '
            '`&& false`) silently disables the gate')

    # r7 P0（评审 MM boundary#1）：全 workflow **每个** run step 都必须显式
    # `shell: bash`。step 级 `shell:` 优先于任何层级的 `defaults.run.shell`（GitHub
    # workflow syntax 明文），因此这一行保证「就算 defaults 已经出现在任何层级（或
    # 根键白名单被绕过），检测步照样执行」。根键 / job 键白名单挡 `defaults` 的
    # **出现**，这里挡的是某个 run step **裸奔**（裸奔步才会继承 defaults）。
    # r8 P1 起 uses step 的 `with` 逐键钉死与 run step 的 shell 巡走共用
    # `_sweep_workflow_steps`；pr-gate.yml 同一巡走（check-pr-gate step 不再是
    # 「没有任何守卫读这个文件」的盲区）。
    _sweep_workflow_steps(yaml, path, jobs)
    gate_path = Path(pr_gate or PR_GATE)
    _check_pr_gate_wiring(gate_path)
    return (f'{len(RETIREMENT_STEP_ORDER)} registered steps + aggregate Require step '
            f'live in {path.name} + {gate_path.name} swept '
            '(PyYAML scalar-exact)')


CHECKS = {
    'budget': check_budget,
    'emit': check_emit,
    'check': check_check,
    'negctl': check_negctl,
    'lock': check_lock,
    'writes': check_writes,
    'wiring': check_wiring,
}
ORDER = ('budget', 'emit', 'check', 'negctl', 'lock', 'writes', 'wiring')


def run_checks(names):
    failures = []
    for name in names:
        try:
            detail = CHECKS[name]()
        except CheckFailure as error:
            failures.append(name)
            print(f'{name}: FAILED — {error}', file=sys.stderr)
        except Exception as error:  # noqa: BLE001 - 装置自身异常同样是红，不是跳过
            failures.append(name)
            print(f'{name}: FAILED — unexpected {error!r}', file=sys.stderr)
        else:
            print(f'{name}: PASS — {detail}')
    return 1 if failures else 0


# —— 变异自检：证明守卫被拆掉时检查会红 ——

def control_budget():
    """体积守卫的两个方向都要能红：单文件超限、总量超限（评审 r7 P2）。

    这里用**字面**上限（1 MiB / 5 MiB，来自任务书与铁律 8），不读
    `FIXTURE_*_LIMIT_BYTES`——否则把常量调大时控件会跟着变宽，守卫被悄悄放开
    也测不出来（变异的正是那个常量）。
    """
    if FIXTURE_FILE_LIMIT_BYTES > (1 << 20) or FIXTURE_TOTAL_LIMIT_BYTES > (5 << 20):
        raise CheckFailure(
            'configured fixture budget exceeds the registered 1 MiB/file, '
            f'5 MiB/total (got {FIXTURE_FILE_LIMIT_BYTES} / '
            f'{FIXTURE_TOTAL_LIMIT_BYTES})')
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-budget-') as tmp:
        root = Path(tmp) / 'fixtures'
        shutil.copytree(FIXTURES, root)
        oversized = root / 'round-01' / 'driver' / 'oversized.bin'
        with open(oversized, 'wb') as handle:
            handle.seek(1 << 20)  # 稀疏文件：长度 1 MiB + 1 B
            handle.write(b'\0')
        try:
            check_budget(root)
        except CheckFailure as error:
            if 'single fixture file over' not in str(error):
                raise CheckFailure(
                    f'oversized fixture rejected by the wrong judgment: {error}') from error
        else:
            raise CheckFailure('budget guard did not reject an oversized fixture '
                               '(the size limit was removed)')
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-budget-') as tmp:
        # 总量方向：每个文件都**不超**单文件上限，合起来超总量上限——只判单文件的
        # 守卫对这批文件全放行（评审 r7 P2：此前总量判定没有任何违反样本）。
        root = Path(tmp) / 'fixtures'
        shutil.copytree(FIXTURES, root)
        padding = 1 << 20
        for index in range(4):
            filler = root / f'round-0{index + 1}' / 'driver' / f'filler-{index}.bin'
            with open(filler, 'wb') as handle:
                handle.truncate(padding)  # 每个恰 1 MiB，不触单文件线
        try:
            check_budget(root)
        except CheckFailure as error:
            if 'fixtures total' not in str(error):
                raise CheckFailure(
                    f'total-limit violation rejected by the wrong judgment: {error}') from error
        else:
            raise CheckFailure('budget guard did not reject a total-size violation '
                               '(the total limit was removed)')
    return 'oversized and over-total fixtures are both rejected'


def control_lock():
    """锁守卫的四个方向都要能红：并发不串行、敌意锁文件被接受、时戳锚抓错行、
    负控守卫 fd 退回旧口径。

    * 串行性（变异：把 `output_lock` 里等锁的**阻塞 flock 换成 pass**——吞掉
      `BlockingIOError` 继续跑的完整弱化形态）与敌意锁文件由 `check_lock`
      重跑兜着。
    * r8 P0 的时戳锚控件（两层）：
      ① 锚点定向——同一条流里同时含 `ACQUIRED` 与 `RELEASED` 时，RELEASED 锚
      必须取到 RELEASED、ACQUIRED 锚必须取到 ACQUIRED；旧实现的合并正则取
      **首个**匹配，在这条流上确定性地锚到 ACQUIRED（`RELEASED 锚取成取锁
      时刻`），控件顺带复现这一错取；
      ② 弱化 flock 变异——临时 checker 副本里把阻塞 flock 换成 `pass`，第二个
      持锁子的 ACQUIRED 提前到第一个的 RELEASED 之前，时戳序断言必须红（吞掉
      `BlockingIOError` 的弱化照样打出「等锁」字符串，只有时戳序拦得住）。
      锚串只认时戳序分支的专属文案（r9 P2 concurrency#3：两个分支曾共享
      'not actually mutually exclusive' 子串，前者触发时控件被算 KILLED 而时戳序
      从未验证——等待分支文案已改写，锚不再共享）。
    * r9 P2（sibling#5 = boundary#5）守卫 fd 控件：临时 negative_controls 副本
      逐个退回 r8 之前的口径（去 `O_NONBLOCK` / 去 `S_ISREG` / 去 `O_NOFOLLOW`），
      check_lock 的守卫 fd 探针必须红在各自的专属判定上。
    """
    detail = check_lock()
    interleaved = b'LOCKED ACQUIRED 1.500000000\nRELEASED 2.500000000\n'
    if _monotonic_stamp(interleaved, RELEASED_STAMP,
                        'control RELEASED anchor') != 2.5:
        raise CheckFailure('the RELEASED anchor picked the wrong stamp while the '
                           'stream carries both ACQUIRED and RELEASED (r8 P0 '
                           'regression: the anchor is ambiguous again)')
    if _monotonic_stamp(interleaved, ACQUIRED_STAMP,
                        'control ACQUIRED anchor') != 1.5:
        raise CheckFailure('the ACQUIRED anchor picked the wrong stamp while the '
                           'stream carries both ACQUIRED and RELEASED (r8 P0 '
                           'regression: the anchor is ambiguous again)')
    legacy_first_match = re.compile(rb'(?:ACQUIRED|RELEASED) (\d+\.\d+)')
    legacy = legacy_first_match.search(interleaved)
    if legacy is None or float(legacy.group(1)) != 1.5:
        # 旧实现（合并正则 + 首个匹配）在这条流上必错：确定性复现，不靠概率。
        raise CheckFailure('the legacy first-match regex no longer mis-anchors on '
                           'an interleaved stream; this control has rotted and '
                           'must be updated alongside the anchors')
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-lock-') as tmp:
        source = CHECKER.read_text(encoding='utf-8')
        mutation = '                fcntl.flock(fd, fcntl.LOCK_EX)\n'
        if source.count(mutation) != 1:
            raise CheckFailure('the blocking-flock line is not uniquely present in '
                               f'{CHECKER.name} to mutate; update this control '
                               'with the current anchor')
        weakened = source.replace(mutation, '                pass\n')
        mutated = Path(tmp) / 'check_outcome_parity.py'
        mutated.write_text(weakened, encoding='utf-8')
        try:
            check_lock(mutated)
        except CheckFailure as error:
            if 'not actually mutually exclusive' not in str(error) \
                    or 'weakened flock' not in str(error):
                raise CheckFailure(
                    f'weakened flock rejected by the wrong judgment: {error}') from error
        else:
            raise CheckFailure(
                'the timestamp-ordering guard did not reject a weakened flock '
                '(blocking flock replaced by pass — the second holder proceeds '
                'while the first still holds the lock); the r8 P0 fix has rotted')

        # 守卫 fd 的三种退化（r9 P2，登记簿 GUARD_FD_MUTATIONS——外层预算的锁
        # 趟数从它推导，r10 P0）。每种都必须红在**点名的判定**上：锚串取自
        # `_assert_guard_fd_fail_closed` 的两条专属文案，别的判定（真锁敌意探针）
        # 不含这些串。
        guard_source = CONTROLS.read_text(encoding='utf-8')
        for guard_label, old, new, anchor in GUARD_FD_MUTATIONS:
            if guard_source.count(old) != 1:
                raise CheckFailure(f'the guard-fd anchor for `{guard_label}` is '
                                   'not uniquely present in '
                                   f'{CONTROLS.name} to mutate; update this '
                                   'control with the current anchor')
            mutated_controls = Path(tmp) / 'negative_controls.py'
            mutated_controls.write_text(guard_source.replace(old, new, 1),
                                       encoding='utf-8')
            try:
                check_lock(controls=mutated_controls)
            except CheckFailure as error:
                if anchor not in str(error):
                    raise CheckFailure(
                        f'{guard_label} rejected by the wrong judgment: '
                        f'{error}') from error
            else:
                raise CheckFailure(
                    f'the summary guard fd check did not reject `{guard_label}` '
                    '(the guard regressed to the pre-r8 open discipline and '
                    'nothing turned red); the r9 fix has rotted')
    return detail


def _step_range(lines, name):
    """workflow 文本里某个 `- name: <name>` step 的 [start, end) 行号。"""
    start = next((index for index, line in enumerate(lines)
                  if line.rstrip('\n') == f'      - name: {name}'), None)
    if start is None:
        raise CheckFailure(f'CI workflow has no `{name}` step to mutate')
    end = next((index for index in range(start + 1, len(lines))
                if STEP_ITEM.match(lines[index])
                or JOB_HEADER.match(lines[index].rstrip('\n'))), len(lines))
    return start, end


def _run_range(lines, start, end, label):
    """step 里 `        run: |` 与其脚本体的 [run_index, body_end)。"""
    run_index = next((index for index in range(start, end)
                      if lines[index].rstrip('\n') == '        run: |'), None)
    if run_index is None:
        raise CheckFailure(f'step `{label}` has no `run: |` block to mutate')
    body_end = next((index for index in range(run_index + 1, end)
                     if lines[index].strip() and _indent_of(lines[index]) <= 8), end)
    if body_end == run_index + 1:
        raise CheckFailure(f'step `{label}` has an empty `run: |` block to mutate')
    return run_index, body_end


def _job_block(lines, job):
    """workflow 文本里 `  <job>:` 头（含）到下一个 job 头（不含）的 [start, end)。"""
    start = next((index for index, line in enumerate(lines)
                  if line.rstrip('\n') == f'  {job}:'), None)
    if start is None:
        raise CheckFailure(f'CI workflow has no `{job}` job block to mutate')
    end = next((index for index in range(start + 1, len(lines))
               if JOB_HEADER.match(lines[index].rstrip('\n'))), len(lines))
    return start, end


def wiring_samples():
    """所有接线变异样本 `[(名字, 变异后的 workflow 文本)]`。

    覆盖：删整个 job / 整段注释 / step 级三种挡法（`if: false`、引号键 `"if"`
    `continue-on-error`）/ **job 级** `continue-on-error`（4 空格缩进）/ 命令整行挪进
    `env.UNUSED: |` / 嵌套 `env: {run: …}` 冒充 Require 的执行体 / 条件块 `if false; then`
    包住整条命令 / 三种 shell 级抑制（`|| true`、`echo` 前缀、`! ` 取反退出码）/
    聚合 Require 的五种改法（禁用、改弱、忽略失败、追加 `&& false`、`exit 0`）/
    删掉一个闸门 step / r5 标量形态四种（run 行内续行、聚合 job `if` 续行、Require
    `if` 续行、`|`→`>` 折叠块）加 job 挪出 `jobs:`（行首正则会认 `on:` 下的假块）与
    **根级** `defaults.run.shell`（workflow 级 shell 模板让全部闸门无条件 rc=0，评审
    r6 P0）/ r7 执行层五条（闸门 step 删 `shell` 行、闸门 step 换 shell 值、Require
    step 换 shell 值、**非闸门 job**（rust fmt）删 `shell` 行——专测全 workflow 巡走、
    checkout 加 `with: {ref: …}` 跑另一棵树——DS P1）/ r8 P1-1 三条（**另外三个
    job** 的 checkout 各加 `with: {ref: …}`——uses 逐键钉死挪进全 workflow 巡走后，
    换 job 的同一手法必须红）。

    每条样本都**真的能造成假绿**，不是语法坏掉的变异体：`bash -e` 下要么命令没执行、
    要么失败被吞、要么 step / job 被跳过，而 job 结果仍是 success。
    """
    text = WORKFLOW.read_text(encoding='utf-8')
    lines = text.splitlines(keepends=True)
    samples = []

    stripped = [line for line in lines if 'outcome-parity' not in line.lower()]
    if len(stripped) == len(lines):
        raise CheckFailure('CI workflow has no retirement anchor to remove')
    samples.append(('removed', ''.join(stripped)))

    # 整段注释：job 块连同聚合 `needs` 条目与 Require 步骤一起注掉，命令文本仍躺在
    # 文件里——只有「注释不算活配置」才能抓到。
    job_start = next((index for index, line in enumerate(lines)
                      if line.rstrip('\n') == f'  {RETIREMENT_JOB}:'), None)
    if job_start is None:
        raise CheckFailure('CI workflow has no retirement job to comment out')
    job_end = next((index for index in range(job_start + 1, len(lines))
                    if JOB_HEADER.match(lines[index].rstrip('\n'))), len(lines))
    require_start, require_end = _step_range(lines, REQUIRE_STEP_NAME)
    dead = set(range(job_start, job_end)) | set(range(require_start, require_end))
    dead |= {index for index, line in enumerate(lines)
             if f'{RETIREMENT_JOB}]' in line}
    samples.append(('commented-out',
                    ''.join('# ' + line if index in dead else line
                            for index, line in enumerate(lines))))

    # step 级挡法：加一个 `if:` / 引号键 `"if"` / `continue-on-error` 键，闸门 step
    # 就不执行或失败被忽略，而 job 结果仍是 success。
    check_start, check_end = _step_range(lines, 'check 5 轮')
    for label, injected in (
            ('disabled-step', '        if: false\n'),
            ('quoted-if-step', '        "if": false\n'),
            ('continue-on-error-step', '        continue-on-error: true\n')):
        samples.append((label, ''.join(
            lines[:check_start + 1] + [injected] + lines[check_start + 1:])))

    # job 级 `continue-on-error`（4 空格缩进，此前完全扫不到）：六个闸门 step 全红，
    # job 仍 success，聚合 Require 看到的是 success。
    samples.append(('job-level-continue-on-error', ''.join(
        lines[:job_start + 1] + ['    continue-on-error: true\n']
        + lines[job_start + 1:])))

    # 命令文本整行挪进同一个 step 的 `env.UNUSED: |`：YAML 合法，run 里只剩
    # `echo disabled`——按「文件里有没有这一行」判的守卫会假 PASS。重建的 step
    # 保留 `shell: bash`，让失败落在 env 这个键上而不是「缺 shell」。
    controls_start, controls_end = _step_range(lines, '接线守卫变异自检')
    if lines[controls_start + 1].rstrip('\n') != '        shell: bash' \
            or lines[controls_start + 2].rstrip('\n') != '        run: |':
        raise CheckFailure('the 接线守卫变异自检 step is not in the registered '
                           'shape (name / shell / run) to move into env')
    samples.append(('commands-moved-to-env', ''.join(
        lines[:controls_start]
        + ['      - name: 接线守卫变异自检\n',
           '        shell: bash\n',
           '        run: |\n',
           '          echo disabled\n',
           '        env:\n',
           '          UNUSED: |\n']
        + ['    ' + line for line in lines[controls_start + 3:controls_end]]
        + lines[controls_end:])))

    # 聚合 Require 的 `run: exit 1` 被嵌套键冒充：`env:` 下的 `run: exit 1` 只是环境
    # 变量，真正执行的是 `run: echo success`——按「有没有一行 strip 后等于 run: exit 1」
    # 判的守卫会假 PASS。
    require_run = next((index for index in range(require_start, require_end)
                        if lines[index].rstrip('\n') == '        run: exit 1'), None)
    if require_run is None:
        raise CheckFailure('the aggregate Require step has no `run: exit 1` line')
    nested = list(lines)
    nested[require_run] = ('        env:\n'
                           '          run: exit 1\n'
                           '        run: echo success\n')
    samples.append(('nested-env-run', ''.join(nested)))

    # 条件块：命令仍整行独占，但被 `if false; then … fi` 包住，`bash -e` 返回 0。
    check_run, check_body_end = _run_range(lines, check_start, check_end, 'check 5 轮')
    samples.append(('false-if-block', ''.join(
        lines[:check_run + 1]
        + ['          if false; then\n']
        + lines[check_run + 1:check_body_end]
        + ['          fi\n']
        + lines[check_body_end:])))

    # `! cmd`：取反退出码，闸门失败时这一步反而 rc=0（`bash -e` 也不会中止）。
    negated = list(lines)
    negated[check_run + 2] = '          ! ' + lines[check_run + 2].lstrip()
    samples.append(('negated-command', ''.join(negated)))

    # `|| true` 必须加在一行**不以续行反斜杠结尾**的完整命令末尾，否则反斜杠会把
    # 空格转义、下一行不再是参数（变异体 rc=127，根本没造出假绿）。check 块的末行
    # `--round "$F/round-05"` 正好是多行命令的收尾。
    last = lines[check_body_end - 1].rstrip('\n')
    if last.endswith('\\'):
        raise CheckFailure('the check step body ends in a continuation; '
                           '`|| true` would not suppress the real command')
    suppressed = list(lines)
    suppressed[check_body_end - 1] = last + ' || true\n'
    samples.append(('suppressed-command', ''.join(suppressed)))

    # `echo` 前缀：命令只打印不执行。加在 python3 那一行（块的第 2 行）。
    echoed = list(lines)
    echoed[check_run + 2] = '          echo ' + lines[check_run + 2].lstrip()
    samples.append(('echoed-command', ''.join(echoed)))

    # 删掉一个闸门 step：其余命令照跑，少一道闸门而 job 仍 success。
    samples.append(('removed-gate-step',
                    ''.join(lines[:controls_start] + lines[controls_end:])))

    # 聚合闸门那一侧的同型拆法：Require step 被禁用 / 条件被改弱 / 失败被忽略 /
    # `exit 0`，retirement job 照跑照过，只扫本 job step 的守卫会假 PASS。
    if_index = next((index for index in range(require_start, require_end)
                     if lines[index].lstrip().startswith('if:')), None)
    if if_index is None:
        raise CheckFailure('the aggregate Require step has no `if:` line to weaken')
    for label, replacement in (
            ('disabled-require-step', '        if: false\n'),
            ('weakened-require-step',
             "        if: ${{ needs.retirement-outcome-parity.result == 'failure' }}\n")):
        mutated = list(lines)
        mutated[if_index] = replacement
        samples.append((label, ''.join(mutated)))
    samples.append(('continue-on-error-require-step', ''.join(
        lines[:require_start + 1] + ['        continue-on-error: true\n']
        + lines[require_start + 1:])))
    weakened_if = lines[if_index].rstrip('\n')
    if not weakened_if.endswith(' }}'):
        raise CheckFailure(f'the Require condition line does not end in ` }}}}`: '
                           f'{weakened_if!r}')
    samples.append(('and-false-require-step', ''.join(
        lines[:if_index] + [weakened_if[:-3] + ' && false }}\n']
        + lines[if_index + 1:])))
    samples.append(('exit-zero-require-step', ''.join(
        lines[:require_run] + ['        run: exit 0\n'] + lines[require_run + 1:])))

    # 聚合 `check` job 自己是必需检查：GitHub 把被跳过的 job 报成 success，所以给它
    # 加 `if: false`（或任何别的条件）之后没有任何东西会挡合并。
    check_job_start = next(index for index, line in enumerate(lines)
                           if line.rstrip('\n') == f'  {AGGREGATE_JOB}:')
    check_job_end = next((index for index in range(check_job_start + 1, len(lines))
                          if JOB_HEADER.match(lines[index].rstrip('\n'))), len(lines))
    check_job_if = next((index for index in range(check_job_start, check_job_end)
                         if lines[index].startswith('    if:')), None)
    if check_job_if is None:
        raise CheckFailure(f'the `{AGGREGATE_JOB}` job has no `if:` line to weaken')
    samples.append(('aggregate-job-if-false', ''.join(
        lines[:check_job_if] + ['    if: false\n'] + lines[check_job_if + 1:])))

    # 多文档：守卫只读第一份，`---` 之后的文档被静默忽略。
    samples.append(('multi-document', text + '---\n  ignored: job\n'))

    # —— r6（评审 P0）：根级 `defaults.run.shell`。GitHub 支持 workflow 级
    # `defaults.run`；自定义 shell 模板里 `{0}` 是脚本路径占位，`/bin/true {0}` 让
    # **每个** run step 无条件 rc=0——全部闸门假绿，而两个 job 的每个被比对标量仍
    # 逐字等于登记值；只取 `jobs:` 的守卫完全看不见这个键。——
    if text.count('\njobs:\n') != 1:
        raise CheckFailure('the workflow must carry exactly one `jobs:` line to anchor '
                           'the root-level defaults injection')
    samples.append(('root-defaults-shell', text.replace(
        '\njobs:\n',
        '\ndefaults:\n  run:\n    shell: /bin/true {0}\njobs:\n', 1)))

    # —— r7（评审 P0 的执行层守卫）：ci.yml 每个 run step 都显式 `shell: bash`，
    # 删掉 / 换掉这一行都必须红。gate / Require step 各有「键集 + 值」两条判定；
    # 非闸门 job 的 run step（rust job 的 fmt 是必需检查的一环）只有 r7 新增的
    # 全 workflow 巡走兜着——`rust-step-shell-removed` 专测那个新判定。——
    if lines[check_start + 1].rstrip('\n') != '        shell: bash':
        raise CheckFailure('the check step does not carry `shell: bash` at the '
                           'registered position to mutate')
    samples.append(('gate-step-shell-removed', ''.join(
        lines[:check_start + 1] + lines[check_start + 2:])))
    gate_shell_swapped = list(lines)
    gate_shell_swapped[check_start + 1] = '        shell: /bin/true {0}\n'
    samples.append(('gate-step-shell-swapped', ''.join(gate_shell_swapped)))
    if lines[require_start + 2].rstrip('\n') != '        shell: bash':
        raise CheckFailure('the aggregate Require step does not carry `shell: bash` '
                           'at the registered position to mutate')
    require_shell_swapped = list(lines)
    require_shell_swapped[require_start + 2] = '        shell: /bin/true {0}\n'
    samples.append(('require-step-shell-swapped', ''.join(require_shell_swapped)))
    fmt_run = next((index for index, line in enumerate(lines)
                    if line.rstrip('\n') == '        run: cargo fmt --all --check'), None)
    if fmt_run is None or lines[fmt_run - 1].rstrip('\n') != '        shell: bash':
        raise CheckFailure('the rust job fmt step is not in the registered shape '
                           'to drop its shell line')
    samples.append(('rust-step-shell-removed', ''.join(
        lines[:fmt_run - 1] + lines[fmt_run:])))

    # —— r7（DS P1）：uses step 的 `with` 逐键钉死的违反样本——给 retirement job 的
    # checkout 加 `with: {ref: …}`，登记的 `run` 标量逐字不变，闸门却跑的是
    # 另一棵树的脚本。——
    checkout_line = next((index for index in range(job_start, job_end)
                          if lines[index].rstrip('\n')
                          == '      - uses: actions/checkout@v4'), None)
    if checkout_line is None:
        raise CheckFailure('the retirement job has no checkout step to mutate')
    samples.append(('checkout-with-ref', ''.join(
        lines[:checkout_line + 1]
        + ['        with:\n', '          ref: attacker-branch\n']
        + lines[checkout_line + 1:])))

    # —— r8（评审 P1-1）：`with` 逐键钉死挪进全 workflow 巡走后的违反样本——同一
    # 手法换一个 job 的 checkout 也必须红。r7 的 `checkout-with-ref` 只测了
    # retirement job 自己的两个 uses step；metadata-observer / negative-controls /
    # gateway-schema-baseline 三个 job 的 checkout 此前无人拦截（negative-controls
    # 的 checkout 本就带 `fetch-depth: 0`，往那个 `with` 块里加 `ref`）——三个
    # job 的**每个被比对标量**都逐字不变，闸门却各自跑另一棵树的脚本。——
    for job in ('metadata-observer', 'negative-controls', 'gateway-schema-baseline'):
        job_start, job_end = _job_block(lines, job)
        sibling_checkout = next(
            (index for index in range(job_start, job_end)
             if lines[index].rstrip('\n') == '      - uses: actions/checkout@v4'),
            None)
        if sibling_checkout is None:
            raise CheckFailure(f'job `{job}` has no checkout step to mutate')
        if lines[sibling_checkout + 1].rstrip('\n') == '        with:':
            mutated = (lines[:sibling_checkout + 2]
                       + ['          ref: attacker-branch\n']
                       + lines[sibling_checkout + 2:])
        else:
            mutated = (lines[:sibling_checkout + 1]
                       + ['        with:\n', '          ref: attacker-branch\n']
                       + lines[sibling_checkout + 1:])
        samples.append((f'{job}-checkout-with-ref', ''.join(mutated)))

    # —— r5：标量形态绕过。r4 的行式 / 缩进式切块在源码行层面看文本，而这四种
    # 形态的**执行文本**（YAML 解析后的完整标量）已经不同——只有按解析后的标量
    # 比对并显式判红形态才能抓住（#2017 P0 复现 P1-1 / P1-2a / P1-2b / P1-3 / P1-4）。——

    # 行内续行：`run: |` 块改成 plain 多行标量，YAML 把换行折叠成空格，执行值变成
    # 「登记命令 + ' || true'」；按源码行首值比对的守卫恰好等于登记脚本 → 假 PASS。
    if ('        run: |\n'
        '          python3 -I scripts/retirement-outcome-parity/self_check.py '
        '--self-check-controls\n') not in text:
        raise CheckFailure('the 接线守卫变异自检 step block is not found to mutate')
    samples.append(('run-continuation', text.replace(
        '        run: |\n'
        '          python3 -I scripts/retirement-outcome-parity/self_check.py '
        '--self-check-controls\n',
        '        run: python3 -I scripts/retirement-outcome-parity/self_check.py '
        '--self-check-controls\n'
        '          || true\n', 1)))

    # 聚合 job `if` 续行 `&& false`：plain 多行标量折叠后是 `${{ always() }} &&
    # false`，job 从不执行；只收 4 空格行的守卫只看见 `${{ always() }}` → 假 PASS。
    # 按行号改写（`check_job_if` 已锚定聚合 job 块内的 `    if:` 行）：`text.replace`
    # 的 4 空格目标串同时是 8 空格 step 级 `if:` 的后缀，左起首个命中落在无关 step
    # （评审 r5 P1）。样本构造前提：变异体必须仍是合法的单文档 YAML——否则 KILLED
    # 只是「YAML 坏了被误杀」，对这条形态零回归保护；`control_wiring` 的
    # SAMPLE_FAILURE_ANCHORS 另断言失败文案落在聚合 job 条件那条判定上。
    aggregate_continuation = ''.join(
        lines[:check_job_if] + ['    if: ${{ always() }}\n', '      && false\n']
        + lines[check_job_if + 1:])
    _sample_yaml = _yaml_module()
    try:
        _sample_documents = list(_sample_yaml.compose_all(aggregate_continuation))
    except _sample_yaml.YAMLError as error:
        raise CheckFailure(
            f'aggregate-job-if-continuation must stay parsable as YAML: {error}') from error
    if len(_sample_documents) != 1:
        raise CheckFailure('aggregate-job-if-continuation must stay a single YAML document')
    samples.append(('aggregate-job-if-continuation', aggregate_continuation))

    # Require step `if` 续行 `&& false`：续行落在「缩进更深的子行」里被旧守卫
    # 静默忽略，折叠后的条件恒假 → step 从不执行，job 仍 success。
    require_if = ('        if: ${{ always() && '
                  'needs.retirement-outcome-parity.result != \'success\' && '
                  '!(needs.retirement-outcome-parity.result == \'skipped\' && '
                  'needs.scope.outputs.docs_only == \'true\') }}\n')
    if require_if not in text:
        raise CheckFailure('the aggregate Require `if` line is not found to mutate')
    samples.append(('require-if-continuation', text.replace(
        require_if, require_if + '          && false\n', 1)))

    # 块标量 `|` → `>`：源码行一字未动，而 Actions 执行的是折叠后的标量（换行全部
    # 折成空格——多行命令变成一串参数，`bash -e` 下第一条就 rc≠0 也只是失败不绕过，
    # 但若整块恰好折叠成可执行的等价物时闸门已非登记脚本）。
    folded = list(lines)
    folded[check_run] = '        run: >\n'
    samples.append(('folded-block', ''.join(folded)))

    # job 查找按行首：把两个 job 的登记样子整块挪到 `on:` 下（未知事件名被 GitHub
    # 静默忽略），真 `jobs:` 里的 retirement job 删除、聚合 needs 与 Require step
    # 同步摘除——按 `^  <job>:\s*$` 行首找块的守卫在 `on:` 下找到了假块 → 假 PASS。
    fake_job_start = next(index for index, line in enumerate(lines)
                          if line == f'  {RETIREMENT_JOB}:\n')
    fake_job_end = next(index for index, line in enumerate(lines)
                        if line == f'  {AGGREGATE_JOB}:\n')
    fake_retirement = lines[fake_job_start:fake_job_end]
    fake_require_end = require_start + 4  # name / if / shell / run 四行整块
    fake = (['on:\n', f'  {RETIREMENT_JOB}:\n'] + fake_retirement[1:]
            + [f'  {AGGREGATE_JOB}:\n',
               '    needs: [scope, metadata-observer, rust, a51-coverage, '
               'negative-controls, retirement-outcome-parity]\n',
               '    if: ${{ always() }}\n',
               '    runs-on: ubuntu-latest\n',
               '    steps:\n']
            + lines[require_start:fake_require_end])
    aggregate_needs = ('    needs: [scope, metadata-observer, rust, a51-coverage, '
                       'negative-controls, retirement-outcome-parity]\n')
    outside = []
    for index, line in enumerate(lines):
        if line == 'on:\n':
            outside.extend(fake)
        elif fake_job_start <= index < fake_job_end \
                or require_start <= index < fake_require_end:
            continue
        elif line == aggregate_needs:
            outside.append('    needs: [scope, metadata-observer, rust, a51-coverage, '
                          'negative-controls]\n')
        else:
            outside.append(line)
    samples.append(('job-outside-jobs', ''.join(outside)))

    # —— r9（评审 P2 concurrency#2）：外层预算被砍。timeout-minutes 低于装置内部
    # 最坏和（INTERNAL_WORST_CASE_SECONDS 派生机检）时，挂住的检查打不满自己的
    # 内部超时、失败退化为平台硬杀（cancelled，哪个检查红了都看不见）。——
    timeout_line = f'    timeout-minutes: {RETIREMENT_JOB_TIMEOUT_MINUTES}\n'
    if text.count(timeout_line) != 1:
        raise CheckFailure('the retirement job does not carry exactly one '
                           f'`{timeout_line.strip()}` line to mutate')
    samples.append(('retirement-job-timeout-lowered', text.replace(
        timeout_line, '    timeout-minutes: 55\n', 1)))

    # —— #2451（r11 P0 的 step 级声明上限）：三个直跑 gate 各自的 step
    # `timeout-minutes` 是预算遍历的入算上限。降值：挂住的检查打不满自己的内部
    # 超时，失败退化成 step 硬杀（cancelled，哪个 sub-check 红了都看不见）；升值
    # 同样改变入算和（外层 170 min 的派生判据跟着变）。钉值判定住在
    # `retirement_budget_rows`（workflow 样本里只有 check_wiring 的预算接线块会调
    # 它）——预算接线块被删掉时，这四条 SURVIVED：守卫退化成「没人消费的常量
    # 比对」的机器证据，正是 #2451 点名的形态。第五条（删键）红在 step 键集判定：
    # 键没了是第一道拦截（`plus its registered timeout-minutes`）。——
    # 待匹配的声明值从 `GATE_STEP_TIMEOUT_MINUTES` 登记面派生（评审 r1 P2：手抄
    # 5/30 是第二份拷贝，登记值合法变更时这里会红且文案误导为「registered」）；
    # 定位一律先 `_step_range` 圈住目标 step 块（评审 r1：全文件首匹配在别的
    # step 未来声明同值时会锚错步）。
    emit_start, emit_end = _step_range(lines, 'emit 复算与金样逐字节比对')
    emit_registered = GATE_STEP_TIMEOUT_MINUTES['emit 复算与金样逐字节比对']
    emit_timeout = next((index for index in range(emit_start, emit_end)
                         if lines[index].rstrip('\n')
                         == f'        timeout-minutes: {emit_registered}'), None)
    if emit_timeout is None:
        raise CheckFailure('the emit step does not carry the registered '
                           f'`timeout-minutes: {emit_registered}` line to mutate')
    for label, value in (
            ('gate-step-timeout-lowered',
             f'        timeout-minutes: {emit_registered - 1}\n'),
            ('gate-step-timeout-raised',
             f'        timeout-minutes: {emit_registered + 1}\n')):
        mutated = list(lines)
        mutated[emit_timeout] = value
        samples.append((label, ''.join(mutated)))
    # 负控 step 的登记值是 `NEGCTL_TIMEOUT_SECONDS // 60`（声明限把原始贡献压低
    # 一个量级，降级语义见 NEGCTL_STEP_CAP_ABSORBED_CHECKER_TIMEOUTS）。
    negctl_start, negctl_end = _step_range(lines, '负控 N1–N17 全 KILLED')
    negctl_registered = GATE_STEP_TIMEOUT_MINUTES['负控 N1–N17 全 KILLED']
    negctl_timeout = next((index for index in range(negctl_start, negctl_end)
                          if lines[index].rstrip('\n')
                          == f'        timeout-minutes: {negctl_registered}'), None)
    if negctl_timeout is None:
        raise CheckFailure('the 负控 step does not carry the registered '
                           f'`timeout-minutes: {negctl_registered}` line to mutate')
    for label, value in (
            ('negctl-step-timeout-lowered',
             f'        timeout-minutes: {negctl_registered - 1}\n'),
            ('negctl-step-timeout-raised',
             f'        timeout-minutes: {negctl_registered + 1}\n')):
        mutated = list(lines)
        mutated[negctl_timeout] = value
        samples.append((label, ''.join(mutated)))
    samples.append(('negctl-step-timeout-removed', ''.join(
        lines[:negctl_timeout] + lines[negctl_timeout + 1:])))
    return samples


def pr_gate_samples():
    """pr-gate.yml 的接线变异样本 `[(名字, 变异后的 workflow 文本)]`（r8 P1-2 + r9 P0）。

    pr-gate.yml 是合并闸门第 2、3 步（git fetch base / check-pr-gate.py）的宿主，
    此前**没有任何守卫读这个文件**：根级 `defaults.run.shell: /bin/true {0}` 让
    check-pr-gate step 无条件 rc=0 → 合并闸门假 PASS，而 ci.yml 两个守卫逐字全等。
    r9 P0 起再加 step 级三形态（评审 boundary#1 = sibling#1 = concurrency#1）：
    - `pr-gate-step-shell-removed`：check-pr-gate step（合并闸门本体）的
      `shell: bash` 被删——r9 修正：pr-gate.yml:37 的 shell 行带行尾注释，
      r8 的字面量 `'        shell: bash'` 匹配不到它、`shells[-1]` 落在 fetch 步
      ——docstring 声称删 check 步、实际删的是 fetch 步的 shell，名不副实。
      r9 起 step 直属键集**恰等**登记先行拦下（found 缺 `shell`），全 workflow
      巡走的 shell 判定仍是兜底（本样本锚定键集判定）；
    - `pr-gate-root-defaults-shell`：根级 `defaults.run.shell`（根键白名单判定
      在 pr-gate.yml 上的落点）；
    - `pr-gate-job-if-false`：job `if: false`——被跳过的必需检查被 GitHub 报成
      success（P0 形态②，job `if` 逐字判定的落点）；
    - `pr-gate-gate-continue-on-error`：check 步加 `continue-on-error: true`
      （P0 形态①，step 键集白名单的落点）；
    - `pr-gate-run-exit-0`：check 步 `run` 换壳 `exit 0`（P0 形态③，run 标量
      逐字判定的落点）；
    - `pr-gate-fetch-continue-on-error-removed` / `pr-gate-fetch-continue-on-error-
      swapped`：fetch 基线步的登记键被删 / 登记值被换（fetch 步键集与值钉死的
      落点——键集白名单对两个 run step 分开登记）；
    - `pr-gate-env-key-added`：check 步 env 加一个未登记键（env 键集钉死的落点）；
    - `pr-gate-gate-step-removed`：整个 check 步被删（step 顺序/条数钉死的落点）。
    """
    text = PR_GATE.read_text(encoding='utf-8')
    lines = text.splitlines(keepends=True)
    samples = []

    def step_range(name):
        start = next((index for index, line in enumerate(lines)
                      if line.rstrip('\n') == f'      - name: {name}'), None)
        if start is None:
            raise CheckFailure(f'the pr-gate workflow has no `{name}` step to mutate')
        end = next((index for index in range(start + 1, len(lines))
                    if STEP_ITEM.match(lines[index])
                    or JOB_HEADER.match(lines[index].rstrip('\n'))), len(lines))
        return start, end

    gate_start, gate_end = step_range('角色审计结果块与 issue 关联')
    fetch_start, fetch_end = step_range('显式取 base 分支与 PR head（不依赖 checkout 的默认 refspec）')

    # r9 修正：check 步的 shell 行带行尾注释（pr-gate.yml:37），按「行首值 + 可选
    # 注释」锚定，真正删掉 check 步的 shell 而不是 fetch 步的。
    gate_shell = next((index for index in range(gate_start, gate_end)
                       if re.fullmatch(r'        shell: bash(\s*#.*)?\n',
                                       lines[index])), None)
    if gate_shell is None:
        raise CheckFailure('the pr-gate check step has no `shell: bash` line to drop')
    samples.append(('pr-gate-step-shell-removed', ''.join(
        lines[:gate_shell] + lines[gate_shell + 1:])))

    if text.count('\njobs:\n') != 1:
        raise CheckFailure('the pr-gate workflow must carry exactly one `jobs:` line '
                           'to anchor the root-level defaults injection')
    samples.append(('pr-gate-root-defaults-shell', text.replace(
        '\njobs:\n',
        '\ndefaults:\n  run:\n    shell: /bin/true {0}\njobs:\n', 1)))

    # P0 形态②：job `if: false`。
    job_if = next((index for index, line in enumerate(lines)
                   if line.rstrip('\n') == '    if: github.event.pull_request.draft == false'),
                  None)
    if job_if is None:
        raise CheckFailure('the pr-gate workflow has no registered job `if:` line '
                           'to mutate')
    samples.append(('pr-gate-job-if-false', ''.join(
        lines[:job_if] + ['    if: false\n'] + lines[job_if + 1:])))

    # P0 形态①：check 步加 step 级 `continue-on-error: true`（步全红而 job 仍
    # success）。键集白名单（恰等）当场红。
    samples.append(('pr-gate-gate-continue-on-error', ''.join(
        lines[:gate_start + 1] + ['        continue-on-error: true\n']
        + lines[gate_start + 1:])))

    # P0 形态③：check 步 run 换壳。登记标量是单行 `python3 -I scripts/check-pr-gate.py`。
    gate_run = next((index for index in range(gate_start, gate_end)
                     if lines[index].rstrip('\n')
                     == '        run: python3 -I scripts/check-pr-gate.py'), None)
    if gate_run is None:
        raise CheckFailure('the pr-gate check step has no registered `run:` line '
                           'to swap')
    samples.append(('pr-gate-run-exit-0', ''.join(
        lines[:gate_run] + ['        run: exit 0\n'] + lines[gate_run + 1:])))

    # fetch 基线步的登记键 continue-on-error 被删 / 登记值被换：键集（恰等）与
    # 值钉死两个判定各自要有违反路径，删错方向才看得见。
    fetch_coe = next((index for index in range(fetch_start, fetch_end)
                      if lines[index].startswith('        continue-on-error:')), None)
    if fetch_coe is None:
        raise CheckFailure('the pr-gate fetch step has no `continue-on-error:` '
                           'line to mutate')
    samples.append(('pr-gate-fetch-continue-on-error-removed', ''.join(
        lines[:fetch_coe] + lines[fetch_coe + 1:])))
    swapped = list(lines)
    swapped[fetch_coe] = '        continue-on-error: false\n'
    samples.append(('pr-gate-fetch-continue-on-error-swapped', ''.join(swapped)))

    # env 键集：check 步 env 加未登记键。
    gate_env = next((index for index in range(gate_start, gate_end)
                     if lines[index].rstrip('\n') == '        env:'), None)
    if gate_env is None:
        raise CheckFailure('the pr-gate check step has no `env:` block to mutate')
    samples.append(('pr-gate-env-key-added', ''.join(
        lines[:gate_env + 1] + ['          EXTRA: drift\n'] + lines[gate_env + 1:])))

    # 整步删除：step 条数 / 顺序钉死。
    samples.append(('pr-gate-gate-step-removed', ''.join(
        lines[:gate_start] + lines[gate_end:])))
    return samples


# 样本必须红在**点名的判定**上（评审 r5 P1 / r6；r7 P1 起全量登记）：改错行的样本
# 只是把 YAML 弄坏被「误杀」，KILLED 不能证明对应形态真的被守卫看见。这份字典是
# 接线变异样本的**登记簿**：键为样本名（顺序与 `wiring_samples() + pr_gate_samples()`
# 一致），值为该样本失败文案必含的锚串——锚串落点同时证明变异体仍是可解析的单文档
# YAML（否则红在解析失败）。`control_wiring` 断言样本集合与登记簿**逐名全等**（少一条
# = 有形态失保，多一条 = 没登记就上岗），任何一条红在别的判定上都算 misattributed；
# 条数另钉死在 `EXPECTED_WIRING_SAMPLES`（评审 r9 P2 sibling#3：样本 + 锚串**一起**
# 删时逐名全等照样成立、登记簿静默缩水——对齐 LOCK_HOSTILE_PLANTS 先例）；数量
# 同步登记在装置 README（r9 起四十四条；#2451 起四十九条——补 step 级声明上限五条）。
# 锚串全部取自真实运行的失败文案
# （2026-11-07/08 实测，`_tmp/20261107-issue-1809-r7/`、`_tmp/20261107-issue-1809-r8/`
# 与 `_tmp/20261026-issue-1809/fix-r9/`；#2451 五条 2026-11-09 实测，
# `_tmp/20261109-p2batch-g/s4-controls-wiring.txt`）。
SAMPLE_FAILURE_ANCHORS = {
    'removed': 'no `retirement-outcome-parity` job under `jobs:`',
    'commented-out': 'no `retirement-outcome-parity` job under `jobs:`',
    'disabled-step': 'may only carry `name`, `run` and `shell`',
    'quoted-if-step': 'may only carry `name`, `run` and `shell`',
    'continue-on-error-step': 'may only carry `name`, `run` and `shell`',
    'job-level-continue-on-error':
        'job carries the unregistered key `continue-on-error`',
    'commands-moved-to-env': "found ['env', 'name', 'run', 'shell']",
    'nested-env-run': "found ['env', 'if', 'name', 'run', 'shell']",
    'false-if-block': 'does not run the registered script',
    'negated-command': 'does not run the registered script',
    'suppressed-command': 'does not run the registered script',
    'echoed-command': 'does not run the registered script',
    'removed-gate-step': 'must keep exactly 8 steps',
    'disabled-require-step': 'the aggregate Require step condition',
    'weakened-require-step': 'the aggregate Require step condition',
    'continue-on-error-require-step':
        "found ['continue-on-error', 'if', 'name', 'run', 'shell']",
    'and-false-require-step': 'the aggregate Require step condition',
    'exit-zero-require-step': 'must run exactly `exit 1`',
    'aggregate-job-if-false': '`check` job condition',
    'multi-document': 'expected exactly one YAML document',
    'root-defaults-shell': 'unregistered root key',
    'gate-step-shell-removed':
        f"found {sorted(_retirement_run_step_keys('gate', 'check 5 轮') - {'shell'})}",
    'gate-step-shell-swapped': 'must pin `shell: bash`',
    'require-step-shell-swapped': 'the aggregate Require step must pin `shell: bash`',
    'rust-step-shell-removed': 'has a run step without the pinned `shell: bash`',
    'checkout-with-ref': 'must carry exactly the registered `with` keys',
    'metadata-observer-checkout-with-ref': 'carries the unregistered `with` keys',
    'negative-controls-checkout-with-ref': 'carries the unregistered `with` keys',
    'gateway-schema-baseline-checkout-with-ref': 'carries the unregistered `with` keys',
    'run-continuation': 'plain (continuation-folding) scalar',
    'aggregate-job-if-continuation': '`check` job condition',
    'require-if-continuation': 'the aggregate Require step condition',
    'folded-block': 'is a folded block',
    'job-outside-jobs': 'no `retirement-outcome-parity` job under `jobs:`',
    'retirement-job-timeout-lowered':
        f'must stay exactly `{RETIREMENT_JOB_TIMEOUT_MINUTES}`',
    # #2451：直跑 gate 的 step 级声明上限钉值——前四条红在预算遍历的钉值判定
    # （锚串即该判定文案），第五条删键红在 step 键集判定（第一道拦截）。
    'gate-step-timeout-lowered': '#1809 r11 step timeout',
    'gate-step-timeout-raised': '#1809 r11 step timeout',
    'negctl-step-timeout-lowered': '#1809 r11 step timeout',
    'negctl-step-timeout-raised': '#1809 r11 step timeout',
    'negctl-step-timeout-removed': 'plus its registered timeout-minutes',
    'pr-gate-step-shell-removed': "found ['env', 'name', 'run']",
    'pr-gate-root-defaults-shell':
        'the pr-gate workflow carries the unregistered root key',
    'pr-gate-job-if-false': 'the `pr-gate` job condition',
    'pr-gate-gate-continue-on-error': "may only carry ['env', 'name', 'run', 'shell']",
    'pr-gate-run-exit-0': 'does not run the registered script',
    'pr-gate-fetch-continue-on-error-removed':
        "found ['env', 'name', 'run', 'shell']",
    'pr-gate-fetch-continue-on-error-swapped':
        '`continue-on-error` must stay exactly `true`',
    'pr-gate-env-key-added': 'env must carry exactly the registered keys',
    'pr-gate-gate-step-removed': 'must keep exactly 3 steps',
}
# 条数常量（评审 r9 P2 sibling#3）：与 LOCK_HOSTILE_PLANTS 同口径——删样本 +
# 删锚串两边一起删时逐名全等仍成立，条数当场红。数量同步登记在装置 README。
# #2451 起 49（补 step 级声明上限的五条：gate/negctl 钉值四条 + 负控删键一条）。
EXPECTED_WIRING_SAMPLES = 49
# #2451：r11 预算接线的另外五个判定只有预算面（`retirement_budget_rows` /
# `_assert_step_budget_coverage` / `check_wiring` 的 budget total）消费——workflow
# 样本够不到：改 step 顺序 / 键集 / 脚本标量的变异会先红在顺序 / 键集 / 脚本判定
# 上，到不了预算判定。control_wiring 对**登记簿本身**做进程内变异直攻点名的判定
# （口径对齐 GUARD_FD_MUTATIONS / LOCK_HOSTILE_PLANTS：样本名 + 锚串 + 条数一起
# 钉死，少一条当场红）。
REGISTRY_MUTATION_ANCHORS = {
    'gate-timeout-registry-ghost': 'step cap coverage',
    'overhead-registry-ghost': 'overhead coverage',
    'overhead-registry-drop-upload': 'budget rule missing',
    'step-order-registry-swap': 'budget coverage',
    'emit-timeout-constant-drift': 'budget total',
}
EXPECTED_REGISTRY_MUTATIONS = 5


def control_check():
    """P7 非退化机检的三个方向都要能红（评审 r9 P2 = r8 boundary#4；r10 P1 加方向）。

    变异体就是评审点名的攻击形态：保留全部判据消费字段（`(outcome, errorCode)`
    逐轮不变），只给一份 `methods.json` 塞一个**不消费**的尾字节——sha256 互异
    照样成立（仍 5 个互异值）、checker 照样 PASS，只有 PROVENANCE 交叉核对能红。
    第二个方向（r10 P1，评审 MM boundary#2）：对 `frames.jsonl` 塞一个判据不消费
    的字节（首行 `"seq":1` 的冒号后加空格——json.loads 仍过、frame 值不变），
    r9 的交叉核对只查 methods 时这形态全绿，必须红在 frames 的专属判定上。
    第三个方向：删掉 PROVENANCE 表里的一行登记（登记簿静默缩水）。
    """
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-check-') as tmp:
        root = Path(tmp) / 'fixtures'
        shutil.copytree(FIXTURES, root)
        target = root / 'round-03' / 'driver' / 'methods.json'
        with open(target, 'ab') as handle:  # 判据 json.loads 会忽略的尾随空白
            handle.write(b' ')
        try:
            check_check(root)
        except CheckFailure as error:
            if 'fixtures/PROVENANCE.md' not in str(error):
                raise CheckFailure(
                    f'unconsumed byte rejected by the wrong judgment: {error}') from error
        else:
            raise CheckFailure(
                'the provenance cross-check did not reject an unconsumed trailing '
                'byte (five byte-distinct rounds re-sliced from one collection '
                'would pass as "independent"); the r9 fix has rotted')
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-check-') as tmp:
        root = Path(tmp) / 'fixtures'
        shutil.copytree(FIXTURES, root)
        # r10 P1：JSON 冒号后加空格——判据（json.loads 解析后的 frame 值）不消费，
        # sha256 变、P1–P8 结论不变，只有 frames 的 PROVENANCE 交叉核对能红。
        target = root / 'round-02' / 'driver' / 'frames.jsonl'
        original = target.read_text(encoding='utf-8')
        anchor = '{"seq":1,"connection":1,'
        first_line = original.partition('\n')[0]
        if not original.startswith(anchor) or first_line.count(anchor) != 1:
            raise CheckFailure('the frames.jsonl first-line anchor is not uniquely '
                               'present to mutate; update this control with the '
                               'current anchor')
        target.write_text(original.replace(anchor, '{"seq": 1,"connection":1,', 1),
                         encoding='utf-8')
        try:
            check_check(root)
        except CheckFailure as error:
            if 'frames.jsonl digests no longer match' not in str(error):
                raise CheckFailure(
                    f'unconsumed frames byte rejected by the wrong judgment: '
                    f'{error}') from error
        else:
            raise CheckFailure(
                'the provenance cross-check did not reject an unconsumed byte in '
                'frames.jsonl (the frames column had no external anchor before '
                'r10 and a re-sliced round would pass as "independent"); the r10 '
                'fix has rotted')
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-check-') as tmp:
        root = Path(tmp) / 'fixtures'
        shutil.copytree(FIXTURES, root)
        table = root / 'PROVENANCE.md'
        lines = table.read_text(encoding='utf-8').split('\n')
        rows = [index for index, line in enumerate(lines)
                if re.match(r'^\| round-\d{2} \| `[0-9a-f]{64}` \| `[0-9a-f]{64}` \|$',
                            line)]
        if len(rows) != EXPECTED_ROUNDS:
            raise CheckFailure('the provenance digest table does not carry '
                                f'{EXPECTED_ROUNDS} rows to drop; update this '
                                'control with the current anchor')
        del lines[rows[0]]
        table.write_text('\n'.join(lines), encoding='utf-8')
        try:
            check_check(root)
        except CheckFailure as error:
            if 'fixtures/PROVENANCE.md' not in str(error):
                raise CheckFailure(
                    f'missing provenance row rejected by the wrong judgment: '
                    f'{error}') from error
        else:
            raise CheckFailure('the provenance cross-check did not reject a '
                               'fixture set with a dropped registration row')
    return ('unconsumed bytes (methods and frames) and dropped provenance rows '
            'are all rejected')


def control_wiring():
    """`wiring_samples() + pr_gate_samples()` 的每一条都必须让接线守卫变红。

    样本集合必须与 `SAMPLE_FAILURE_ANCHORS` 登记簿逐名全等（评审 r7 P1：样本被删 /
    漏登记时不能静默缩水）且条数等于 `EXPECTED_WIRING_SAMPLES`（r9 P2 sibling#3：
    样本 + 锚串一起删时逐名全等仍成立，条数当场红）；每条样本的失败文案必须含各自
    的锚串（红在别的判定 = 形态被误杀，对该形态零回归保护）。ci.yml 变异体配真实
    pr-gate.yml、pr-gate.yml 变异体配真实 ci.yml（r8 P1-2）：另一侧的意外红不能
    挡住 / 冒充点名的判定。
    """
    samples = wiring_samples() + pr_gate_samples()
    labels = [label for label, _ in samples]
    registered = list(SAMPLE_FAILURE_ANCHORS)
    if labels != registered:
        raise CheckFailure(
            f'wiring sample set drifted from the registry: got {labels}, '
            f'registered {registered}; a missing sample leaves that mutation '
            'shape unprotected and an unregistered sample proves nothing')
    if len(samples) != EXPECTED_WIRING_SAMPLES:
        raise CheckFailure(
            f'the wiring sample registry carries {len(samples)} samples but '
            f'{EXPECTED_WIRING_SAMPLES} are registered; deleting a sample (and '
            'its anchor) must go red here, not silently narrow the guard')
    survived = []
    misattributed = []
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-wiring-') as tmp:
        for label, mutated in samples:
            # ci.yml 变异体配**真实** pr-gate.yml、pr-gate.yml 变异体配**真实** ci.yml：
            # 另一侧的缺文件 / 意外红不能把样本挡在点名的判定之外，也不能冒充判定本身。
            if label.startswith('pr-gate-'):
                workflow_arg, gate_arg = WORKFLOW, Path(tmp) / f'gate-{label}.yml'
                gate_arg.write_text(mutated, encoding='utf-8')
            else:
                workflow_arg, gate_arg = Path(tmp) / f'ci-{label}.yml', PR_GATE
                workflow_arg.write_text(mutated, encoding='utf-8')
            try:
                check_wiring(workflow_arg, gate_arg)
            except CheckFailure as error:
                anchor = SAMPLE_FAILURE_ANCHORS[label]
                if anchor not in str(error):
                    misattributed.append(
                        f'{label}: rejected by the wrong judgment ({error}), '
                        f'expected the failure to mention `{anchor}`')
                continue
            survived.append(label)
    if survived:
        raise CheckFailure('wiring guard did not reject: '
                           f'{survived} (the CI wiring check was weakened)')
    if misattributed:
        raise CheckFailure('wiring samples rejected by the wrong judgment: '
                           f'{misattributed}')
    # —— #2451：登记簿内变异控件（REGISTRY_MUTATION_ANCHORS 注）。每条：临时改
    # 模块级登记簿 → 调消费点名的判定的探针 → 期望 CheckFailure 文案含锚串 →
    # finally 还原。探针直调预算面（emit-timeout-constant-drift 例外：budget total
    # 判定只住在 check_wiring），让锚串落在点名的判定上，不被顺序 / 键集 / 脚本
    # 判定提前拦截——那正是 workflow 样本够不到这些判定的原因。变异只在本控件
    # 进程内、finally 必还原，不落任何文件。——
    if len(REGISTRY_MUTATION_ANCHORS) != EXPECTED_REGISTRY_MUTATIONS:
        raise CheckFailure(
            f'the registry-mutation control carries {len(REGISTRY_MUTATION_ANCHORS)} '
            f'entries but {EXPECTED_REGISTRY_MUTATIONS} are registered; deleting an '
            'entry (and its anchor) must go red here, not silently narrow the control')

    registry_survived = []
    registry_misattributed = []

    def expect_registry_failure(label, probe):
        executed.append(label)
        try:
            probe()
        except CheckFailure as error:
            anchor = REGISTRY_MUTATION_ANCHORS[label]
            if anchor not in str(error):
                registry_misattributed.append(
                    f'{label}: rejected by the wrong judgment ({error}), '
                    f'expected the failure to mention `{anchor}`')
        else:
            registry_survived.append(label)

    executed = []
    gate_timeouts_saved = dict(GATE_STEP_TIMEOUT_MINUTES)
    GATE_STEP_TIMEOUT_MINUTES['幽灵 gate（#2451 内变异）'] = 5
    try:
        # 幽灵闸门超时登记：预算行的 cap 集合（3 个真声明）不再等于登记面（4 条）
        # → step cap coverage 当场红。
        expect_registry_failure(
            'gate-timeout-registry-ghost',
            lambda: _assert_step_budget_coverage(retirement_budget_rows()))
    finally:
        GATE_STEP_TIMEOUT_MINUTES.clear()
        GATE_STEP_TIMEOUT_MINUTES.update(gate_timeouts_saved)
    overhead_saved = JOB_OVERHEAD_STEPS
    globals()['JOB_OVERHEAD_STEPS'] = overhead_saved | {
        ('uses', '幽灵上传（#2451 内变异）')}
    try:
        # 幽灵 overhead 桶：行的桶集合（3 个真桶）不再等于登记面（4 条）。
        expect_registry_failure(
            'overhead-registry-ghost',
            lambda: _assert_step_budget_coverage(retirement_budget_rows()))
    finally:
        globals()['JOB_OVERHEAD_STEPS'] = overhead_saved
    globals()['JOB_OVERHEAD_STEPS'] = frozenset(
        entry for entry in overhead_saved
        if entry != ('uses', '上传负控证据'))
    try:
        # 摘掉 upload 步的桶登记：该步掉进「无预算规则」分支——上传步 0 秒入算
        # 恰是靠登记桶显式给的，撤销登记必须红而不是静默归零。
        expect_registry_failure(
            'overhead-registry-drop-upload',
            lambda: retirement_budget_rows())
    finally:
        globals()['JOB_OVERHEAD_STEPS'] = overhead_saved
    step_order_saved = RETIREMENT_STEP_ORDER
    globals()['RETIREMENT_STEP_ORDER'] = (
        step_order_saved[:2] + (step_order_saved[3], step_order_saved[2])
        + step_order_saved[4:])
    try:
        # 重排登记顺序：行的实际顺序（按 workflow 里的 step 顺序）不再等于登记面
        # ——kinds 映射不受顺序影响，红点恰在双向覆盖判定上。
        expect_registry_failure(
            'step-order-registry-swap',
            lambda: _assert_step_budget_coverage(retirement_budget_rows()))
    finally:
        globals()['RETIREMENT_STEP_ORDER'] = step_order_saved
    emit_timeout_saved = EMIT_TIMEOUT_SECONDS
    globals()['EMIT_TIMEOUT_SECONDS'] = emit_timeout_saved + 60
    try:
        # 常量漂移：模块级 INTERNAL_WORST_CASE_SECONDS（导入时求值）不再等于按步
        # 遍历的和——预算接线块被绕过 / 常量被单独改时，正是这条判定在挡。
        expect_registry_failure('emit-timeout-constant-drift',
                                lambda: check_wiring())
    finally:
        globals()['EMIT_TIMEOUT_SECONDS'] = emit_timeout_saved
    if executed != list(REGISTRY_MUTATION_ANCHORS):
        raise CheckFailure(
            f'the registry-mutation cases drifted from the registry: ran '
            f'{executed}, registered {list(REGISTRY_MUTATION_ANCHORS)}')
    if registry_survived:
        raise CheckFailure('budget-registry mutations not rejected: '
                           f'{registry_survived} (the r11 budget wiring has '
                           'no live consumer)')
    if registry_misattributed:
        raise CheckFailure('registry mutations rejected by the wrong judgment: '
                           f'{registry_misattributed}')
    return (f'{len(samples)} mutated workflows '
            f'({", ".join(s for s, _ in samples)}) rejected + '
            f'{EXPECTED_REGISTRY_MUTATIONS} in-registry budget mutations '
            f'({", ".join(REGISTRY_MUTATION_ANCHORS)}) rejected')


def control_emit():
    """emit 发布失败孪生的牙齿（fix-r15，DS r14 sibling#3 / MM sibling#1 / MM
    boundary#1 三撞 P2a）：把清理 unlink 裸化的变异必须让孪生红在专属判定上。

    与 control_lock 的弱化 flock 变异同一形态：临时 checker 副本退回 r14 之前的
    口径（失败路径清理错顶掉原发布错），孪生必须红在「surfaced the wrong error」
    的专属文案上——裸 unlink 变异下②的 `except OSError` 收到的是清理错
    `#emit-twin cleanup boom`，不含 `#emit-twin publish boom`。正控只跑进程内
    孪生本身（check_emit 的子进程金样复算在七项自检里已跑，这里不重复起子进程，
    预算行无新增贡献）。
    """
    _assert_emit_publish_failure_surface()
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-emit-') as tmp:
        source = CHECKER.read_text(encoding='utf-8')
        mutation = ('        with suppress(OSError):\n'
                    '            tmp.unlink(missing_ok=True)\n')
        if source.count(mutation) != 1:
            raise CheckFailure('the emit cleanup-suppress anchor is not uniquely '
                               f'present in {CHECKER.name} to mutate; update this '
                               'control with the current anchor')
        weakened = source.replace(
            mutation, '        tmp.unlink(missing_ok=True)\n')
        mutated = Path(tmp) / 'check_outcome_parity.py'
        mutated.write_text(weakened, encoding='utf-8')
        try:
            _assert_emit_publish_failure_surface(mutated)
        except CheckFailure as error:
            if 'surfaced the wrong error' not in str(error):
                raise CheckFailure(
                    'the bare-unlink mutant was rejected by the wrong judgment: '
                    f'{error}') from error
        else:
            raise CheckFailure(
                'the emit publish twin did not reject a bare cleanup unlink (the '
                'failure-path cleanup error masks the original publish error; '
                'the r14 invariant has rotted)')
    return 'the bare cleanup-unlink mutant is rejected by the publish twin'


CONTROLS_FOR_CHECK = {
    'budget': control_budget,
    'emit': control_emit,
    'check': control_check,
    'lock': control_lock,
    'wiring': control_wiring,
}


def run_self_check_controls(names):
    failures = []
    for name in names:
        try:
            detail = CONTROLS_FOR_CHECK[name]()
        except CheckFailure as error:
            failures.append(name)
            print(f'{name}-guard: SURVIVED — {error}', file=sys.stderr)
        except Exception as error:  # noqa: BLE001
            failures.append(name)
            print(f'{name}-guard: SURVIVED — unexpected {error!r}', file=sys.stderr)
        else:
            print(f'{name}-guard: KILLED — {detail}')
    return 1 if failures else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--only', action='append', choices=ORDER, default=None,
                        help='run only these checks (repeatable)')
    parser.add_argument('--self-check-controls', action='store_true',
                        help='mutation self-check: each new guard must reject a '
                             'violating sample')
    args = parser.parse_args(argv)
    if args.self_check_controls:
        # `--only` 对本模式同样生效（只跑这些守卫的变异控件）；点名的检查若没有
        # 对应控件就明说 rc2，不静默跑成「零个控件全通过」。
        names = args.only or list(CONTROLS_FOR_CHECK)
        without = [name for name in names if name not in CONTROLS_FOR_CHECK]
        if without:
            print(f'self_check: no mutation control for {without} '
                  f'(available: {list(CONTROLS_FOR_CHECK)})', file=sys.stderr)
            return 2
        return run_self_check_controls(names)
    return run_checks(args.only or ORDER)


# 唯一的完整内部和：实际声明每步贡献的遍历累加；兄弟测试同源导入这个导出值。
INTERNAL_WORST_CASE_SECONDS = sum(row['seconds'] for row in retirement_budget_rows())


if __name__ == '__main__':
    sys.exit(main())
