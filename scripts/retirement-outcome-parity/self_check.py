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
# YAML 允许引号键（`"if": false` 与 `if: false` 等价），缩进也可能不是恰好 8 空格；
# 只认「键名叫 if」会漏掉这两种写法，而漏掉一次就是闸门被跳过而 job 仍 success。
STEP_IF = re.compile(r'''^ {8,}(?:if|'if'|"if")\s*:\s*(.*)$''')
STEP_IGNORE_FAILURE = re.compile(
    r'''^ {8,}(?:continue-on-error|'continue-on-error'|"continue-on-error")\s*:''')
RETIREMENT_JOB = 'retirement-outcome-parity'
AGGREGATE_JOB = 'check'
REQUIRE_STEP_NAME = 'Require retirement outcome-parity success'
REQUIRE_RUN = 'run: exit 1'
# 聚合闸门的 Require step 本来就该带 `always()` 条件，所以不能像 job 内 step 那样
# 一律拒 `if:`；要求它**恰好**是这一条（归一化空白后逐字相等）——只核前缀 + 子串
# 挡不住在末尾追加 `&& false`（条件恒假 → step 从不执行，job 仍 success）。
REQUIRE_CONDITION = (
    f"always() && needs.{RETIREMENT_JOB}.result != 'success' && "
    f"!(needs.{RETIREMENT_JOB}.result == 'skipped' && "
    "needs.scope.outputs.docs_only == 'true')")
# 闸门命令必须是**独立的整行**：`echo <命令>`（只打印不跑）与 `<命令> || true`
# （失败被吞）都能让子串 needle 照样命中而闸门实际已失效。
STEP_NEEDLES = (
    ('emit step',
     'python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py emit \\'),
    ('check step',
     'python3 -I scripts/retirement-outcome-parity/check_outcome_parity.py check \\'),
    ('negative controls step',
     'python3 -I scripts/retirement-outcome-parity/negative_controls.py \\'),
    ('self-check step',
     'python3 -I scripts/retirement-outcome-parity/self_check.py'),
    ('self-check controls step',
     'python3 -I scripts/retirement-outcome-parity/self_check.py --self-check-controls'),
)
# run 块里的 shell 级抑制：任一条都能让「命令失败了」变成 step success。
RUN_TOKENS = ('||', 'set +e', 'set +o errexit', 'exit 0')
RUN_EXACT_LINES = ('true', ':')


def _step_conditions(step):
    return [match[1].strip() for match in (STEP_IF.match(line) for line in step)
            if match]


def _run_block(step):
    """step 的 `run:` 脚本行（`run: |` 之后的缩进行；单行 `run: cmd` 返回 [cmd]）。"""
    for index, line in enumerate(step):
        stripped = line.strip()
        if not (stripped == 'run:' or stripped.startswith('run: ')):
            continue
        inline = stripped[len('run:'):].strip()
        if inline in ('|', '>', '|-', '>-', '|+', '>+'):
            return [rest.strip() for rest in step[index + 1:] if rest.strip()]
        return [inline] if inline else []
    return []


def _reject_ignored_failure(path, step):
    if any(STEP_IGNORE_FAILURE.match(line) for line in step):
        raise CheckFailure(
            f'{path}: a retirement gate step sets `continue-on-error`; its failure '
            'would be ignored and the job would still be success')


