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
import time
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
        self.assertTrue(len(actual) == len(expected) and inputs(actual) == expected
                        and [item['case'] for item in actual] == manifest, marker)
        # #1402: the length comparison is derived from the registration, so a
        # dropped row must change its outcome (the old literal 72 could not).
        self.assertFalse(len(actual) == len(expected[:-1]),
                         '#1402 a dropped registration row must fail the length comparison')
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
            # #1402: never use 5 here -- 72 * 5 = 360 equals the literal the
            # hardcoded-unchecked-count mutant writes, so that iteration would
            # let the mutant survive; 1, 2 and 3 all stay clear of 360.
            for repeats in (1, 2, 3):
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

    def test_memory_rehearsal_holds_tree_lock(self):
        marker = '#1398 stimulus-plan memory rehearsal holds the source tree lock'
        path = Path(__file__).with_name('negative_controls.py')
        try:
            spec = importlib.util.spec_from_file_location('plan_memory_lock', path)
            runner = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(runner)
            seen, order = [], []

            class recording_lock:
                def __enter__(self):
                    order.append('lock')

                def __exit__(self, *_exc):
                    return None

            def recording_tree_lock(*arguments, **keywords):
                seen.append((arguments, keywords))
                return recording_lock()

            def fake_run_owned(command, **_keywords):
                order.append('child')
                return subprocess.CompletedProcess([], 0, '', 'Ran 1 test in 0.001s\n\nOK\n')

            with tempfile.TemporaryDirectory(prefix='pdrs-1398-plan-') as tmp:
                output = Path(tmp) / 'results'
                with mock.patch.object(sys, 'argv', [str(path), str(output), '--memory',
                                                   '--case', 'memory-rehearsal-without-lock']), \
                        mock.patch.object(runner, 'execution_environment', return_value={}), \
                        mock.patch.object(runner, 'preflight'), \
                        mock.patch.object(runner, 'tree_lock', recording_tree_lock), \
                        mock.patch.object(runner, 'run_owned', fake_run_owned), \
                        mock.patch('sys.stdout', new_callable=io.StringIO):
                    runner.main()
            self.assertTrue(seen == [((runner.root,), {'timeout': 900})]
                            and order[:1] == ['lock'] and 'child' in order, marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))


class FakeProduct:
    """Scripted stand-in for the s2-s4 product surfaces the driver polls.

    Witness receipts are written the way the product writes them (atomic,
    into ``<dir>/witness``) so the driver's delta scanning is what is under
    test; frames come from the step script. No receipt is ever fabricated by
    the collector side of this simulation.
    """

    def __init__(self, loop_dir, storage_dir, pid=4242):
        self.loop_dir, self.storage_dir, self.pid = Path(loop_dir), Path(storage_dir), pid
        self.loop_seq = 0

    def _publish(self, directory, name, receipt):
        witness = Path(directory) / 'witness'
        witness.mkdir(parents=True, exist_ok=True)
        (witness / name).write_text(json.dumps(receipt))

    def loop(self, phase, key, pid=None, **extra):
        """One sidecar/gateway receipt in the loop stimulus witness dir.

        ``pid`` overrides the serving process (a receipt trailing in from
        another turn in the cross-case negative controls).
        """
        pid = self.pid if pid is None else pid
        receipt = {'seq': self.loop_seq, 'pid': pid, 'site': 'loop',
                   'phase': phase, 'key': key, 'at': '2026-11-08T00:00:00Z'}
        receipt.update(extra)
        self._publish(self.loop_dir, f'{pid:08d}-{self.loop_seq:06d}.json', receipt)
        self.loop_seq += 1

    def fence(self, phase, key):
        receipt = {'seq': self.loop_seq, 'pid': self.pid, 'site': 'fence',
                   'phase': phase, 'key': key, 'at': '2026-11-08T00:00:00Z'}
        self._publish(self.loop_dir, f'{self.pid:08d}-{self.loop_seq:06d}.json', receipt)
        self.loop_seq += 1

    def storage(self, site, phase, key, seq, **extra):
        receipt = {'seq': seq, 'site': site, 'phase': phase, 'key': key,
                   'at': '2026-11-08T00:00:00Z'}
        receipt.update(extra)
        self._publish(self.storage_dir, f'{seq:06d}.json', receipt)

    def control(self, name, *, storage=False):
        # loop and storage each have a release.json; the caller names which.
        directory = self.storage_dir if storage or name == 'stimulus.json' else self.loop_dir
        return json.loads((Path(directory) / name).read_text())


