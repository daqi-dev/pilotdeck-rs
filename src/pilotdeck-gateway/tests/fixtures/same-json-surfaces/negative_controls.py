#!/usr/bin/env python3
"""#2039 same_json surface mutations; memory rehearsal or guarded post-commit run.

Each case reverts one of the six comparisons this batch moved to the strict
``same_json`` walk back to Python's bare ``==``/``!=`` (the exact regression
shape: ``True == 1`` passing a drifted JSON boolean as identical).  The bound
tests are the #2039 pins in ``scripts/gateway-parity/test_safety.py``
(``SameJsonSurfaces``); only that class's own single-line ``#2039`` assertion
counts as a kill.

--memory patches the loader in-process (no source bytes are written, the
shared tree lock is still held for the whole drill); the default mode writes
mutated bytes under ``mutating_sources`` and therefore requires the candidate
sources to equal HEAD -- run it from a committed tree, not this worktree.
"""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import re
import signal
import unittest
from unittest.mock import patch

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (SourceBaselineError, defer_signal_exit,
                              mutating_sources, run_owned, tree_lock,
                              write_source_atomic)
from negative_selection import claim_dir, refuse, registered, verdict_exit

session = root / 'scripts/gateway-parity/session_domain.py'
native = root / 'scripts/gateway-parity/native_evidence.py'
test_safety = root / 'scripts/gateway-parity/test_safety.py'
# Mutating any scripts/gateway-parity source puts the run under the shared
# parity lease, and negative_process._parity_mutation_lock refuses to load
# that lease from anything but guarded common.py bytes (#166).
common = root / 'scripts/gateway-parity/common.py'

# (name, file, old bytes, new bytes, bound test).  The old bytes are the
# post-#2039 spelling, so an anchor that no longer matches is drift, not a
# pass: preflight refuses to run and nothing is recorded as SURVIVED.
cases = [
    ('surfaces-frames-bare-not-equal', session,
     "    if not native_evidence.same_json(ts['frames'], rust['frames']):",
     "    if ts['frames'] != rust['frames']:",
     'test_boolean_drift_surfaces_on_frames'),
    ('surfaces-durable-bare-not-equal', session,
     "    if not native_evidence.same_json(ts['durableStatus'], rust['durableStatus']):",
     "    if ts['durableStatus'] != rust['durableStatus']:",
     'test_boolean_drift_surfaces_on_durable_status'),
    ('surfaces-applicable-bare-not-equal', session,
     "    if not native_evidence.same_json(expected.get('applicable'), registered.get('applicable')):",
     "    if expected.get('applicable') != registered.get('applicable'):",
     'test_applicable_drift_surfaces_before_the_field_guards'),
    ('surfaces-registry-fields-bare-not-equal', session,
     "            if not native_evidence.same_json(expected.get(field), registered.get(field)):",
     "            if expected.get(field) != registered.get(field):",
     'test_registry_field_drift_surfaces'),
    ('cleanup-replay-bare-equal', native,
     "    checked['cleanup']['allStopped'] = same_json(evidence['cleanup'], replay['cleanup'])",
     "    checked['cleanup']['allStopped'] = evidence['cleanup'] == replay['cleanup']",
     'test_cleanup_replay_drift_is_not_coerced'),
    ('prelaunch-bare-equal', native,
     "    need(same_json({k: v for k, v in contract.items() if k != 'launcherPid'}, prelaunch),",
     "    need({k: v for k, v in contract.items() if k != 'launcherPid'} == prelaunch,",
     'test_prelaunch_drift_is_not_coerced'),
    ('receipt-tracer-exit-bare-equal', native,
     "    need(receipt['status'] == 'WITNESS_CAPTURED' and same_json(receipt['tracerExitCode'], 0),",
     "    need(receipt['status'] == 'WITNESS_CAPTURED' and receipt['tracerExitCode'] == 0,",
     'test_tracer_exit_code_drift_is_not_coerced'),
    ('ws-profile-opcode-rsv-bare-equal', native,
     "    need(same_json(wire.get('opcode'), 1) and wire.get('fin') is True and same_json(wire.get('rsv'), 0),",
     "    need(wire.get('opcode') == 1 and wire.get('fin') is True and wire.get('rsv') == 0,",
     'test_ws_profile_opcode_drift_is_not_coerced'),
    ('session-profile-connections-bare-equal', native,
     "    need(same_json(contract.get('connections'), 1), 'session profile connections')",
     "    need(contract.get('connections') == 1, 'session profile connections')",
     'test_session_profile_connections_drift_is_not_coerced'),
    ('memory-rehearsal-without-lock', Path(__file__).resolve(),
     '        if memory:\n            lock = tree_' + 'lock(root, timeout=900)\n'
     '            try:\n                lock.__enter__()\n'
     '            except Exception as error:\n                startup_refusal(error)\n'
     '            try:\n                for row in selected:\n                    one(row)\n'
     '            finally:\n                lock.__exit__(None, None, None)\n        else:',
     '        if memory:\n            try:\n                for row in selected:\n                    one(row)\n'
     '            finally:\n                pass\n        else:',
     'test_memory_rehearsal_holds_tree_lock'),
]