def _reject_suppressed_run(path, step):
    """run 块里出现 shell 级抑制 → 命令失败也不影响 step 结果。"""
    for command in _run_block(step):
        for token in RUN_TOKENS:
            if token in command:
                raise CheckFailure(
                    f'{path}: a gate step neutralises its own exit status with '
                    f'`{token}` in `{command[:80]}`; the step would still succeed '
                    'when the gate fails')
        if command in RUN_EXACT_LINES:
            raise CheckFailure(
                f'{path}: a gate step runs the no-op `{command}`, which swallows the '
                'real gate command\'s failure')


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
    """CI 里这几件事必须真的接着：job 存在、三步都在、`check` 依赖它。

    只认**活配置**：整行注释先剥掉；闸门命令必须整行出现（`echo` / `|| true` 判红）；
    retirement job 的 step 不得带 `if:`（含引号键 `"if": false`）、`continue-on-error`
    或 run 块内的 shell 抑制；聚合 `check` job 的 Require step 必须仍以 `run: exit 1`
    结束、且 `if:` 逐字等于登记的恒执行条件——否则 step 不生效或失败了不报，job 结果
    仍是 success，聚合闸门就放行了。
    """
    path = Path(workflow or WORKFLOW)
    try:
        text = path.read_text(encoding='utf-8')
    except OSError as error:
        raise CheckFailure(f'cannot read {path}: {error}') from error
    lines = _live_lines(text)
    live = '\n'.join(lines)
    required = (
        ('job', f'{RETIREMENT_JOB}:'),
        *STEP_NEEDLES,
        ('aggregate needs', f'negative-controls, {RETIREMENT_JOB}]'),
        ('aggregate require step', REQUIRE_STEP_NAME),
    )
    missing = [label for label, needle in required if needle not in live]
    if missing:
        raise CheckFailure(f'{path} is missing the retirement gate: {missing}')
    # needle 只是子串；命令还必须**整行**出现，否则 `echo <命令>` 或 `<命令> || true`
    # 都能让 needle 命中而闸门根本没执行 / 失败了也不报。
    shell_lines = {line.strip() for line in lines}
    missing_commands = [needle for _, needle in STEP_NEEDLES if needle not in shell_lines]
    if missing_commands:
        raise CheckFailure(
            f'{path}: each gate command must be its own shell line (an `echo ` prefix '
            f'or a `|| true` suffix silently disables it): {missing_commands}')
    steps = _steps(_job_block(lines, RETIREMENT_JOB))
    if not steps:
        raise CheckFailure(f'{path}: `{RETIREMENT_JOB}` has no steps')
    for step in steps:
        gate = '\n'.join(step)
        if not any(needle in gate for _, needle in STEP_NEEDLES):
            continue
        conditions = _step_conditions(step)
        if conditions:
            raise CheckFailure(
                f'{path}: a retirement gate step is guarded by `if: {conditions[0]}`; '
                'a skipped step still leaves the job success, so the gate would be '
                'silently disabled')
        _reject_ignored_failure(path, step)
        _reject_suppressed_run(path, step)
    # 聚合闸门那一侧同样要守：`check` job 的 Require step 被 `if: false` 挡掉时，
    # retirement job 照跑照过、needle 照命中，只扫本 job 的 step 会假 PASS。
    require = next((step for step in _steps(_job_block(lines, AGGREGATE_JOB))
                    if any(REQUIRE_STEP_NAME in line for line in step)), None)
    if require is None:
        raise CheckFailure(f'{path}: `{AGGREGATE_JOB}` has no retirement Require step')
    _reject_ignored_failure(path, require)
    _reject_suppressed_run(path, require)
    if not any(line.strip() == REQUIRE_RUN for line in require):
        raise CheckFailure(
            f'{path}: the aggregate Require step no longer ends in `{REQUIRE_RUN}`; '
            'it would run and still succeed, so a failed retirement job would not '
            'block the merge')
    conditions = _step_conditions(require)
    if len(conditions) != 1:
        raise CheckFailure(
            f'{path}: the aggregate Require step must carry exactly one `if:` '
            f'({len(conditions)} found); without it the gate is unconditional or '
            'unspecified')
    expression = ' '.join(
        conditions[0].removeprefix('${{').removesuffix('}}').split())
    if expression != REQUIRE_CONDITION:
        raise CheckFailure(
            f'{path}: the aggregate Require step condition is `{expression}` but must '
            f'be exactly `{REQUIRE_CONDITION}`; a weakened condition (e.g. a trailing '
            '`&& false`) silently disables the gate')
    return f'{len(required)} wiring anchors live in {path.name}'


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


