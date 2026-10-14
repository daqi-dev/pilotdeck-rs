#!/usr/bin/env python3
"""Offline recording completeness controls; no behavioral comparator."""
# Admit source-only apparatus before json or any local module can be imported.
import os as _boot_os
import sys as _boot_sys
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
with open(_boot_source, 'rb') as _boot_file:
    exec(compile(_boot_file.read(), _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import json
import base64
from pathlib import Path
import tempfile
import unittest
import contextlib
import io
from unittest.mock import patch
from common import ClockMonitor, ClockRefused, validate_clock_check, MAX_BRACKET_ATTEMPTS
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
    def monitor(self, brackets, walls):
        mono = iter(v for pair in brackets for v in pair)
        wall = iter(1700000000000000000 + v for v in walls)
        return ClockMonitor(wall=lambda: next(wall), monotonic=lambda: next(mono))

    def test_two_ms_bracket_then_three_ms_regression_is_refused(self):
        # Old predicate admitted equality: 3 ms drop minus 2 ms bracket = 1 ms.
        meta = self.simulate_monitor([0, 250_000_000], [0, 247_000_000])
        meta['clock_regression_check']['samples'][0]['mono_after_ns'] += 2_000_000
        meta['clock_regression_check']['status'] = 'PASS'
        first, second = meta['clock_regression_check']['samples']
        self.assertEqual(first['wall_ns'] - first['mono_after_ns']
                         - (second['wall_ns'] - second['mono_before_ns']), 1_000_000)
        with self.assertRaisesRegex(ClockRefused, 'bracket-delayed'):
            validate_clock_check(meta)
        # Live recording discards the broad bracket, immediately retries, and
        # the new narrow reading exposes the drop from the last valid sample.
        monitor = self.monitor([(1_000_000, 1_000_100), (251_000_000, 253_000_000),
                                (253_000_001, 253_000_101)], [0, 247_000_000, 249_000_001])
        with patch('common.threading.Thread'):
            meta = {'wall_start_ms': monitor.start()}
            check = monitor.finish(meta)
        self.assertEqual(check['status'], 'REFUSED')
        self.assertIn('relative to monotonic', check['reason'])
        self.assertEqual(len(check['rejected_samples']), 1)
        self.assertEqual(len(check['samples']), 2)

    def test_normal_bracket_and_immediate_retry_do_not_false_refuse(self):
        monitor = self.monitor([(1_000_000, 1_000_100), (251_000_000, 253_000_000),
                                (253_000_001, 253_000_101)], [0, 250_000_000, 252_000_001])
        with patch('common.threading.Thread'):
            meta = {'wall_start_ms': monitor.start()}
            check = monitor.finish(meta)
        self.assertEqual(check['status'], 'PASS')
        self.assertEqual(validate_clock_check(meta), 'present')
        self.assertEqual(len(check['rejected_samples']), 1)

    def test_adjacent_combined_bracket_budget_is_enforced(self):
        meta = self.meta()
        for sample in meta['clock_regression_check']['samples'][:2]:
            sample['mono_after_ns'] = sample['mono_before_ns'] + 600_000
        with self.assertRaisesRegex(ClockRefused, 'adjacent clock bracket'):
            validate_clock_check(meta)

    def test_retry_cannot_reset_two_second_gap(self):
        monitor = self.monitor([(1_000_000, 1_000_100), (1_999_000_000, 2_000_100_000),
                                (2_001_000_001, 2_001_000_101)], [0, 1_998_000_000, 2_000_000_001])
        with patch('common.threading.Thread'):
            meta = {'wall_start_ms': monitor.start()}
            check = monitor.finish(meta)
        self.assertEqual(check['status'], 'REFUSED')
        self.assertIn('gap', check['reason'])
        self.assertEqual(len(check['samples']), 1)

    def test_repeated_broad_start_and_end_are_persisted_as_refused(self):
        import run
        for phase in ('start', 'end'):
            broad = [(10_000_000 + i * 3_000_000, 12_000_000 + i * 3_000_000)
                     for i in range(MAX_BRACKET_ATTEMPTS)]
            brackets = ([(1_000_000, 1_000_100)] if phase == 'end' else []) + broad
            monitor = self.monitor(brackets, [i * 3_000_000 for i in range(len(brackets))])
            with tempfile.TemporaryDirectory() as tmp, patch('common.threading.Thread'), \
                    patch.object(run, 'ClockMonitor', return_value=monitor), \
                    patch.object(run, 'source_hashes', return_value={}), \
                    patch.object(run, 'record_monitored_side', return_value={str(i): {'status': 'PASS'} for i in range(6)}):
                with self.subTest(phase=phase), self.assertRaises(ClockRefused):
                    run.record_side(Path(tmp), 'direct')
                check = json.loads((Path(tmp) / 'direct/run-meta.json').read_text())['clock_regression_check']
                self.assertEqual(check['status'], 'REFUSED')
                self.assertIn('8 consecutive attempts', check['reason'])
                self.assertEqual(len(check['rejected_samples']), MAX_BRACKET_ATTEMPTS)

    def test_periodic_refusal_is_not_repaired_by_good_end(self):
        brackets = [(1_000_000, 1_000_100)] + [
            (10_000_000 + i * 3_000_000, 12_000_000 + i * 3_000_000)
            for i in range(MAX_BRACKET_ATTEMPTS)] + [(50_000_000, 50_000_100)]
        monitor = self.monitor(brackets, [i * 3_000_000 for i in range(len(brackets))])
        with patch('common.threading.Thread'):
            meta = {'wall_start_ms': monitor.start()}
            with self.assertRaises(ClockRefused) as caught:
                monitor.sample('periodic')
            monitor.error = str(caught.exception)  # _poll preserves this failure.
            check = monitor.finish(meta)
        self.assertEqual(check['status'], 'REFUSED')
        self.assertIn('8 consecutive attempts', check['reason'])

    def simulate_monitor(self, elapsed, walls):
        wall = iter(1700000000000000000 + n for n in walls)
        # Zero-width brackets make the exact 1 ms boundary observable.
        mono = iter(1000000 + n for t in elapsed for n in (t, t))
        monitor = ClockMonitor(wall=lambda: next(wall), monotonic=lambda: next(mono))
        with patch('common.threading.Thread'):
            meta = {'wall_start_ms': monitor.start()}
            for _ in elapsed[1:-1]:
                try:
                    monitor.sample('periodic')
                except ClockRefused as error:
                    monitor.error = str(error)  # Same retention as the production polling thread.
                    break
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
                patch.object(run, 'record_monitored_side', return_value={str(i): {'status': 'PASS'} for i in range(6)}):
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

    def test_rejected_broad_sample_with_proven_rollback_cannot_be_retried_away(self):
        for walls in ([0, -1, 252_000_001], [0, 250_000_000, 249_999_999]):
            monitor = self.monitor([(1_000_000, 1_000_100), (251_000_000, 253_000_000),
                                    (253_000_001, 253_000_101)], walls)
            monitor.sample('start')
            with self.subTest(walls=walls), self.assertRaisesRegex(ClockRefused, 'including rejected brackets'):
                monitor.sample('periodic')
            self.assertEqual(len(monitor.samples), 1)
            self.assertEqual(monitor.rejected_samples[-1]['reason'], 'wall clock regression between adjacent samples')

    def test_broad_start_retry_with_wall_rollback_is_refused(self):
        monitor = self.monitor([(1_000_000, 3_000_000), (3_000_001, 3_000_101)], [0, -1])
        with self.assertRaisesRegex(ClockRefused, 'including rejected brackets'):
            monitor.start()
        check = monitor.finish({})
        self.assertEqual(check['status'], 'REFUSED')
        self.assertEqual(len(check['rejected_samples']), 2)

    def test_archive_cannot_claim_PASS_after_rollback_in_rejected_sample(self):
        meta = self.simulate_monitor([0, 250_000_000], [0, 250_000_000])
        first = meta['clock_regression_check']['samples'][0]
        meta['clock_regression_check']['rejected_samples'] = [{
            'phase': 'periodic', 'wall_ns': first['wall_ns'] - 1,
            'mono_before_ns': first['mono_after_ns'] + 1, 'mono_after_ns': first['mono_after_ns'] + 2_000_001}]
        with self.assertRaisesRegex(ClockRefused, 'including rejected brackets'):
            validate_clock_check(meta)

    def test_dut_fail_and_clock_refusal_are_both_saved_without_masking_fail(self):
        import run
        for scenario_fail in (False, True):
            monitor = self.monitor([(1_000_000, 1_000_100), (251_000_000, 251_000_100)], [0, -1])
            outcome = {'return_value': {'case': {'status': 'FAIL'}}} if scenario_fail else {
                'side_effect': RuntimeError('DUT recording FAIL')}
            with tempfile.TemporaryDirectory() as tmp, patch('common.threading.Thread'), \
                    patch.object(run, 'ClockMonitor', return_value=monitor), \
                    patch.object(run, 'source_hashes', return_value={}), \
                    patch.object(run, 'record_monitored_side', **outcome):
                with self.subTest(scenario_fail=scenario_fail), self.assertRaises(RuntimeError) as caught:
                    run.record_side(Path(tmp), 'proxy')
                self.assertNotIsInstance(caught.exception, ClockRefused)
                meta = json.loads((Path(tmp) / 'proxy/run-meta.json').read_text())
                self.assertEqual(meta['dut_result']['status'], 'FAIL')
                self.assertEqual(meta['clock_regression_check']['status'], 'REFUSED')

    def test_recording_exit_precedence_across_sides_and_coverage(self):
        import run
        passed = {str(i): {'status': 'PASS'} for i in range(6)}
        failed = {**passed, '0': {'status': 'FAIL'}}
        cases = [([ClockRefused('clock'), RuntimeError('DUT FAIL')], 1, 'FAIL'),
                 ([RuntimeError('DUT FAIL'), ClockRefused('clock')], 1, 'FAIL'),
                 ([ClockRefused('clock'), failed], 1, 'FAIL'),
                 ([ClockRefused('clock'), passed], 2, 'REFUSED'),
                 ([passed, passed], 0, 'PASS')]
        for outcomes, expected_code, status in cases:
            with tempfile.TemporaryDirectory() as tmp, \
                    patch('sys.argv', ['run.py', '--out', str(Path(tmp) / 'capture')]), \
                    patch.object(run, 'build'), patch.object(run, 'enumerate_routes'), \
                    patch.object(run.b1, 'make_home'), \
                    patch.object(run.subprocess, 'run', return_value=type('Result', (), {'returncode': 0})()), \
                    patch.object(run, 'record_side', side_effect=outcomes), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.subTest(status=status, outcomes=outcomes):
                    self.assertEqual(run.main(), expected_code)
                    report = json.loads((Path(tmp) / 'capture/result.json').read_text())
                    self.assertEqual(report['status'], status)

    def test_real_wrapper_to_main_keeps_dut_FAIL_and_clock_REFUSED_in_result(self):
        import run
        monitor = self.monitor([(1_000_000, 1_000_100), (251_000_000, 251_000_100)], [0, -1])
        with tempfile.TemporaryDirectory() as tmp, patch('common.threading.Thread'), \
                patch('sys.argv', ['run.py', '--out', str(Path(tmp) / 'capture'), '--side', 'proxy']), \
                patch.object(run, 'ClockMonitor', return_value=monitor), \
                patch.object(run, 'source_hashes', return_value={}), \
                patch.object(run, 'build'), patch.object(run, 'enumerate_routes'), \
                patch.object(run.b1, 'make_home'), patch.object(run.subprocess, 'run'), \
                patch.object(run, 'record_monitored_side', side_effect=RuntimeError('DUT failed')), \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(run.main(), 1)
            report = json.loads((Path(tmp) / 'capture/result.json').read_text())
            self.assertEqual(report['status'], 'FAIL')
            self.assertEqual(report['capture_checks']['proxy']['dut_result']['status'], 'FAIL')
            self.assertEqual(report['capture_checks']['proxy']['clock_regression_check']['status'], 'REFUSED')


if __name__ == '__main__':
    unittest.main()