class FakeClient:
    """One scripted step per ``receive_poll``; callable steps may publish."""

    def __init__(self, steps, product):
        self.steps, self.product = list(steps), product

    def receive_poll(self, timeout):
        if not self.steps:
            return None
        step = self.steps.pop(0)
        return step(self.product) if callable(step) else step


def frame(request_id, seq, event, final=False):
    return {'type': 'event', 'id': request_id, 'seq': seq, 'final': final, 'event': event}


def drive_case(case, steps, *, run_id='run-01', request_id='case-01', timeout_ms=400,
               registry_events=None):
    """Prepare, drive and collect evidence for one scripted case.

    The stderr text and registry surfaces stand in for the product's own
    records so ``evidence['construction']`` carries a parsed (still scripted,
    but product-shaped) entry; everything else is what the driver really saw.
    """
    plan = c.stimuli.plan(case)
    registry = plan['registry']
    with tempfile.TemporaryDirectory(prefix='stimulus-drive-') as folder:
        driver = c.stimulus_driver.StimulusDriver(Path(folder) / 'loop',
                                                  Path(folder) / 'storage',
                                                  registry_on=registry)
        driver.prepare(case, 1, plan, run_id)
        client = FakeClient(steps, FakeProduct(driver.loop_dir, driver.storage_dir))
        with mock.patch.object(c.stimulus_driver, 'CASE_BUDGET_SECONDS', 1.0), \
                mock.patch.object(c.stimulus_driver, 'MILESTONE_SECONDS', 0.05), \
                mock.patch.object(c.stimulus_driver, 'POLL_SECONDS', 0.001):
            driven = driver.drive(case, plan, client, request_id, time.monotonic() + 5,
                                  timeout_ms=timeout_ms)
        stderr = (f'native run registry {"on" if registry else "off"}\n'
                  f'native storage stimulus on {driver.storage_dir}\n'
                  f'native loop stimulus on {driver.loop_dir}\n')
        if registry_events is None and registry:
            registry_events = [{'type': 'model_request_started'},
                               {'type': 'agent_status', 'event': 'turn_timeout'}]
        return driver.evidence(case, run_id, request_id, driven, stderr,
                               registry_events or [], {'state': 'failed'} if registry else None,
                               not registry)


