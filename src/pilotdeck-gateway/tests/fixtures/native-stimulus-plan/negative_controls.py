#!/usr/bin/env python3
"""#1368 guarded Python mutations, with per-mutant Rust no-run preflight."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import json
import os
from pathlib import Path
import re

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import invalidate_completed, publish_summary

source = root / 'scripts/native-witness/stimulus_plan.py'
collector = root / 'scripts/native-witness/collect.py'
fixture = Path(__file__).with_name('test_plan.py')
cases = [
    ('missing-cell', source, "for cell in CELLS for variant in cell['variants'] for phase in ('model', 'tool')]", "for cell in CELLS for variant in cell['variants'] for phase in ('model',)]", 'test_exact_registration'),
    ('wrong-stimulus', source, "{'name': 'status-slow', 'slowStatus': True}", "{'name': 'status-slow-wrong', 'slowStatus': True}", 'test_exact_registration'),
    ('wrong-stimulus-field', source, "{'name': 'status-slow', 'slowStatus': True}", "{'name': 'status-slow', 'slowStatus': False}", 'test_exact_registration'),
    ('wrong-fault-target', source, "event(kind, key=variant[field], error={'code': 'EIO'})", "event(kind, key='wrong-key', error={'code': 'EIO'})", 'test_boundary_coverage'),
    ('missing-boundary', source, "'order': order, 'boundaries': targets}", "'order': order, 'boundaries': targets[:-1]}", 'test_boundary_coverage'),
    ('missing-reasons', source, "'missing': [item['name'] for item in stimulus['boundaries']]", "'missing': []", 'test_unwitnessed_is_not_run'),
    ('same-name-is-executed', collector, "if execution['status'] != 'WITNESSED':", 'if False:', 'test_unwitnessed_is_not_run'),
    ('manifest-gap-accepted', collector, "w.need([item['case'] for item in stimuli.plans()] == cases,", 'w.need(True,', 'test_manifest_rejects_gap'),
]
messages = {
    'missing-cell': '#1368 all 72 inputs match TS preregistration',
    'wrong-stimulus': '#1368 all 72 inputs match TS preregistration',
    'wrong-stimulus-field': '#1368 all 72 inputs match TS preregistration',
    'wrong-fault-target': '#1368 every registered stimulus has its precise target boundary',
    'missing-boundary': '#1368 every registered stimulus has its precise target boundary',
    'missing-reasons': '#1368 same-name requests cannot execute an unwitnessed stimulus',
    'same-name-is-executed': '#1368 same-name requests cannot execute an unwitnessed stimulus',
    'manifest-gap-accepted': '#1368 manifest refuses a missing plan before launch',
}


def preflight():
    errors = []
    for name, path, before, after, _test in cases:
        text = path.read_text()
        if text.count(before) != 1:
            errors.append('STALE ANCHOR ' + name)
        else:
            try:
                compile(text.replace(before, after, 1), str(path), 'exec')
            except SyntaxError as error:
                errors.append('INVALID compile ' + name + ': ' + str(error))
    if errors:
        raise SystemExit('\n'.join(errors))
    print(f'#1368 {len(cases)} unique anchors; all Python mutants compile')


def passed(result):
    log = result.stdout + result.stderr
    return result.returncode == 0 and re.search(r'^Ran 1 test in ', log, re.M) and re.search(r'^OK$', log, re.M)


def classify(result, test, marker):
    log = result.stdout + result.stderr
    if passed(result):
        return 'SURVIVED'
    if (result.returncode == 1 and re.search(r'^FAIL: ' + re.escape(test)
            + r' \(__main__\.PlanTests\.' + re.escape(test) + r'\)', log, re.M)
            and re.search(r'^AssertionError:[^\n]*' + re.escape(marker), log, re.M)
            and re.search(r'^Ran 1 test in ', log, re.M) and 'FAILED (failures=' in log):
        return 'KILLED'
    return 'INVALID'


def main():
    preflight()
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT | --check-anchors')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by caller')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(label, test):
        result = run_owned([sys.executable, '-I', str(fixture), 'PlanTests.' + test],
                           cwd=root, env=os.environ.copy(), timeout=30)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    try:
        paths = [source, collector, fixture, Path(__file__).resolve(),
                 Path(__file__).with_name('preregistration.mjs'),
                 root / 'src/pilotdeck-gateway/tests/native_stimulus_plan.rs']
        with mutating_sources(root, paths) as originals:
            for name, path, before, after, test in cases:
                status, reason, compiled = 'INVALID', '', False
                try:
                    text = originals[path].decode()
                    if text.count(before) != 1:
                        raise ValueError('STALE ANCHOR ' + name)
                    changed = text.replace(before, after, 1)
                    compile(changed, str(path), 'exec')
                    if not passed(run(name + '-positive', test)):
                        raise ValueError('positive bound test did not pass')
                    path.write_text(changed)
                    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
                    (out / (name + '-slots.log')).write_text(slot.stdout + slot.stderr)
                    build = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                                       '-p', 'pilotdeck-gateway', '--test', 'native_stimulus_plan', '--no-run'],
                                      cwd=root, env=os.environ.copy(), timeout=900)
                    (out / (name + '-compile.log')).write_text(build.stdout + build.stderr)
                    if build.returncode != 0:
                        raise ValueError('mutant no-run failed')
                    compiled = True
                    status = classify(run(name + '-negative', test), test, messages[name])
                    reason = {'KILLED': 'bound assertion matched', 'SURVIVED': 'bound test passed',
                              'INVALID': 'missing bound runtime assertion'}[status]
                except Exception as error:
                    status, reason = 'INVALID', str(error)
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not passed(run(name + '-restored', test)):
                        status, reason = 'INVALID', reason + '; restored control failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + '; restore: ' + str(error)
                results.append({'name': name, 'status': status, 'compiled': compiled, 'reason': reason})
    except BaseException as error:
        invalidate_completed(results, 'source guard: ' + str(error))
        for name, *_ in cases[len(results):]:
            results.append({'name': name, 'status': 'INVALID', 'compiled': False,
                            'reason': 'guard refusal: ' + str(error)})
    # Shared publisher atomically renames only after the guard verifies restore.
    publish_summary(out, results)
    print(json.dumps(results, ensure_ascii=False, indent=2))
    return 0 if len(results) == len(cases) and all(r['status'] == 'KILLED' for r in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
