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
            case_count = len(registration_rows({'cells': v.s.CELLS, 'phases': ['model', 'tool']}))
            for repeats in (1, 2, 5):
                measurement_fixture(folder, repeats)
                with mock.patch.object(v.s, 'plan', side_effect=unchecked_plan):
                    result = v.verify(folder)
                self.assertTrue(result['status'] == 'NOT_RUN'
                                and len(result['uncheckedBoundaries']) == case_count * repeats
                                and all(row['boundary'] == 'registry' and '未执行' in row['reason']
                                        for row in result['uncheckedBoundaries'])
                                and result['missingEventControls'] == result['reversedOrderControls']
                                == result['emptyBeforeControls'] == 0, marker)

    def test_empty_order_not_witnessed(self):
        marker = '#1368 every ordered boundary rejects empty before'
        for plan in c.stimuli.plans():
            for target in plan['boundaries']:
                if target['name'] == 'registry':
                    continue
                with self.subTest(case=plan['case'], boundary=target['name']):
                    with self.assertRaisesRegex(ValueError, '^#1368 missing required order edges: ', msg=marker):
                        v.witnessed(dict(target, before=[]), target['events'])

    def test_measurement_rejects_empty_order(self):
        marker = '#1368 verifier independently rejects missing order before skipping events'
        targets = {target['name']: target for plan in c.stimuli.plans()
                   for target in plan['boundaries'] if target['name'] != 'registry'}
        with tempfile.TemporaryDirectory(prefix='stimulus-empty-order-') as tmp:
            folder = Path(tmp)
            measurement_fixture(folder, 1)
            for target in targets.values():
                for events in (target['events'], []):
                    def damaged_plan(case):
                        plan = unchecked_plan(case)
                        plan['boundaries'].append(dict(target, before=[], events=events))
                        return plan
                    # Bypass the witness function deliberately: verify itself
                    # must reject invalid plans, including eventless targets.
                    with mock.patch.object(v.s, 'plan', side_effect=damaged_plan), \
                            mock.patch.object(v, 'witnessed', return_value=True):
                        with self.assertRaisesRegex(ValueError, '^#1368 missing required order edges: ', msg=marker):
                            v.verify(folder)

    def test_measurement_empty_order_controls(self):
        marker = '#1368 empty-before data controls cover every ordered measurement'
        def phase_plan(case):
            plan = c.stimuli.plan(case)
            plan['boundaries'] = [plan['boundaries'][1]]
            return plan
        with tempfile.TemporaryDirectory(prefix='stimulus-order-controls-') as tmp:
            folder = Path(tmp)
            repeats = 2
            measurement_fixture(folder, repeats)
            for cell in v.s.CELLS:
                path = folder / (cell['name'] + '.json')
                document = json.loads(path.read_text())
                for observation in document['observations']:
                    target = phase_plan(observation['id'].rsplit('/', 1)[0])['boundaries'][0]
                    # Producer, client frame, wait satisfy the declared edge;
                    # this synthetic control makes no TS scheduling claim.
                    observation['timeline'] = target['events']
                path.write_text(json.dumps(document))
            with mock.patch.object(v.s, 'plan', side_effect=phase_plan):
                result = v.verify(folder)
                expected = len(registration_rows({'cells': v.s.CELLS, 'phases': ['model', 'tool']})) * repeats
                self.assertTrue(result['status'] == 'PASS'
                                and result['emptyBeforeControls'] == expected, marker)
                # Only empty declarations are incorrectly accepted; original,
                # missing-event and reversed-order controls still run normally.
                original = v.witnessed
                def accepts_empty(target, timeline):
                    return True if not target['before'] else original(target, timeline)
                with mock.patch.object(v, 'witnessed', side_effect=accepts_empty):
                    with self.assertRaisesRegex(ValueError, '^#1368 empty-before data control SURVIVED:', msg=marker):
                        v.verify(folder)
                def unrelated_error(target, timeline):
                    if not target['before']:
                        raise ValueError('#1368 unrelated witness failure')
                    return original(target, timeline)
                with mock.patch.object(v, 'witnessed', side_effect=unrelated_error):
                    with self.assertRaisesRegex(ValueError, '^#1368 unrelated witness failure$', msg=marker):
                        v.verify(folder)

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
                case_count = len(registration_rows({'cells': v.s.CELLS, 'phases': ['model', 'tool']}))
                with mock.patch.object(v.s, 'plan', side_effect=unchecked_plan):
                    for repeats in (1, 2):
                        measurement_fixture(folder, repeats)
                        result = v.verify(folder)
                        self.assertTrue(result['measurements'] == case_count * repeats, marker)
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

    def test_memory_runners_do_not_compile_tree(self):
        marker = '#1368 memory runners never launch cargo or claim a mutated build'
        for folder in ('native-stimulus-plan', 'native-session-e2e'):
            path = Path(__file__).parent.parent / folder / 'negative_controls.py'
            spec = importlib.util.spec_from_file_location('memory_runner', path)
            runner = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(runner)
            name, _path, _before, _after, test = runner.cases[0]
            passed = subprocess.CompletedProcess([], 0, 'Ran 1 test in 0.001s\nOK\n', '')
            def run(command, **_kwargs):
                self.assertTrue(command[0] != 'cargo', marker)
                return passed
            with tempfile.TemporaryDirectory(prefix='stimulus-memory-runner-') as tmp:
                output = Path(tmp) / 'results'
                with mock.patch.object(sys, 'argv', [str(path), str(output), '--memory', '--case', name]), \
                        mock.patch.object(runner, 'execution_environment', return_value={}), \
                        mock.patch.object(runner, 'preflight'), \
                        mock.patch.object(runner, 'run_owned', side_effect=run), \
                        mock.patch.object(runner, 'wait_for_slot', side_effect=AssertionError(marker)), \
                        mock.patch('sys.stdout', new_callable=io.StringIO):
                    # The synthetic child passes even under mutation, so the
                    # runner must finish as SURVIVED, never as a build failure.
                    self.assertTrue(runner.main() == 1, marker)
                summary = json.loads((output / 'summary.json').read_text())
                rows = summary['controls'] if isinstance(summary, dict) else summary
                self.assertTrue(len(rows) == 1 and rows[0]['status'] == 'SURVIVED'
                                and rows[0]['compiled'] is False
                                and rows[0]['test'].endswith('.' + test)
                                and not list(output.glob('*-compile.log'))
                                and not list(output.glob('*-slots.log')), marker)


if __name__ == '__main__':
    unittest.main(verbosity=2)
