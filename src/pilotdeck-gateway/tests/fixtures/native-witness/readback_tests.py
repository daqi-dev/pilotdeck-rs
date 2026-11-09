#!/usr/bin/env python3
# anchor-selfcheck: unsupported collector readback regression; no source mutation table.
"""Candidate collector regression using immutable real native captures.

Unit loading uses the actual dependency tree, with candidate bytes explicitly
bound separately. It is not a formal collector provenance or scoring run.
"""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
import argparse
import copy
import hashlib
import json
from pathlib import Path
import types
import unittest
from unittest import mock

parser = argparse.ArgumentParser()
parser.add_argument('--root', type=Path, required=True)
parser.add_argument('--candidate', type=Path, required=True)
parser.add_argument('--capture', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=False)
candidate = args.candidate.read_bytes()
c = types.ModuleType('collector_readback_candidate')
c.__file__ = str(args.root / 'scripts/native-witness/collect.py')
exec(compile(candidate, str(args.candidate), 'exec'), c.__dict__)
folder = args.capture / 'session-domain'
rows = json.loads((folder / 'cases.json').read_text())['cases']
sources = json.loads((folder / 'sources.json').read_text())
selected = [(row, source) for row, source in zip(rows, sources)
            if row['case'].startswith('registry-') and source.get('collectionError')]
assert len(selected) == 8, 'fixed r38 real nonterminal corpus drifted'


class ReadbackTests(unittest.TestCase):
    def test_real_eight_nonterminal_reads_survive_but_never_score(self):
        for row, source in selected:
            with self.subTest(case=row['case']):
                identity = row['identity']
                home = args.capture / 'home'
                raw = c.w.native.disk_observation(source, identity, str(home))
                self.assertEqual(raw['registry']['record']['state'], 'running')
                self.assertTrue(raw['registry']['events'])
                with mock.patch.object(c, 'read_observation', return_value=(raw, identity, source)), \
                        mock.patch.object(c, 'SESSION_SETTLE_LIMIT_SECONDS', 0):
                    scored, actual, kept = c.wait_observation(
                        home, identity['projectKey'], identity['sessionKey'], identity['runId'],
                        [], identity['requestId'], c.time.monotonic(), expectation={'registry': {'applicable': True}})
                self.assertIsNone(scored)
                self.assertEqual(actual, identity)
                self.assertEqual(kept['lastReadableObservation'], raw)
                self.assertEqual(c.readable_registry_evidence(scored, kept), raw['registry'])
                # Even complete boundary attestation must not promote an
                # unconfirmed durable state into the scoring observation.
                with mock.patch.object(c.stimuli, 'execution', return_value={'status': 'WITNESSED'}):
                    result = c.session_row(row['case'], identity, scored, kept, {})
                self.assertIsNone(result['observation'])
                self.assertIn('落盘未完成', result['reason'])

    def test_missing_retained_read_is_not_fabricated(self):
        self.assertEqual(c.readable_registry_evidence(None, {}), {})
        for malformed in (None, [], 'not-an-observation'):
            self.assertEqual(c.readable_registry_evidence(None, {'lastReadableObservation': malformed}), {})

    def test_successful_observation_has_priority_over_stale_retained(self):
        current = {'registry': {'record': {'state': 'completed'}, 'events': []}}
        stale = {'lastReadableObservation': {'registry': {'record': {'state': 'running'}}}}
        self.assertEqual(c.readable_registry_evidence(current, stale), current['registry'])

    def test_nonterminal_state_is_not_relabelled(self):
        row, source = selected[0]
        raw = c.w.native.disk_observation(source, row['identity'], str(args.capture / 'home'))
        kept = {'lastReadableObservation': copy.deepcopy(raw)}
        self.assertEqual(c.readable_registry_evidence(None, kept)['record']['state'], 'running')
        self.assertEqual(kept['lastReadableObservation'], raw)


suite = unittest.defaultTestLoader.loadTestsFromTestCase(ReadbackTests)
with (args.output / 'tests.log').open('w') as log:
    result = unittest.TextTestRunner(stream=log, verbosity=2).run(suite)
binding = {'candidateSha256': hashlib.sha256(candidate).hexdigest(),
           'capture': str(args.capture), 'realCases': [r['case'] for r, _ in selected],
           'sourceSha256': hashlib.sha256((folder / 'sources.json').read_bytes()).hexdigest(),
           'passed': result.wasSuccessful(), 'formalAcceptance': False}
(args.output / 'result.json').write_text(json.dumps(binding, indent=2))
print(json.dumps(binding))
raise SystemExit(0 if result.wasSuccessful() else 1)
