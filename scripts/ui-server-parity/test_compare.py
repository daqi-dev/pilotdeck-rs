#!/usr/bin/env -S python3 -I
"""Boundary regressions beyond single-mutation controls; no live services."""
# Isolated bootstrap: verify the source loader before executing local code.
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:
    print('{"verdict":"FAIL","errors":["os must be preloaded from CPython 3.11+ frozen modules"]}')
    raise SystemExit(1)
import os as _boot_os
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (fix-r3 P1, same defect as #701 fix-r2). Nonexistent sys.path entries skip.
if any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.isdir(root)
                and _boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))
        for entry in _boot_sys.path):
    print('{"verdict":"FAIL","errors":["apparatus directory on sys.path; run the entry file directly with -I"]}')
    raise SystemExit(1)
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
import hashlib as _boot_hashlib
import subprocess as _boot_subprocess
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_os.path.isfile(_boot_manifest):
        with open(_boot_manifest) as _boot_file:
            _boot_pins = [line.split()[0] for line in _boot_file
                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]
        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:
            raise ValueError('apparatus bootstrap hash mismatch')
    else:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import base64
import copy
import json
import re
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from check_frozen import check as frozen
from check_ruler import check as ruler
from compare import Normalizer, Side, compare_sides, diff, read_jsonl, sse_events, validate_ndjson
from policy import (CHAT_FIXTURE_SESSIONS, CLOCK_FORMATS, CLOCK_PAIRS,
                    ISO_MS, SQL_S, EPOCH_MS, MTIME_MS)
from snapshot import capture

HERE = Path(__file__).resolve().parent
from apparatus import load_source
controls = load_source('control_fixtures', HERE / 'controls/run_all.py')


