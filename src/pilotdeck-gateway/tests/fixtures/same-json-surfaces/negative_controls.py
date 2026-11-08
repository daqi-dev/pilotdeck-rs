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
import unittest
from unittest.mock import patch

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, tree_lock

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
]

messages = {
    'surfaces-frames-bare-not-equal': '#2039 boolean drift must surface on frames',
    'surfaces-durable-bare-not-equal': '#2039 boolean drift must surface on durableStatus',
    'surfaces-applicable-bare-not-equal': '#2039 registry.applicable boolean drift must surface',
    'surfaces-registry-fields-bare-not-equal': '#2039 registry.events boolean drift must surface',
    'cleanup-replay-bare-equal': '#2039 cleanup allStopped=1 must not coerce to True',
    'prelaunch-bare-equal': '#2039 boolean drift in the prelaunch mirror is rejected',
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


def main():
    if sys.argv[1:2] == ['--memory-run']:
        return memory_run(sys.argv[2])
    if not preflight():
        return 2
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    arguments = sys.argv[1:]
    memory = '--memory' in arguments
    arguments = [a for a in arguments if a != '--memory']
    if len(arguments) not in (1, 3) or (len(arguments) == 3 and arguments[1] != '--case'):
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT [--memory] [--case NAME] | --check-anchors')
    selected = cases if len(arguments) == 1 else [row for row in cases if row[0] == arguments[2]]
    if not selected:
        raise SystemExit('unknown mutation')
    out = Path(arguments[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
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
                path.write_text(changed)
                result = run(name + '-mutant', command)
            status = classify(result, test, messages[name])
            reason = 'compiled; ' + full + '; ' + messages[name]
        except Exception as error:
            reason = repr(error)
        finally:
            if restore:
                path.write_bytes(originals[path])
        try:
            restored = run(name + '-restored', command)
            if restored.returncode != 0:
                status, reason = 'INVALID', 'restored bound test failed'
        except Exception as error:
            status, reason = 'INVALID', 'restored run failed: ' + repr(error)
        results.append({'name': name, 'status': status, 'reason': reason})
    try:
        if memory:
            with tree_lock(root, timeout=900):
                for row in selected:
                    one(row)
        else:
            with mutating_sources(root, [session, native, common]) as originals:
                for row in selected:
                    one(row, True)
    except Exception as error:
        results = [{'name': row[0], 'status': 'INVALID', 'reason': 'guard: ' + repr(error)} for row in selected]
    summary = {'mode': 'memory-only' if memory else 'guarded-source', 'results': results,
               'counts': {key: sum(row['status'] == key for row in results) for key in ('KILLED', 'SURVIVED', 'INVALID')}}
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps(summary, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(summary, indent=2))
    return 0 if len(results) == len(selected) and all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
