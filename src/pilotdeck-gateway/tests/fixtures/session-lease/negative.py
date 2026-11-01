#!/usr/bin/env python3
"""#1221 committed-source lease controls; no scoring or HEAD guard bypass."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import json
import os
from pathlib import Path
import re
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'scripts/gateway-parity/session_domain.py'
test_file = Path(__file__).with_name('test_lease.py')
cases = [
    ('old-score-only-release', source, 'verified = verified or fixture_lease_stopped(child, audit)', 'verified = verified', 'test_blocked_fixture_releases_lease'),
    ('nonpass-score-only-release', source, 'verified = verified or fixture_lease_stopped(child, audit)', 'verified = verified', 'test_failed_and_cancelled_fixture_release'),
    ('unregistered-release', source, 'if (child.pid not in common.LIVE_GROUPS', 'if (False', 'test_lease_probe_is_fail_closed'),
    ('unreaped-release', source, '            or child.returncode is None\n            or probe.get', '            or False\n            or probe.get', 'test_lease_probe_is_fail_closed'),
    ('wrong-pgid-release', source, "or probe.get('pgid') != child.pid", 'or False', 'test_lease_probe_is_fail_closed'),
    ('lost-identity-release', source, "or probe.get('leaderUnreaped') is not True", 'or False', 'test_lease_probe_is_fail_closed'),
    ('unsettled-probe-release', source, "or probe.get('leaderReaped') is not True", 'or False', 'test_lease_probe_is_fail_closed'),
    ('truthy-identity-release', source, "or probe.get('leaderUnreaped') is not True", "or not probe.get('leaderUnreaped')", 'test_lease_probe_is_fail_closed'),
    ('truthy-reap-release', source, "or probe.get('leaderReaped') is not True", "or not probe.get('leaderReaped')", 'test_lease_probe_is_fail_closed'),
    ('cleanup-error-release', source, "            or audit.get('cleanupBlocked')", '            or False', 'test_lease_probe_is_fail_closed'),
    ('cleanup-raised-release', source, "            or audit.get('cleanupRaised'))", '            or False)', 'test_lease_probe_is_fail_closed'),
    ('probe-error-release', source, "audit['leaseGroupProbe'] = f'unverified: {type(error).__name__}: {error}'\n            return False", "audit['leaseGroupProbe'] = f'unverified: {type(error).__name__}: {error}'\n            return True", 'test_lease_probe_is_fail_closed'),
    ('live-group-release', source, "audit['leaseGroupProbe'] = 'group still exists after 2 seconds'\n            return False", "audit['leaseGroupProbe'] = 'group still exists after 2 seconds'\n            return True", 'test_lease_probe_is_fail_closed'),
    ('no-settle-window', source, "    deadline = time.monotonic() + 2\n    while True:", "    deadline = time.monotonic() + 0\n    while True:", 'test_lease_probe_waits_boundedly'),
]
messages = {
    'test_blocked_fixture_releases_lease': '#1221 BLOCKED empty fixture releases lease for successor',
    'test_failed_and_cancelled_fixture_release': '#1221 non-PASS cleanup releases only a settled empty group',
    'test_lease_probe_is_fail_closed': '#1221 lease proof requires held reaped leader and final ESRCH',
    'test_lease_probe_waits_boundedly': '#1221 lease probe waits for ESRCH within bounded deadline',
}


def preflight():
    errors = []
    for name, path, before, after, test in cases:
        text = path.read_text()
        try:
            if text.count(before) != 1:
                raise ValueError(f'STALE ANCHOR: {text.count(before)} matches')
            compile(text.replace(before, after, 1), str(path), 'exec')
        except Exception as error:
            errors.append(name + ': ' + str(error))
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'#1221 {len(cases)} unique anchors; all Python mutants compile')


def passed(result):
    log = result.stdout + result.stderr
    return (result.returncode == 0 and re.search(r'^Ran 1 test in ', log, re.M)
            and re.search(r'^OK$', log, re.M))


def main():
    preflight()
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative.py NEW_OUTPUT | --check-anchors')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(name, test):
        # Python apparatus only: no cargo process needs wait_for_slot.
        result = run_owned([sys.executable, '-I', str(test_file), 'LeaseTests.' + test],
                           cwd=root, env=os.environ.copy(), timeout=30)
        (out / (name + '.log')).write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(root, [source, test_file, Path(__file__).resolve()]) as originals:
            for name, path, before, after, test in cases:
                status, reason = 'INVALID', ''
                try:
                    text = originals[path].decode()
                    if text.count(before) != 1:
                        raise ValueError('STALE ANCHOR ' + name)
                    changed = text.replace(before, after, 1)
                    if not passed(run(name + '-positive', test)):
                        raise ValueError('positive bound test did not pass')
                    path.write_text(changed)
                    compile(path.read_bytes(), str(path), 'exec')
                    (out / (name + '-compile.log')).write_text('Python compile: PASS\n')
                    result = run(name + '-negative', test)
                    log = result.stdout + result.stderr
                    if passed(result):
                        status, reason = 'SURVIVED', 'bound test passed'
                    elif (result.returncode == 1 and re.search(r'^FAIL: ' + re.escape(test) + r'\b', log, re.M)
                          and re.search(r'^AssertionError:[^\n]*' + re.escape(messages[test]), log, re.M)
                          and re.search(r'^Ran 1 test in ', log, re.M) and 'FAILED (failures=' in log
                          and 'ERROR:' not in log):
                        status, reason = 'KILLED', 'named assertion matched'
                    else:
                        reason = 'missing bound runtime failure'
                except Exception as error:
                    status, reason = 'INVALID', str(error)
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not passed(run(name + '-restored', test)):
                        status, reason = 'INVALID', reason + '; restored control failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + '; restored: ' + str(error)
                results.append({'name': name, 'status': status, 'reason': reason})
    except BaseException as error:
        for row in results:
            row.update(status='INVALID', reason='guard: ' + repr(error))
        for name, *_ in cases[len(results):]:
            results.append({'name': name, 'status': 'INVALID', 'reason': 'guard: ' + repr(error)})
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results}, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps({'controls': results}, indent=2))
    return 0 if len(results) == len(cases) and all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
