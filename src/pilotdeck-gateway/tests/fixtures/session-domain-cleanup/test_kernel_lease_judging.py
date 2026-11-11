#!/usr/bin/env python3
# anchor-selfcheck: unsupported read-only fixture tests; no source mutations
"""Pin missing real lease evidence to INVALID and direct coverage to skip."""
import importlib.util
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class KernelLeaseJudging(unittest.TestCase):
    def test_all_five_judges_need_a_real_parent_positive(self):
        module = load('kernel_lease_judge_fixture', HERE / 'negative_controls.py')
        common = {'mode': 'kernel-lease', 'parentProof': {'parentPid': 70, 'treeLockInode': 800},
                  'branchControls': 'FAIL', 'positiveRestored': True, 'passed': False,
                  'unownedPassed': True, 'foreignPassed': True}
        branches = {'cleanupPending': {'accepted': True, 'reason': None},
                    'token': {'accepted': True, 'reason': None}, 'timestamp': {'accepted': True, 'reason': None},
                    'flock': {'accepted': True, 'reason': None}}
        healthy = {**common, 'branches': branches}
        names = [name for name in module.JUDGES if name.startswith('kernel-source-lease-')]
        self.assertEqual(len(names), 5)
        for name in names:
            with self.subTest(name=name):
                def classify(rows):
                    with patch.object(module, 'read_summary', return_value=({'status':'FAIL','results':rows},None)):
                        return module.classify(name, module.JUDGES[name], SimpleNamespace(returncode=1), HERE)[0]
                self.assertEqual(classify([healthy]), 'KILLED')
                for rows in ([], [healthy, healthy], [{**healthy,'parentProof':None}],
                             [{**healthy,'positiveRestored':False}], [{**healthy,'branchControls':'NOT_RUN'}]):
                    self.assertEqual(classify(rows), 'INVALID')
                broken = {**healthy, 'branches': {}, 'unownedPassed': None}
                self.assertEqual(classify([broken]), 'INVALID')

    def test_direct_all_records_an_explicit_unexercised_branch(self):
        module = load('kernel_lease_cleanup_fixture', HERE / 'check_cleanup.py')
        own = SimpleNamespace(owner=lambda _: None, LOCK=Path('/tmp/pdrs-parity.lock'),
                              aggregate_status=lambda *statuses: ('PASS_WITH_SKIPS', 1)
                              if 'PASS_WITH_SKIPS' in statuses else ('FAIL', 1))
        domain = SimpleNamespace(common=own)
        module.md = domain
        reasons = ['parent parity owner record is absent','parity owner is not the direct parent']
        rows = [{'status':'BLOCKED','passed':False,'report':None,'reason':reason} for reason in reasons]
        with patch.object(module, 'linux_kernel_controls', side_effect=rows), \
                patch.object(module, 'kernel_upper_lease', return_value=(None,'no live source owner')):
            value = module.kernel_lease_control(domain,HERE)
        self.assertIsNone(value['passed'])
        self.assertEqual(value['branchControls'], 'NOT_RUN')
        self.assertTrue(value['skipped'])
        aggregate, skipped = module.summarize_results([{'mode':'positive','passed':True},value])
        self.assertEqual(aggregate, 'PASS_WITH_SKIPS')
        self.assertEqual(skipped,[value])


if __name__ == '__main__':
    unittest.main()
