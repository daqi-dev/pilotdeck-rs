#!/usr/bin/env python3
# anchor-selfcheck: unsupported registration and in-memory data tests, no source mutation table
"""#1368 input coverage controls; no synthetic evidence is a native witness."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[5]
spec = importlib.util.spec_from_file_location('collector', ROOT / 'scripts/native-witness/collect.py')
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)
spec = importlib.util.spec_from_file_location('measurement', Path(__file__).with_name('verify_measurement.py'))
v = importlib.util.module_from_spec(spec)
spec.loader.exec_module(v)


def measurement_fixture(folder, repeats):
    """Synthetic denominator control, deliberately not TS boundary evidence."""
    registration = {'cells': v.s.CELLS, 'phases': ['model', 'tool'], 'repeats': repeats}
    (folder / 'preregistration.json').write_text(json.dumps(registration))
    for cell in registration['cells']:
        observations = [dict(id=row['case'] + '/' + str(repeat), variant=row['variant'],
                             phase=row['phase'], registry={'applicable': row['registry']}, timeline=[])
                        for row in registration_rows({'cells': [cell], 'phases': registration['phases']})
                        for repeat in range(1, repeats + 1)]
        (folder / (cell['name'] + '.json')).write_text(json.dumps({'observations': observations}))


def unchecked_plan(case):
    plan = c.stimuli.plan(case)
    plan['boundaries'] = [plan['boundaries'][0]]
    return plan


def registration_rows(registration):
    return [{'case': f"{cell['name']}/{variant['name']}/{phase}",
             'registry': cell['registry'], 'variant': variant, 'phase': phase}
            for cell in registration['cells'] for variant in cell['variants']
            for phase in registration['phases']]


def inputs(plans):
    return [{key: item[key] for key in ('case', 'registry', 'variant', 'phase')} for item in plans]


class PlanTests(unittest.TestCase):
    def test_exact_registration(self):
        marker = '#1368 all 72 inputs match TS preregistration'
        try:
            run = subprocess.run(['node', str(Path(__file__).with_name('preregistration.mjs'))],
                                 capture_output=True, text=True, timeout=20, check=False)
            self.assertTrue(run.returncode == 0, marker + ': ' + run.stdout + run.stderr)
            actual = c.stimuli.plans()
            expected = registration_rows(json.loads(run.stdout))
            manifest = c.session_manifest()
        except Exception as error:
            self.fail(marker + ': ' + str(error))
        self.assertTrue(len(actual) == 72 and inputs(actual) == expected
                        and [item['case'] for item in actual] == manifest, marker)
        # Data negatives are independent of the source mutation runner.
        self.assertFalse(inputs(actual[:-1]) == expected, '#1368 missing cell rejected')
        for field, replacement in (('registry', True), ('phase', 'tool'),
                                   ('variant', {'name': 'wrong-stimulus'})):
            damaged = copy.deepcopy(actual)
            damaged[0][field] = replacement
            self.assertFalse(inputs(damaged) == expected, '#1368 wrong stimulus field rejected: ' + field)

    def test_boundary_coverage(self):
        marker = '#1368 every registered stimulus has its precise target boundary'
        # Input-table independence is checked by test_exact_registration; this
        # part also runs in CI without a pinned Node installation.
        for registered in registration_rows({'cells': c.stimuli.CELLS, 'phases': ['model', 'tool']}):
            with self.subTest(case=registered['case']):
                try:
                    plan = c.case_plan(registered['case'])['stimulus']
                    targets = {item['name']: item for item in plan['boundaries']}
                    variant = registered['variant']
                    fields = set(variant) - {'name', 'order'}
                    correct = (set(targets) == {'registry', 'phase', 'order'} | fields
                               and targets['registry']['configured'] is registered['registry']
                               and targets['order']['target'] == variant.get('order', 'timeout-first'))
                    for name in fields:
                        correct &= bool(targets[name]['events'])
                    # Registry construction has no receipt yet, so it remains
                    # explicitly unchecked. Every event-bearing boundary must
                    # declare an order edge, including phase and fault entry.
                    correct &= all(bool(target['before']) for name, target in targets.items()
                                   if name != 'registry')
                    for name in ('failStatus', 'failAppend'):
                        if name in fields:
                            correct &= targets[name]['events'][-1]['key'] == variant[name]
                            correct &= targets[name]['events'][-1]['error'] == {'code': 'EIO'}
                    if 'slowAppend' in fields:
                        correct &= targets['slowAppend']['blockedKeys'] == [
                            'agent_status:' + variant['slowAppend'], 'error:' + variant['slowAppend']]
                    self.assertTrue(correct, marker)
                except Exception as error:
                    self.fail(marker + ': ' + str(error))

    def test_unwitnessed_is_not_run(self):
        marker = '#1368 same-name requests cannot execute an unwitnessed stimulus'
        # Even perfectly matching terminal output cannot attest input control.
        expectations = c.session_expectations()
        for case in c.session_manifest():
            try:
                observation = expectations[case]
                row = c.session_row(case, {'requestId': case}, observation, {})
                pending = row['stimulus']
                correct = (row['observation'] is None and pending['status'] == 'NOT_RUN'
                           and pending['witnesses'] == [] and '未执行' in row['reason']
                           and pending['missing'] == [b['name'] for b in pending['plan']['boundaries']])
            except Exception as error:
                self.fail(marker + ': ' + str(error))
            self.assertTrue(correct, marker)
        missing = c.session_row(c.session_manifest()[0], {}, None, {'collectionError': '落盘未完成'})
        self.assertTrue(missing['observation'] is None and '落盘未完成' in missing['reason']
                        and '未执行' in missing['reason'], '#1368 preserve both missing boundaries and disk failure')

    def test_unknown_and_detached_plan(self):
        for case in ('', 'no-registry-fast/fast/other', 'registry-fast/unknown/model',
                     'registry-fast/fast/model/1'):
            with self.assertRaises(ValueError, msg='#1368 unknown stimulus fails closed'):
                c.case_plan(case)
        plan = c.case_plan('registry-fast/fast/model')['stimulus']
        plan['variant']['name'] = 'changed'
        self.assertTrue(c.case_plan('registry-fast/fast/model')['stimulus']['variant'] == {'name': 'fast'},
                        '#1368 returned plan cannot mutate registration')

    def test_manifest_rejects_gap(self):
        declared = c.stimuli.plans()
        with mock.patch.object(c.stimuli, 'plans', return_value=declared[:-1]):
            with self.assertRaises(c.w.Incomplete, msg='#1368 manifest refuses a missing plan before launch'):
                c.session_manifest()

    def test_manifest_rejects_duplicate(self):
        marker = '#1368 manifest refuses duplicate stimulus plans'
        declared = c.stimuli.plans()
        for damaged in (declared + [declared[0]], [declared[0], *declared[:-1]]):
            with mock.patch.object(c.stimuli, 'plans', return_value=damaged):
                with self.assertRaises(c.w.Incomplete, msg=marker):
                    c.session_manifest()

    def test_unknown_plan_reason(self):
        marker = '#1368 unknown stimulus has a distinct diagnostic'
        for case in ('', 'registry-fast/unknown/model'):
            with self.assertRaisesRegex(ValueError, r'^#1368 unknown stimulus case:', msg=marker):
                c.stimuli.plan(case)

    def test_duplicate_plan_reason(self):
        marker = '#1368 duplicate stimulus has a distinct diagnostic'
        cells = copy.deepcopy(c.stimuli.CELLS)
        cells.append(copy.deepcopy(cells[0]))
        with mock.patch.object(c.stimuli, 'CELLS', cells):
            with self.assertRaisesRegex(ValueError, r'^#1368 duplicate stimulus case:', msg=marker):
                c.stimuli.plan('no-registry-fast/fast/model')

    def test_empty_boundary_not_witnessed(self):
        marker = '#1368 empty boundary declaration never witnesses execution'
        target = c.stimuli.plan('no-registry-fast/fast/model')['boundaries'][0]
        self.assertFalse(v.witnessed(target, []), marker)
        self.assertFalse(v.witnessed(target, [{'kind': 'unrelated'}]), marker)

    def test_measurement_unchecked_not_pass(self):
        marker = '#1368 unchecked measurement boundaries cannot report PASS'
        with tempfile.TemporaryDirectory(prefix='stimulus-measurement-') as tmp:
            folder = Path(tmp)
            measurement_fixture(folder, 5)
            with mock.patch.object(v.s, 'plan', side_effect=unchecked_plan):
                result = v.verify(folder)
            self.assertTrue(result['status'] == 'NOT_RUN'
                            and len(result['uncheckedBoundaries']) == 360
                            and all(row['boundary'] == 'registry' and '未执行' in row['reason']
                                    for row in result['uncheckedBoundaries'])
                            and result['missingEventControls'] == result['reversedOrderControls'] == 0, marker)

    def test_phase_requires_prefix_before_wait(self):
        marker = '#1368 phase production must precede controlled loop wait'
        for plan in c.stimuli.plans():
            target = next(row for row in plan['boundaries'] if row['name'] == 'phase')
            expected = {'kind': 'registry.append.attempt' if plan['registry'] else 'queue.enqueue',
                        'event': {'type': 'tool_call_started' if plan['phase'] == 'tool' else 'model_request_started'}}
            wait = {'kind': 'loop.wait', 'phase': plan['phase']}
            client = {'kind': 'client.frame', 'event': expected['event']}
            self.assertTrue(target['before'] == [(expected, wait)], marker)
            self.assertTrue(v.witnessed(target, [expected, wait, client]), marker)
            self.assertFalse(v.witnessed(target, [wait, expected, client]), marker)
            self.assertFalse(v.witnessed(target, [wait, client]), marker)

    def test_fault_requires_entry_before_failure(self):
        marker = '#1368 faults must follow their real write entry'
        for plan in c.stimuli.plans():
            for target in plan['boundaries']:
                if target['name'] not in ('failStatus', 'failAppend'):
                    continue
                entered = {'kind': 'status.attempt' if target['name'] == 'failStatus' else 'registry.persist.enter',
                           'key': plan['variant'][target['name']]}
                failed = {'kind': 'status.failure' if target['name'] == 'failStatus' else 'registry.persist.failure',
                          'key': entered['key'], 'error': {'code': 'EIO'}}
                self.assertTrue(target['before'] == [(entered, failed)], marker)
                self.assertTrue(v.witnessed(target, [entered, failed]), marker)
                self.assertFalse(v.witnessed(target, [failed, entered]), marker)

    def test_measurement_registered_denominator(self):
        marker = '#1368 measurement denominator follows registered repeats and exact IDs'
        try:
            with tempfile.TemporaryDirectory(prefix='stimulus-denominator-') as tmp:
                folder = Path(tmp)
                with mock.patch.object(v.s, 'plan', side_effect=unchecked_plan):
                    for repeats in (1, 2):
                        measurement_fixture(folder, repeats)
                        result = v.verify(folder)
                        self.assertTrue(result['measurements'] == 72 * repeats, marker)
                    path = folder / 'no-registry-fast.json'
                    document = json.loads(path.read_text())
                    rows = document['observations']
                    for damaged in (rows[:-1], rows + [rows[0]], list(reversed(rows))):
                        path.write_text(json.dumps({'observations': damaged}))
                        with self.assertRaisesRegex(ValueError, 'denominator/order mismatch', msg=marker):
                            v.verify(folder)
        except Exception as error:
            self.fail(marker + ': ' + str(error))

    def test_measurement_rejects_empty_repeats(self):
        marker = '#1368 measurement repeats must be positive'
        with tempfile.TemporaryDirectory(prefix='stimulus-empty-') as tmp:
            folder = Path(tmp)
            measurement_fixture(folder, 0)
            path = folder / 'preregistration.json'
            registration = json.loads(path.read_text())
            for repeats in (0, -1, True, 0.5, '5', None):
                registration['repeats'] = repeats
                path.write_text(json.dumps(registration))
                with self.assertRaisesRegex(ValueError, 'repeats must be a positive integer', msg=marker):
                    v.verify(folder)

    def test_runner_target_environment(self):
        marker = '#1368 both negative runners reject unsafe or missing cargo targets'
        for folder in ('native-stimulus-plan', 'native-session-e2e'):
            path = Path(__file__).parent.parent / folder / 'negative_controls.py'
            spec = importlib.util.spec_from_file_location('negative_runner', path)
            runner = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(runner)
            for target in ('', '.', '/', str(ROOT), str(ROOT / 'src'), '   '):
                with mock.patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                    with self.assertRaises((RuntimeError, SystemExit), msg=marker):
                        runner.execution_environment()
            with mock.patch.dict(os.environ, {}, clear=True):
                with self.assertRaises(SystemExit, msg=marker):
                    runner.execution_environment()
            with tempfile.TemporaryDirectory(prefix='stimulus-target-') as tmp:
                with mock.patch.dict(os.environ, {'CARGO_TARGET_DIR': tmp}):
                    self.assertTrue(runner.execution_environment()['CARGO_TARGET_DIR'] == str(Path(tmp).resolve()), marker)

    def test_registry_free_settle_synthetic_control(self):
        marker = '#1368 registry-free settle controls explicitly attest synthetic stimuli'
        fixture = Path(__file__).parent.parent / 'native-witness/test_r11.py'
        spec = importlib.util.spec_from_file_location('settle_controls', fixture)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        suite = unittest.defaultTestLoader.loadTestsFromNames([
            'R11Tests.test_collector_inapplicable_without_registry_file',
            'R11Tests.test_collector_wait_wired'], module)
        log = io.StringIO()
        result = unittest.TextTestRunner(stream=log, verbosity=2).run(suite)
        self.assertTrue(result.wasSuccessful(), marker + ': ' + log.getvalue())


if __name__ == '__main__':
    unittest.main(verbosity=2)
