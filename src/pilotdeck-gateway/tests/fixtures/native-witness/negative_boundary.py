#!/usr/bin/env python3
"""#1206 source/document controls; all mutations require committed HEAD."""
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
gateway_doc = root / 'scripts/gateway-parity/README.md'
witness_doc = root / 'scripts/native-witness/README.md'
controls_doc = root / 'src/pilotdeck-gateway/tests/fixtures/session-domain-native/README.md'
session = root / 'scripts/gateway-parity/session_domain.py'
test_file = Path(__file__).with_name('test_boundary.py')
cases = [
    ('gateway-independent-claim', gateway_doc, '只防协作写者', 'driver 记录独立锚', 'test_documented_boundary'),
    ('witness-independent-claim', witness_doc, '能够同时重写三者的对抗写者不在此威胁模型内', 'driver 记录提供独立锚', 'test_documented_boundary'),
    ('controls-independent-claim', controls_doc, '不防共同重钉', '独立锚', 'test_documented_boundary'),
    ('drop-consistency-check', session, "isinstance(driver, dict) and driver.get('binding') == contract['binding']", 'True', 'test_binding_writer_boundary'),
]
messages = {
    'test_documented_boundary': '#1206 document the cooperative-writer boundary',
    'test_binding_writer_boundary': '#1206 observer-only edit rejected, joint rewrite is a known boundary',
}


def preflight():
    errors = []
    for name, path, old, new, test in cases:
        text = path.read_text()
        try:
            if text.count(old) != 1:
                raise ValueError(f'STALE ANCHOR: {text.count(old)} matches')
            if path.suffix == '.py':
                compile(text.replace(old, new, 1), str(path), 'exec')
            compile(test_file.read_bytes(), str(test_file), 'exec')
        except Exception as error:
            errors.append(name + ': ' + str(error))
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'#1206 {len(cases)} unique anchors; Python mutant and bound tests compile; Markdown has no compiler')


def passed(result):
    log = result.stdout + result.stderr
    return (result.returncode == 0 and re.search(r'^Ran 1 test in ', log, re.M)
            and re.search(r'^OK$', log, re.M))


def main():
    preflight()
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative_boundary.py NEW_OUTPUT | --check-anchors')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(name, test):
        # No Rust source/build: wait_for_slot is unnecessary for Python controls.
        result = run_owned([sys.executable, '-I', str(test_file), 'BoundaryTests.' + test],
                           cwd=root, env=os.environ.copy(), timeout=60)
        (out / (name + '.log')).write_text(result.stdout + result.stderr)
        return result

    paths = [gateway_doc, witness_doc, controls_doc, session, test_file, Path(__file__).resolve()]
    try:
        with mutating_sources(root, paths) as originals:
            for name, path, old, new, test in cases:
                status, reason = 'INVALID', ''
                try:
                    text = originals[path].decode()
                    if text.count(old) != 1:
                        raise ValueError('STALE ANCHOR ' + name)
                    if not passed(run(name + '-positive', test)):
                        raise ValueError('positive bound test did not pass')
                    path.write_text(text.replace(old, new, 1))
                    if path.suffix == '.py':
                        compile(path.read_bytes(), str(path), 'exec')
                    compile(test_file.read_bytes(), str(test_file), 'exec')
                    (out / (name + '-compile.log')).write_text('Python compile: PASS; Markdown compilation: N/A\n')
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
