#!/usr/bin/env python3
"""Post-commit source negative controls for #164.

This runner mutates the tracked apparatus source in place and requires the
behavior check to turn red for the specific mutation, not for an unrelated
error.  It must run only after the #164 changes are committed:
``mutating_sources`` proves the named file is byte-identical to its baseline
before writing anything, and refuses otherwise.  The new check/launcher files
beside this script are not mutated, so their pre-commit absence from HEAD does
not matter.

For a pre-commit local rehearsal the default ``--baseline HEAD`` can be pointed
at a tree object of the current worktree (``git stash create``); that creates a
dangling object only, never a branch commit.  The coordinator's host run uses
the default after applying the patch.

Run on the coordinator's host (no cargo is launched):

  python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py \
    --output _tmp/20261026-issue-164/negative/source

``--only NAME`` (repeatable) restricts the source mutations to the named rows;
the restored positive still runs.  ``MUTATIONS`` is the literal
``(name, source, old, new)`` table read by #820's anchor selfcheck, and
``JUDGES`` holds the mode/judge/launcher metadata keyed by mutation name.

Exit codes: 0 every mutant killed and the restored positive is PASS; 1 a
mutant survived, the restored source failed, or the restored positive only
passed with platform skips (PASS_WITH_SKIPS); 2 an anchor is stale (refuse to
mutate) or a case is INVALID/BLOCKED (environment/startup failure, never
counted as KILLED).  The top-level status and exit code come from the one
shared ``common.aggregate_status()``; the restored positive is a child too.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-session-cleanup-neg-pyc-')
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'scripts/gateway-parity/session_domain.py'
CONTROLS_SOURCE = ROOT / 'scripts/gateway-parity/controls/session_domain.py'
COMMON_SOURCE = ROOT / 'scripts/gateway-parity/common.py'
CHECK = Path(__file__).resolve().with_name('check_cleanup.py')
NEGATIVE = Path(__file__).resolve()
_spec = importlib.util.spec_from_file_location('session_cleanup_common', COMMON_SOURCE)
common = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(common)

# Batch verdict -> child status for the shared aggregator.  INVALID is an
# environment/startup failure: BLOCKED, never PASS and never counted as KILLED.
MUTATION_STATUS = {'KILLED': 'PASS', 'SURVIVED': 'FAIL', 'INVALID': 'BLOCKED'}

_MISSING = object()


def dig(row, path):
    current = row
    for part in path.split('.'):
        if not isinstance(current, dict):
            return _MISSING
        current = current.get(part, _MISSING)
    return current


def match(rows, mode, expected):
    """One row for *mode* with exactly the mutant's expected (wrong) fields."""
    matched = [row for row in rows if row.get('mode') == mode]
    if len(matched) != 1:
        return False, f'mode {mode}: expected 1 result row, saw {len(matched)}'
    row = matched[0]
    observed = {}
    for path, want in expected.items():
        got = dig(row, path)
        observed[path] = None if got is _MISSING else got
        if got != want:
            return False, f'{path}={observed[path]!r}, want {want!r}'
    if row.get('passed') is not False:
        return False, f"passed={row.get('passed')!r}, want False"
    return True, (f"{mode}: "
                  + ', '.join(f'{path}={observed[path]!r}' for path in expected)
                  + ', passed=False')


def status_mapping(rows):
    """The relaxed helper must turn both non-VERIFIED rows into PASS."""
    matched = [row for row in rows if row.get('mode') == 'status']
    by_state = {row.get('processTreeCleanup'): row for row in matched}
    for state in ('GROUP_NOT_EMPTY', 'BLOCKED'):
        row = by_state.get(state)
        if row is None:
            return False, f'status: missing {state} row'
        if row.get('observed') != 'PASS':
            return False, f'{state}: observed={row.get("observed")!r}, want PASS'
        if row.get('passed') is not False:
            return False, f'{state}: passed={row.get("passed")!r}, want False'
    return True, 'status: GROUP_NOT_EMPTY/BLOCKED -> observed=PASS, passed=False'