class AdapterTests(unittest.TestCase):
    """#2396 s2-s4 adapter: choreography, evidence binding and attestation."""

    def test_storage_arms_match_the_arming_table(self):
        marker = '#2396 per-case arms follow plan section 6, one barrier-or-fault per site'
        for index, plan in enumerate(c.stimuli.plans(), start=1):
            with self.subTest(case=plan['case']):
                variant = plan['variant']
                arms = c.stimulus_driver.storage_arms(plan['case'], index, variant)
                writer, registry = arms['writer'], arms['registry']
                self.assertTrue(not (writer.get('barrier') and writer.get('fault'))
                                and not (registry.get('barrier') and registry.get('fault')), marker)
                if variant.get('slowStatus'):
                    self.assertTrue(writer.get('barrier', {}).get('key') == 'turn_timeout', marker)
                if variant.get('slowCatchStatus'):
                    self.assertTrue(writer.get('barrier', {}).get('key') == 'gateway_submit_failed', marker)
                if variant.get('failStatus'):
                    self.assertTrue(writer.get('fault', {}).get('key') == variant['failStatus'], marker)
                special = (variant.get('slowAppend') == 'turn_timeout'
                           and variant.get('failStatus') == 'turn_timeout')
                if variant.get('slowAppend'):
                    expected = 'error:turn_timeout' if special else 'agent_status:' + variant['slowAppend']
                    self.assertTrue(registry.get('barrier', {}).get('key') == expected, marker)
                if variant.get('failAppend'):
                    self.assertTrue(registry.get('fault', {}).get('key') == variant['failAppend'], marker)
                for site in (writer, registry):
                    for arm in site.values():
                        self.assertTrue(arm.get('id', '').startswith(f"{plan['case']}#{index}-")
                                        and arm.get('boundMs', 8000) == 8000, marker)
                second = c.stimulus_driver.second_append_key(variant)
                self.assertTrue(second == (None if special and variant.get('slowAppend')
                                            else 'error:' + variant['slowAppend']
                                            if variant.get('slowAppend') else None), marker)

    def test_driver_slow_status_choreography(self):
        marker = '#2396 slowStatus: block gates the outcome release, FIFO then release precedes final'
        case = 'no-registry-slow/status-slow/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', 'no-registry-slow/status-slow/model#1', planPhase='model'), prefix)[1],
            lambda p: p.loop('wait', 'no-registry-slow/status-slow/model#1', planPhase='model'),
            lambda p: p.storage('writer', 'block', 'turn_timeout', 1, boundMs=8000),
            lambda p: p.storage('writer', 'queued', 'turn_result', 2),
            lambda p: self._assert_outcome_released(p, 'unknown'),
            lambda p: (self.assertEqual(p.control('release.json', storage=True), {'seq': 1}, marker),
                       p.storage('writer', 'release', 'turn_timeout', 3),
                       p.storage('writer', 'complete', 'turn_timeout', 4), final)[3],
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps)
        actions = [action['action'] for action in evidence['controlActions']]
        self.assertTrue(actions.index('release-loop') < actions.index('control.status.fifo.checked')
                        < actions.index('release-storage'), marker)
        fifo = next(action for action in evidence['controlActions']
                   if action['action'] == 'control.status.fifo.checked')
        self.assertTrue(fifo['detail']['queuedKey'] == 'turn_result', marker)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'WITNESSED'
                        and {w['target'] for w in verdict['witnesses']}
                        == {'registry', 'phase', 'order', 'slowStatus'}, marker)
        self.assertTrue(evidence['notes']['outcomeReleaseAfterFinal'] is False, marker)

    def test_driver_slow_append_two_barriers(self):
        marker = '#2396 slowAppend: first block, FIFO, re-arm the second key, then release'
        case = 'registry-slow/status-fast-append-slow/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)

        def rearm_key(p):
            arms = p.control('stimulus.json')
            return arms['registry']['barrier']['key'], arms['registry']['barrier']['id']

        steps = [
            lambda p: (p.loop('input', 'registry-slow/status-fast-append-slow/model#1'), prefix)[1],
            lambda p: p.loop('wait', 'registry-slow/status-fast-append-slow/model#1', planPhase='model'),
            lambda p: p.storage('registry', 'block', 'agent_status:turn_timeout', 1, boundMs=8000),
            lambda p: p.storage('registry', 'queued', 'error:turn_timeout', 2),
            lambda p: self._assert_outcome_released(p, 'unknown'),
            lambda p: (self.assertEqual(p.control('release.json', storage=True), {'seq': 1}, marker),
                       self.assertEqual(rearm_key(p), ('error:turn_timeout',
                                                       'registry-slow/status-fast-append-slow/model#1-r2'), marker),
                       p.storage('registry', 'release', 'agent_status:turn_timeout', 3))[2],
            lambda p: p.storage('registry', 'block', 'error:turn_timeout', 4, boundMs=8000),
            lambda p: (self.assertEqual(p.control('release.json', storage=True), {'seq': 4}, marker),
                       p.storage('registry', 'release', 'error:turn_timeout', 5), final)[2],
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps)
        actions = [action['action'] for action in evidence['controlActions']]
        self.assertTrue(actions.index('control.registry.fifo.checked')
                        < actions.index('rearm-storage')
                        < actions.index('release-storage'), marker)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'WITNESSED'
                        and 'slowAppend' in {w['target'] for w in verdict['witnesses']}, marker)

    def test_driver_bounded_release_receipt_does_not_attest(self):
        marker = '#2396 a bound-expiry self-release (bounded: true) never attests the held write'
        case = 'no-registry-slow/status-slow/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', case + '#1', planPhase='model'), prefix)[1],
            lambda p: p.loop('wait', case + '#1', planPhase='model'),
            lambda p: p.storage('writer', 'block', 'turn_timeout', 1, boundMs=8000),
            lambda p: p.storage('writer', 'queued', 'turn_result', 2),
            lambda p: p.storage('writer', 'release', 'turn_timeout', 3, bounded=True),
            lambda p: p.storage('writer', 'complete', 'turn_timeout', 4),
            final,
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'NOT_RUN'
                        and any('status.release(turn_timeout)' in row
                                for row in verdict['missing']), marker)

    def test_driver_cross_case_loop_final_does_not_attest(self):
        marker = '#2396 a loop final trailing in from another turn (foreign pid) never attests'
        case = 'no-registry-slow/status-slow/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', case + '#1', planPhase='model'), prefix)[1],
            lambda p: p.loop('wait', case + '#1', planPhase='model'),
            lambda p: p.storage('writer', 'block', 'turn_timeout', 1, boundMs=8000),
            lambda p: p.storage('writer', 'queued', 'turn_result', 2),
            lambda p: self._assert_outcome_released(p, 'unknown'),
            lambda p: p.loop('final', 'result_unknown', pid=9090),
            lambda p: p.storage('writer', 'release', 'turn_timeout', 3),
            lambda p: p.storage('writer', 'complete', 'turn_timeout', 4),
            final,
            lambda p: p.loop('final', 'result_unknown', pid=9090),
        ]
        evidence = drive_case(case, steps)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'NOT_RUN'
                        and any('loop.final(result_unknown)' in row
                                for row in verdict['missing']), marker)

    def test_driver_foreign_request_final_is_ignored(self):
        marker = '#2396 a final frame keyed to another request never ends this case'
        case = 'no-registry-slow/status-slow/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        foreign = frame('other-req', 9, {'type': 'turn_completed'}, final=True)
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', case + '#1', planPhase='model'), prefix)[1],
            lambda p: p.loop('wait', case + '#1', planPhase='model'),
            foreign,
            lambda p: p.storage('writer', 'block', 'turn_timeout', 1, boundMs=8000),
            lambda p: p.storage('writer', 'queued', 'turn_result', 2),
            lambda p: self._assert_outcome_released(p, 'unknown'),
            lambda p: p.storage('writer', 'release', 'turn_timeout', 3),
            lambda p: p.storage('writer', 'complete', 'turn_timeout', 4),
            final,
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'WITNESSED', marker + ': ' + json.dumps(verdict['missing']))
        self.assertTrue(evidence['notes']['finalIndex'] == len(evidence['frames']) - 1, marker)

    def test_driver_order_edge_final_before_held_release(self):
        marker = '#2396 slowStatus: the final frame preceding the held release breaks the order edge'
        case = 'no-registry-slow/status-slow/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', case + '#1', planPhase='model'), prefix)[1],
            lambda p: p.loop('wait', case + '#1', planPhase='model'),
            final,
            lambda p: p.storage('writer', 'block', 'turn_timeout', 1, boundMs=8000),
            lambda p: p.storage('writer', 'queued', 'turn_result', 2),
            lambda p: self._assert_outcome_released(p, 'unknown'),
            lambda p: p.storage('writer', 'release', 'turn_timeout', 3),
            lambda p: p.storage('writer', 'complete', 'turn_timeout', 4),
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps)
        self.assertTrue(evidence['notes']['outcomeReleaseAfterFinal'] is True, marker)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'NOT_RUN'
                        and any('outcome release before the final frame' in row
                                for row in verdict['missing']), marker)

    def test_driver_slow_append_out_of_order_receipts_do_not_attest(self):
        marker = '#2396 slowAppend: an error block preceding the status release fails the sequence edges'
        case = 'registry-slow/status-fast-append-slow/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', case + '#1'), prefix)[1],
            lambda p: p.loop('wait', case + '#1', planPhase='model'),
            lambda p: p.storage('registry', 'block', 'agent_status:turn_timeout', 1, boundMs=8000),
            lambda p: p.storage('registry', 'queued', 'error:turn_timeout', 2),
            lambda p: self._assert_outcome_released(p, 'unknown'),
            lambda p: (self.assertEqual(p.control('release.json', storage=True), {'seq': 1}, marker),
                       p.storage('registry', 'block', 'error:turn_timeout', 3, boundMs=8000))[1],
            lambda p: (self.assertEqual(p.control('release.json', storage=True), {'seq': 3}, marker),
                       p.storage('registry', 'release', 'agent_status:turn_timeout', 4),
                       p.storage('registry', 'release', 'error:turn_timeout', 5), final)[2],
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'NOT_RUN'
                        and any('status release before the re-armed error block' in row
                                for row in verdict['missing']), marker)

    def test_driver_known_final_pending_release(self):
        marker = '#2396 K-P: known outcome first, return held, released only after the final frame'
        case = 'no-registry-fast/known-final-before-timeout-pending/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        timeout = frame('case-01', 1, {'type': 'agent_status', 'event': 'turn_timeout'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', case + '#1'), prefix)[1],
            lambda p: p.loop('wait', case + '#1', planPhase='model'),
            lambda p: self._assert_outcome_released(p, 'known'),
            lambda p: p.loop('return', 'block'),
            timeout,
            final,
            lambda p: (self.assertEqual(p.control('release.json'),
                                        {'id': case + '#1', 'stage': 'return'}, marker),
                       p.loop('return', 'release'), p.loop('final', 'completed-suppressed'))[2],
        ]
        evidence = drive_case(case, steps)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'WITNESSED', marker + ': ' + json.dumps(verdict['missing']))
        # The held return released only after the timer-decided final; the
        # completion receipt that follows the delivery is `completed` when the
        # bounded delivery lands (the real smoke's outcome) and
        # `completed-suppressed` when the retired turn rejects it — both are
        # this cell, only the release order is pinned.
        self.assertTrue(evidence['notes']['returnReleaseAfterFinal'] is True, marker)
        keys = {receipt['key'] for receipt in evidence['loopReceipts']
                if receipt['phase'] == 'final'}
        self.assertTrue(keys <= {'completed', 'completed-suppressed'}, marker)

    def test_driver_after_complete_fence(self):
        marker = '#2396 after-complete: the fence complete receipt gates the late unknown release'
        case = 'no-registry-fast/after-complete/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        timeout = frame('case-01', 1, {'type': 'agent_status', 'event': 'turn_timeout'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', case + '#1', planPhase='model'), prefix)[1],
            lambda p: p.loop('wait', case + '#1', planPhase='model'),
            timeout,
            final,
            lambda p: self._assert_outcome_withheld(p),
            lambda p: p.fence('complete', 'run-01'),
            lambda p: self._assert_outcome_released(p, 'unknown'),
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps)
        self.assertTrue(evidence['notes']['outcomeReleaseAfterFinal'] is True, marker)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'WITNESSED', marker + ': ' + json.dumps(verdict['missing']))

    def test_driver_unknown_first_timer_observation(self):
        marker = '#2396 unknown-first: release before final, timer.clear observed, no timeout frame'
        case = 'no-registry-fast/unknown-first/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        final = frame('case-01', 1, {'type': 'turn_completed'}, final=True)
        steps = [
            lambda p: (p.loop('input', case + '#1', planPhase='model'), prefix)[1],
            lambda p: p.loop('wait', case + '#1', planPhase='model'),
            lambda p: self._assert_outcome_released(p, 'unknown'),
            final,
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps, timeout_ms=30000)
        notes = evidence['notes']
        self.assertTrue(notes['outcomeReleaseAfterFinal'] is False
                        and notes['noTimeoutFrameBeforeFinal'] is True
                        and notes['finalElapsedMs'] < notes['timeoutMs'] == 30000, marker)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'WITNESSED', marker + ': ' + json.dumps(verdict['missing']))

    def test_adapter_precise_missing_per_target(self):
        marker = '#2396 a target without its raw receipts keeps the row NOT_RUN with a precise missing'
        case = 'no-registry-slow/status-slow/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        verdicts = {}
        base = None
        for damaged in ('intact', 'wait', 'fifo', 'outcome-release', 'prefix'):
            steps = [
                lambda p: (p.loop('input', case + '#1', planPhase='model'), prefix)[1],
                lambda p: p.loop('wait', case + '#1', planPhase='model'),
                lambda p: p.storage('writer', 'block', 'turn_timeout', 1, boundMs=8000),
                lambda p: p.storage('writer', 'queued', 'turn_result', 2),
                lambda p: p.storage('writer', 'release', 'turn_timeout', 3),
                lambda p: p.storage('writer', 'complete', 'turn_timeout', 4),
                lambda p: final,
                lambda p: p.loop('final', 'result_unknown'),
            ]
            evidence = drive_case(case, steps)
            if damaged == 'wait':
                evidence['loopReceipts'] = [row for row in evidence['loopReceipts']
                                            if row['phase'] != 'wait']
            elif damaged == 'fifo':
                evidence['controlActions'] = [row for row in evidence['controlActions']
                                              if row['action'] != 'control.status.fifo.checked']
                evidence['storageReceipts'] = [row for row in evidence['storageReceipts']
                                               if row['phase'] != 'queued']
            elif damaged == 'outcome-release':
                evidence['controlActions'] = [row for row in evidence['controlActions']
                                              if not (row['action'] == 'release-loop'
                                                      and row['detail'].get('stage') == 'outcome')]
            elif damaged == 'prefix':
                evidence['frames'] = [row for row in evidence['frames']
                                      if row['type'] != 'model_request_started']
            verdicts[damaged] = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
            if damaged == 'intact':
                base = verdicts[damaged]
        self.assertTrue(base['status'] == 'WITNESSED', marker)
        self.assertTrue('loop/wait receipt' in verdicts['wait']['missing'][0]
                        and any('control.status.fifo.checked' in row
                                for row in verdicts['fifo']['missing']), marker)
        self.assertTrue(verdicts['prefix']['status'] == 'NOT_RUN'
                        and any('client.frame(' in row for row in verdicts['prefix']['missing']), marker)
        self.assertTrue(verdicts['outcome-release']['status'] == 'NOT_RUN'
                        and any('control.release.unknown' in row
                                for row in verdicts['outcome-release']['missing']), marker)
        for damaged, verdict in verdicts.items():
            if damaged != 'intact':
                self.assertTrue(verdict['status'] == 'NOT_RUN' and verdict['missing'], marker + ': ' + damaged)
                # The passing targets keep their witnesses; only the damaged
                # one is named in the missing list.
                self.assertTrue(all(row['target'] not in {item.split(':')[0] for item in verdict['missing']}
                                    for row in verdict['witnesses']), marker)

    def test_adapter_cross_case_receipts_do_not_attest(self):
        marker = '#2396 receipts keyed to another plan never witness this case'
        case = 'no-registry-slow/status-slow/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', 'other-case#7', planPhase='model'), prefix)[1],
            lambda p: p.loop('wait', 'other-case#7', planPhase='model'),
            lambda p: p.storage('writer', 'block', 'turn_timeout', 1, boundMs=8000),
            lambda p: p.storage('writer', 'queued', 'turn_result', 2),
            lambda p: final,
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        phase_missing = [row for row in verdict['missing'] if row.startswith('phase')]
        self.assertTrue(verdict['status'] == 'NOT_RUN' and len(phase_missing) == 1
                        and 'loop/input receipt' in phase_missing[0]
                        and 'loop/wait receipt' in phase_missing[0], marker)

    def test_adapter_registry_closure_requires_product_receipt(self):
        marker = '#2396 no-registry closure is the product receipt, never file absence alone'
        case = 'no-registry-fast/fast/model'
        prefix = frame('case-01', 0, {'type': 'model_request_started'})
        timeout = frame('case-01', 1, {'type': 'agent_status', 'event': 'turn_timeout'})
        final = frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)
        steps = [
            lambda p: (p.loop('input', case + '#1', planPhase='model'), prefix)[1],
            lambda p: p.loop('wait', case + '#1', planPhase='model'),
            timeout,
            final,
            lambda p: p.loop('final', 'result_unknown'),
        ]
        evidence = drive_case(case, steps)
        verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'WITNESSED', marker + ': ' + json.dumps(verdict['missing']))
        for damaged, missing_row in (
                ({'construction': dict(evidence['construction'], runRegistry='on')},
                 'registry: closure receipt (native run registry off)'),
                ({'construction': dict(evidence['construction'], runRegistry=None)},
                 'registry: closure receipt (native run registry off)'),
                ({'runsJsonAbsent': False},
                 'registry: closure check (runs.json absent across the case)')):
            verdict = c.stimuli.execution(c.case_plan(case)['stimulus'], dict(evidence, **damaged))
            self.assertTrue(verdict['status'] == 'NOT_RUN'
                            and verdict['missing'] == [missing_row], marker)
        # The registry cell of the same shape demands the on-construction
        # receipt plus this run's registry surfaces; its own receipts, re-keyed
        # to its plan, then witness it in full.
        registry_case = 'registry-fast/fast/model'
        registry_plan = c.stimuli.plan(registry_case)
        plan_id = registry_case + '#1'
        rekeyed = [dict(receipt, key=plan_id) if receipt.get('phase') in ('input', 'wait')
                   else dict(receipt) for receipt in evidence['loopReceipts']]
        off = dict(evidence, planId=plan_id, loopReceipts=rekeyed)
        verdict = c.stimuli.execution(registry_plan, off)
        # The re-keyed evidence has no registry surfaces at all, so phase also
        # misses its registry.append witness; the registry row itself must be
        # the full three-part missing.
        registry_rows = [row for row in verdict['missing'] if row.startswith('registry:')]
        self.assertTrue(verdict['status'] == 'NOT_RUN'
                        and registry_rows == ['registry: construction receipt (native run registry on); '
                                              'registry applicability (runs.json record for this run); '
                                              'registry events (runs.json events for this run)'], marker)
        on = dict(off, construction=dict(evidence['construction'], runRegistry='on'),
                  registryRecord={'state': 'failed'},
                  registryEvents=[{'type': 'model_request_started'},
                                  {'type': 'agent_status', 'event': 'turn_timeout'}],
                  runsJsonAbsent=False)
        verdict = c.stimuli.execution(registry_plan, on)
        self.assertTrue(verdict['status'] == 'WITNESSED', marker + ': ' + json.dumps(verdict['missing']))

    @staticmethod
    def _assert_outcome_released(product, outcome):
        release = product.control('release.json')
        assert release.get('stage') == 'outcome' and release.get('outcome') == outcome, release
        return None

    @staticmethod
    def _assert_outcome_withheld(product):
        try:
            release = product.control('release.json')
        except FileNotFoundError:
            return None  # nothing released at all yet — withheld
        assert release.get('stage') != 'outcome', release
        return None


if __name__ == '__main__':
    unittest.main(verbosity=2)
