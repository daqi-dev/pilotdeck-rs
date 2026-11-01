#!/usr/bin/env python3
# anchor-selfcheck: unsupported registration and in-memory data tests, no source mutation table
"""#1368 input coverage controls; no synthetic evidence is a native witness."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[5]
spec = importlib.util.spec_from_file_location('collector', ROOT / 'scripts/native-witness/collect.py')
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)


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
                    for name in ('failStatus', 'failAppend'):
                        if name in fields:
                            correct &= targets[name]['events'][0]['key'] == variant[name]
                            correct &= targets[name]['events'][0]['error'] == {'code': 'EIO'}
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


if __name__ == '__main__':
    unittest.main(verbosity=2)
