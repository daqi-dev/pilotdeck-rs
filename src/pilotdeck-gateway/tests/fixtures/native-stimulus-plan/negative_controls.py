#!/usr/bin/env python3
"""#1368 guarded Python mutations, with per-mutant Rust no-run preflight."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import json
import argparse
import importlib.machinery
import importlib.util
import os
from contextlib import suppress
from pathlib import Path
import re
import unittest
from unittest.mock import patch

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, tree_lock
from negative_process import target_environment
from negative_suite import invalidate_completed, publish_summary

source = root / 'scripts/native-witness/stimulus_plan.py'
driver = root / 'scripts/native-witness/stimulus_driver.py'
collector = root / 'scripts/native-witness/collect.py'
fixture = Path(__file__).with_name('test_plan.py')
measurement = Path(__file__).with_name('verify_measurement.py')
runner = Path(__file__).resolve()
e2e_runner = fixture.parent.parent / 'native-session-e2e/negative_controls.py'
r11 = fixture.parent.parent / 'native-witness/test_r11.py'
PUBLISH_DIR = root / '.negctl-publish'
# fix-r2 (#1528 review): the guarded branch below holds only this tree's
# .negative-source.lock, acquired by mutating_sources' internal tree lock
# with the shared default budget (900 s). Every path it guards is outside
# scripts/gateway-parity/, so the shared parity lease is a nullcontext for
# this runner and no cross-tree window can starve the acquire -- same
# lock and same budget as the memory rehearsal below.
SLOT_WAIT_SECONDS = 900
BUILD_SECONDS = 900
BOUND_TEST_SECONDS = 30


def write_atomic(path, data):
    """Publish the source in one rename (#1692 §9 r2 review).

    `path.write_text` truncates first: a SIGKILL in the middle leaves a half
    file that `mutating_sources` then reads as "differs from HEAD" and refuses
    to run over. The temp lives at the repository root, outside the scanned
    `src` area -- that location is the protection. The `.gitignore`d
    `.negctl-*` name only keeps `git status` clean: the preflight scan uses
    `git ls-files --others` and includes ignored files on purpose.
    """
    PUBLISH_DIR.mkdir(parents=True, exist_ok=True)
    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'
    mode = path.stat().st_mode & 0o7777
    try:
        with tmp.open('wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    finally:
        # fix-r12 (review DS r11 sibling #1, same family as write_source_atomic):
        # the cleanup unlink is the only swallowed error, so a failed publish
        # always surfaces its own error instead of a PermissionError here.
        with suppress(OSError):
            tmp.unlink(missing_ok=True)


cases = [
    ('missing-cell', source, "for cell in CELLS for variant in cell['variants'] for phase in ('model', 'tool')]", "for cell in CELLS for variant in cell['variants'] for phase in ('model',)]", 'test_exact_registration'),
    ('wrong-stimulus', source, "{'name': 'status-slow', 'slowStatus': True}", "{'name': 'status-slow-wrong', 'slowStatus': True}", 'test_exact_registration'),
    ('wrong-stimulus-field', source, "{'name': 'status-slow', 'slowStatus': True}", "{'name': 'status-slow', 'slowStatus': False}", 'test_exact_registration'),
    ('wrong-fault-target', source, "event(kind, key=variant[field], error={'code': 'EIO'})", "event(kind, key='wrong-key', error={'code': 'EIO'})", 'test_boundary_coverage'),
    ('missing-boundary', source, "'order': order, 'boundaries': targets}", "'order': order, 'boundaries': targets[:-1]}", 'test_boundary_coverage'),
    ('missing-reasons', source, "'missing': [item['name'] for item in stimulus['boundaries']]", "'missing': []", 'test_unwitnessed_is_not_run'),
    ('same-name-is-executed', collector, "if execution['status'] != 'WITNESSED':", 'if False:', 'test_unwitnessed_is_not_run'),
    ('manifest-gap-accepted', collector, "w.need([item['case'] for item in stimuli.plans()] == cases,", 'w.need(True,', 'test_manifest_rejects_gap'),
    ('manifest-duplicate-accepted', collector, "w.need([item['case'] for item in stimuli.plans()] == cases,", 'w.need(True,', 'test_manifest_rejects_duplicate'),
    ('unknown-as-duplicate', source, "raise ValueError('#1368 unknown stimulus case: ' + str(case))", "raise ValueError('#1368 duplicate stimulus case: ' + str(case))", 'test_unknown_plan_reason'),
    ('duplicate-as-unknown', source, "raise ValueError('#1368 duplicate stimulus case: ' + str(case))", "raise ValueError('#1368 unknown stimulus case: ' + str(case))", 'test_duplicate_plan_reason'),
    ('empty-declaration-witnessed', measurement, "bool(target['events'])", 'True', 'test_empty_boundary_not_witnessed'),
    ('unchecked-reports-pass', measurement, "'NOT_RUN' if unchecked else 'PASS'", "'PASS'", 'test_measurement_unchecked_not_pass'),
    ('missing-phase-order', source, "[emitted, prefix, loop_wait], [(emitted, loop_wait)]", "[emitted, prefix, loop_wait], []", 'test_boundary_coverage'),
    ('phase-client-order', source, "[(emitted, loop_wait)]", "[(prefix, loop_wait)]", 'test_phase_requires_prefix_before_wait'),
    ('phase-enqueue-with-registry', source, "'registry.append.attempt' if cell['registry'] else 'queue.enqueue'", "'queue.enqueue'", 'test_phase_requires_prefix_before_wait'),
    ('missing-fault-order', source, "[entered, failed], [(entered, failed)]", "[entered, failed], []", 'test_fault_requires_entry_before_failure'),
    ('phase-order-not-verified', measurement, "min(positions(left)) < min(positions(right))", "True", 'test_phase_requires_prefix_before_wait'),
    ('hardcoded-measurement-count', measurement, 'if seen != expected:', 'if seen != expected or len(seen) != 360:', 'test_measurement_registered_denominator'),
    ('measurement-id-check-disabled', measurement, 'if seen != expected:', 'if False:', 'test_measurement_registered_denominator'),
    ('zero-repeats-accepted', measurement, "if type(registration['repeats']) is not int or registration['repeats'] <= 0:", 'if False:', 'test_measurement_rejects_empty_repeats'),
    ('invalid-repeats-type-accepted', measurement, "type(registration['repeats']) is not int", 'False', 'test_measurement_rejects_empty_repeats'),
    ('unsafe-plan-target', runner, '\n    return target_environment(root)\n', '\n    return os.environ.copy()\n', 'test_runner_target_environment'),
    ('unsafe-e2e-target', e2e_runner, 'return target_environment(root)', 'return os.environ.copy()', 'test_runner_target_environment'),
    ('registry-free-control-without-mock', r11, 'fixture._collect(output, real_settle=True, synthetic_stimuli=True)', 'fixture._collect(output, real_settle=True, synthetic_stimuli=False)', 'test_registry_free_settle_synthetic_control'),
    ('empty-order-witnessed', measurement, "if target['name'] != 'registry' and not target['before']:", 'if False:', 'test_empty_order_not_witnessed'),
    ('verify-order-guard-skipped', measurement, "                require_order(target)\n                if not target['events']:", "                if not target['events']:", 'test_measurement_rejects_empty_order'),
    ('empty-before-control-skipped', measurement, '                        witnessed(damaged, timeline)\n                    except ValueError as error:', "                        raise ValueError('#1368 missing required order edges: ' + target['name'])\n                    except ValueError as error:", 'test_measurement_empty_order_controls'),
    ('empty-before-count-omitted', measurement, 'empty_before_controls += 1', 'empty_before_controls += 0', 'test_measurement_empty_order_controls'),
    ('hardcoded-unchecked-count', fixture, "len(result['uncheckedBoundaries']) == case_count * repeats", "len(result['uncheckedBoundaries']) == 360", 'test_measurement_unchecked_not_pass'),
    # #1530: the literals above track the frozen calibration total; the pin
    # test fails the moment they go stale, and this mutant proves it can fail.
    ('stale-hardcoded-count', runner, 'len(se' + 'en) != 360', 'len(seen) != 361', 'test_hardcoded_counts_track_calibration'),
    # fix-r1 (#1528 review): the stale-literal proof above only covered the
    # measurement half of the pin; the unchecked half needs its own, or
    # weakening that comparison alone would keep every control green.
    ('stale-hardcoded-unchecked-count', runner, "len(result['uncheckedBoundar" + "ies']) == 360", "len(result['uncheckedBoundaries']) == 361", 'test_hardcoded_counts_track_calibration'),
    ('registry-wait-control-without-mock', r11, 'fixture._collect(output, real_settle=True, after_final=after_final,\n                                 synthetic_stimuli=True)', 'fixture._collect(output, real_settle=True, after_final=after_final,\n                                 synthetic_stimuli=False)', 'test_registry_free_settle_synthetic_control'),
    ('memory-plan-launches-cargo', runner, '            if not args.memory:\n                slot = wait_for_slot(', '            if True:\n                slot = wait_for_slot(', 'test_memory_runners_do_not_compile_tree'),
    ('memory-e2e-launches-cargo', e2e_runner, '            if not memory:\n                slot = wait_for_slot(', '            if True:\n                slot = wait_for_slot(', 'test_memory_runners_do_not_compile_tree'),
    ('memory-plan-claims-compiled', runner, "        status, reason, compiled = 'INVALID', '', False\n        try:", "        status, reason, compiled = 'INVALID', '', True\n        try:", 'test_memory_runners_do_not_compile_tree'),
    ('memory-e2e-claims-compiled', e2e_runner, "        status, reason, compiled = 'INVALID', '', False\n        try:", "        status, reason, compiled = 'INVALID', '', True\n        try:", 'test_memory_runners_do_not_compile_tree'),
    ('empty-before-wrong-error-counted', measurement, "if str(error) != '#1368 missing required order edges: ' + target['name']:", 'if False:', 'test_measurement_empty_order_controls'),
    ('memory-rehearsal-without-lock', runner, '        if args.memory:\n            with tree_' + 'lock(root, timeout=900):\n                for row in selected:\n                    one(row)\n        else:', '        if args.memory:\n            for row in selected:\n                one(row)\n        else:', 'test_memory_rehearsal_holds_tree_lock'),
    # #2396 adapter mutants: the attestation seam and the choreography notes
    # the new AdapterTests pin with markers.
    ('adapter-registry-closure-skipped', source, "        else:\n            if receipt != 'off':\n                absent.append('closure receipt (native run registry off)')", "        else:\n            if False:\n                absent.append('closure receipt (native run registry off)')", 'AdapterTests.test_adapter_registry_closure_requires_product_receipt'),
    ('adapter-cross-case-accepted', source, "        inputs, waits = _loop(evidence, 'input', plan_id), _loop(evidence, 'wait', plan_id)", "        inputs, waits = _loop(evidence, 'input', None), _loop(evidence, 'wait', None)", 'AdapterTests.test_adapter_cross_case_receipts_do_not_attest'),
    ('adapter-missing-not-precise', source, "            missing.append(target['name'] + ': ' + '; '.join(absent))", "            missing.append(target['name'] + ': ')", 'AdapterTests.test_adapter_precise_missing_per_target'),
    ('adapter-witness-loses-target', source, "            witnesses.append({'target': target['name'], 'evidence': found})", "            witnesses.append({'target': '', 'evidence': found})", 'AdapterTests.test_driver_slow_status_choreography'),
    ('driver-kp-release-order-unpinned', driver, "        state['notes']['returnReleaseAfterFinal'] = bool(\n            returns and state.get('finalAt') is not None\n            and returns[0]['monotonic'] >= state['finalAt'])", "        state['notes']['returnReleaseAfterFinal'] = bool(\n            returns and state.get('finalAt') is not None\n            and returns[0]['monotonic'] < state['finalAt'])", 'AdapterTests.test_driver_known_final_pending_release'),
    ('driver-arms-special-case-broken', driver, "        key = 'error:turn_timeout' if (append == 'turn_timeout'\n                                       and variant.get('failStatus') == 'turn_timeout') \\", "        key = 'error:turn_timeout' if (append == 'turn_timeout'\n                                       and variant.get('failStatus') in ('turn_timeout', 'gateway_submit_failed')) \\", 'AdapterTests.test_storage_arms_match_the_arming_table'),
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
    'manifest-duplicate-accepted': '#1368 manifest refuses duplicate stimulus plans',
    'unknown-as-duplicate': '#1368 unknown stimulus has a distinct diagnostic',
    'duplicate-as-unknown': '#1368 duplicate stimulus has a distinct diagnostic',
    'empty-declaration-witnessed': '#1368 empty boundary declaration never witnesses execution',
    'unchecked-reports-pass': '#1368 unchecked measurement boundaries cannot report PASS',
    'missing-phase-order': '#1368 every registered stimulus has its precise target boundary',
    'phase-client-order': '#1368 phase production must precede controlled loop wait',
    'phase-enqueue-with-registry': '#1368 phase production must precede controlled loop wait',
    'missing-fault-order': '#1368 faults must follow their real write entry',
    'phase-order-not-verified': '#1368 phase production must precede controlled loop wait',
    'hardcoded-measurement-count': '#1368 measurement denominator follows registered repeats and exact IDs',
    'measurement-id-check-disabled': '#1368 measurement denominator follows registered repeats and exact IDs',
    'zero-repeats-accepted': '#1368 measurement repeats must be positive',
    'invalid-repeats-type-accepted': '#1368 measurement repeats must be positive',
    'unsafe-plan-target': '#1368 both negative runners reject unsafe or missing cargo targets',
    'unsafe-e2e-target': '#1368 both negative runners reject unsafe or missing cargo targets',
    'registry-free-control-without-mock': '#1368 registry-free settle controls explicitly attest synthetic stimuli',
    'empty-order-witnessed': '#1368 every ordered boundary rejects empty before',
    'verify-order-guard-skipped': '#1368 verifier independently rejects missing order before skipping events',
    'empty-before-control-skipped': '#1368 empty-before data controls cover every ordered measurement',
    'empty-before-count-omitted': '#1368 empty-before data controls cover every ordered measurement',
    'hardcoded-unchecked-count': '#1368 unchecked measurement boundaries cannot report PASS',
    'stale-hardcoded-count': '#1530 hardcoded count mutants track the frozen calibration total',
    'stale-hardcoded-unchecked-count': '#1530 hardcoded count mutants track the frozen calibration total',
    'registry-wait-control-without-mock': '#1368 registry-free settle controls explicitly attest synthetic stimuli',
    'memory-plan-launches-cargo': '#1368 memory runners never launch cargo or claim a mutated build',
    'memory-e2e-launches-cargo': '#1368 memory runners never launch cargo or claim a mutated build',
    'memory-plan-claims-compiled': '#1368 memory runners never launch cargo or claim a mutated build',
    'memory-e2e-claims-compiled': '#1368 memory runners never launch cargo or claim a mutated build',
    'empty-before-wrong-error-counted': '#1368 empty-before data controls cover every ordered measurement',
    'memory-rehearsal-without-lock': '#1398 stimulus-plan memory rehearsal holds the source tree lock',
    'adapter-registry-closure-skipped': '#2396 no-registry closure is the product receipt, never file absence alone',
    'adapter-cross-case-accepted': '#2396 receipts keyed to another plan never witness this case',
    'adapter-missing-not-precise': '#2396 a target without its raw receipts keeps the row NOT_RUN with a precise missing',
    'adapter-witness-loses-target': '#2396 slowStatus: block gates the outcome release, FIFO then release precedes final',
    'driver-kp-release-order-unpinned': '#2396 K-P: known outcome first, return held, released only after the final frame',
    'driver-arms-special-case-broken': '#2396 per-case arms follow plan section 6, one barrier-or-fault per site',
}


def full_test(test):
    """Bound test name with its class; #2396 added the AdapterTests family."""
    return test if test.startswith(('PlanTests.', 'AdapterTests.')) else 'PlanTests.' + test


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
    short = re.escape(full_test(test).split('.')[-1])
    if (result.returncode == 1 and re.search(r'^FAIL: ' + short
            + r' \((?:__main__|bound_tests)\.(?:PlanTests|AdapterTests)\.' + short + r'\)', log, re.M)
            and re.search(r'^AssertionError:[^\n]*' + re.escape(marker), log, re.M)
            and re.search(r'^Ran 1 test in ', log, re.M) and 'FAILED (failures=' in log):
        return 'KILLED'
    return 'INVALID'


def memory_run(name):
    """Read-only rehearsal on an uncommitted tree; never a source-guard bypass."""
    _, path, before, after, test = next(row for row in cases if row[0] == name)
    changed = path.read_bytes().replace(before.encode(), after.encode(), 1)
    compile(changed, str(path), 'exec')
    original_read = Path.read_bytes
    original_code = importlib.machinery.SourceFileLoader.get_code

    def read(p):
        return changed if p.resolve() == path else original_read(p)

    def code(loader, fullname):
        return compile(changed, str(path), 'exec') if Path(loader.path).resolve() == path else original_code(loader, fullname)

    with patch.object(Path, 'read_bytes', read), patch.object(importlib.machinery.SourceFileLoader, 'get_code', code):
        spec = importlib.util.spec_from_file_location('bound_tests', fixture)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        suite = unittest.defaultTestLoader.loadTestsFromName(full_test(test), module)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def execution_environment():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by caller')
    # #1402: this guard pair is byte-duplicated in native-session-e2e's
    # runner; consolidating it means changing the shared negative_process.py,
    # which belongs to the M6 lane (#1099). The #1528 family batch did
    # change that shared file once, under coordinator dispensation (the
    # parity-lease acquire budget, min(lock_timeout, 60) -> the caller's
    # value, for the e2e runner's queueing budget); every path this runner
    # guards is outside scripts/gateway-parity/, so that acquire is a
    # nullcontext here (see the fix-r2 note above the constants). M6 #1099
    # has no active lane and no sibling branch edits that file, so no
    # parallel conflict; coordinator-approved 2026-11-13, retained pending
    # human re-review.
    return target_environment(root)


def main():
    if sys.argv[1:2] == ['--memory-run']:
        return memory_run(sys.argv[2])
    preflight()
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    parser = argparse.ArgumentParser()
    parser.add_argument('output', type=Path)
    parser.add_argument('--memory', action='store_true', help='read-only Python rehearsal, not guarded-source evidence')
    parser.add_argument('--case', choices=[row[0] for row in cases])
    args = parser.parse_args()
    env = execution_environment()
    selected = [row for row in cases if args.case is None or row[0] == args.case]
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(label, test, mutant=None):
        command = ([sys.executable, '-I', __file__, '--memory-run', mutant] if mutant else
                   [sys.executable, '-I', str(fixture), full_test(test)])
        result = run_owned(command, cwd=root, env=env, timeout=BOUND_TEST_SECONDS)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    def one(row):
        name, path, before, after, test = row
        status, reason, compiled = 'INVALID', '', False
        try:
            text = path.read_text()
            if text.count(before) != 1:
                raise ValueError('STALE ANCHOR ' + name)
            changed = text.replace(before, after, 1)
            compile(changed, str(path), 'exec')
            if not passed(run(name + '-positive', test)):
                raise ValueError('positive bound test did not pass')
            if not args.memory:
                write_atomic(path, changed.encode())
            if not args.memory:
                slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=SLOT_WAIT_SECONDS)
                (out / (name + '-slots.log')).write_text(slot.stdout + slot.stderr)
                build = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                                   '-p', 'pilotdeck-gateway', '--test', 'native_stimulus_plan', '--no-run'],
                                  cwd=root, env=env, timeout=BUILD_SECONDS)
                (out / (name + '-compile.log')).write_text(build.stdout + build.stderr)
                if build.returncode != 0:
                    raise ValueError('no-run failed')
                compiled = True
            status = classify(run(name + '-negative', test, name if args.memory else None), test, messages[name])
            reason = {'KILLED': 'bound assertion matched', 'SURVIVED': 'bound test passed',
                      'INVALID': 'missing bound runtime assertion'}[status]
        except Exception as error:
            status, reason = 'INVALID', str(error)
        finally:
            if not args.memory:
                write_atomic(path, originals[path])
        try:
            if not passed(run(name + '-restored', test)):
                status, reason = 'INVALID', reason + '; restored control failed'
        except Exception as error:
            status, reason = 'INVALID', reason + '; restore: ' + str(error)
        results.append({'name': name, 'status': status, 'compiled': compiled, 'reason': reason,
                        'test': full_test(test), 'marker': messages[name],
                        'mode': 'memory-only' if args.memory else 'guarded-source'})

    try:
        if args.memory:
            with tree_lock(root, timeout=900):
                for row in selected:
                    one(row)
        else:
            paths = [source, driver, collector, fixture, measurement, runner, e2e_runner, r11,
                     Path(__file__).with_name('preregistration.mjs'),
                     root / 'src/pilotdeck-gateway/tests/native_stimulus_plan.rs']
            with mutating_sources(root, paths) as originals:
                for row in selected:
                    one(row)
    except BaseException as error:
        invalidate_completed(results, 'source guard: ' + str(error))
        for name, *_ in selected[len(results):]:
            results.append({'name': name, 'status': 'INVALID', 'compiled': False,
                            'reason': 'guard refusal: ' + str(error)})
    # Shared publisher atomically renames only after the guard verifies restore.
    publish_summary(out, results)
    print(json.dumps(results, ensure_ascii=False, indent=2))
    return 0 if len(results) == len(selected) and all(r['status'] == 'KILLED' for r in results) else 1



if __name__ == '__main__':
    raise SystemExit(main())
