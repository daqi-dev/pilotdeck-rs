#!/usr/bin/env python3
"""retirement-outcome-parity 装置的自动闸门（#1809）。

把「装置自己还能不能用」变成可机检的一条命令，供 CI 与本地共用：

  budget   切片夹具的体积预算（单文件 < 1 MiB、总量 < 5 MiB，铁律 8 的入库上限）
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
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

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
LOCK_PROBE_HOLD_SECONDS = 2.0
LOCK_PROBE_MIN_WAIT_SECONDS = 1.0

# 子进程里持有输出目录锁的最小驱动（与 negative_controls 用**同一个** output_lock）。
LOCK_HOLDER = r'''
import importlib.util, sys, time
spec = importlib.util.spec_from_file_location('check_outcome_parity', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with module.output_lock(sys.argv[2]):
    print('LOCKED', flush=True)
    time.sleep(float(sys.argv[3]))
'''


class CheckFailure(Exception):
    """一条检查不通过（rc 1）。"""


def load_checker():
    import importlib.util
    spec = importlib.util.spec_from_file_location('check_outcome_parity', CHECKER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


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


def _spawn_lock_holder(root, hold_seconds):
    return subprocess.Popen(
        [sys.executable, '-I', '-c', LOCK_HOLDER, str(CHECKER), str(root),
         str(hold_seconds)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def check_lock():
    """两个进程抢同一输出目录：第二个必须等（不是静默并行写）。"""
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-lock-') as tmp:
        root = Path(tmp) / 'out'
        first = _spawn_lock_holder(root, LOCK_PROBE_HOLD_SECONDS)
        try:
            line = first.stdout.readline().strip()
            if line != 'LOCKED':
                raise CheckFailure(f'first lock holder did not report LOCKED: {line!r} '
                                   f'{first.stderr.read()[:400]}')
            started = time.monotonic()
            second = subprocess.run(
                [sys.executable, '-I', '-c', LOCK_HOLDER, str(CHECKER), str(root), '0'],
                capture_output=True, text=True, timeout=60, check=False)
            waited = time.monotonic() - started
        finally:
            first.wait(timeout=60)
        if second.returncode != 0:
            raise CheckFailure(f'second lock holder rc={second.returncode}: '
                               f'{second.stderr.strip()[:400]}')
        if 'waiting for output lock' not in second.stderr:
            raise CheckFailure('second holder never reported waiting: the output '
                               'directory is not actually mutually exclusive')
        if waited < LOCK_PROBE_MIN_WAIT_SECONDS:
            raise CheckFailure(f'second holder returned after {waited:.2f}s '
                               f'(< {LOCK_PROBE_MIN_WAIT_SECONDS}s): it did not wait')
    return f'second waited {waited:.2f}s for the output lock'


def check_wiring(workflow=None):
    """CI 里这几件事必须真的接着：job 存在、三步都在、`check` 依赖它。"""
    path = Path(workflow or WORKFLOW)
    try:
        text = path.read_text(encoding='utf-8')
    except OSError as error:
        raise CheckFailure(f'cannot read {path}: {error}') from error
    required = (
        ('job', 'retirement-outcome-parity:'),
        ('emit step', 'check_outcome_parity.py emit'),
        ('check step', 'check_outcome_parity.py check'),
        ('negative controls step', 'retirement-outcome-parity/negative_controls.py'),
        ('self-check step', 'retirement-outcome-parity/self_check.py'),
        ('aggregate needs', 'negative-controls, retirement-outcome-parity]'),
        ('aggregate require step', 'Require retirement outcome-parity success'),
    )
    missing = [label for label, needle in required if needle not in text]
    if missing:
        raise CheckFailure(f'{path} is missing the retirement gate: {missing}')
    return f'{len(required)} wiring anchors present in {path.name}'


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
    """给夹具造一个超大文件：体积守卫必须报红。"""
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-budget-') as tmp:
        root = Path(tmp) / 'fixtures'
        shutil.copytree(FIXTURES, root)
        oversized = root / 'round-01' / 'driver' / 'oversized.bin'
        with open(oversized, 'wb') as handle:
            handle.seek(FIXTURE_FILE_LIMIT_BYTES)  # 空心情景下 seek 后写入 1 字节
            handle.write(b'\0')
        try:
            check_budget(root)
        except CheckFailure:
            return 'oversized fixture is rejected'
    raise CheckFailure('budget guard did not reject an oversized fixture '
                       '(the size limit was removed)')


def control_lock():
    """两个持锁进程必须串行；守卫被拆掉时第二个不再等待，本控件即红。"""
    detail = check_lock()
    return detail


def control_wiring():
    """把 CI 里的 retirement 步骤删掉：接线守卫必须报红。"""
    text = WORKFLOW.read_text(encoding='utf-8')
    original = text.splitlines(keepends=True)
    lines = [line for line in original if 'outcome-parity' not in line.lower()]
    if len(lines) == len(original):
        raise CheckFailure('CI workflow has no retirement anchor to remove')
    with tempfile.TemporaryDirectory(prefix='pdrs-retirement-ctl-wiring-') as tmp:
        stripped = Path(tmp) / 'ci.yml'
        stripped.write_text(''.join(lines), encoding='utf-8')
        try:
            check_wiring(stripped)
        except CheckFailure:
            return 'stripped workflow is rejected'
    raise CheckFailure('wiring guard did not reject a workflow with the '
                       'retirement steps removed')


CONTROLS_FOR_CHECK = {
    'budget': control_budget,
    'lock': control_lock,
    'wiring': control_wiring,
}


def run_self_check_controls():
    failures = []
    for name in ('budget', 'lock', 'wiring'):
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
        return run_self_check_controls()
    return run_checks(args.only or ORDER)


if __name__ == '__main__':
    sys.exit(main())