def control_wiring():
    """十三种拆法都必须让接线守卫变红：删行、整段注释、job 内 step 四种挡法、
    聚合 Require step 五种挡法、shell 级三种抑制。"""
    text = WORKFLOW.read_text(encoding='utf-8')
    original = text.splitlines(keepends=True)
    samples = []

    stripped = [line for line in original if 'outcome-parity' not in line.lower()]
    if len(stripped) == len(original):
        raise CheckFailure('CI workflow has no retirement anchor to remove')
    samples.append(('removed', ''.join(stripped)))

    # 整段注释：job 块连同聚合 `needs` 条目与 Require 步骤一起注掉，needle 仍作为
    # 子串躺在文本里——只有「注释不算活配置」才能抓到。
    raw = text.splitlines(keepends=True)
    job_start = next((index for index, line in enumerate(raw)
                      if line.rstrip('\n') == f'  {RETIREMENT_JOB}:'), None)
    if job_start is None:
        raise CheckFailure('CI workflow has no retirement job to comment out')
    job_end = next((index for index in range(job_start + 1, len(raw))
                    if JOB_HEADER.match(raw[index].rstrip('\n'))), len(raw))
    dead = set(range(job_start, job_end))
    require_start = next((index for index, line in enumerate(raw)
                          if 'Require retirement outcome-parity success' in line), None)
    if require_start is None:
        raise CheckFailure('CI workflow has no Require step to comment out')
    require_end = next((index for index in range(require_start + 1, len(raw))
                        if STEP_ITEM.match(raw[index])
                        or JOB_HEADER.match(raw[index].rstrip('\n'))), len(raw))
    dead |= set(range(require_start, require_end))
    dead |= {index for index, line in enumerate(raw)
             if f'{RETIREMENT_JOB}]' in line}
    commented = ['# ' + line if index in dead else line
                 for index, line in enumerate(raw)]
    samples.append(('commented-out', ''.join(commented)))

    # step 级 `if: false` / 引号键 `"if": false` / `continue-on-error`：三种都能让闸门
    # step 不生效，而 job 结果仍是 success → 聚合闸门放行。
    anchor = '      - name: check 5 轮\n'
    if text.count(anchor) != 1:
        raise CheckFailure(f'CI workflow has no unique anchor to disable: {anchor!r}')
    for label, injected in (
            ('disabled-step', '        if: false\n'),
            ('quoted-if-step', '        "if": false\n'),
            ('continue-on-error-step', '        continue-on-error: true\n')):
        samples.append((label, text.replace(anchor, anchor + injected, 1)))
    # `set +e` 不改变命令行本身（整行比对照样命中），只能靠 run 块扫描抓到。
    run_anchor = anchor + '        run: |\n'
    if text.count(run_anchor) != 1:
        raise CheckFailure(f'CI workflow has no unique run block to weaken: {run_anchor!r}')
    samples.append(('set-plus-e-step',
                    text.replace(run_anchor, run_anchor + '          set +e\n', 1)))

    # 聚合闸门那一侧的同型拆法：Require step 被禁用 / 条件被改弱 / 失败被忽略，
    # retirement job 本身照跑照过，只扫本 job step 的守卫会假 PASS。
    lines = text.splitlines(keepends=True)
    require_start = next((index for index, line in enumerate(lines)
                          if line.rstrip('\n') == f'      - name: {REQUIRE_STEP_NAME}'),
                         None)
    if require_start is None:
        raise CheckFailure(
            f'CI workflow has no Require step to disable: {REQUIRE_STEP_NAME!r}')
    require_end = next((index for index in range(require_start + 1, len(lines))
                        if STEP_ITEM.match(lines[index])
                        or JOB_HEADER.match(lines[index].rstrip('\n'))), len(lines))
    if_index = next((index for index in range(require_start, require_end)
                     if STEP_IF.match(lines[index])), None)
    if if_index is None:
        raise CheckFailure('the aggregate Require step has no `if:` line to weaken')
    for label, replacement in (
            ('disabled-require-step', '        if: false\n'),
            ('weakened-require-step',
             "        if: ${{ needs.retirement-outcome-parity.result == 'failure' }}\n")):
        mutated = list(lines)
        mutated[if_index] = replacement
        samples.append((label, ''.join(mutated)))
    ignored = list(lines)
    ignored.insert(require_start + 1, '        continue-on-error: true\n')
    samples.append(('continue-on-error-require-step', ''.join(ignored)))

    # shell 级拆法：命令只打印不执行（`echo`）、失败被吞（`|| true`）、条件恒假
    # （`&& false`）、失败动作改成成功（`run: exit 0`）——四者都让 needle 照命中。
    check_command = STEP_NEEDLES[1][1]
    command_index = next((index for index, line in enumerate(lines)
                          if line.strip() == check_command), None)
    if command_index is None:
        raise CheckFailure(f'CI workflow has no `{check_command}` line to weaken')
    command_line = lines[command_index].rstrip('\n')
    command_indent = command_line[:len(command_line) - len(command_line.lstrip())]
    for label, replacement in (
            ('suppressed-command', command_line + ' || true\n'),
            ('echoed-command', command_indent + 'echo ' + check_command + '\n')):
        mutated = list(lines)
        mutated[command_index] = replacement
        samples.append((label, ''.join(mutated)))
    weakened_if = lines[if_index].rstrip('\n')
    if not weakened_if.endswith(' }}'):
        raise CheckFailure(f'the Require condition line does not end in ` }}}}`: '
                           f'{weakened_if!r}')
    samples.append(('and-false-require-step',
                    ''.join(lines[:if_index] + [weakened_if[:-3] + ' && false }}\n']
                            + lines[if_index + 1:])))
    run_index = next((index for index in range(require_start, require_end)
                      if lines[index].strip() == REQUIRE_RUN), None)
    if run_index is None:
        raise CheckFailure(f'the aggregate Require step has no `{REQUIRE_RUN}` line')
    exit_zero = list(lines)
    exit_zero[run_index] = '        run: exit 0\n'
    samples.append(('exit-zero-require-step', ''.join(exit_zero)))

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
    return f'{len(samples)} mutated workflows ({", ".join(s for s, _ in samples)}) rejected'


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
