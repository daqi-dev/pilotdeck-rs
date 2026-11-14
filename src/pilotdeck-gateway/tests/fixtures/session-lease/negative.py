#!/usr/bin/env python3
"""#1221 lease controls: memory rehearsal or guarded committed-source mutations."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import re
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, tree_lock

root = Path(__file__).resolve().parents[5]
source = root / 'scripts/gateway-parity/session_domain.py'
common = root / 'scripts/gateway-parity/common.py'
test_file = Path(__file__).with_name('test_lease.py')
cli = root / 'scripts/cli-parity/run_round.py'
capture = root / 'src/pilotdeck-cli/tests/fixtures/sdk-signals/capture.py'
readme = Path(__file__).with_name('README.md')
# #1662: capture.py is never an in-place guarded source here. Its disk bytes are
# sha256-verified by the resident sdk_signal_exit test, so the capture case is
# injected in memory (see one()) in every mode; an in-place committed mutation
# would false-fail that test and a SIGKILLed run would leave a mutant behind.
guarded_sources = [common, source, test_file, Path(__file__).resolve(), cli, readme]
EXPECTED_CASES = 27
# Independent total: update deliberately when adding or retiring a control.
cases = [
    ('cli-term-raises-twice', cli, 'def interrupted(signum, frame):\n    if isinstance(sys.exception(), KeyboardInterrupt):', 'def interrupted(signum, frame):\n    if isinstance(None, KeyboardInterrupt):', 'test_cli_cancel_unwind'),
    ('cli-alarm-raises-twice', cli, 'def lease_expired(signum, frame):\n    if isinstance(sys.exception(), KeyboardInterrupt):', 'def lease_expired(signum, frame):\n    if isinstance(None, KeyboardInterrupt):', 'test_cli_cancel_unwind'),
    ('old-all-stopped-definition', readme, '#1221 更正 #209 E1 的 `allStopped` 定义', '#209 E1 的 `allStopped` 定义不变', 'test_documented_release_definition'),
    ('all-stopped-means-score', readme, '`allStopped` 是租约释放证明，不是判分通过', '`allStopped` 是判分通过', 'test_documented_release_definition'),
    ('omit-common-guard', Path(__file__).resolve(), 'guarded_' + 'sources = [common,', 'guarded_' + 'sources = [', 'test_guarded_sources_include_common'),
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
    ('omit-term-registration', cli, 'return signal.' + 'signal(signal.SIGTERM, interrupted)', 'return signal.getsignal(signal.SIGTERM)', 'test_cli_signal_handlers_registered'),
    ('omit-alarm-registration', cli, 'return signal.' + 'signal(signal.SIGALRM, lease_expired)', 'return signal.getsignal(signal.SIGALRM)', 'test_cli_signal_handlers_registered'),
    ('capture-term-raises-twice', capture, '    if isinstance(sys.exception(), KeyboardInterrupt):\n        return\n    raise KeyboardInterrupt(f\'#1145 capture interrupted by signal {signum}\')', '    if isinstance(None, KeyboardInterrupt):\n        return\n    raise KeyboardInterrupt(f\'#1145 capture interrupted by signal {signum}\')', 'test_capture_cancel_unwind'),
    ('memory-rehearsal-without-lock', Path(__file__).resolve(), '        if memory:\n            with tree_' + 'lock(root, timeout=900):\n                for row in selected:\n                    one(row)\n        else:', '        if memory:\n            for row in selected:\n                one(row)\n        else:', 'test_memory_rehearsal_holds_tree_lock'),
    ('omit-lease-case', Path(__file__).resolve(), "    ('cl" + "i-term-raises-twice', cli, 'def interrupted(signum, frame):\\n    if isinstance(sys.exception(), KeyboardInterrupt):', 'def interrupted(signum, frame):\\n    if isinstance(None, KeyboardInterrupt):', 'test_cli_cancel_unwind'),\n", '', 'test_expected_case_count'),
    # fix-r2 (#1528 review): an equal-length swap (drop one row, paste
    # another over it) keeps len(cases) == EXPECTED_CASES, so the count
    # check alone let a retired control vanish with the summary still
    # green; validate_case_count now also refuses non-unique names.
    ('duplicate-lease-case', Path(__file__).resolve(), "    ('cli-term-raises-twi" + "ce', cli, 'def interrupted(signum, frame):\\n    if isinstance(sys.exception(), KeyboardInterrupt):', 'def interrupted(signum, frame):\\n    if isinstance(None, KeyboardInterrupt):', 'test_cli_cancel_unwind'),\n", "    ('cli-alarm-raises-twi" + "ce', cli, 'def lease_expired(signum, frame):\\n    if isinstance(sys.exception(), KeyboardInterrupt):', 'def lease_expired(signum, frame):\\n    if isinstance(None, KeyboardInterrupt):', 'test_cli_cancel_unwind'),\n", 'test_expected_case_count'),
    ('capture-guarded-again', Path(__file__).resolve(), 'guarded_' + 'sources = [common, source, test_file, Path(__file__).resolve(), cli, readme]', 'guarded_' + 'sources = [common, source, test_file, Path(__file__).resolve(), cli, readme, capture]', 'test_guarded_sources_exclude_captured_fixture'),
    ('capture-committed-disk-write', Path(__file__).resolve(), 'inject = memory or path not in guarded_' + 'sources', 'inject = memory', 'test_capture_case_mutates_memory_only'),
]
messages = {
    'test_cli_signal_handlers_registered': '#a5b CLI signal handlers are installed by production code',
    'test_expected_case_count': '#a5b lease negative case inventory is complete',
    'test_cli_cancel_unwind': '#1194 CLI preserves first cancellation and accepts later signals',
    'test_capture_cancel_unwind': '#1194 capture rehearsal preserves first cancellation and accepts later signals',
    'test_memory_rehearsal_holds_tree_lock': '#1398 lease memory rehearsal holds the source tree lock',
    'test_documented_release_definition': '#1221 allStopped documents lease proof separately from scoring',
    'test_guarded_sources_include_common': '#a5b parity mutation guards include common.py',
    'test_guarded_sources_exclude_captured_fixture': '#1662 guarded sources exclude capture.py whose disk bytes are golden-verified',
    'test_capture_case_mutates_memory_only': '#1662 the capture case mutates memory only, never the disk fixture',
    'test_blocked_fixture_releases_lease': '#1221 BLOCKED empty fixture releases lease for successor',
    'test_failed_and_cancelled_fixture_release': '#1221 non-PASS cleanup releases only a settled empty group',
    'test_lease_probe_is_fail_closed': '#1221 lease proof requires held reaped leader and final ESRCH',
    'test_lease_probe_waits_boundedly': '#1221 lease probe waits for ESRCH within bounded deadline',
}


# #2929 fix-r2: the parity-lease acquire budget for the one mutating_sources
# window this runner holds: FAMILY_MAX -- the family roster's largest
# whole-round hold bound (gateway-batch; the live value is the roster's FAMILY_MAX) -- plus this runner's own
# restore tail and one stage of handoff margin, rounded up to the 900 s
# family unit.  One derivation for the whole family, recomputed live in
# test_lease_acquire_budgets.py; its closed roster keeps a new global-lease
# runner from silently missing a budget.  A live holder always carries
# cleanupPending and no taker takes such an owner over, so the budget
# deliberately exceeds common.MAX_AGE -- MAX_AGE only ages out
# never-published stale locks; a published owner, dead pid included, keeps
# the cleanupPending protection and is never taken over automatically
# (fix-r3: such a residual now fails the acquire fast with a diagnostic
# instead of waiting the budget out) -- and just has to cover the family's
# largest live hold (#1528/#1529).
LEASE_ACQUIRE_BUDGET_SECONDS = 65700


def validate_case_count():
    # fix-r2 (#1528 review): the count alone cannot catch an equal-length
    # swap -- drop one row and paste another over it and len(cases) still
    # matches, so the uniqueness of the names is refused here too (same
    # shape as the e2e runner's #1535 guard).
    names = {row[0] for row in cases}
    if len(cases) != EXPECTED_CASES:
        raise SystemExit(f'#a5b lease negative case inventory is complete: '
                         f'expected {EXPECTED_CASES}, got {len(cases)}')
    if len(names) != EXPECTED_CASES:
        raise SystemExit(f'#a5b lease negative case inventory is complete: '
                         f'expected {EXPECTED_CASES} unique names, got {len(names)}')


def preflight():
    validate_case_count()
    errors = []
    test_source = test_file.read_text()
    compile(test_source, str(test_file), 'exec')
    for name, path, before, after, test in cases:
        text = path.read_text()
        try:
            if test not in messages or '    def ' + test + '(' not in test_source:
                raise ValueError('unknown bound test or assertion marker: ' + test)
            if text.count(before) != 1:
                raise ValueError(f'STALE ANCHOR: {text.count(before)} matches')
            if path.suffix == '.py':
                compile(text.replace(before, after, 1), str(path), 'exec')
        except Exception as error:
            errors.append(name + ': ' + str(error))
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'#1221 {len(cases)} unique anchors; all Python mutants compile; Markdown has no compiler')


def passed(result):
    log = result.stdout + result.stderr
    return (result.returncode == 0 and re.search(r'^Ran 1 test in ', log, re.M)
            and re.search(r'^OK$', log, re.M))


def memory_run(name):
    """Candidate rehearsal in one child; no disk mutation or HEAD impersonation."""
    _, path, before, after, test = next(row for row in cases if row[0] == name)
    original = path.read_bytes()
    if original.count(before.encode()) != 1:
        raise ValueError('STALE ANCHOR ' + name)
    changed = original.replace(before.encode(), after.encode(), 1)
    if path.suffix == '.py':
        compile(changed, str(path), 'exec')
    read_bytes, read_text = Path.read_bytes, Path.read_text
    get_code = importlib.machinery.SourceFileLoader.get_code

    def bytes_for(p):
        return changed if p.resolve() == path else read_bytes(p)

    def text_for(p, *args, **kwargs):
        return changed.decode() if p.resolve() == path else read_text(p, *args, **kwargs)

    def code_for(loader, fullname):
        if Path(loader.path).resolve() == path:
            return compile(changed, str(path), 'exec')
        return get_code(loader, fullname)

    with patch.object(Path, 'read_bytes', bytes_for), patch.object(Path, 'read_text', text_for), \
            patch.object(importlib.machinery.SourceFileLoader, 'get_code', code_for):
        spec = importlib.util.spec_from_file_location('bound_tests', test_file)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        suite = unittest.defaultTestLoader.loadTestsFromName('LeaseTests.' + test, module)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def main():
    validate_case_count()
    if sys.argv[1:2] == ['--memory-run']:
        return memory_run(sys.argv[2])
    if sys.argv[1:] == ['--check-anchors']:
        preflight()
        return 0
    arguments = sys.argv[1:]
    memory = '--memory' in arguments
    arguments = [arg for arg in arguments if arg != '--memory']
    if len(arguments) not in (1, 3) or (len(arguments) == 3 and arguments[1] != '--case'):
        raise SystemExit('usage: negative.py NEW_OUTPUT [--memory] [--case NAME] | --check-anchors')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    selected = cases if len(arguments) == 1 else [row for row in cases if row[0] == arguments[2]]
    if not selected:
        raise SystemExit('unknown mutation')
    out = Path(arguments[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(name, test, mutant=None):
        # Python apparatus only: no cargo process needs wait_for_slot.
        command = ([sys.executable, '-I', __file__, '--memory-run', mutant] if mutant else
                   [sys.executable, '-I', str(test_file), 'LeaseTests.' + test])
        result = run_owned(command,
                           cwd=root, env=os.environ.copy(), timeout=30)
        (out / (name + '.log')).write_text(result.stdout + result.stderr)
        return result

    def one(row, restore=False):
        name, path, before, after, test = row
        status, reason = 'INVALID', ''
        original, wrote = None, False
        try:
            if test not in messages or '    def ' + test + '(' not in test_file.read_text():
                raise ValueError('unknown bound test or assertion marker: ' + test)
            text = path.read_text()
            if text.count(before) != 1:
                raise ValueError('STALE ANCHOR ' + name)
            changed = text.replace(before, after, 1)
            if not passed(run(name + '-positive', test)):
                raise ValueError('positive bound test did not pass')
            if path.suffix == '.py':
                compile(changed, str(path), 'exec')
            # #1662: a case outside the guarded set (capture.py is disk-verified
            # by sdk_signal_exit) runs its mutant through the in-memory child
            # even in committed mode; only guarded sources are ever written.
            original = path.read_bytes()
            inject = memory or path not in guarded_sources
            if not inject:
                path.write_text(changed)
                wrote = True
            (out / (name + '-compile.log')).write_text('Python compile: PASS; Markdown compilation: N/A\n')
            result = run(name + '-negative', test, name if inject else None)
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
            if restore and wrote:
                path.write_bytes(original)
        try:
            if not passed(run(name + '-restored', test)):
                status, reason = 'INVALID', reason + '; restored control failed'
        except Exception as error:
            status, reason = 'INVALID', reason + '; restored: ' + str(error)
        results.append({'name': name, 'status': status, 'reason': reason})
    try:
        if memory:
            with tree_lock(root, timeout=900):
                for row in selected:
                    one(row)
        else:
            with mutating_sources(root, guarded_sources,
                                 lock_timeout=LEASE_ACQUIRE_BUDGET_SECONDS):
                for row in selected:
                    one(row, True)
    except BaseException as error:
        for row in results:
            row.update(status='INVALID', reason='guard: ' + repr(error))
        for name, *_ in selected[len(results):]:
            results.append({'name': name, 'status': 'INVALID', 'reason': 'guard: ' + repr(error)})
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'mode': 'memory' if memory else 'committed', 'controls': results}, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps({'mode': 'memory' if memory else 'committed', 'controls': results}, indent=2))
    return 0 if len(results) == len(selected) and all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
