"""Calibration safety tests with fake runner output; never build or listen."""
from datetime import datetime, timezone
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import calibrate
from calibrate import Batch, gate_report
from common import ClockRefused
from controls.run_all import clock_evidence


class CalibrationControls(unittest.TestCase):
    def test_exit_two_is_not_enough_to_pass_ruler_gate(self):
        good = dict(verdict='PENDING_REVIEW', calibration_eligible=True, comparison_mode='calibration',
                    self_compare=True, pairs=[{'verdict': 'PASS'} for _ in range(10)],
                    per_rule_negative=True, same_bad_rejected=True, errors=[], independent_review=None,
                    proxy_compare={'verdict': 'PASS'})
        gate_report(good, proxy=True)
        for mutation in ({'verdict': 'REFUSED'}, {'comparison_mode': 'historical-replay'},
                         {'pairs': [{'verdict': 'FAIL'}] * 10}, {'pairs': []},
                         {'proxy_compare': {'verdict': 'FAIL'}}, {'calibration_eligible': False}):
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                gate_report({**good, **mutation}, proxy=True)

    def batch(self, tmp):
        root = Path(tmp)
        (root / 'attempts').mkdir()
        state = dict(baseline='/readonly/baseline', utc_day=calibrate.utc_day(), runs={},
                     pinned={'apparatus': {}, 'source': {'node': 'a' * 64, 'baseline': 'b' * 64,
                                                       'recorder_proxy': 'c' * 64}})
        return Batch(root, state)

    def fake_run(self, command, **kwargs):
        slot = Path(command[command.index('--out') + 1])
        (slot / 'direct').mkdir(parents=True)
        (slot / 'direct/run-meta.json').write_text('{"clock_regression_check":{"status":"REFUSED"}}')
        (slot / 'result.json').write_text('{"status":"REFUSED"}')
        return type('Result', (), {'returncode': 2})()

    def test_refused_run_archived_and_only_named_slot_retried(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            with patch.object(batch, 'guard'), patch('calibrate.subprocess.run', side_effect=self.fake_run) as run:
                for retry in (False, True):
                    with self.assertRaisesRegex(ValueError, 'retry ONLY this run'):
                        batch.capture('ref1', retry=retry)
                self.assertEqual(run.call_count, 2)
            self.assertEqual(set(batch.state['runs']), {'ref1'})
            self.assertEqual(batch.state['runs']['ref1']['status'], 'REFUSED')
            attempts = batch.state['attempts']
            self.assertNotEqual(attempts[0]['archive'], attempts[1]['archive'])
            for entry in attempts:
                self.assertEqual(json.loads((Path(entry['archive']) / 'result.json').read_text())['status'], 'REFUSED')
            self.assertFalse(batch.slot.exists())

    def test_success_is_archived_once_and_resume_never_recaptures_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)

            def success(command, **kwargs):
                slot = Path(command[command.index('--out') + 1])
                (slot / 'direct').mkdir(parents=True)
                (slot / 'result.json').write_text('{"status":"PASS"}')
                return type('Result', (), {'returncode': 0})()

            with patch.object(batch, 'guard'), patch.object(batch, 'admit', return_value={'run_id': 'fresh'}), \
                    patch('calibrate.subprocess.run', side_effect=success) as run:
                batch.capture('ref1')
                batch.capture('ref1')
                self.assertEqual(run.call_count, 1)
            self.assertEqual(batch.state['attempts'][0]['archive'], batch.state['runs']['ref1']['archive'])
            self.assertTrue(Path(batch.state['attempts'][0]['archive']).is_dir())
            self.assertFalse(batch.slot.exists())

    def test_retry_cannot_replace_pool_after_scoring_or_overwrite_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            batch.state['runs'] = {'ref1': {'status': 'REFUSED'}, 'ts1': {'status': 'PASS'}}
            with self.assertRaisesRegex(ValueError, 'pool already consumed'):
                batch.capture('ref1', retry=True)
            batch.state['runs']['ref1']['status'] = 'PASS'
            with self.assertRaisesRegex(ValueError, 'exact run'):
                batch.capture('ref1', retry=True)
            with self.assertRaisesRegex(ValueError, 'earlier slots'):
                batch.capture('ref3')

    def test_resume_rejects_day_or_source_change(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            with patch('calibrate.utc_day', return_value='other-day'), self.assertRaisesRegex(ValueError, 'UTC midnight'):
                batch.guard()
            with patch.object(batch, 'pin', return_value={}), self.assertRaisesRegex(ValueError, 'source changed'):
                batch.guard()

    def test_pass_capture_with_missing_or_bad_clock_is_not_admitted(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            directory = Path(tmp) / 'ref1/direct'
            directory.mkdir(parents=True)
            for meta in ({}, {'clock_regression_check': {'schema': 1, 'status': 'REFUSED', 'reason': 'wide bracket'}}):
                (directory / 'run-meta.json').write_text(json.dumps(meta))
                with self.assertRaises(ClockRefused):
                    batch.admit('ref1')

    def test_source_path_day_and_overlap_are_checked_on_admission(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            directory = Path(tmp) / 'ref2/direct'
            directory.mkdir(parents=True)
            start = int(datetime.now(timezone.utc).timestamp() * 1000)
            meta = dict(baseline_path=batch.state['baseline'], wall_start_ms=start, wall_end_ms=start + 1000,
                        clock_regression_check=clock_evidence(start, start + 1000))
            (directory / 'run-meta.json').write_text(json.dumps(meta))
            identity = dict(source_sha256=batch.state['pinned']['source'], capture_utc_day=batch.state['utc_day'],
                            fixture_path=str(batch.slot / 'direct'), run_id='fresh', recorder_pid=200,
                            recording_sha256='e' * 64, wall_start_ms=start, wall_end_ms=start + 1000)
            with patch('calibrate.capture_identity', return_value=identity):
                self.assertEqual(batch.admit('ref2'), identity)
            for mutation in ({'capture_utc_day': 'other-day'}, {'fixture_path': '/another/slot'},
                             {'source_sha256': {}}):
                with patch('calibrate.capture_identity', return_value={**identity, **mutation}), self.assertRaises(ValueError):
                    batch.admit('ref2')
            old = {**identity, 'run_id': 'previous', 'recorder_pid': 100, 'recording_sha256': 'f' * 64}
            batch.state['runs']['ref1'] = {'status': 'PASS', 'identity': old}
            with patch('calibrate.capture_identity', return_value=identity), self.assertRaisesRegex(ValueError, 'overlap'):
                batch.admit('ref2')

    def test_space_preflight_uses_projected_total_and_disk_free(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'reference/direct').mkdir(parents=True)
            (root / 'reference/direct/recording.jsonl').write_text('x')
            free = type('Disk', (), {'free': 50_000_000_000})()
            with patch('calibrate.size', side_effect=[12_000_000, 29_000_000_000]), \
                    patch('calibrate.shutil.disk_usage', return_value=free), self.assertRaisesRegex(ValueError, 'space budget'):
                calibrate.space_check(root, root / 'reference')
            with patch('calibrate.size', side_effect=[12_000_000, 16_000_000_000]), \
                    patch('calibrate.shutil.disk_usage', return_value=free):
                report = calibrate.space_check(root, root / 'reference')
            self.assertEqual(report['projected_bytes'], 18_468_000_000)
