#!/usr/bin/env python3
"""Offline recording completeness controls; no behavioral comparator."""
import json
import base64
from pathlib import Path
import tempfile
import unittest
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


if __name__ == '__main__':
    unittest.main()
