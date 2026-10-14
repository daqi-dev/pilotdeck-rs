#!/usr/bin/env python3
"""Offline recording completeness controls; no behavioral comparator."""
import json
from pathlib import Path
import tempfile
import unittest
from run import HERE, validate_recording


class RecordingControls(unittest.TestCase):
    def fixture(self):
        records = [{'kind': 'run_start'}]
        for number, case in enumerate(json.loads((HERE / 'scenarios.json').read_text())['http_cases'], 1):
            records.extend([{'kind': 'http_request', 'id': number, 'case': case['id']},
                            {'kind': 'http_response', 'id': number, 'status': case['expected_statuses'][0]}])
        records.append({'kind': 'run_end'})
        for seq, record in enumerate(records, 1):
            record['seq'] = seq
        return records

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
        records[2]['status'] = 599
        with self.assertRaisesRegex(RuntimeError, 'HTTP request/response'):
            self.validate(records)
        records = self.fixture()
        records[2]['seq'] = 1
        with self.assertRaisesRegex(RuntimeError, 'seq'):
            self.validate(records)

    def test_recording_error_is_not_pass(self):
        records = self.fixture()
        records.append({'kind': 'recorder_error', 'seq': len(records) + 1})
        with self.assertRaisesRegex(RuntimeError, 'error present'):
            self.validate(records)


if __name__ == '__main__':
    unittest.main()
