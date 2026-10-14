#!/usr/bin/env python3
"""Offline recording completeness controls; no behavioral comparator."""
import json
import base64
from pathlib import Path
import tempfile
import unittest
import contextlib
import io
from unittest.mock import patch
from common import ClockMonitor, ClockRefused, validate_clock_check
from controls.run_all import clock_evidence
from run import HERE, validate_recording


class RecordingControls(unittest.TestCase):
    @staticmethod
    def length_header(length):
        return {'name': 'content-length', 'value_b64': base64.b64encode(str(length).encode()).decode()}

    @staticmethod
    def sequence(records):
        for seq, record in enumerate(records, 1):
            record['seq'] = seq
        return records

    def fixture(self):
        records = [{'kind': 'run_start'}]
        for number, case in enumerate(json.loads((HERE / 'scenarios.json').read_text())['http_cases'], 1):
            records.extend([
                {'kind': 'http_request', 'id': number, 'case': case['id'], 'headers': []},
                {'kind': 'http_end', 'id': number, 'direction': 'request', 'offset': 0},
                {'kind': 'http_response', 'id': number, 'status': case['expected_statuses'][0],
                 'headers': [self.length_header(3)]},
                {'kind': 'http_chunk', 'id': number, 'direction': 'response', 'offset': 0,
                 'bytes': 1, 'body_b64': 'YQ=='},
                {'kind': 'http_chunk', 'id': number, 'direction': 'response', 'offset': 1,
                 'bytes': 2, 'body_b64': 'YmM='},
                {'kind': 'http_end', 'id': number, 'direction': 'response', 'offset': 3}])
        records.append({'kind': 'run_end'})
        return self.sequence(records)

    def validate(self, records):
        with tempfile.TemporaryDirectory(prefix='exp010-log-') as directory:
            path = Path(directory) / 'recording.jsonl'
            path.write_text(''.join(json.dumps(row) + '\n' for row in records))
            validate_recording(path)

    def test_complete_recording_and_missing_end(self):
        records = self.fixture()
        self.validate(records)
        with self.assertRaisesRegex(RuntimeError, 'start/end'):
            self.validate(records[:-1])

    def test_missing_case_response_and_seq_gap_fail(self):
        records = self.fixture()
        records[3]['status'] = 599
        with self.assertRaisesRegex(RuntimeError, 'HTTP request/response'):
            self.validate(records)
        records = self.fixture()
        records[2]['seq'] = 1
        with self.assertRaisesRegex(RuntimeError, 'seq'):
            self.validate(records)

    def test_each_direction_requires_http_end(self):
        for direction in ('request', 'response'):
            with self.subTest(direction=direction):
                records = [r for r in self.fixture() if not
                           (r.get('id') == 1 and r.get('direction') == direction and r['kind'] == 'http_end')]
                with self.assertRaisesRegex(RuntimeError, f'{direction}: body must finish'):
                    self.validate(self.sequence(records))

    def test_http_aborted_rejected_even_with_later_end(self):
        for direction in ('request', 'response'):
            with self.subTest(direction=direction):
                records = self.fixture()
                records.insert(-1, {'kind': 'http_aborted', 'id': 1, 'direction': direction, 'offset': 0})
                with self.assertRaisesRegex(RuntimeError, 'http_aborted in case'):
                    self.validate(self.sequence(records))

    def test_content_length_must_match_both_directions(self):
        for direction, kind in (('request', 'http_request'), ('response', 'http_response')):
            for length in (1, 4):
                with self.subTest(direction=direction, length=length):
                    records = self.fixture()
                    metadata = next(r for r in records if r.get('id') == 1 and r['kind'] == kind)
                    metadata['headers'] = [self.length_header(length)]
                    with self.assertRaisesRegex(RuntimeError, 'disagrees with Content-Length'):
                        self.validate(records)

    def test_chunked_trailers_and_empty_body_pass(self):
        records = self.fixture()
        records[1]['headers'] = [self.length_header(0)]
        records[3]['headers'] = []  # Chunked/unknown length still requires EOF.
        records.insert(6, {'kind': 'http_chunk', 'id': 1, 'direction': 'response', 'offset': 3,
                           'trailers': [{'name': 'x-last', 'value_b64': 'b25l'}]})
        self.validate(self.sequence(records))

    def test_chunk_and_terminal_offsets_must_match_bytes(self):
        for index in (4, 5, 6):
            with self.subTest(index=index):
                records = self.fixture()
                records[index]['offset'] += 1
                with self.assertRaisesRegex(RuntimeError, 'offset disagrees'):
                    self.validate(records)

    def test_ws_partial_in_case_rejected(self):
        records = self.fixture()
        records[-1:-1] = [
            {'kind': 'http_request', 'id': 100, 'case': 'ws.watch.1'},
            {'kind': 'http_response', 'id': 100, 'status': 101, 'connection': 100},
            {'kind': 'ws_end', 'connection': 100, 'error': None}]
        self.validate(self.sequence(records))
        records.insert(-1, {'kind': 'ws_partial', 'connection': 100, 'leg': 'downstream-read', 'wire_b64': 'gQ=='})
        with self.assertRaisesRegex(RuntimeError, 'ws_partial in case'):
            self.validate(self.sequence(records))

    def test_incomplete_repeat_cannot_hide_behind_success(self):
        records = self.fixture()
        repeat = dict(records[1], id=100)
        records.insert(-1, repeat)
        with self.assertRaisesRegex(RuntimeError, 'HTTP request/response evidence'):
            self.validate(self.sequence(records))

    def test_malformed_content_length_and_chunk_bytes_fail(self):
        for length in ('-1', 'three', '3, 4'):
            with self.subTest(length=length):
                records = self.fixture()
                records[3]['headers'] = [self.length_header(length)]
                with self.assertRaisesRegex(RuntimeError, 'invalid Content-Length'):
                    self.validate(records)
        records = self.fixture()
        records[4]['bytes'] = 2
        with self.assertRaisesRegex(RuntimeError, 'chunk bytes disagree'):
            self.validate(records)

    def test_recording_error_is_not_pass(self):
        records = self.fixture()
        records.append({'kind': 'recorder_error', 'seq': len(records) + 1})
        with self.assertRaisesRegex(RuntimeError, 'error present'):
            self.validate(records)


