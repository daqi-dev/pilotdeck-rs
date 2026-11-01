#!/usr/bin/env python3
"""#1206 controls: memory rehearsal or guarded committed-source mutations."""
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
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
gateway_doc = root / 'scripts/gateway-parity/README.md'
witness_doc = root / 'scripts/native-witness/README.md'
controls_doc = root / 'src/pilotdeck-gateway/tests/fixtures/session-domain-native/README.md'
session = root / 'scripts/gateway-parity/session_domain.py'
common = root / 'scripts/gateway-parity/common.py'
test_file = Path(__file__).with_name('test_boundary.py')
guarded_sources = [common, gateway_doc, witness_doc, controls_doc, session, test_file, Path(__file__).resolve()]
cases = [
    ('omit-common-guard', Path(__file__).resolve(), 'guarded_' + 'sources = [common,', 'guarded_' + 'sources = [', 'test_guarded_sources_include_common'),
    ('gateway-independent-claim', gateway_doc, '只防协作写者', 'driver 记录独立锚', 'test_documented_boundary'),
    ('witness-observer-scope', witness_doc, 'observer 记录独立于被测网关与预注册 contract，不独立于 driver。', 'observer 独立于 driver。', 'test_documented_boundary'),
    ('witness-hello-scope', witness_doc, 'hello 观测独立于被测网关与预注册 contract，不独立于 driver。', 'hello 观测独立于 driver。', 'test_documented_boundary'),
    ('witness-independent-claim', witness_doc, '能够同时重写三者的对抗写者不在此威胁模型内', 'driver 记录提供独立锚', 'test_documented_boundary'),
    ('controls-independent-claim', controls_doc, '不防共同重钉', '独立锚', 'test_documented_boundary'),
    ('drop-consistency-check', session, "isinstance(driver, dict) and driver.get('binding') == contract['binding']", 'True', 'test_binding_writer_boundary'),
]
messages = {
    'test_guarded_sources_include_common': '#a5b parity mutation guards include common.py',
    'test_documented_boundary': '#1206 document the cooperative-writer boundary',
    'test_binding_writer_boundary': '#1206 observer-only edit rejected, joint rewrite is a known boundary',
}


def preflight():
    errors = []
    test_source = test_file.read_text()
    compile(test_source, str(test_file), 'exec')
    for name, path, old, new, test in cases:
        text = path.read_text()
        try:
            if test not in messages or '    def ' + test + '(' not in test_source:
                raise ValueError('unknown bound test or assertion marker: ' + test)
            if text.count(old) != 1:
                raise ValueError(f'STALE ANCHOR: {text.count(old)} matches')
            if path.suffix == '.py':
                compile(text.replace(old, new, 1), str(path), 'exec')
        except Exception as error:
            errors.append(name + ': ' + str(error))
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'#1206 {len(cases)} unique anchors; Python mutant and bound tests compile; Markdown has no compiler')


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
        suite = unittest.defaultTestLoader.loadTestsFromName('BoundaryTests.' + test, module)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def main():
    if sys.argv[1:2] == ['--memory-run']:
        return memory_run(sys.argv[2])
    if sys.argv[1:] == ['--check-anchors']:
        preflight()
        return 0
    arguments = sys.argv[1:]
    memory = '--memory' in arguments
    arguments = [arg for arg in arguments if arg != '--memory']
    if len(arguments) not in (1, 3) or (len(arguments) == 3 and arguments[1] != '--case'):
        raise SystemExit('usage: negative_boundary.py NEW_OUTPUT [--memory] [--case NAME] | --check-anchors')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    selected = cases if len(arguments) == 1 else [row for row in cases if row[0] == arguments[2]]
    if not selected:
        raise SystemExit('unknown mutation')
    out = Path(arguments[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(name, test, mutant=None):
        # No Rust source/build: wait_for_slot is unnecessary for Python controls.
        command = ([sys.executable, '-I', __file__, '--memory-run', mutant] if mutant else
                   [sys.executable, '-I', str(test_file), 'BoundaryTests.' + test])
        result = run_owned(command,
                           cwd=root, env=os.environ.copy(), timeout=60)
        (out / (name + '.log')).write_text(result.stdout + result.stderr)
        return result

    def one(row, restore=False):
        name, path, old, new, test = row
        status, reason = 'INVALID', ''
        try:
            if test not in messages or '    def ' + test + '(' not in test_file.read_text():
                raise ValueError('unknown bound test or assertion marker: ' + test)
            text = path.read_text()
            if text.count(old) != 1:
                raise ValueError('STALE ANCHOR ' + name)
            if not passed(run(name + '-positive', test)):
                raise ValueError('positive bound test did not pass')
            changed = text.replace(old, new, 1)
            if path.suffix == '.py':
                compile(changed, str(path), 'exec')
            if not memory:
                path.write_text(changed)
            (out / (name + '-compile.log')).write_text('Python compile: PASS; Markdown compilation: N/A\n')
            result = run(name + '-negative', test, name if memory else None)
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
            if restore:
                path.write_bytes(originals[path])
        try:
            if not passed(run(name + '-restored', test)):
                status, reason = 'INVALID', reason + '; restored control failed'
        except Exception as error:
            status, reason = 'INVALID', reason + '; restored: ' + str(error)
        results.append({'name': name, 'status': status, 'reason': reason})
    try:
        if memory:
            for row in selected:
                one(row)
        else:
            with mutating_sources(root, guarded_sources) as originals:
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