# Static roster (#1666): the denominator of this run is a literal in the
# source, never len(cases) -- a dropped, renamed, duplicated or reordered row
# is a startup refusal (rc 2), not a smaller green run.
EXPECTED_NAMES = ('surfaces-frames-bare-not-equal',
                  'surfaces-durable-bare-not-equal',
                  'surfaces-applicable-bare-not-equal',
                  'surfaces-registry-fields-bare-not-equal',
                  'cleanup-replay-bare-equal',
                  'prelaunch-bare-equal',
                  'receipt-tracer-exit-bare-equal',
                  'ws-profile-opcode-rsv-bare-equal',
                  'session-profile-connections-bare-equal',
                  'memory-rehearsal-without-lock')

messages = {
    'surfaces-frames-bare-not-equal': '#2039 boolean drift must surface on frames',
    'surfaces-durable-bare-not-equal': '#2039 boolean drift must surface on durableStatus',
    'surfaces-applicable-bare-not-equal': '#2039 registry.applicable boolean drift must surface',
    'surfaces-registry-fields-bare-not-equal': '#2039 registry.events boolean drift must surface',
    'cleanup-replay-bare-equal': '#2039 cleanup allStopped=1 must not coerce to True',
    'prelaunch-bare-equal': '#2039 boolean drift in the prelaunch mirror is rejected',
    'receipt-tracer-exit-bare-equal': '#2039 tracer exit code boolean drift is rejected',
    'ws-profile-opcode-rsv-bare-equal': '#2039 ws profile opcode boolean drift is rejected',
    'session-profile-connections-bare-equal': '#2039 session profile connections boolean drift is rejected',
    'memory-rehearsal-without-lock': '#2039 same-json memory rehearsal holds the source tree lock',
}


def bound(test):
    # The pins live in the apparatus test entry's SameJsonSurfaces class;
    # resolve the full name before any run so an unknown test is an error,
    # never a silent pass.
    source = test_safety.read_text()
    if '    def ' + test + '(' not in source:
        raise ValueError('unknown bound test ' + test)
    return test_safety, 'SameJsonSurfaces.' + test


def preflight():
    errors = []
    for name, path, old, new, test in cases:
        source = path.read_text()
        try:
            if source.count(old) != 1:
                raise ValueError('anchor count ' + str(source.count(old)))
            compile(source.replace(old, new, 1), str(path), 'exec')
            bound(test)
        except Exception as error:
            errors.append(name + ': ' + str(error))
    print(json.dumps({'compiled': len(cases) - len(errors), 'errors': errors}))
    return not errors


def memory_run(name):
    row = next(row for row in cases if row[0] == name)
    _, path, old, new, test = row
    changed = path.read_bytes().replace(old.encode(), new.encode(), 1)
    compile(changed, str(path), 'exec')
    original_read = Path.read_bytes
    original_code = importlib.machinery.SourceFileLoader.get_code
    def read(p):
        return changed if p.resolve() == path else original_read(p)
    def code(loader, fullname):
        return compile(changed, str(path), 'exec') if Path(loader.path).resolve() == path else original_code(loader, fullname)
    fixture, full_name = bound(test)
    with patch.object(Path, 'read_bytes', read), patch.object(importlib.machinery.SourceFileLoader, 'get_code', code):
        spec = importlib.util.spec_from_file_location('bound_tests', fixture)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        suite = unittest.defaultTestLoader.loadTestsFromName(full_name, module)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def classify(result, test, marker):
    log = result.stdout + result.stderr
    if result.returncode == 0 and re.search(r'^OK$', log, re.M):
        return 'SURVIVED'
    # Only the named test's own assertion is a kill; unrelated errors/timeouts
    # and setup failures stay INVALID.
    if (result.returncode == 1 and re.search(r'^FAIL: ' + re.escape(test) + r' \(', log, re.M)
            and re.search(r'^AssertionError:[^\n]*' + re.escape(marker), log, re.M)
            and 'ERROR:' not in log and 'Ran 1 test in ' in log):
        return 'KILLED'
    return 'INVALID'


def terminate_cleanly(signum, _frame):
    """Unwind on SIGTERM/SIGHUP/SIGQUIT so the finally blocks restore the
    sources; the default disposition would leave a mutant in the tree and the
    parity lease held (mirror of session-domain-cleanup's #892 r23 form)."""
    if defer_signal_exit(signum):
        return
    raise SystemExit(128 + signum)


def install_terminate_handlers():
    signal.signal(signal.SIGTERM, terminate_cleanly)
    if hasattr(signal, 'SIGHUP'):
        signal.signal(signal.SIGHUP, terminate_cleanly)
    if hasattr(signal, 'SIGQUIT'):
        signal.signal(signal.SIGQUIT, terminate_cleanly)