class CaptureClockControls(unittest.TestCase):
    def simulate_monitor(self, elapsed, walls):
        wall = iter(1700000000000000000 + n for n in walls)
        # Zero-width brackets make the exact 1 ms boundary observable.
        mono = iter(1000000 + n for t in elapsed for n in (t, t))
        monitor = ClockMonitor(wall=lambda: next(wall), monotonic=lambda: next(mono))
        with patch('common.threading.Thread'):
            meta = {'wall_start_ms': monitor.start()}
            for _ in elapsed[1:-1]:
                monitor.sample('periodic')
            monitor.finish(meta)
        return meta

    def test_negative_500ppm_slew_for_ten_minutes_is_admitted(self):
        elapsed = list(range(0, 600_000_000_000 + 1, 250_000_000))
        meta = self.simulate_monitor(elapsed, [t - t // 2000 for t in elapsed])
        self.assertEqual(len(meta['clock_regression_check']['samples']), 2401)
        self.assertEqual(meta['clock_regression_check']['status'], 'PASS')
        self.assertEqual(validate_clock_check(meta), 'present')

    def test_single_two_ms_step_is_refused_while_wall_still_increases(self):
        meta = self.simulate_monitor([0, 250_000_000, 500_000_000], [0, 248_000_000, 498_000_000])
        self.assertEqual(meta['clock_regression_check']['status'], 'REFUSED')
        # Recompute even if a recorder claims PASS, including replay.
        meta['clock_regression_check']['status'] = 'PASS'
        for historical in (False, True):
            with self.assertRaisesRegex(ClockRefused, 'relative to monotonic'):
                validate_clock_check(meta, historical=historical)

    def test_wall_backwards_has_no_tolerance(self):
        meta = self.simulate_monitor([0, 100, 200], [0, -1, 200])
        self.assertEqual(meta['clock_regression_check']['status'], 'REFUSED')
        self.assertIn('between adjacent samples', meta['clock_regression_check']['reason'])

    def test_offset_drop_tolerance_is_strictly_greater_than_one_ms(self):
        for drop, status in ((999_999, 'PASS'), (1_000_000, 'PASS'), (1_000_001, 'REFUSED')):
            with self.subTest(drop=drop):
                meta = self.simulate_monitor([0, 250_000_000], [0, 250_000_000 - drop])
                self.assertEqual(meta['clock_regression_check']['status'], status)

    def meta(self):
        return dict(wall_start_ms=1700000000000, wall_end_ms=1700000003000,
                    clock_regression_check=clock_evidence(1700000000000, 1700000003000))

    def test_missing_only_allowed_for_explicit_historical_replay(self):
        with self.assertRaises(ClockRefused):
            validate_clock_check({})
        self.assertEqual(validate_clock_check({}, historical=True), 'absent')
        self.assertEqual(validate_clock_check(self.meta()), 'present')

    def test_regression_recomputed_even_with_pass_status_and_historical_mode(self):
        for backwards in (False, True):
            meta = self.meta()
            # Both a sampled wall reversal and a step hidden by positive elapsed
            # wall time must be rejected. Never modify the machine clock.
            meta['clock_regression_check']['samples'][1]['wall_ns'] -= (2_000_000_000 if backwards else 100_000_000)
            for historical in (False, True):
                with self.assertRaisesRegex(ClockRefused, 'regression'):
                    validate_clock_check(meta, historical=historical)

    def test_gaps_missing_end_and_bounds_cannot_claim_checked(self):
        for mutation in ('gap', 'phase', 'bounds', 'status', 'empty'):
            meta = self.meta()
            samples = meta['clock_regression_check']['samples']
            if mutation == 'gap':
                del samples[1:3]
            elif mutation == 'phase':
                samples[-1]['phase'] = 'periodic'
            elif mutation == 'bounds':
                meta['wall_end_ms'] += 1
            elif mutation == 'status':
                meta['clock_regression_check']['status'] = 'REFUSED'
            else:
                samples.clear()
            with self.subTest(mutation=mutation), self.assertRaises(ClockRefused):
                validate_clock_check(meta)

    def test_monitor_records_start_periodic_end_and_refusal(self):
        for regression in (False, True):
            wall = iter([1700000000000000000, 1700000001000000000 - (100000000 if regression else 0),
                         1700000002000000000])
            mono = iter([1000000, 1000100, 1001000000, 1001000100, 2001000000, 2001000100])
            monitor = ClockMonitor(wall=lambda: next(wall), monotonic=lambda: next(mono))
            with patch('common.threading.Thread'):
                meta = {'wall_start_ms': monitor.start()}
                monitor.sample('periodic')
                check = monitor.finish(meta)
            self.assertEqual([s['phase'] for s in check['samples']], ['start', 'periodic', 'end'])
            self.assertEqual(check['status'], 'REFUSED' if regression else 'PASS')

    def test_capture_wrapper_persists_refused_evidence_without_services(self):
        import run
        wall = iter([1700000000000000000, 1700000000900000000])
        mono = iter([1000000, 1000100, 1001000000, 1001000100])
        monitor = ClockMonitor(wall=lambda: next(wall), monotonic=lambda: next(mono))
        with tempfile.TemporaryDirectory() as tmp, patch('common.threading.Thread'), \
                patch.object(run, 'ClockMonitor', return_value=monitor), \
                patch.object(run, 'source_hashes', return_value={}), \
                patch.object(run, 'record_monitored_side', return_value={}):
            with self.assertRaises(ClockRefused):
                run.record_side(Path(tmp), 'direct')
            meta = json.loads((Path(tmp) / 'direct/run-meta.json').read_text())
            self.assertEqual(meta['clock_regression_check']['status'], 'REFUSED')

    def test_recording_entrypoint_returns_exit_two_for_clock_refusal(self):
        import run
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'new-capture'
            with patch('sys.argv', ['run.py', '--out', str(out), '--side', 'direct']), \
                    patch.object(run, 'build'), patch.object(run, 'enumerate_routes'), \
                    patch.object(run.b1, 'make_home'), patch.object(run.subprocess, 'run'), \
                    patch.object(run, 'record_side', side_effect=ClockRefused('wall clock regression')), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = run.main()
            report = json.loads((out / 'result.json').read_text())
            self.assertEqual(code, 2)
            self.assertEqual(report['status'], 'REFUSED')
            self.assertEqual(report['sides']['direct']['device']['status'], 'REFUSED')


if __name__ == '__main__':
    unittest.main()
