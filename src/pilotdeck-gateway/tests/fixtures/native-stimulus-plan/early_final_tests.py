#!/usr/bin/env python3
# anchor-selfcheck: unsupported collector schedule regression; no source mutation table.
"""Unscored schedule controls and a read-only retained-45b predicate replay."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import time
import types
import unittest

p = argparse.ArgumentParser()
p.add_argument('--root', type=Path, required=True)
p.add_argument('--candidate', type=Path, required=True)
p.add_argument('--baseline-driver', type=Path, required=True)
p.add_argument('--capture', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--new-only', action='store_true')
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=False)


def driver(path):
    module = types.ModuleType('schedule_candidate_' + path.stem)
    module.__file__ = str(a.root / 'scripts/native-witness/stimulus_driver.py')
    exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
    return module


spec = importlib.util.spec_from_file_location('existing_plan_tests', a.root / 'src/pilotdeck-gateway/tests/fixtures/native-stimulus-plan/test_plan.py')
t = importlib.util.module_from_spec(spec)
spec.loader.exec_module(t)
old = driver(a.baseline_driver)
new = driver(a.candidate)
t.c.stimulus_driver = new


class EarlyFinalTests(unittest.TestCase):
    def test_final_does_not_disarm_late_configured_storage_actions(self):
        case = 'registry-slow/status-fast-append-slow/model'
        prefix = t.frame('case-01', 0, {'type': 'model_request_started'})
        final = t.frame('case-01', 2, {'type': 'error', 'code': 'turn_timeout'}, final=True)

        def delayed(product, phase, key, seq):
            # Exceeds the helper's fixed final top-up (0.05s), yet is well
            # inside its unchanged per-barrier case budget (1s).
            time.sleep(0.08)
            product.storage('registry', phase, key, seq)

        steps = [
            lambda product: (product.loop('input', case + '#1'), prefix)[1],
            lambda product: product.loop('wait', case + '#1', planPhase='model'),
            lambda product: product.storage('registry', 'block', 'agent_status:turn_timeout', 1),
            lambda product: product.loop('final', 'result_unknown'),
            final,
            lambda product: delayed(product, 'queued', 'error:turn_timeout', 2),
            lambda product: delayed(product, 'release', 'agent_status:turn_timeout', 3),
            lambda product: delayed(product, 'block', 'error:turn_timeout', 4),
            lambda product: delayed(product, 'release', 'error:turn_timeout', 5),
        ]
        evidence = t.drive_case(case, steps)
        verdict = t.c.stimuli.execution(t.c.case_plan(case)['stimulus'], evidence)
        self.assertTrue(verdict['status'] == 'WITNESSED',
                        '#schedule: late configured actions survive wire final: ' + repr(verdict['missing']))
        self.assertTrue(any(row['phase'] == 'release' and row['key'] == 'error:turn_timeout'
                            for row in evidence['storageReceipts']),
                        '#schedule: observes the actual last release before ending collection')

    def test_retained_45b_partial_actions_are_not_a_complete_turn(self):
        folder = a.capture / 'session-domain'
        rows = json.loads((folder / 'cases.json').read_text())['cases']
        case = 'registry-slow/status-fast-append-slow/model'
        index, row = next((index, row) for index, row in enumerate(rows, 1) if row['case'] == case)
        identity = row['identity']
        storage = [json.loads(path.read_text()) for path in (a.capture / 'stimulus/storage/witness').glob('*.json')]
        block = next(value for value in storage if value.get('appendRunId') == identity['runId']
                     and value['phase'] == 'block' and value['key'] == 'agent_status:turn_timeout')
        queued = next(value for value in storage if value.get('appendRunId') == identity['runId']
                      and value['phase'] == 'queued' and value['key'] == 'error:turn_timeout')
        release = min((value for value in storage if value['site'] == 'registry'
                       and value['phase'] == 'release' and value['key'] == block['key']
                       and value['seq'] > block['seq']), key=lambda value: value['seq'])
        self.assertNotIn('bounded', release)
        self.assertGreater(queued['seq'], block['seq'])
        self.assertFalse(any(value.get('appendRunId') == identity['runId'] and value['phase'] == 'block'
                             and value['key'] == 'error:turn_timeout' for value in storage))
        observer = json.loads((folder / 'observer.json').read_text())['rows']
        final = next(value['frame'] for value in observer if value['direction'] == 'receive'
                     and isinstance(value.get('frame'), dict) and value['frame'].get('id') == identity['requestId']
                     and value['frame'].get('final') is True)
        loop = [json.loads(path.read_text()) for path in (a.capture / 'stimulus/loop/witness').glob('*.json')]
        plan_id = case + '#' + str(index)
        pids = {value['pid'] for value in loop if value.get('key') == plan_id and value['phase'] == 'input'}
        loop = [value for value in loop if value.get('pid') in pids]
        answers = {}
        for label, implementation in [('before', old), ('after', new)]:
            with tempfile.TemporaryDirectory(prefix='pdrs-real-schedule-') as temporary:
                instance = implementation.StimulusDriver(Path(temporary) / 'loop', Path(temporary) / 'storage', registry_on=True)
                plan = t.c.stimuli.plan(case)
                instance.prepare(case, index, plan, identity['runId'])
                state = instance._active
                state.update(final=final, outcome='unknown', loopReceipts=loop, storageReceipts=storage)
                # Under the unchanged old driver, a non-bounded first release
                # with a second-arm plan implies rearm-before-release happened.
                # No unavailable action timestamp or second receipt is invented.
                state['storageSteps']['slowAppend'] = {'phase': 'second-arm', 'blockSeq': block['seq']}
                answers[label] = instance._turn_complete(plan)
        self.assertTrue(answers['before'])
        self.assertFalse(answers['after'], '#schedule: retained incomplete actions are not complete')
        (a.output / 'real-45b-replay.json').write_text(json.dumps({
            'case': case, 'capture': str(a.capture), 'before': answers['before'], 'after': answers['after'],
            'retainedVerdict': row['stimulus']['status'], 'retainedMissing': row['stimulus']['missing'],
            'facts': {'block': block, 'queued': queued, 'firstRelease': release, 'wireFinal': final},
            'derivation': 'Read-only completion-predicate replay; no original raw receipt/order rewritten, no new runtime witness claimed.',
            'formalAcceptance': False,
        }, indent=2))


suite = unittest.TestSuite()
if not a.new_only:
    suite.addTests(unittest.defaultTestLoader.loadTestsFromModule(t))
suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(EarlyFinalTests))
with (a.output / 'tests.log').open('w') as log:
    result = unittest.TextTestRunner(stream=log, verbosity=2).run(suite)
(a.output / 'result.json').write_text(json.dumps({
    'candidateSha256': hashlib.sha256(a.candidate.read_bytes()).hexdigest(),
    'baselineSha256': hashlib.sha256(a.baseline_driver.read_bytes()).hexdigest(),
    'realCaseSourceSha256': hashlib.sha256((a.capture / 'session-domain/cases.json').read_bytes()).hexdigest(),
    'tests': result.testsRun, 'passed': result.wasSuccessful(), 'formalAcceptance': False,
}, indent=2))
raise SystemExit(0 if result.wasSuccessful() else 1)
