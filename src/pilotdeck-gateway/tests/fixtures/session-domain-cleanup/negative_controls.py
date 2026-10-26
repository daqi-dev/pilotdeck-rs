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

Exit codes: 0 every mutant killed and the restored source passes; 1 a mutant
survived or the restored source failed; 2 an anchor is stale (refuse to mutate);
3 an environment/startup failure made a case INVALID (never counted as KILLED).
"""
import argparse
import hashlib
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
CHECK = Path(__file__).resolve().with_name('check_cleanup.py')

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
    if error != 'OSError: denied':
        return False, f'error={error!r}, want "OSError: denied"'
    cases = row.get('cases') or []
    if [case.get('case') for case in cases] != ['permission-denied']:
        return False, f"cases={[case.get('case') for case in cases]!r}, want ['permission-denied']"
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


# (name, old, new, mode, judge, uses_launcher, allowed_probe_errors).  Every old
# block must occur once.  ``allowed_probe_errors`` names the probe result fields
# (``signalError`` / ``probeError``) this mutation's own fault is allowed to
# produce; any other mutation that sees one is INVALID, never KILLED.
MUTATIONS = (
    ('empty-group-probe-removed',
     "        return 'empty', (f'killpg0=EPERM with the unreaped zombie leader present '\n"
     "                         f'and an empty live-member census: {error}')",
     "        return 'alive', 'mutant: empty-group probe removed'",
     'clean',
     lambda rows: match(rows, 'clean', {
         'processTreeCleanup': 'GROUP_NOT_EMPTY',
         'probe.emptyAfterSignal': False,
         'probe.signal': 'SIGKILL',
     }),
     True,
     ('signalError', 'probeError')),
    ('cleanup-status-write-removed',
     "            audit['processTreeCleanup'] = audit['processGroupProbe']['status']",
     "            audit['processTreeCleanup'] = CLEANUP_VERIFIED",
     'leak',
     lambda rows: match(rows, 'leak', {
         'processTreeCleanup': 'VERIFIED',
         'probe.status': 'GROUP_NOT_EMPTY',
     }),
     True,
     ()),
    ('top-level-mapping-relaxed',
     "    return ('PASS' if audit.get('processTreeCleanup') == CLEANUP_VERIFIED\n"
     "            else CLEANUP_UNVERIFIED_STATUS)",
     "    return 'PASS'",
     'status', status_mapping, False, ()),
    ('sigterm-removed',
     "    for sig in (signal.SIGTERM, signal.SIGKILL):",
     "    for sig in (signal.SIGKILL,):",
     'leak',
     lambda rows: match(rows, 'leak', {
         'processTreeCleanup': 'GROUP_NOT_EMPTY',
         'probe.signal': 'SIGKILL',
     }),
     True,
     ()),
    ('blocked-branch-removed',
     "    except OSError as error:\n        return 'blocked', str(error)",
     "    except OSError:\n        raise",
     'blocked', blocked_branch_removed, False, ()),
    ('sigkill-escalation-removed',
     "    for sig in (signal.SIGTERM, signal.SIGKILL):",
     "    for sig in (signal.SIGTERM,):",
     'ignore',
     lambda rows: match(rows, 'ignore', {
         'processTreeCleanup': 'GROUP_NOT_EMPTY',
         'probe.signal': 'SIGTERM',
         'probe.emptyAfterSignal': False,
     }),
     True,
     ()),
    ('top-level-wiring-relaxed',
     "                overall = cleanup_status(audit)",
     "                overall = 'PASS'",
     'wiring', wiring_relaxed, False, ()),
    ('eperm-census-ignored',
     "        live = live_group_members(pgid)",
     "        live = set()  # mutant: ignore the independent live-member census",
     'eperm-live-member',
     lambda rows: match(rows, 'eperm-live-member', {
         'processTreeCleanup': 'VERIFIED',
         'topLevel': 'PASS',
     }),
     False,
     ()),
    ('unavailable-census-as-empty',
     "            return 'blocked', f'killpg0=EPERM; independent group census unavailable: {error}'",
     "            live = set()  # mutant: treat an unavailable census as empty",
     'census-unavailable',
     lambda rows: match(rows, 'census-unavailable', {
         'processTreeCleanup': 'VERIFIED',
         'topLevel': 'PASS',
     }),
     False,
     ()),
    ('reap-guard-removed',
     "                if child.returncode is None and leader_state == 'RUNNING':",
     "                if child.returncode is None:  # mutant: signal after REAPED",
     'reap-state',
     lambda rows: match(rows, 'reap-state', {'killCalls': 1}),
     False,
     ()),
)


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


def classify(mutation, result, case):
    """KILLED only for the mutation's own expected failure fields.

    A probe/environment error that the mutation did not declare is INVALID:
    otherwise an unmutated implementation whose group signal is denied can
    satisfy the expected fields and be mis-counted as a kill.
    """
    name, mode, judge, uses_launcher = mutation[0], mutation[3], mutation[4], mutation[5]
    allowed_errors = mutation[6]
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
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pgrep-before.txt').write_text(pgrep_launcher())
    summary = []
    invalid = []
    survived = []
    with mutating_sources(ROOT, [SOURCE], baseline=args.baseline) as snapshots:
        original = snapshots[SOURCE]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        stale = [(item[0], content.count(item[1])) for item in MUTATIONS
                 if content.count(item[1]) != 1]
        if stale:
            (output / 'summary.json').write_text(json.dumps({
                'status': 'ANCHOR_EXPIRED', 'anchors': stale}, ensure_ascii=False, indent=2) + '\n')
            for name, count in stale:
                print(json.dumps({'status': 'ANCHOR_EXPIRED', 'mutation': name,
                                  'anchorCount': count}, ensure_ascii=False))
            return 2
        try:
            for mutation in MUTATIONS:
                name, old, new, mode = mutation[:4]
                changed = content.replace(old, new, 1)
                case = output / name  # check_cleanup.py owns/creates this directory
                SOURCE.write_text(changed)
                returncode = None
                try:
                    try:
                        run = run_check(case, mode)
                        returncode = run.returncode
                        log = run.stdout + run.stderr
                        (output / f'{name}.log').write_text(log)
                        verdict, detail = classify(mutation, run, case)
                    except (subprocess.SubprocessError, OSError) as error:
                        (output / f'{name}.log').write_text(f'{type(error).__name__}: {error}\n')
                        verdict, detail = classify(mutation, error, case)
                finally:
                    SOURCE.write_bytes(original)
                    if hashlib.sha256(SOURCE.read_bytes()).hexdigest() != before:
                        raise RuntimeError('source restore hash mismatch')
                entry = {'mutation': name, 'mode': mode, 'verdict': verdict,
                         'detail': detail, 'returncode': returncode}
                summary.append(entry)
                if verdict == 'INVALID':
                    invalid.append(entry)
                elif verdict == 'SURVIVED':
                    survived.append(entry)
                print(f'{name}: {verdict} {detail}'.rstrip(), flush=True)
        finally:
            SOURCE.write_bytes(original)
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
        restored_ok = False
        if restored_summary is not None:
            rows = restored_summary.get('results') or []
            wiring = [row for row in rows if row.get('mode') == 'wiring']
            restored_ok = (restored.returncode == 0 and restored_summary.get('status') == 'PASS'
                           and bool(rows) and all(row.get('passed') is True for row in rows)
                           and bool(wiring) and wiring[0].get('status')
                           == 'PASS_WITH_CLEANUP_UNVERIFIED')
            if not restored_ok:
                restored_error = (f'rc={restored.returncode} status={restored_summary.get("status")!r} '
                                  f'rows={len(rows)} wiring={bool(wiring)}')
        (output / 'pgrep-after.txt').write_text(pgrep_launcher())
        status = ('INVALID' if invalid else 'SURVIVED' if survived
                  else 'PASS' if restored_ok else 'FAIL')
        report = {'status': status, 'sourceSha256': before, 'baseline': args.baseline,
                  'mutations': summary, 'restoredPass': restored_ok,
                  'restoredError': restored_error,
                  'invalid': [row['mutation'] for row in invalid],
                  'survived': [row['mutation'] for row in survived]}
        (output / 'summary.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
        print(f'restored positive: {"PASS" if restored_ok else "FAIL"} '
              f'{restored_error or ""}'.rstrip(), flush=True)
        print(json.dumps({'status': status, 'killed': len(summary) - len(invalid) - len(survived),
                          'invalid': len(invalid), 'survived': len(survived),
                          'restoredPass': restored_ok}, ensure_ascii=False))
        if invalid:
            return 3
        if survived or not restored_ok:
            return 1
        return 0


if __name__ == '__main__':
    raise SystemExit(main())