def blocked_branch_removed(rows):
    """Removing the generic BLOCKED branch raises instead of returning BLOCKED."""
    matched = [row for row in rows if row.get('mode') == 'blocked']
    if len(matched) != 1:
        return False, f'blocked: expected 1 result row, saw {len(matched)}'
    row = matched[0]
    error = row.get('error')
    if not (isinstance(error, str) and error.startswith('RuntimeError:')):
        return False, f'error={error!r}, want a RuntimeError from the removed BLOCKED branch'
    reported_cases = row.get('cases') or []
    if [case.get('case') for case in reported_cases] != ['permission-denied']:
        return False, f"cases={[case.get('case') for case in reported_cases]!r}, want ['permission-denied']"
    if row.get('passed') is not False:
        return False, f"passed={row.get('passed')!r}, want False"
    return True, f'blocked: error={error!r}, cases=[permission-denied], passed=False'


def wiring_relaxed(rows):
    """`overall = 'PASS'` must show up in the real report tail."""
    return match(rows, 'wiring', {
        'status': 'PASS',
        'executionStatus': 'PASS',
        'freezeStatus': 'DRAFT_AWAITING_A5_4',
        'returncode': 0,
    })


# (name, source, old, new).  Every old block must occur once.  Per-mutant
# judge metadata (mode, judge, uses_launcher, allowed_probe_errors) lives in
# JUDGES below: #820's anchor selfcheck reads literal replacement tables only,
# so callables stay out of MUTATIONS.
MUTATIONS = [
    ('empty-group-probe-removed', SOURCE,
     "    return 'empty', (f'killpg0=EPERM with the unreaped zombie leader present '\n"
     "                     f'and an empty live-member census: {detail}')",
     "    return 'alive', 'mutant: empty-group probe removed'"),
    ('cleanup-status-write-removed', SOURCE,
     "            audit['processTreeCleanup'] = audit['processGroupProbe']['status']",
     "            audit['processTreeCleanup'] = CLEANUP_VERIFIED"),
    ('top-level-mapping-relaxed', SOURCE,
     "    return ('PASS' if audit.get('processTreeCleanup') == CLEANUP_VERIFIED\n"
     "            else CLEANUP_UNVERIFIED_STATUS)",
     "    return 'PASS'"),
    ('sigterm-removed', SOURCE,
     "    for sig in (signal.SIGTERM, signal.SIGKILL):",
     "    for sig in (signal.SIGKILL,):"),
    ('blocked-branch-removed', SOURCE,
     "    if outcome == 'error':\n        return 'blocked', detail",
     "    if outcome == 'error':\n        raise  # mutant: remove the BLOCKED branch"),
    ('sigkill-escalation-removed', SOURCE,
     "    for sig in (signal.SIGTERM, signal.SIGKILL):",
     "    for sig in (signal.SIGTERM,):"),
    ('top-level-wiring-relaxed', SOURCE,
     "                overall = cleanup_status(audit)",
     "                overall = 'PASS'"),
    ('eperm-census-ignored', SOURCE,
     "    live = live_group_members(held.pid)",
     "    live = set()  # mutant: ignore the independent live-member census"),
    ('unavailable-census-as-empty', SOURCE,
     "        return 'blocked', ('killpg0=EPERM; independent group census unavailable '\n"
     "                           f'or incomplete: {detail}')",
     "        live = set()  # mutant: treat an unavailable census as empty"),
    ('reap-guard-removed', SOURCE,
     "        return self._signal(sig, group=False, states=('RUNNING',))",
     "        return self._signal(sig, group=False, states=('RUNNING', 'EXITED', 'REAPED'))"),
    ('snapshot-race-accepted', SOURCE,
     "        if first != second:\n"
     "            # Membership changed between complete snapshots: a descendant may\n"
     "            # exist that neither snapshot can vouch for.\n"
     "            continue",
     "        if first != second:\n            return first  # mutant: accept the first snapshot"),
    ('unstable-census-accepted', SOURCE,
     "        if first != second:\n"
     "            # Membership changed between complete snapshots: a descendant may\n"
     "            # exist that neither snapshot can vouch for.\n"
     "            continue",
     "        if first != second:\n            return second  # mutant: accept a changing census"),
    ('truncation-accepted', SOURCE,
     "            if written < capacity and written % 4 == 0:",
     "            if written <= capacity:  # mutant: accept a full (truncated) buffer"),
    ('enumeration-failure-empty', SOURCE,
     "        if first is None:\n            continue",
     "        if first is None:\n            return set()  # mutant: enumeration failure == empty"),
    ('signal-gate-removed', SOURCE,
     "        state = self._state()\n        if state not in states:\n            return 'refused', state",
     "        state = self._state()\n        # mutant: signal without proving ownership"),
    ('pid-blocked-as-gone', SOURCE,
     "        except ProcessLookupError:\n            return 'missing'\n"
     "        except OSError:\n            return 'blocked'",
     "        except ProcessLookupError:\n            return 'missing'\n"
     "        except OSError:\n            return 'missing'  # mutant: blocked == gone"),
    ('linux-visible-census-trusted', SOURCE,
     "    if sys.platform != 'darwin':\n",
     "    if False:  # mutant: trust the platform's visible process set\n"),
    ('batch-status-relaxed', CONTROLS_SOURCE,
     "    cleanup = cleanup_controls(output)\n"
     "    status, _ = common.aggregate_status(\n"
     "        'PASS' if all(r['passed'] for r in results) else 'FAIL',\n"
     "        'PASS_WITH_SKIPS' if not_run else None,\n"
     "        cleanup['status'])\n"
     "    report = {'status': status,\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n"
     "              'notRun': [{'case': case, 'reason': 'requires real calibration --source'} for case in not_run]}\n"
     "    report['cleanup'] = cleanup\n"
     "    report['cleanupSkipped'] = cleanup.get('skipped', [])",
     "    cleanup = cleanup_controls(output)\n"
     "    report = {'status': 'PASS' if all(r['passed'] for r in results) else 'FAIL',\n"
     "              'scope': 'apparatus mutation tests only', 'cases': results,\n"
     "              'notRun': [{'case': case, 'reason': 'requires real calibration --source'} for case in not_run]}\n"
     "    report['cleanup'] = cleanup\n"
     "    report['cleanupSkipped'] = cleanup.get('skipped', [])\n"
     "    if cleanup['status'] == 'FAIL':\n"
     "        report['status'] = 'FAIL'"),
    ('fixture-summary-relaxed', CHECK,
     "    if (malformed or not executed\n"
     "            or any(row.get('passed') is False for row in executed)):\n"
     "        return md.common.aggregate_status('FAIL')[0], skipped\n"
     "    status, _ = md.common.aggregate_status(\n"
     "        'PASS', 'PASS_WITH_SKIPS' if skipped else None)\n"
     "    return status, skipped",
     "    if not results or not all(row.get('passed') for row in results):\n"
     "        return 'FAIL', []\n"
     "    return 'PASS', []"),
    ('calibrate-controls-skip-accepted', SOURCE,
     "                if controls_status == 'PASS_WITH_SKIPS':\n"
     "                    # A skipped positive control is not proof of the batch.\n"
     "                    # Keep the skipped names visible and never report PASS:\n"
     "                    # BLOCKED / exit 2 is the apparatus' non-PASS convention.\n"
     "                    report['controlsSkipped'] = (\n"
     "                        report['e1Controls'].get('cleanupSkipped', [])\n"
     "                        + [row['case'] for row in report['e1Controls'].get('notRun', [])])\n"
     "                    report['failure'] = {\n"
     "                        'rule': 'CONTROLS_SKIPPED',\n"
     "                        'detail': 'E1 controls skipped platform-specific positives; not PASS'}\n"
     "                    overall = 'BLOCKED'",
     "                # mutant: accept a skipped controls batch as PASS"),
    ('aggregation-exit-bypassed', CONTROLS_SOURCE,
     "        return common.aggregate_status(report['status'])[1]",
     "        return int(report['status'] != 'PASS')"),
    ('restored-skip-accepted', NEGATIVE,
     "        top_status, exit_code = common.aggregate_status(\n"
     "            *(MUTATION_STATUS.get(entry['verdict'], 'FAIL') for entry in summary),\n"
     "            restored_summary.get('status') if restored_ok else 'FAIL')",
     "        top_status, exit_code = common.aggregate_status(\n"
     "            *(MUTATION_STATUS.get(entry['verdict'], 'FAIL') for entry in summary))"),
]

