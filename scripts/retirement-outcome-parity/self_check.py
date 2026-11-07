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
    '装置守卫自检（六项）':
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
    ('gate', '装置守卫自检（六项）'),
    ('gate', '接线守卫变异自检'),
    ('uses', '上传负控证据'),
)
# 非闸门 step 必须是 action：action step 不执行 shell，吞不掉闸门结果。
USE_STEP_KEYS = frozenset({'name', 'uses', 'if', 'with'})
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


def check_wiring(workflow=None):
    """CI 里的闸门必须**按登记的样子**接着（PyYAML 完整标量逐字比对）。

    用 YAML 解析器取**解析后的完整标量值**比对，不做任何行式 / 缩进式文本切块：

    - 根文档直属键必须在白名单内（workflow 级 `defaults.run.shell` 会替换每个 run
      step 的 shell——全部闸门无条件 rc=0；根级 `env` 给所有 job 注入变量；评审 r6
      P0）；重复键即红——两个解析器可以对「哪个键活着」各执一词；
    - 只在 `jobs:` 映射里找两个 job（挂在 `on:` 等别处的同名块不是 job）；
    - 两个 job 的直属键必须在白名单内（job 级 `continue-on-error` 会让全红的 job 仍
      success）；重复键即红——两个解析器可以对「哪个键活着」各执一词；
    - retirement job 的 step 顺序、闸门 step 的名字、直属键集合（只有 `name` + 一个
      `run`）必须与登记一致——`env: {run: …}` / `env.UNUSED: |` / `if:` /
      `continue-on-error` 一律判红；
    - 每个闸门 step 的 `run` 必须是 literal 块标量（`|`）且**完整标量值**与
      `GATE_SCRIPTS` 逐字相等——续行拼接（plain 多行标量）、`>` 折叠块、引号标量
      **显式判红并写明形态**；`echo` 前缀、`|| true` 后缀、外层 `if false; then … fi`、
      多一行 `set +e` 全部逐字不等即红；
    - 聚合 `check` job 依赖 retirement job，`if` 归一化后恰为 `always()`（GitHub 把
      被跳过的 job 报成 success，而 `check` 自己是必需检查）；Require step 只有
      `name` / `if` / `run` 三个键，`run` 恰为 `exit 1`，`if` 归一化后逐字等于登记的
      恒执行条件。

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
            if set(step) != {'name', 'run'}:
                raise CheckFailure(
                    f'{path}: {kind} step `{label}` may only carry `name` and one '
                    f'top-level `run`, found {sorted(step)}; `env` / `with` / `if` / '
                    '`continue-on-error` can disable the step or replace what it executes')
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
    if set(require) != {'name', 'if', 'run'}:
        raise CheckFailure(
            f'{path}: the aggregate Require step may only carry `name`, `if` and `run`, '
            f'found {sorted(require)}; an extra key can swallow the failure')
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
    return (f'{len(RETIREMENT_STEP_ORDER)} registered steps + aggregate Require step '
            f'live in {path.name} (PyYAML scalar-exact)')


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
    删掉一个闸门 step / r5 标量形态四种（run 行内续行、聚合 job `if` 续行、Require
    `if` 续行、`|`→`>` 折叠块）加 job 挪出 `jobs:`（行首正则会认 `on:` 下的假块）与
    **根级** `defaults.run.shell`（workflow 级 shell 模板让全部闸门无条件 rc=0，评审
    r6 P0）。

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
    fake_require_end = require_start + 3
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
    return samples


# 样本必须红在**点名的判定**上（评审 r5 P1 / r6）：改错行的样本只是把 YAML 弄坏被
# 「误杀」，KILLED 不能证明对应形态真的被守卫看见。键为样本名，值为该样本失败文案
# 必含的锚串——锚串落点同时证明变异体仍是可解析的单文档 YAML（否则红在解析失败）。
SAMPLE_FAILURE_ANCHORS = {
    'aggregate-job-if-continuation': '`check` job condition',
    'root-defaults-shell': 'unregistered root key',
}


def control_wiring():
    """`wiring_samples()` 的每一条都必须让接线守卫变红。"""
    samples = wiring_samples()
    survived = []
    misattributed = []
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-wiring-') as tmp:
        for label, mutated in samples:
            candidate = Path(tmp) / f'ci-{label}.yml'
            candidate.write_text(mutated, encoding='utf-8')
            try:
                check_wiring(candidate)
            except CheckFailure as error:
                anchor = SAMPLE_FAILURE_ANCHORS.get(label)
                if anchor is not None and anchor not in str(error):
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
