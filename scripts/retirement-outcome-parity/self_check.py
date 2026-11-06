#!/usr/bin/env python3
"""retirement-outcome-parity 装置的自动闸门（#1809）。

把「装置自己还能不能用」变成可机检的一条命令，供 CI 与本地共用：

  budget   切片夹具的体积预算（单文件 ≤ 1 MiB、总量 ≤ 5 MiB，铁律 8 的入库上限）
  emit     `emit` 从入库夹具复算金样，必须与已提交的 `ts-oracle.json` **逐字节**相同
  check    `check` 5 轮全 PASS，且 `p7Evaluated` 为真（跨轮稳定性真的核过）
  negctl   负控 N1–N17 全 KILLED（任一 SURVIVED / INVALID 即红）
  lock     输出目录互斥：第二个进程必须**等**第一个（不是静默并行）
  wiring   `.github/workflows/ci.yml` 里这四件事都接着，且 `check` 聚合 job 依赖它

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
import itertools
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
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
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
WORKFLOW = REPO_ROOT / '.github' / 'workflows' / 'ci.yml'

# 铁律 8 的入库上限：单文件超 10 MB 要先想清楚。这里收得更紧，给未来重采留余量。
FIXTURE_FILE_LIMIT_BYTES = 1 << 20          # 1 MiB
FIXTURE_TOTAL_LIMIT_BYTES = 5 << 20         # 5 MiB
EXPECTED_CONTROLS = 17
EXPECTED_ROUNDS = 5
# 持锁方**等释放信号**（不是睡固定秒数），因此这里只是「父进程失联」时的兜底上限，
# 不参与任何判定；判定用的事件是第二个进程打出「等锁」提示。
LOCK_PROBE_HOLD_SECONDS = 60.0
LOCK_PROBE_WAIT_TIMEOUT_SECONDS = 60.0
# 预置敌意锁文件（FIFO / 符号链接 / 目录）时，持锁方必须**很快**失败；这个上限只用来
# 把「挂起」判成红，正常路径远低于它。
LOCK_HOSTILE_TIMEOUT_SECONDS = 20.0
# 与 check_outcome_parity.LOCK_BASENAME 同步的**字面**副本：故意不 import，
# 这样改了常量却忘了改控件会当场红（下面还有一条源码断言兜住重命名）。
LOCK_BASENAME_LITERAL = '.retirement-outcome-parity.lock'

# 子进程里持有输出目录锁的最小驱动（与 negative_controls 用**同一个** output_lock）。
LOCK_HOLDER = r'''
import importlib.util, os, select, sys, tempfile
# 同族口径：加载 checker **之前**先把字节码缓存关掉。`-I` 不等于 `-B`，
# `spec_from_file_location` 在 exec 前会读本目录的 __pycache__/*.pyc——
# 预置的 unchecked-hash pyc 可以只打印 LOCKED 而不真持锁，锁守卫就假 PASS 了。
try:
    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')
except OSError:
    print('lock holder: cannot reserve a fresh pycache prefix', file=sys.stderr)
    raise SystemExit(2)
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('check_outcome_parity', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
try:
    with module.output_lock(sys.argv[2]):
        print('LOCKED', flush=True)
        # 持到父进程写一行（或 stdin 关闭返回 EOF）为止；argv[3] 只是兜底上限。
        readable, _, _ = select.select([sys.stdin], [], [], float(sys.argv[3]))
        if readable:
            sys.stdin.readline()
except module.InputError as error:
    print('INPUT_ERROR', error, file=sys.stderr)
    raise SystemExit(2)
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


def fixture_round_dirs():
    rounds = sorted(path for path in FIXTURES.iterdir()
                    if (path / 'driver' / 'methods.json').is_file())
    if len(rounds) != EXPECTED_ROUNDS:
        raise CheckFailure(f'expected {EXPECTED_ROUNDS} fixture rounds, '
                           f'found {[path.name for path in rounds]}')
    return rounds


def check_emit():
    """`emit` 复算必须与已提交金样逐字节相同（金样来源可复现）。"""
    rounds = fixture_round_dirs()
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-emit-') as tmp:
        target = Path(tmp) / 'ts-oracle.json'
        command = [sys.executable, '-I', str(CHECKER), 'emit', '--oracle', str(target)]
        for round_dir in rounds:
            command += ['--round', str(round_dir)]
        result = subprocess.run(command, capture_output=True, text=True,
                                timeout=300, check=False)
        if result.returncode != 0:
            raise CheckFailure(f'emit rc={result.returncode}: '
                               f'{result.stderr.strip()[:800]}')
        produced = target.read_bytes()
    committed = ORACLE.read_bytes()
    if produced != committed:
        raise CheckFailure('emit output differs from the committed ts-oracle.json; '
                           'regenerate the oracle (see fixtures/PROVENANCE.md)')
    return f'byte-identical to {ORACLE.name} ({len(committed)} B)'


def check_check():
    """`check` 必须对入库的 5 轮全 PASS，并且 P7 真的跨轮核过。"""
    rounds = fixture_round_dirs()
    command = [sys.executable, '-I', str(CHECKER), 'check', '--oracle', str(ORACLE)]
    for round_dir in rounds:
        command += ['--round', str(round_dir)]
    result = subprocess.run(command, capture_output=True, text=True,
                            timeout=300, check=False)
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


def check_negctl():
    """负控必须全部 KILLED。"""
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-negctl-') as tmp:
        result = subprocess.run(
            [sys.executable, '-I', str(CONTROLS), '--output', tmp],
            capture_output=True, text=True, timeout=1800, check=False)
        summary_path = Path(tmp) / 'summary.json'
        if not summary_path.is_file():
            raise CheckFailure(f'negative controls produced no summary '
                               f'(rc={result.returncode}): {result.stderr.strip()[:800]}')
        summary = json.loads(summary_path.read_text(encoding='utf-8'))
    counts = summary.get('counts') or {}
    if result.returncode != 0 or summary.get('status') != 'PASS':
        raise CheckFailure(f'negative controls rc={result.returncode} '
                           f'status={summary.get("status")!r} counts={counts}')
    if counts.get('KILLED') != EXPECTED_CONTROLS:
        raise CheckFailure(f'expected {EXPECTED_CONTROLS} KILLED, got {counts}')
    return f'{counts["KILLED"]}/{EXPECTED_CONTROLS} KILLED, 0 SURVIVED/INVALID'


def _spawn_lock_holder(root, hold_seconds, stdin):
    return subprocess.Popen(
        [sys.executable, '-I', '-c', LOCK_HOLDER, str(CHECKER), str(root),
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


def _assert_lock_path_rejected(root, label):
    """预置敌意锁文件时，持锁方必须**快速**非零退出，绝不能挂起或假装持锁。

    这是 P0 回归钉：锁文件用 `O_RDONLY|O_CREAT` 打开，若不带 `O_NONBLOCK` 且不核
    `S_ISREG`，预置一个同名 FIFO 就会让 `open` 永久阻塞（Codex r2 实测），整个装置
    连同 CI job 一起挂死；预置目录 / 符号链接则分别是静默成功与 ELOOP。
    """
    process = _spawn_lock_holder(root, 0.0, subprocess.DEVNULL)
    try:
        out, err = process.communicate(timeout=LOCK_HOSTILE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=30)
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


def check_lock():
    """两个进程抢同一输出目录：第二个必须等（不是静默并行写）。

    时序**事件驱动**，不靠固定秒数：第一个取锁后停在那里等释放信号，第二个启动后
    必须打出「等锁」提示——那行只有在第一个真的抱着锁时才可能出现——父进程看到它
    才放开第一个。因此慢机、冷启动、runner 抖动都不会把这个守卫推成假红。
    之后再验一遍**敌意锁文件**路径：预置 FIFO / 目录 / 符号链接时必须快速失败。
    """
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-lock-') as tmp:
        root = Path(tmp) / 'out'
        first = _spawn_lock_holder(root, LOCK_PROBE_HOLD_SECONDS, subprocess.PIPE)
        second = None
        try:
            _read_until(first.stdout, b'LOCKED', LOCK_PROBE_WAIT_TIMEOUT_SECONDS,
                        'first lock holder')
            started = time.monotonic()
            second = _spawn_lock_holder(root, 0.0, subprocess.DEVNULL)
            seen = _read_until(second.stderr, b'waiting for output lock',
                               LOCK_PROBE_WAIT_TIMEOUT_SECONDS, 'second lock holder')
            waited = time.monotonic() - started
            if b'waiting for output lock' not in seen:
                raise CheckFailure(
                    'second holder never reported waiting: the output directory is '
                    f'not actually mutually exclusive (got {seen[-400:]!r})')
            # 看到「等锁」才放开第一个：第二个随后取到锁、读 stdin 见 EOF 退出。
            first.stdin.write(b'\n')
            first.stdin.flush()
            first.stdin.close()
            first_rc = first.wait(timeout=LOCK_PROBE_WAIT_TIMEOUT_SECONDS)
            second_rc = second.wait(timeout=LOCK_PROBE_WAIT_TIMEOUT_SECONDS)
        finally:
            for process in (first, second):
                if process is not None and process.poll() is None:
                    process.kill()
                    process.wait(timeout=30)
        if first_rc != 0:
            raise CheckFailure(f'first lock holder rc={first_rc}: '
                               f'{first.stderr.read()[:400]!r}')
        if second_rc != 0:
            raise CheckFailure(f'second lock holder rc={second_rc}: '
                               f'{second.stderr.read()[:400]!r}')

        # 敌意预置：锁路径上放 FIFO / 目录 / 指向别处的符号链接，都必须被拒。
        if LOCK_BASENAME_LITERAL not in CHECKER.read_text(encoding='utf-8'):
            raise CheckFailure(
                f'{CHECKER.name} no longer uses the lock basename '
                f'{LOCK_BASENAME_LITERAL!r}; update this control with it')
        hostile_rcs = []
        for label, plant in (
                ('planted FIFO', os.mkfifo),
                ('planted directory', os.mkdir),
                ('planted symlink', lambda path: os.symlink('/etc/hosts', path))):
            hostile = Path(tempfile.mkdtemp(prefix='pdrs-retirement-lock-bad-', dir=tmp))
            plant(hostile / LOCK_BASENAME_LITERAL)
            hostile_rcs.append(_assert_lock_path_rejected(hostile, label))
    return (f'second waited {waited:.2f}s for the output lock; '
            f'non-regular lock paths rejected with rc={hostile_rcs}')


JOB_HEADER = re.compile(r'^  ([\w-]+):\s*$')
STEP_ITEM = re.compile(r'^      - +(\S.*)$')
# 按**缩进**取键，不按「哪一行含某个词」取：job 的直属键在 4 空格，step 的直属键在
# 8 空格（`      - ` 之后）。`env: {run: exit 1}` 里的 run 不是这一步执行的脚本，
# `env.UNUSED: |` 里的命令也不是——只有 step 直属的那个 `run` 会被 Actions 执行。
JOB_KEY = re.compile(r'^ {4}([A-Za-z_][\w-]*)\s*:\s*(.*)$')
STEP_KEY = re.compile(r'''^ {8}([A-Za-z_][\w-]*|'[^']+'|"[^"]+")\s*:\s*(.*)$''')
BLOCK_SCALARS = ('|', '>', '|-', '>-', '|+', '>+')
RETIREMENT_JOB = 'retirement-outcome-parity'
AGGREGATE_JOB = 'check'
REQUIRE_STEP_NAME = 'Require retirement outcome-parity success'
REQUIRE_RUN = 'exit 1'
# 两个 job 的直属键白名单：出现 `continue-on-error`（job 级，4 空格缩进，此前完全
# 扫不到）、`env`、`defaults` 等任何未登记键即红。job 级 `continue-on-error: true`
# 会让六个闸门 step 全红而 job 仍 success。
RETIREMENT_JOB_KEYS = frozenset({'needs', 'if', 'runs-on', 'timeout-minutes', 'steps'})
AGGREGATE_JOB_KEYS = frozenset({'needs', 'if', 'runs-on', 'steps'})
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
# 闸门 step 的 `run` 体必须与这里**逐字相等**（去缩进后逐行比较）。整行 `needle`
# 只能证明「文本还在」：`echo <命令>`、`<命令> || true`、外层包一层 `if false; then … fi`、
# 或者把命令整行挪进同一个 step 的 `env.UNUSED: |`，文本都还在而闸门已经失效。
# 逐字相等把这些形态一次性挡掉，且不会误拒（不靠猜哪些 token 危险）。
GATE_SCRIPTS = {
    'emit 复算与金样逐字节比对': (
        'F=scripts/retirement-outcome-parity/fixtures',
        'python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py emit \\',
        '  --oracle "$RUNNER_TEMP/ts-oracle.json" \\',
        '  --round "$F/round-01" --round "$F/round-02" --round "$F/round-03" \\',
        '  --round "$F/round-04" --round "$F/round-05"',
        'cmp "$RUNNER_TEMP/ts-oracle.json" '
        'scripts/retirement-outcome-parity/ts-oracle.json',
    ),
    'check 5 轮': (
        'F=scripts/retirement-outcome-parity/fixtures',
        'python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py check \\',
        '  --oracle scripts/retirement-outcome-parity/ts-oracle.json \\',
        '  --round "$F/round-01" --round "$F/round-02" --round "$F/round-03" \\',
        '  --round "$F/round-04" --round "$F/round-05"',
    ),
    '负控 N1–N17 全 KILLED': (
        'python3 -I scripts/retirement-outcome-parity/negative_controls.py \\',
        '  --output "$RUNNER_TEMP/retirement-outcome-parity-negctl"',
    ),
    '装置守卫自检（六项）': (
        'python3 -I scripts/retirement-outcome-parity/self_check.py',
    ),
    '接线守卫变异自检': (
        'python3 -I scripts/retirement-outcome-parity/self_check.py --self-check-controls',
    ),
}
# retirement job 的 step 列表按**顺序**登记：删一个闸门 step、插一个 `run: exit 0`
# 的 step、或把两个 step 调换，都会让「登记的命令」与实际执行的不再对应。
RETIREMENT_STEP_ORDER = (
    ('uses', 'actions/checkout@v4'),
    ('gate', 'emit 复算与金样逐字节比对'),
    ('gate', 'check 5 轮'),
    ('gate', '负控 N1–N17 全 KILLED'),
    ('gate', '装置守卫自检（六项）'),
    ('gate', '接线守卫变异自检'),
    ('uses', '上传负控证据'),
)
# 非闸门 step 必须是 action：action step 不执行 shell，吞不掉闸门结果。
USE_STEP_KEYS = frozenset({'name', 'uses', 'if', 'with'})


def _indent_of(line):
    return len(line) - len(line.lstrip(' '))


def _step_entries(path, step, label):
    """step 的直属键 → [[键, 行内值, 缩进更深的子行], …]。

    首行 `      - name: X` 归一化成 8 空格缩进，此后 8 空格 = 直属键、>8 = 上一条键
    的值（块标量的脚本行）。<8 说明这个 step 的块已经结束，属解析错误而不是「没看见」。
    """
    header = step[0]
    item = STEP_ITEM.match(header)
    if item is None:
        raise CheckFailure(f'{path}: step `{label}` has no `- ` item header')
    entries = []
    for line in [' ' * 8 + header[item.start(1):], *step[1:]]:
        if not line.strip():
            continue
        indent = _indent_of(line)
        if indent > 8:
            if not entries:
                raise CheckFailure(
                    f'{path}: step `{label}` starts with an indented line {line!r}')
            entries[-1][2].append(line)
            continue
        if indent < 8:
            raise CheckFailure(
                f'{path}: step `{label}` has a line at indent {indent}: {line!r}')
        match = STEP_KEY.match(line)
        if match is None:
            raise CheckFailure(f'{path}: step `{label}` has an unparseable key {line!r}')
        entries.append([match.group(1).strip('\'"'), match.group(2).strip(), []])
    return entries


def _step_label(entries):
    """step 的显示名：`name` 优先，其次 `uses`。"""
    for key in ('name', 'uses'):
        for entry_key, inline, _ in entries:
            if entry_key == key:
                return inline
    return None


def _script_lines(path, label, entries):
    """step **直属**的 `run` 看到的多行脚本文本（去缩进、去空行）。"""
    runs = [entry for entry in entries if entry[0] == 'run']
    if len(runs) != 1:
        raise CheckFailure(
            f'{path}: `{label}` must have exactly one top-level `run:` ({len(runs)} '
            'found); a nested or duplicated `run` is not what the runner executes')
    _, inline, body = runs[0]
    if inline in BLOCK_SCALARS:
        live = [line for line in body if line.strip()]
        if not live:
            raise CheckFailure(f'{path}: `{label}` has an empty `run: {inline}` body')
        base = min(_indent_of(line) for line in live)
        return [line[base:].rstrip() for line in live]
    if not inline:
        raise CheckFailure(f'{path}: `{label}` has an empty `run:`')
    return [inline]


def _job_entries(path, block, name):
    entries = []
    for line in block:
        if not line.strip() or _indent_of(line) != 4:
            continue
        match = JOB_KEY.match(line)
        if match is None:
            raise CheckFailure(f'{path}: cannot parse a `{name}` job key from {line!r}')
        entries.append((match.group(1), match.group(2).strip()))
    return entries


def _require_job_keys(path, block, name, allowed):
    for key, _ in _job_entries(path, block, name):
        if key not in allowed:
            raise CheckFailure(
                f'{path}: `{name}` job carries the unregistered key `{key}`; a job-level '
                '`continue-on-error` / `env` / `defaults` can change or swallow the gate '
                'without touching any step')


def _normalised_expression(condition):
    return ' '.join(condition.removeprefix('${{').removesuffix('}}').split())


def _needs_list(value):
    """`needs: [a, b]` 或 `needs: a` → 逐项列表（不做子串匹配）。"""
    inner = value.strip()
    if inner.startswith('[') and inner.endswith(']'):
        return [item.strip() for item in inner[1:-1].split(',') if item.strip()]
    return [inner] if inner else []


def _live_lines(text):
    """去掉整行注释后的行。YAML 的 `#` 注释里的文本不是活的配置。

    不做子串匹配就分不清「活的 YAML」与「被注释/禁用掉的文本」：把整个 job 连同
    `needs:` 条目和 Require 步骤一起注释掉，needle 仍作为子串存在，接线守卫会假 PASS
    而闸门已被静默拆掉。
    """
    return [line for line in text.splitlines()
            if not line.lstrip().startswith('#')]


def _job_block(lines, name):
    start = next((index for index, line in enumerate(lines)
                  if (match := JOB_HEADER.match(line)) and match[1] == name), None)
    if start is None:
        raise CheckFailure(f'no `{name}:` job in the workflow')
    end = next((index for index in range(start + 1, len(lines))
                if JOB_HEADER.match(lines[index])), len(lines))
    return lines[start:end]


def _steps(block):
    """把 job 块切成 step 列表（每项含该 step 的全部行）。"""
    starts = [index for index, line in enumerate(block) if STEP_ITEM.match(line)]
    return [block[start:(starts[position + 1] if position + 1 < len(starts)
                      else len(block))]
            for position, start in enumerate(starts)]


def check_wiring(workflow=None):
    """CI 里的闸门必须**按登记的样子**接着，而不是「文本还在」。

    只认活配置（整行注释先剥掉），然后按 YAML 缩进取真值逐字比对：

    - 两个 job 的直属键必须在白名单内（job 级 `continue-on-error` 会让全红的 job 仍 success）；
    - retirement job 的 step 顺序、闸门 step 的名字、直属键集合（只有 `name` + 顶层 `run`）
      必须与登记一致——`env: {run: …}` / `env.UNUSED: |` / `if:` / `with:` 一律判红；
    - 每个闸门 step 的 `run` 体必须与 `GATE_SCRIPTS` 逐字相等——`echo` 前缀、`|| true`
      后缀、外层 `if false; then … fi`、多一行 `set +e` 全部逐字不等即红；
    - 聚合 `check` job 依赖 retirement job，它的 Require step 只有 `name`/`if`/`run`
      三个键，`run` 恰为 `exit 1`，`if:` 逐字等于登记的恒执行条件。

    任一条不满足，闸门要么不执行、要么失败了不报，job 结果仍是 success，聚合闸门就放行了。
    """
    path = Path(workflow or WORKFLOW)
    try:
        text = path.read_text(encoding='utf-8')
    except OSError as error:
        raise CheckFailure(f'cannot read {path}: {error}') from error
    lines = _live_lines(text)
    if any(line.rstrip() == '---' or line.startswith('%YAML') for line in lines):
        raise CheckFailure(
            f'{path}: YAML document separators / directives are not accepted; the runner '
            'rejects a multi-document workflow, and the guard would only read the first one')

    retirement = _job_block(lines, RETIREMENT_JOB)
    _require_job_keys(path, retirement, RETIREMENT_JOB, RETIREMENT_JOB_KEYS)
    retirement_keys = dict(_job_entries(path, retirement, RETIREMENT_JOB))
    if 'scope' not in _needs_list(retirement_keys.get('needs', '')):
        raise CheckFailure(
            f'{path}: `{RETIREMENT_JOB}` must still need `scope` (docs-only classifier)')
    job_condition = _normalised_expression(
        retirement_keys.get('if', ''))
    expected_job_condition = _normalised_expression(RETIREMENT_JOB_CONDITION)
    if job_condition != expected_job_condition:
        raise CheckFailure(
            f'{path}: `{RETIREMENT_JOB}` job condition is `{job_condition}` but must be '
            f'exactly `{expected_job_condition}`; any other condition can skip the job '
            'on PRs the classifier does not mark docs-only')

    steps = _steps(retirement)
    if len(steps) != len(RETIREMENT_STEP_ORDER):
        raise CheckFailure(
            f'{path}: `{RETIREMENT_JOB}` must keep exactly {len(RETIREMENT_STEP_ORDER)} '
            f'steps (found {len(steps)}); adding, dropping or reordering steps changes '
            'which script actually runs')
    for step, (kind, wanted) in zip(steps, RETIREMENT_STEP_ORDER):
        entries = _step_entries(path, step, wanted)
        label = _step_label(entries)
        if label != wanted:
            raise CheckFailure(
                f'{path}: expected the `{wanted}` step at this position, found `{label}`')
        keys = [entry[0] for entry in entries]
        if kind == 'gate':
            if set(keys) != {'name', 'run'} or len(keys) != 2:
                raise CheckFailure(
                    f'{path}: gate step `{label}` may only carry `name` and one top-level '
                    f'`run`, found {keys}; `env` / `with` / `if` / `continue-on-error` '
                    'can disable the step or replace what it executes')
            script = _script_lines(path, label, entries)
            expected = list(GATE_SCRIPTS[label])
            if script != expected:
                first = next((f'line {number}: {got!r} != {want!r}'
                              for number, (got, want)
                              in enumerate(itertools.zip_longest(script, expected,
                                                                 fillvalue='<missing>'), 1)
                              if got != want), 'length differs')
                raise CheckFailure(
                    f'{path}: gate step `{label}` does not run the registered script '
                    f'({first}); the script is pinned verbatim, so an `echo` prefix, a '
                    '`|| true` / `! ` suffix, an `if false; then … fi` wrapper or an '
                    'extra line all change the text')
        else:
            unknown = set(keys) - USE_STEP_KEYS
            if unknown or 'uses' not in keys:
                raise CheckFailure(
                    f'{path}: step `{label}` must stay an action (`uses:`), found {keys}')

    # 聚合闸门那一侧同样要守：Require step 被 `if: false` 挡掉、条件被改弱、或
    # `run: exit 0` 时，retirement job 照跑照过，job 结果仍是 success。
    aggregate = _job_block(lines, AGGREGATE_JOB)
    _require_job_keys(path, aggregate, AGGREGATE_JOB, AGGREGATE_JOB_KEYS)
    aggregate_keys = dict(_job_entries(path, aggregate, AGGREGATE_JOB))
    aggregate_condition = _normalised_expression(aggregate_keys.get('if', ''))
    expected_aggregate_condition = _normalised_expression(AGGREGATE_JOB_CONDITION)
    if aggregate_condition != expected_aggregate_condition:
        raise CheckFailure(
            f'{path}: `{AGGREGATE_JOB}` job condition is `{aggregate_condition}` but must '
            f'be exactly `{expected_aggregate_condition}`; a skipped required job is '
            "reported as success by GitHub, so nothing else would block the merge")
    if RETIREMENT_JOB not in _needs_list(aggregate_keys.get('needs', '')):
        raise CheckFailure(
            f'{path}: `{AGGREGATE_JOB}` no longer needs `{RETIREMENT_JOB}`; without the '
            'dependency the Require step cannot see the gate result')
    require = None
    for step in _steps(aggregate):
        entries = _step_entries(path, step, REQUIRE_STEP_NAME)
        if _step_label(entries) == REQUIRE_STEP_NAME:
            require = entries
            break
    if require is None:
        raise CheckFailure(f'{path}: `{AGGREGATE_JOB}` has no retirement Require step')
    keys = [entry[0] for entry in require]
    if set(keys) != {'name', 'if', 'run'} or len(keys) != 3:
        raise CheckFailure(
            f'{path}: the aggregate Require step may only carry `name`, `if` and `run`, '
            f'found {keys}; an extra key can swallow the failure')
    script = _script_lines(path, REQUIRE_STEP_NAME, require)
    if script != [REQUIRE_RUN]:
        raise CheckFailure(
            f'{path}: the aggregate Require step must run exactly `{REQUIRE_RUN}`, '
            f'found {script}; anything else lets a failed retirement job pass')
    condition = next(inline for key, inline, _ in require if key == 'if')
    expression = _normalised_expression(condition)
    if expression != REQUIRE_CONDITION:
        raise CheckFailure(
            f'{path}: the aggregate Require step condition is `{expression}` but must '
            f'be exactly `{REQUIRE_CONDITION}`; a weakened condition (e.g. a trailing '
            '`&& false`) silently disables the gate')
    return (f'{len(RETIREMENT_STEP_ORDER)} registered steps + aggregate Require step '
            f'live in {path.name}')


CHECKS = {
    'budget': check_budget,
    'emit': check_emit,
    'check': check_check,
    'negctl': check_negctl,
    'lock': check_lock,
    'wiring': check_wiring,
}
ORDER = ('budget', 'emit', 'check', 'negctl', 'lock', 'wiring')


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
    """给夹具造一个超大文件：体积守卫必须报红。

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
        except CheckFailure:
            return 'oversized fixture is rejected'
    raise CheckFailure('budget guard did not reject an oversized fixture '
                       '(the size limit was removed)')