class ComparatorBoundaries(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='b2-boundary-')
        self.root = Path(self.temp.name)
        self.enumeration, self.matrix, self.manifest = controls.fixture(self.root)
        self.left, self.right = self.root / 'left', self.root / 'right'

    def tearDown(self):
        self.temp.cleanup()

    def compare(self):
        return compare_sides(self.left, self.right, self.enumeration, self.matrix, self.manifest)

    def mutate_log(self, fn):
        path = self.right / 'recording.jsonl'
        rows = read_jsonl(path)
        fn(rows)
        controls.write_log(path, rows)

    def test_activity_revision_gaps_and_interleaved_sessions_use_exact_oob(self):
        for side, revision_map in ((self.left, {7: 101, 11: 900}), (self.right, {7: 3, 11: 8000})):
            rows = read_jsonl(side / 'recording.jsonl')
            def rewrite(body):
                if body.get('type') == 'session-activity':
                    body['activity']['revision'] = revision_map[body['activity']['revision']]
                elif body.get('type') == 'session-activity-snapshot':
                    for item in body['activities']:
                        item['revision'] = revision_map[item['revision']]
            controls.mutate_frames(rows, rewrite)
            controls.write_log(side / 'recording.jsonl', rows)
            path = side / 'activity-direct.jsonl'
            observers = read_jsonl(path)
            for row in observers:
                for item in row['frame']['activities']:
                    item['revision'] = revision_map[item['revision']]
            # Other sessions/users consume global revisions between this user's
            # revisions. An overlapping session is witnessed only via direct IO.
            extra = dict(observers[1], kind='ws_frame', dir='u2c', opcode='text')
            extra.pop('frame')
            extra['payload_b64'] = controls.b64(json.dumps({'type': 'session-activity', 'activity': {
                'sessionId': 'session-2', 'revision': revision_map[7]+1, 'processing': True}}))
            observers.insert(2, extra)
            for index, row in enumerate(observers, 1):
                row['seq'], row['arrival_ns'] = index, str(index)
            path.write_text(''.join(json.dumps(row)+'\n' for row in observers))
        self.assertEqual(self.compare()['verdict'], 'PASS')
        def shift_only_dut(rows):
            controls.mutate_frames(rows, lambda body: body.get('activity', {}).update(revision=999999)
                                   if body.get('type') == 'session-activity' else None)
        self.mutate_log(shift_only_dut)
        self.assertIn('activity.out-of-band', self.compare()['rules'])

    def test_ndjson_integrity_is_checked_even_when_both_sides_have_same_bad_bytes(self):
        for raw, valid, path in ((b'{"value":"a"}\n{"value":"b"}\n', True, '/api/ndjson'),
                                 (b'{"value":"unterminated', False, '/api/ndjson'),
                                 (b'{"stage":"complete"}', False, '/api/update/apply/')):
            for side in (self.left, self.right):
                rows = read_jsonl(side / 'recording.jsonl')
                rows = [row for row in rows if row.get('id') != 99]
                response = copy.deepcopy(next(row for row in rows if row['kind'] == 'http_response' and row['id'] == 3))
                response['id'] = 99
                response['headers'] = [controls.hdr('content-type', 'application/x-ndjson')] + (
                    [controls.hdr('x-pilotdeck-handler', 'ts')] if side == self.right else [])
                method = 'POST' if path.startswith('/api/update') else 'GET'
                extra = [{'kind': 'http_request', 'id': 99, 'method': method, 'path': path,
                          'query': None, 'headers': [], 'case': f'_probe:{method}:{path}'},
                         {'kind': 'http_end', 'id': 99, 'direction': 'request', 'offset': 0},
                         response,
                         {'kind': 'http_chunk', 'id': 99, 'direction': 'response', 'offset': 0,
                          'bytes': len(raw), 'body_b64': controls.b64(raw)},
                         {'kind': 'http_end', 'id': 99, 'direction': 'response', 'offset': len(raw)}]
                for row in extra:
                    row["arrival_ns"] = rows[-1]["arrival_ns"]
                rows[-1:-1] = extra
                controls.write_log(side / 'recording.jsonl', rows)
            report = self.compare()
            if valid:
                self.assertEqual(report['verdict'], 'PASS', report['differences'])
            else:
                self.assertTrue(any(d['rule'] == 'evidence.integrity' and '.ndjson' in d['location']
                                    for d in report['differences']))

    def test_multivalue_header_name_case_and_grouping(self):
        def mutate(rows):
            for row in rows:
                if row['kind'] == 'http_response':
                    # Order between DIFFERENT names is irrelevant, per-name order is not.
                    row['headers'].sort(key=lambda h: h['name'], reverse=True)
                    for h in row['headers']:
                        h['name'] = h['name'].upper()
        self.mutate_log(mutate)
        self.assertEqual(self.compare()['verdict'], 'PASS')
        def swap(rows):
            for row in rows:
                if row['kind'] == 'http_response' and row['id'] == 1:
                    cookies = [h for h in row['headers'] if h['name'].lower() == 'set-cookie']
                    cookies[0]['value_b64'], cookies[1]['value_b64'] = cookies[1]['value_b64'], cookies[0]['value_b64']
        self.mutate_log(swap)
        self.assertIn('http.headers', self.compare()['rules'])

    def test_trailer_date_is_literal_not_header_clock(self):
        for side, value in ((self.left, 'a'), (self.right, 'b')):
            path = side / 'recording.jsonl'
            rows = read_jsonl(path)
            end = next(r for r in rows if r['kind'] == 'http_end' and r['id'] == 1 and r['direction'] == 'response')
            trailer = {**end, 'kind': 'http_chunk', 'trailers': [controls.hdr('Date', value)]}
            rows.insert(rows.index(end), trailer)
            controls.write_log(path, rows)
        self.assertTrue(any(d['rule'] == 'http.headers' and d['location'].endswith('trailers.date[0]') for d in self.compare()['differences']))

    def test_sse_rechunk_is_equivalent(self):
        def mutate(rows):
            chunks = [r for r in rows if r['kind'] == 'http_chunk' and r['id'] == 3 and r['direction'] == 'response']
            first = chunks[0]
            data = base64.b64decode(first['body_b64'])
            cut = len(data) // 2
            first.update(body_b64=controls.b64(data[:cut]), bytes=cut)
            second = {**first, 'body_b64': controls.b64(data[cut:]), 'bytes': len(data) - cut, 'offset': cut}
            rows.insert(rows.index(first) + 1, second)
        self.mutate_log(mutate)
        self.assertEqual(self.compare()['verdict'], 'PASS')

    def test_sse_crlf_multiline_resume_and_unterminated_dispatch(self):
        raw = b'\xef\xbb\xbfid: 7\r\nevent: progress\r\ndata: one\r\ndata: two\r\n\r\ndata: done\r\n\r\n'
        errors = []
        events = sse_events(raw, [(len(raw), 100)], 1, errors, 'sse', 'left')
        self.assertFalse(errors)
        self.assertEqual([e['id'] for e in events], ['7', '7'])
        self.assertEqual(events[0]['data'], 'one\ntwo')
        self.assertEqual([e['event'] for e in events], ['progress', 'message'])
        errors = []
        sse_events(raw[:-1], [(len(raw) - 1, 100)], 1, errors, 'sse', 'left')
        # CR alone is a valid blank terminator, so remove the entire final separator.
        self.assertFalse(errors)
        sse_events(raw[:-2], [(len(raw) - 2, 100)], 1, errors, 'sse', 'left')
        self.assertTrue(any(e['rule'] == 'sse.complete' for e in errors))

    def test_global_timestamp_race_does_not_reorder_ws_seq(self):
        def mutate(rows):
            frames = [r for r in rows if r['kind'] == 'ws_frame']
            frames[1]['arrival_ns'], frames[2]['arrival_ns'] = frames[2]['arrival_ns'], frames[1]['arrival_ns']
        self.mutate_log(mutate)
        self.assertEqual(self.compare()['verdict'], 'PASS')

    def test_seq_gap_and_mutated_wire_rejected(self):
        path = self.right / 'recording.jsonl'
        rows = read_jsonl(path)
        frame = next(r for r in rows if r['kind'] == 'ws_frame')
        frame['seq'] += 1
        path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
        self.assertIn('ws.sequence', self.compare()['rules'])
        controls.write_log(path, rows)
        frame['wire_b64'] = controls.b64(b'\x81\x00')
        path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
        self.assertIn('evidence.integrity', self.compare()['rules'])

    def test_cross_connection_interleaving_is_not_compared(self):
        for side in (self.left, self.right):
            path = side / 'recording.jsonl'
            rows = read_jsonl(path)
            split = next(i for i, r in enumerate(rows) if r['kind'] == 'ws_frame')
            prefix = rows[:split]
            handshake = [copy.deepcopy(r) for r in prefix if r.get('id') == 4]
            for r in handshake:
                r['id'] = 5
                if r['kind'] == 'http_request':
                    r['case'] = 'ws.probe.2'
                if 'connection' in r:
                    r['connection'] = 2
            prefix.extend(handshake)
            first = [r for r in rows if r['kind'] == 'ws_frame']
            second = copy.deepcopy(first)
            for r in second:
                r['connection'] = 2
            end1 = next(r for r in rows if r['kind'] == 'ws_end')
            end2 = {**end1, 'connection': 2}
            frames = first + second if side == self.left else [r for pair in zip(second, first) for r in pair]
            controls.write_log(path, prefix + frames + [end1, end2, rows[-1]])
        self.assertEqual(self.compare()['verdict'], 'PASS')

    def test_fragment_type_pass_then_orphan_continuation_fails(self):
        for side in (self.left, self.right):
            path = side / 'recording.jsonl'
            rows = read_jsonl(path)
            first = next(r for r in rows if r['kind'] == 'ws_frame')
            raw = base64.b64decode(first['payload_b64'])
            cut = len(raw) // 2
            first.update(fin=False, payload_b64=controls.b64(raw[:cut]), bytes=cut)
            continuation = {**first, 'opcode': 0, 'fin': True, 'payload_b64': controls.b64(raw[cut:]), 'bytes': len(raw) - cut}
            rows.insert(rows.index(first) + 1, continuation)
            controls.write_log(path, rows)
        self.assertEqual(self.compare()['verdict'], 'PASS')
        path = self.right / 'recording.jsonl'
        rows = read_jsonl(path)
        rows.remove(next(r for r in rows if r['kind'] == 'ws_frame'))
        controls.write_log(path, rows)
        self.assertIn('ws.sequence', self.compare()['rules'])

    def test_unknown_volatile_named_field_remains_literal(self):
        def mutate(rows):
            controls.response_body(rows, 1, lambda raw: controls.change_json(raw, lambda body: body.update(timestamp='2026-10-14T00:00:00Z')))
        self.mutate_log(mutate)
        self.assertTrue(any(d['rule'] == 'http.body' and d['location'].endswith('.timestamp') for d in self.compare()['differences']))

    def test_expiry_shift_is_not_erased_as_a_clock(self):
        def mutate(rows):
            controls.response_body(rows, 3, lambda raw: raw.replace(b'2023-11-15T22:13:20.000Z', b'2023-11-15T22:13:20.001Z'))
        self.mutate_log(mutate)
        self.assertTrue(any(d['rule'] == 'normalization.field' and d['location'].endswith('.expiresAt') for d in self.compare()['differences']))

    def test_backend_and_wire_expiry_shift_preserves_ttl_difference(self):
        path = self.right / 'home/.tmp/chat-uploads/upload-1/metadata.json'
        metadata = json.loads(path.read_text())
        metadata['expiresAt'] = '2023-11-15T22:13:20.001Z'
        controls.write_json(path, metadata)
        controls.write_json(self.right / 'snapshot.json', capture(self.right))
        self.mutate_log(lambda rows: controls.response_body(rows, 3, lambda raw: raw.replace(b'2023-11-15T22:13:20.000Z', b'2023-11-15T22:13:20.001Z')))
        report = self.compare()
        self.assertIn('disk.snapshot', report['rules'])
        self.assertIn('sse.sequence', report['rules'])

    def test_literal_dotted_key_cannot_impersonate_identity_path(self):
        for side, value in ((self.left, 8), (self.right, 9)):
            path = side / 'recording.jsonl'
            rows = read_jsonl(path)
            controls.response_body(rows, 1, lambda raw: controls.change_json(raw, lambda body: body.update({'user.id': value})))
            controls.write_log(path, rows)
        self.assertTrue(any(d['rule'] == 'http.body' and d['location'].endswith('."user.id"') for d in self.compare()['differences']))

    def test_sse_clock_rule_is_scoped_to_case_and_event(self):
        self.manifest['http_cases'][2]['id'] = 'custom.sse'
        self.matrix['routes'][2]['cases'] = ['custom.sse']
        for side in (self.left, self.right):
            path = side / 'recording.jsonl'
            rows = read_jsonl(path)
            request = next(r for r in rows if r['kind'] == 'http_request' and r['id'] == 3)
            request['case'] = 'custom.sse'
            for header in request['headers']:
                if header['name'] == 'x-pdrs-parity-case':
                    header['value_b64'] = controls.b64('custom.sse')
            if side == self.right:
                controls.response_body(rows, 3, lambda raw: raw.replace(b'2023-11-15T22:13:20.000Z', b'2023-11-15T22:13:20.001Z'))
            controls.write_log(path, rows)
            path = side / 'cases.jsonl'
            cases = read_jsonl(path)
            cases[2]['case'] = 'custom.sse'
            path.write_text(''.join(json.dumps(c) + '\n' for c in cases))
        self.assertIn('sse.sequence', self.compare()['rules'])

    def test_json_numeric_equivalence_but_identity_types_distinct(self):
        def mutate(rows):
            controls.response_body(rows, 1, lambda raw: controls.change_json(raw, lambda body: body['user'].update(id='1')))
        self.mutate_log(mutate)
        self.assertEqual(self.compare()['verdict'], 'FAIL')

    def test_undeclared_clock_and_invalid_json_both_sides_fail(self):
        for side in (self.left, self.right):
            rows = read_jsonl(side / 'recording.jsonl')
            controls.response_body(rows, 1, lambda raw: b'{"success":NaN}')
            controls.write_log(side / 'recording.jsonl', rows)
        self.assertIn('evidence.integrity', self.compare()['rules'])

    def test_handler_rust_declared_requires_actual_rust_case(self):
        self.matrix['routes'][0]['handler'] = 'rust'
        self.assertEqual(self.compare()['verdict'], 'FAIL')  # static list is still empty
        owned = {self.matrix['routes'][0]['route']}
        self.enterContext(patch('check_coverage.rust_routes', return_value=owned))
        self.enterContext(patch('compare.rust_routes', return_value=owned))
        report = self.compare()
        self.assertTrue(any(d['rule'] == 'coverage.handler' and 'auth.register' in d['location'] for d in report['differences']))
        # A real marker, not client metadata, must change when TS claims Rust.
        def mutate(rows):
            response = next(r for r in rows if r['kind'] == 'http_response' and r['id'] == 1)
            response['handler'] = 'rust'
            for h in response['headers']:
                if h['name'] == 'x-pilotdeck-handler':
                    h['value_b64'] = controls.b64('rust')
        self.mutate_log(mutate)
        path = self.right / 'cases.jsonl'
        clients = read_jsonl(path)
        clients[0]['handler'] = 'rust'
        path.write_text(''.join(json.dumps(r) + '\n' for r in clients))
        self.assertEqual(self.compare()['verdict'], 'PASS')

    def test_disk_captured_then_modified_on_both_sides_fails_archive_check(self):
        for side in (self.left, self.right):
            (side / 'project/note.txt').write_text('same corruption')
        self.assertIn('disk.snapshot', self.compare()['rules'])

    def test_snapshot_sqlite_logical_and_binary_diff(self):
        for side in (self.left, self.right):
            with sqlite3.connect(side / 'home/local.db') as db:
                db.execute('CREATE TABLE x (v INTEGER PRIMARY KEY) WITHOUT ROWID')
                db.execute('INSERT INTO x VALUES (1)')
            (side / 'project/opaque.bin').write_bytes(b'\xff\0')
            controls.write_json(side / 'snapshot.json', capture(side))
        self.assertEqual(self.compare()['verdict'], 'PASS')
        with sqlite3.connect(self.right / 'home/local.db') as db:
            db.execute('UPDATE x SET v = 2')
        controls.write_json(self.right / 'snapshot.json', capture(self.right))
        self.assertIn('disk.snapshot', self.compare()['rules'])

    def test_cli_missing_evidence_returns_fail_json(self):
        proc = subprocess.run([sys.executable, '-I', str(HERE / 'compare.py'), str(self.root / 'missing'), str(self.right),
                               '--enumeration', str(self.root / 'enumeration.json'), '--matrix', str(self.root / 'matrix.json'),
                               '--scenarios', str(self.root / 'manifest.json'), '--paired-only'], capture_output=True, text=True)
        self.assertEqual(proc.returncode, 1)
        self.assertEqual(json.loads(proc.stdout)['verdict'], 'FAIL')

    def test_synthetic_five_archives_cannot_calibrate(self):
        rounds = []
        for i in range(5):
            target = self.root / f'ts{i}' / 'direct'
            target.parent.mkdir()
            shutil.copytree(self.left, target)
            rounds.append(target)
        result = ruler(rounds, self.root / 'enumeration.json', matrix=self.root / 'matrix.json', scenarios=self.root / 'manifest.json', paired_only=True)
        self.assertFalse(result['self_compare'])
        self.assertEqual(result['verdict'], 'FAIL')
        self.assertIsNone(result['independent_review'])

    def test_uuid_namespace_and_derived_suffix_across_channels(self):
        uuid = '11111111-1111-4111-8111-111111111111'
        errors = []
        norm = Normalizer('left', errors)
        norm.wall_bounds = (1700000000000, 1700000010000)
        norm.chat_clocks['session', 1] = ('2023-11-14T22:13:20.000Z', None)
        http = norm.value({'messages': [{'sessionId': 'session', 'sequence': 1, 'timestamp': '2023-11-14T22:13:20.000Z', 'turnId': uuid, 'blockId': uuid + ':text:0'}]}, 'http', 'readonly.messages')
        ws = norm.value({'runId': uuid, 'timeline': {'id': uuid + ':text:0'}}, 'ws', 'text')
        disk = norm.value([{'turnId': uuid, 'message': {'content': [{'blockId': uuid + ':text:0'}]}}], 'disk', '@chat')
        self.assertEqual(http['messages'][0]['turnId'], ws['runId'])
        self.assertEqual(ws['runId'], disk[0]['turnId'])
        self.assertEqual(http['messages'][0]['blockId'], ws['timeline']['id'])
        self.assertEqual(ws['timeline']['id'], disk[0]['message']['content'][0]['blockId'])
        self.assertEqual(ws['timeline']['id'], ws['runId'] + ':text:0')
        self.assertFalse(errors)
        self.assertEqual(norm.value({'other': {'blockId': uuid}}, 'ws', 'text')['other']['blockId'], uuid)
        # Different concrete entity types/indices remain different even when
        # the UUID is consistently renamed across ALL occurrences.
        self.assertNotEqual(norm.ref('block', uuid + ':text:1', 'x'), ws['timeline']['id'])
        self.assertNotEqual(norm.ref('timeline', uuid + ':thinking:0', 'x'), ws['timeline']['id'])
        self.assertNotEqual(norm.ref('run', 'status_' + uuid, 'x'), norm.ref('run', 'text_' + uuid, 'x'))

    def test_history_event_can_witness_multiple_flattened_messages(self):
        errors = []
        norm = Normalizer('left', errors)
        norm.wall_bounds = (1700000000000, 1700000010000)
        stamp = '2023-11-14T22:13:20.000Z'
        norm.chat_clocks['session', 1] = (stamp, 'entry')
        messages = [dict(sessionId='session', sequence=1, entryId='entry', timestamp=stamp, id=str(i))
                    for i in range(2)]
        # Status/error source branches may omit entryId.
        messages.append(dict(sessionId='session', sequence=1, timestamp=stamp, kind='status'))
        norm.history_clocks({'messages': messages}, 'body')
        self.assertEqual(errors, [])

    def test_history_identity_types_and_entry_id_checked_even_at_equal_times(self):
        stamp = '2023-11-14T22:13:20.000Z'
        for patch in ({'sequence': True}, {'sequence': 1.0}, {'sequence': '1'}, {'sequence': []},
                      {'sessionId': []}, {'sessionId': ''}, {'sequence': 0},
                      {'entryId': None}, {'entryId': ''}, {'entryId': 'other-entry'}):
            with self.subTest(patch=patch):
                errors = []
                norm = Normalizer('left', errors)
                norm.wall_bounds = (1700000000000, 1700000010000)
                norm.chat_clocks['session', 1] = (stamp, 'entry')
                norm.chat_clocks['session', 2] = (stamp, 'other-entry')
                norm.history_clocks({'messages': [dict(sessionId='session', sequence=1,
                    entryId='entry', timestamp=stamp) | patch]}, 'body')
                self.assertTrue(any('identity witness' in e['message'] for e in errors), errors)

    def test_duplicate_chat_event_identity_cannot_overwrite_witness(self):
        errors = []
        side = Side.__new__(Side)
        side.label, side.out = 'left', errors
        side.norm = Normalizer('left', errors)
        side.norm.wall_bounds = (1700000000000, 1700000010000)
        entry = dict(sessionId='session', sequence=1, createdAt='2023-11-14T22:13:20.000Z', entryId='entry')
        side.actual_snapshot = {'entries': [dict(path=f'home/projects/exp008-fixture/chats/{name}.jsonl',
                                               data=[entry.copy()]) for name in ('a', 'b', 'c')]}
        side.load_chat_clocks()
        self.assertIsNone(side.norm.chat_clocks['session', 1])
        self.assertEqual(sum('duplicate @chat' in e['message'] for e in errors), 2)
        side.norm.history_clocks({'messages': [dict(sessionId='session', sequence=1,
                                                   entryId='entry', timestamp=entry['createdAt'])]}, 'body')
        self.assertTrue(any('history timestamp differs' in e['message'] for e in errors))

    def test_seed_never_sorts_compared_lists_or_erases_session_aliases(self):
        errors = []
        a, b = Normalizer('left', errors), Normalizer('right', errors)
        for norm in (a, b):
            norm.seed_chat_identities([])
        sessions = [{'id': s} for s in CHAT_FIXTURE_SESSIONS]
        left = a.value({'sessions': sessions}, 'http', 'readonly.sessions')
        right = b.value({'sessions': sessions[::-1]}, 'http', 'readonly.sessions')
        diff(left, right, errors, 'http.body', 'body')
        self.assertTrue(any(d['location'] == 'body.sessions[0].id' for d in errors))
        errors.clear()
        collapsed = b.value({'sessions': [sessions[0], sessions[0]]}, 'http', 'readonly.sessions')
        diff(left, collapsed, errors, 'http.body', 'body')
        self.assertTrue(any(d['location'] == 'body.sessions[1].id' for d in errors))

    def test_ws_disk_uuid_renames_pass_but_reference_collapse_fails(self):
        for side, uuid, other in ((self.left, '11111111-1111-4111-8111-111111111111', '22222222-2222-4222-8222-222222222222'),
                                  (self.right, 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb')):
            chat = side / f'home/projects/exp008-fixture/chats/{CHAT_FIXTURE_SESSIONS[0]}.jsonl'
            chat.parent.mkdir(parents=True)
            chat.write_text(json.dumps({'sessionId': CHAT_FIXTURE_SESSIONS[0], 'sequence': 1, 'createdAt': '2023-11-14T22:13:20.000Z', 'turnId': uuid, 'entryId': other, 'message': {'content': [
                {'blockId': other + ':text:0', 'timeline': {'id': other + ':text:0', 'turnId': uuid}}]}}) + '\n'
                + json.dumps({'sessionId': CHAT_FIXTURE_SESSIONS[0], 'sequence': 2, 'createdAt': '2023-11-14T22:13:20.000Z', 'turnId': uuid}) + '\n')
            controls.write_json(side / 'snapshot.json', capture(side))
            rows = read_jsonl(side / 'recording.jsonl')
            template = next(r for r in rows if r['kind'] == 'ws_frame' and r['dir'] == 'u2c' and r['opcode'] == 1)
            body = {'kind': 'text', 'runId': uuid, 'blockId': other + ':text:0',
                    'timeline': {'id': other + ':text:0', 'turnId': uuid}, 'content': 'retained'}
            raw = json.dumps(body).encode()
            rows.insert(rows.index(template), {**template, 'payload_b64': controls.b64(raw), 'bytes': len(raw)})
            controls.write_log(side / 'recording.jsonl', rows)
        report = self.compare()
        self.assertEqual(report['verdict'], 'PASS', report['differences'])
        def collapse(rows):
            controls.mutate_frames(rows, lambda body: body['timeline'].update(turnId='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb')
                                   if body.get('kind') == 'text' else None)
        self.mutate_log(collapse)
        self.assertTrue(any(d['rule'] == 'ws.structure' and d['location'].endswith('.timeline.turnId')
                            for d in self.compare()['differences']))

    def test_real_recording_hash_gate_remains_strict_including_replay(self):
        for side in (self.left, self.right):
            path = side / 'run-meta.json'
            meta = json.loads(path.read_text())
            meta.update(synthetic=False, apparatus_sha256={'original': 'a' * 64})
            controls.write_json(path, meta)
        strict = self.compare()
        self.assertTrue(strict['calibration_eligible'])
        self.assertEqual(sum('hash mismatch' in d['message'] for d in strict['differences']), 2)
        replay = compare_sides(self.left, self.right, self.enumeration, self.matrix, self.manifest,
                               recorded_apparatus={'wrong': 'b' * 64})
        self.assertFalse(replay['calibration_eligible'])
        self.assertEqual(replay['comparison_mode'], 'historical-replay')
        self.assertEqual(sum('hash mismatch' in d['message'] for d in replay['differences']), 2)

    def test_draft_and_unapproved_frozen_manifest(self):
        folder = self.root / 'specs'
        folder.mkdir()
        self.assertEqual(frozen(folder)['verdict'], 'DRAFT')
        (folder / 'FROZEN.md').write_text('AI 代批\n')
        self.assertEqual(frozen(folder)['verdict'], 'FAIL')


class ClockTests(unittest.TestCase):
    """Producer formats, wall bounds and semantic pairs must fail closed."""

    def norm(self):
        errors = []
        n = Normalizer('test', errors)
        n.wall_bounds = (1791959793119, 1791959806355)
        return n, errors

    def test_every_registered_representation_rejects_bad_format_unit_and_wall(self):
        for (channel, context), paths in CLOCK_FORMATS.items():
            for path, representation in paths.items():
                with self.subTest(channel=channel, context=context, path=path):
                    n, errors = self.norm()
                    good = {ISO_MS: '2026-10-14T06:36:37.003Z', SQL_S: '2026-10-14 06:36:37',
                            EPOCH_MS: 1791959797003, MTIME_MS: 1791959797003.125}[representation]
                    self.assertIsNotNone(n.clock_instant(good, representation, path))
                    self.assertFalse(errors)
                    bad = ['2026-10-14 06:36:37.003', '2026-10-14T06:36:37.003+00:00',
                           '2026-10-14T06:36:37Z', '2026-02-30T06:36:37.003Z',
                           True, float('inf'), float('nan'), 1791959797,
                           '2026-10-14T06:35:37.003Z', '2026-10-14 06:35:37', 1791959737003]
                    for value in bad:
                        self.assertIsNone(n.clock_instant(value, representation, path))
                        self.assertEqual(errors[-1]['rule'], 'normalization.field')

    def test_wall_boundaries_and_sql_source_precision(self):
        n, errors = self.norm()
        for value in (n.wall_bounds[0], n.wall_bounds[1] + 1000):
            self.assertIsNotNone(n.clock_instant(value, EPOCH_MS, 'clock'))
        for value in (n.wall_bounds[0] - 1, n.wall_bounds[1] + 1001):
            self.assertIsNone(n.clock_instant(value, EPOCH_MS, 'clock'))
        self.assertIsNotNone(n.clock_instant('2026-10-14 06:36:33', SQL_S, 'sql'))
        self.assertIsNone(n.clock_instant('2026-10-14 06:36:32', SQL_S, 'sql'))
        self.assertIsNone(n.clock_instant('2026-10-14T06:36:33.000Z', ISO_MS, 'iso'))

    def test_sql_pair_quantization_is_not_alias_equality(self):
        n, errors = self.norm()
        def value(login):
            return n.value({'tables': {'users': {'rows': [{
                'created_at': '2026-10-14 06:36:36', 'last_login': login}]}}},
                'disk', 'home/auth.db')
        self.assertEqual(value('2026-10-14 06:36:36'), value('2026-10-14 06:36:37'))
        self.assertFalse(errors)
        value('2026-10-14 06:36:35')
        self.assertTrue(any('clock order invalid' in e['message'] for e in errors))

    def test_all_registered_pairs_reject_same_bad_order(self):
        # Build only exact registered paths; no schema-wide name inference.
        for (channel, context), objects in CLOCK_PAIRS.items():
            for obj, pairs in objects.items():
                for earlier, later in pairs:
                    with self.subTest(context=context, obj=obj, pair=(earlier, later)):
                        parts = [json_key[1:-1] if json_key.startswith('"') else json_key
                                 for json_key in re.findall(r'"[^"]+"|[^.]+', obj)] if obj else []
                        raw = {earlier: '2026-10-14T06:36:38.000Z', later: '2026-10-14T06:36:37.000Z'}
                        if context == 'home/auth.db':
                            raw = {earlier: '2026-10-14 06:36:38', later: '2026-10-14 06:36:37'}
                        for part in reversed(parts):
                            raw = [raw] if part == '*' else {part: raw}
                        n, errors = self.norm()
                        n.value(raw, channel, context)
                        self.assertTrue(any('clock order invalid' in e['message'] for e in errors), errors)

    def test_http_cross_entity_ranks_aliases_and_unknown_paths(self):
        s = {'createdAt': '2026-10-14T06:36:36.000Z', 'created_at': '2026-10-14T06:36:36.000Z',
             'lastActivity': '2026-10-14T06:36:37.000Z', 'updated_at': '2026-10-14T06:36:37.000Z'}
        other = {k: v.replace('36.', '38.').replace('37.', '39.') for k, v in s.items()}
        for context in ('readonly.projects', 'readonly.sessions'):
            a = [{'sessions': [s, other], 'lastActivity': 1791959799000}] if context.endswith('projects') else {'sessions': [s, other]}
            b = copy.deepcopy(a)
            sessions = b[0]['sessions'] if isinstance(b, list) else b['sessions']
            sessions.reverse()
            n, errors = self.norm()
            left = n.value(a, 'http', context)
            right = n.value(b, 'http', context)
            self.assertFalse(errors)
            diff(left, right, errors, 'http.body', 'body')
            self.assertTrue(errors)
        n, errors = self.norm()
        self.assertEqual(n.value({'createdAt': s['createdAt']}, 'http', 'unknown'), {'createdAt': s['createdAt']})
        self.assertEqual(n.value({'sessions': [{'createdAt': None}]}, 'http', 'readonly.sessions'), {'sessions': [{'createdAt': None}]})
        self.assertFalse(errors)


class NDJSONIntegrity(unittest.TestCase):
    def test_valid_lines_and_corruption_in_both_sides(self):
        for raw, valid in ((b'{"stage":"progress"}\n{"stage":"complete"}\n', True),
                           (b'{"stage":"complete"}', True), (b'{bad}\n', False),
                           (b'\xff\n', False), (b'', True), (b'{}\n\n', False)):
            for side in ('left', 'right'):
                errors = []
                validate_ndjson(raw, errors, 'stream', side)
                self.assertEqual(not errors, valid)
        errors = []
        validate_ndjson(b'{"stage":"complete"}', errors, 'update', 'left', require_final_newline=True)
        self.assertTrue(errors)


if __name__ == '__main__':
    unittest.main()