JUDGES = {
    'empty-group-probe-removed': (
        'clean',
        lambda rows: match(rows, 'clean', {
            'processTreeCleanup': 'GROUP_NOT_EMPTY',
            'probe.status': 'GROUP_NOT_EMPTY',
        }),
        True,
        ('signalError', 'probeError')),
    'cleanup-status-write-removed': (
        'leak',
        lambda rows: match(rows, 'leak', {
            'processTreeCleanup': 'VERIFIED',
            'probe.status': 'GROUP_NOT_EMPTY',
        }),
        True,
        ()),
    'top-level-mapping-relaxed': ('status', status_mapping, False, ()),
    'sigterm-removed': (
        'leak',
        lambda rows: match(rows, 'leak', {
            'processTreeCleanup': 'GROUP_NOT_EMPTY',
            'probe.signal': 'SIGKILL',
        }),
        True,
        ()),
    'blocked-branch-removed': ('blocked', blocked_branch_removed, False, ()),
    'sigkill-escalation-removed': (
        'ignore',
        lambda rows: match(rows, 'ignore', {
            'processTreeCleanup': 'GROUP_NOT_EMPTY',
            'probe.signal': 'SIGTERM',
            'probe.emptyAfterSignal': False,
        }),
        True,
        ()),
    'top-level-wiring-relaxed': ('wiring', wiring_relaxed, False, ()),
    'eperm-census-ignored': (
        'eperm-live-member',
        lambda rows: match(rows, 'eperm-live-member', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'unavailable-census-as-empty': (
        'census-unavailable',
        lambda rows: match(rows, 'census-unavailable', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'reap-guard-removed': ('reap-state', lambda rows: match(rows, 'reap-state', {'killCalls': 1}), False, ()),
    'snapshot-race-accepted': (
        'census-snapshot',
        lambda rows: match(rows, 'census-snapshot', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'unstable-census-accepted': (
        'census-unstable',
        lambda rows: match(rows, 'census-unstable', {'members': [900, 424245]}), False, ()),
    'truncation-accepted': (
        'census-truncated',
        lambda rows: match(rows, 'census-truncated', {'returnedPids': 1}), False, ()),
    'enumeration-failure-empty': (
        'census-enumeration',
        lambda rows: match(rows, 'census-enumeration', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'signal-gate-removed': (
        'signal-entry',
        lambda rows: match(rows, 'signal-entry', {
            'groupOutcome': 'denied',
            'leaderOutcome': 'denied',
            'killCalls': 2,
        }),
        False,
        ()),
    'pid-blocked-as-gone': (
        'pid-blocked',
        lambda rows: match(rows, 'pid-blocked', {
            'grandchildState': 'missing',
            'grandchildAlive': False,
            'grandchildGone': True,
        }),
        False,
        ()),
    'linux-visible-census-trusted': (
        'linux-filtered-proc',
        lambda rows: match(rows, 'linux-filtered-proc', {
            'processTreeCleanup': 'VERIFIED',
            'topLevel': 'PASS',
        }),
        False,
        ()),
    'batch-status-relaxed': (
        'batch-aggregation',
        lambda rows: match(rows, 'batch-aggregation', {
            'withSkips.status': 'PASS',
            'withSkips.returncode': 0,
            'blocked.status': 'PASS',
            'blocked.returncode': 0,
            'notRun.status': 'PASS',
            'notRun.returncode': 0,
        }),
        False,
        ()),
    'fixture-summary-relaxed': (
        'batch-aggregation',
        lambda rows: match(rows, 'batch-aggregation', {
            'summaries.withSkip': 'FAIL',
            'summaries.allSkipped': 'FAIL',
        }),
        False,
        ()),
    'calibrate-controls-skip-accepted': (
        'batch-aggregation',
        lambda rows: match(rows, 'batch-aggregation', {
            'calibrateSkipped.status': 'PASS',
            'calibrateSkipped.failureRule': None,
            'calibrateSkipped.returncode': 0,
        }),
        False,
        ()),
    'aggregation-exit-bypassed': (
        'aggregation-audit',
        lambda rows: match(rows, 'aggregation-audit', {
            'firstFailure': '#164-fix-r7 aggregation-audit: '
                            'scripts/gateway-parity/controls/session_domain.py:main '
                            'must call common.aggregate_status',
            'points.controls-main.shared': False,
        }),
        False,
        ()),
    'restored-skip-accepted': (
        'batch-aggregation',
        lambda rows: match(rows, 'batch-aggregation', {
            'negativeRestored.skipStatus': 'PASS',
            'negativeRestored.skipReturncode': 0,
        }),
        False,
        ()),
}
if set(JUDGES) != {row[0] for row in MUTATIONS}:
    raise SystemExit('JUDGES must name exactly the MUTATIONS rows')


def pgrep_launcher():
    try:
        process = subprocess.run(
            ['pgrep', '-fl', 'session-domain-[c]leanup/launcher.py'],
            capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError) as error:
        return f'BLOCKED: {type(error).__name__}: {error}\n'
    return f'rc={process.returncode}\n{process.stdout}{process.stderr}'


def read_summary(case):
    path = case / 'summary.json'
    if not path.is_file():
        return None, 'summary.json missing'
    try:
        return json.loads(path.read_text()), None
    except (OSError, ValueError) as error:
        return None, f'summary.json unreadable: {type(error).__name__}: {error}'


def run_check(case, mode, timeout=180):
    return run_owned([sys.executable, '-I', str(CHECK), '--output', str(case),
                      '--mode', mode], cwd=ROOT, env=dict(os.environ), timeout=timeout)


def classify(name, judge_spec, result, case):
    """KILLED only for the mutation's own expected failure fields.

    A probe/environment error that the mutation did not declare is INVALID:
    otherwise an unmutated implementation whose group signal is denied can
    satisfy the expected fields and be mis-counted as a kill.
    """
    mode, judge, uses_launcher, allowed_errors = judge_spec
    if isinstance(result, subprocess.TimeoutExpired):
        return 'INVALID', f'check timed out: {result}'
    if isinstance(result, BaseException):
        return 'INVALID', f'check failed to start/finish: {type(result).__name__}: {result}'
    summary, error = read_summary(case)
    if summary is None:
        return 'INVALID', error
    rows = summary.get('results') or []
    if uses_launcher:
        launcher = [row for row in rows if row.get('mode') == mode]
        if not launcher:
            return 'INVALID', f'no launcher result for mode {mode}'
        if launcher[0].get('launcherExitCode') != 0:
            return 'INVALID', (f"launcher startup failed: exit={launcher[0].get('launcherExitCode')!r} "
                               f"status={launcher[0].get('launcherStatus')!r}")
        probe = launcher[0].get('probe') or {}
        for field in ('signalError', 'probeError'):
            value = probe.get(field)
            if value and field not in allowed_errors:
                return 'INVALID', f'undeclared probe environment error {field}={value!r}'
    ok, detail = judge(rows)
    if ok:
        if result.returncode == 0:
            return 'INVALID', 'expected result present but the check exited 0'
        return 'KILLED', detail
    if summary.get('status') != 'FAIL':
        return 'SURVIVED', f'check reported {summary.get("status")!r}; {detail}'
    return 'SURVIVED', f'check failed without this mutation\'s fields: {detail}'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline', default='HEAD',
                        help='commit/tree to prove the source is pristine against '
                             '(default HEAD; a "git stash create" tree for a local rehearsal)')
    parser.add_argument('--only', action='append', default=[],
                        help='mutant name; repeat to select more (still runs the restored positive)')
    args = parser.parse_args()
    known = {row[0] for row in MUTATIONS}
    if set(args.only) - known or len(args.only) != len(set(args.only)):
        parser.error('unknown or duplicate --only mutant')
    selected = ([row for row in MUTATIONS if row[0] in args.only]
                if args.only else list(MUTATIONS))
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pgrep-before.txt').write_text(pgrep_launcher())
    summary = []
    invalid = []
    survived = []
    sources = sorted({Path(row[1]) for row in MUTATIONS}, key=str)
    with mutating_sources(ROOT, sources, baseline=args.baseline) as snapshots:
        originals = {source: snapshots[source] for source in sources}
        digests = {source: hashlib.sha256(originals[source]).hexdigest()
                   for source in sources}
        contents = {source: originals[source].decode() for source in sources}
        stale = [(item[0], contents[Path(item[1])].count(item[2])) for item in selected
                 if contents[Path(item[1])].count(item[2]) != 1]
        if stale:
            (output / 'summary.json').write_text(json.dumps({
                'status': 'ANCHOR_EXPIRED', 'anchors': stale}, ensure_ascii=False, indent=2) + '\n')
            for name, count in stale:
                print(json.dumps({'status': 'ANCHOR_EXPIRED', 'mutation': name,
                                  'anchorCount': count}, ensure_ascii=False))
            return 2
        try:
            for mutation in selected:
                name, source, old, new = mutation
                source = Path(source)
                judge_spec = JUDGES[name]
                mode = judge_spec[0]
                changed = contents[source].replace(old, new, 1)
                case = output / name  # check_cleanup.py owns/creates this directory
                source.write_text(changed)
                returncode = None
                try:
                    try:
                        run = run_check(case, mode)
                        returncode = run.returncode
                        log = run.stdout + run.stderr
                        (output / f'{name}.log').write_text(log)
                        verdict, detail = classify(name, judge_spec, run, case)
                    except (subprocess.SubprocessError, OSError) as error:
                        (output / f'{name}.log').write_text(f'{type(error).__name__}: {error}\n')
                        verdict, detail = classify(name, judge_spec, error, case)
                finally:
                    source.write_bytes(originals[source])
                    if hashlib.sha256(source.read_bytes()).hexdigest() != digests[source]:
                        raise RuntimeError(f'source restore hash mismatch: {source.name}')
                entry = {'mutation': name, 'mode': mode, 'verdict': verdict,
                         'detail': detail, 'returncode': returncode,
                         'source': source.relative_to(ROOT).as_posix()}
                summary.append(entry)
                if verdict == 'INVALID':
                    invalid.append(entry)
                elif verdict == 'SURVIVED':
                    survived.append(entry)
                print(f'{name}: {verdict} {detail}'.rstrip(), flush=True)
        finally:
            for source in sources:
                source.write_bytes(originals[source])
        restored_case = output / 'restored-positive'  # must not pre-exist
        restored_error = None
        try:
            restored = run_check(restored_case, 'all')
            (output / 'restored-positive.log').write_text(restored.stdout + restored.stderr)
            restored_summary, restored_error = read_summary(restored_case)
        except (subprocess.SubprocessError, OSError) as error:
            restored = None
            restored_summary = None
            restored_error = f'{type(error).__name__}: {error}'
        expected_status = 'PASS' if sys.platform == 'darwin' else 'PASS_WITH_SKIPS'
        expected_code = 0 if expected_status == 'PASS' else 1
        restored_ok = False
        if restored_summary is not None:
            rows = restored_summary.get('results') or []
            wiring = [row for row in rows if row.get('mode') == 'wiring']
            if expected_status == 'PASS':
                rows_ok = all(row.get('passed') is True for row in rows)
            else:
                rows_ok = (all(row.get('passed') in (True, None) for row in rows)
                           and any(row.get('passed') is None for row in rows))
            restored_ok = (restored.returncode == expected_code
                           and restored_summary.get('status') == expected_status
                           and bool(rows) and rows_ok
                           and bool(wiring) and wiring[0].get('status')
                           == 'PASS_WITH_CLEANUP_UNVERIFIED')
            if not restored_ok:
                restored_error = (f'rc={restored.returncode} status={restored_summary.get("status")!r} '
                                  f'rows={len(rows)} wiring={bool(wiring)}')
        (output / 'pgrep-after.txt').write_text(pgrep_launcher())
        # The restored positive is a child of the same batch: a PASS_WITH_SKIPS
        # child (non-Darwin platform skips) can never become PASS/0.
        top_status, exit_code = common.aggregate_status(
            *(MUTATION_STATUS.get(entry['verdict'], 'FAIL') for entry in summary),
            restored_summary.get('status') if restored_ok else 'FAIL')
        report = {'status': top_status, 'sourceSha256': digests[SOURCE],
                  'sourceSha256All': {source.relative_to(ROOT).as_posix(): digests[source]
                                      for source in sources},
                  'baseline': args.baseline,
                  'selected': [row[0] for row in selected],
                  'mutations': summary, 'restoredPass': restored_ok,
                  'restoredStatus': (restored_summary.get('status')
                                     if restored_summary is not None else None),
                  'restoredError': restored_error,
                  'invalid': [row['mutation'] for row in invalid],
                  'survived': [row['mutation'] for row in survived]}
        (output / 'summary.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
        restored_label = (restored_summary.get('status')
                          if restored_summary is not None else 'FAIL')
        print(f'restored positive: {restored_label} '
              f'{"ok" if restored_ok else "FAIL"} {restored_error or ""}'.rstrip(),
              flush=True)
        print(json.dumps({'status': top_status, 'exitCode': exit_code,
                          'killed': len(summary) - len(invalid) - len(survived),
                          'invalid': len(invalid), 'survived': len(survived),
                          'restoredPass': restored_ok}, ensure_ascii=False))
        return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