def control_lock():
    """锁守卫的两个方向都要能红：并发不串行、敌意锁文件被接受。

    * 串行性没有独立违反样本：把 `output_lock` 里的 `flock` 删掉，`check_lock`
      行为上就会红（第二个不再打「等锁」），所以重跑正向检查就是变异证据。
    * 敌意锁文件**有**违反样本：`check_lock` 后半段真的预置 FIFO / 目录 / 符号
      链接，去掉 `O_NONBLOCK` 或 `S_ISREG` 判定就会挂起 / 假通过 → 本控件红。
    """
    detail = check_lock()
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


def wiring_samples():
    """所有接线变异样本 `[(名字, 变异后的 workflow 文本)]`。

    覆盖：删整个 job / 整段注释 / step 级三种挡法（`if: false`、引号键 `"if"`
    `continue-on-error`）/ **job 级** `continue-on-error`（4 空格缩进）/ 命令整行挪进
    `env.UNUSED: |` / 嵌套 `env: {run: …}` 冒充 Require 的执行体 / 条件块 `if false; then`
    包住整条命令 / 三种 shell 级抑制（`|| true`、`echo` 前缀、`! ` 取反退出码）/
    聚合 Require 的五种改法（禁用、改弱、忽略失败、追加 `&& false`、`exit 0`）/
    删掉一个闸门 step。

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
    # `echo disabled`——按「文件里有没有这一行」判的守卫会假 PASS。
    controls_start, controls_end = _step_range(lines, '接线守卫变异自检')
    samples.append(('commands-moved-to-env', ''.join(
        lines[:controls_start]
        + ['      - name: 接线守卫变异自检\n',
           '        run: |\n',
           '          echo disabled\n',
           '        env:\n',
           '          UNUSED: |\n']
        + ['    ' + line for line in lines[controls_start + 2:controls_end]]
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
    return samples


def control_wiring():
    """`wiring_samples()` 的每一条都必须让接线守卫变红。"""
    samples = wiring_samples()
    survived = []
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-wiring-') as tmp:
        for label, mutated in samples:
            candidate = Path(tmp) / f'ci-{label}.yml'
            candidate.write_text(mutated, encoding='utf-8')
            try:
                check_wiring(candidate)
            except CheckFailure:
                continue
            survived.append(label)
    if survived:
        raise CheckFailure('wiring guard did not reject: '
                           f'{survived} (the CI wiring check was weakened)')
    return (f'{len(samples)} mutated workflows '
            f'({", ".join(s for s, _ in samples)}) rejected')


CONTROLS_FOR_CHECK = {
    'budget': control_budget,
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


if __name__ == '__main__':
    sys.exit(main())