def main():
    if sys.argv[1:2] == ['--memory-run']:
        return memory_run(sys.argv[2])
    try:
        registered(cases, EXPECTED_NAMES)
    except Exception as error:
        refuse('case roster drifted from the static inventory: ' + repr(error))
    if not preflight():
        return 2
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    arguments = sys.argv[1:]
    memory = '--memory' in arguments
    arguments = [a for a in arguments if a != '--memory']
    if len(arguments) not in (1, 3) or (len(arguments) == 3 and arguments[1] != '--case'):
        refuse('usage: negative_controls.py NEW_OUTPUT [--memory] [--case NAME] | --check-anchors')
    selected = cases if len(arguments) == 1 else [row for row in cases if row[0] == arguments[2]]
    if not selected:
        refuse('unknown mutation: ' + arguments[2])
    out = claim_dir(arguments[0])
    results = []
    def run(label, command):
        value = run_owned(command, cwd=root, env=None, timeout=60)
        (out / (label + '.log')).write_text(value.stdout + value.stderr)
        return value
    def one(row, restore=False):
        name, path, old, new, test = row
        status, reason = 'INVALID', 'not run'
        fixture, full = bound(test)
        command = [sys.executable, '-I', '-B', str(fixture), full]
        try:
            changed = path.read_text().replace(old, new, 1)
            compile(changed, str(path), 'exec')
            positive = run(name + '-positive', command)
            if positive.returncode != 0 or 'Ran 1 test in ' not in positive.stderr:
                raise ValueError('bound positive failed')
            if memory:
                result = run(name + '-mutant', [sys.executable, '-I', '-B', __file__, '--memory-run', name])
            else:
                # #1903: a truncating in-place write leaves a half-file on
                # SIGKILL; publish in one rename instead.
                write_source_atomic(root, path, changed.encode())
                result = run(name + '-mutant', command)
            status = classify(result, test, messages[name])
            reason = 'compiled; ' + full + '; ' + messages[name]
        except Exception as error:
            reason = repr(error)
        finally:
            if restore:
                write_source_atomic(root, path, originals[path])
        try:
            restored = run(name + '-restored', command)
            if restored.returncode != 0:
                status, reason = 'INVALID', 'restored bound test failed'
        except Exception as error:
            status, reason = 'INVALID', 'restored run failed: ' + repr(error)
        results.append({'name': name, 'status': status, 'reason': reason})
    def startup_refusal(error):
        # A guard that never started (dirty tree, lease or source-lock wait
        # exhausted, git or scanner timeout) is a startup refusal, not a red
        # run: rc 2 and no evidence dir that looks finished
        # (specs/negative-controls/README.md:142/:145/:167).  rmdir only
        # removes the empty dir claim_dir made -- a run that already wrote
        # logs keeps them.
        try:
            out.rmdir()
        except OSError:
            pass
        refuse('guard refused to start: ' + repr(error))

    try:
        # Startup is everything __enter__ does before the first mutation:
        # baseline checks, scanner staging, lease and lock acquisition.  Any
        # exception from that phase is a startup refusal regardless of class --
        # _git timeouts raise RuntimeError, the workspace scanner raises
        # subprocess.TimeoutExpired / CalledProcessError, tree_lock can raise
        # OSError on top of the TimeoutError already covered -- so the entry
        # is split from the run loop instead of widening the catch below:
        # restore-phase RuntimeError("restore failed") happens after mutations
        # began and must stay a red run (rc 1).
        if memory:
            lock = tree_lock(root, timeout=900)
            try:
                lock.__enter__()
            except Exception as error:
                startup_refusal(error)
            try:
                for row in selected:
                    one(row)
            finally:
                lock.__exit__(None, None, None)
        else:
            # The fixture guards its own bytes too: the memory-rehearsal case
            # mutates this file, so it must be snapshotted for restore.
            install_terminate_handlers()
            guard = mutating_sources(root, [session, native, common,
                                             Path(__file__).resolve()])
            try:
                originals = guard.__enter__()
            except Exception as error:
                startup_refusal(error)
            try:
                for row in selected:
                    one(row, True)
            finally:
                # Hand the in-flight exception (if any) to __exit__ so the
                # generator's restore finally runs and re-raises afterwards.
                guard.__exit__(*sys.exc_info())
    except Exception as error:
        results = [{'name': row[0], 'status': 'INVALID', 'reason': 'guard: ' + repr(error)} for row in selected]
    summary = {'mode': 'memory-only' if memory else 'guarded-source', 'results': results,
               'counts': {key: sum(row['status'] == key for row in results) for key in ('KILLED', 'SURVIVED', 'INVALID')}}
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps(summary, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(summary, indent=2))
    return verdict_exit(results, key='status')


if __name__ == '__main__':
    raise SystemExit(main())
