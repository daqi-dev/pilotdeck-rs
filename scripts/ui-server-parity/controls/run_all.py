#!/usr/bin/env -S python3 -I
"""Offline rule-specific positive controls; synthetic evidence is never TS calibration."""
# Isolated bootstrap: verify the source loader before executing local code.
import os as _boot_os
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.dirname(_boot_os.path.abspath(__file__)))
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
import argparse
import base64
import copy
import json
from pathlib import Path
import shutil
import sys
import tempfile

HERE = Path(__file__).resolve().parents[1]
from compare import RULES, compare_sides, read_jsonl  # noqa: E402
from snapshot import capture  # noqa: E402
from controls.identity_paths import run_identity_controls  # noqa: E402
from controls.multimodal import run_multimodal_controls
from controls.streams_and_clocks import run_history_clock_controls
from controls.streams_and_clocks import run_stream_clock_controls, run_http_clock_controls  # noqa: E402


def b64(value):
    return base64.b64encode(value.encode() if isinstance(value, str) else value).decode()


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')


def write_log(path, rows):
    # Renumber after semantic mutations; losing a frame must trip ws.sequence,
    # not merely the recording-wide seq continuity gate.
    for seq, row in enumerate(rows, 1):
        row['seq'] = seq
        if row['kind'] == 'ws_frame':
            data = base64.b64decode(row['payload_b64'])
            head = bytes([(128 if row['fin'] else 0) | row['rsv'] << 4 | row['opcode']])
            masked = 128 if row['masked'] else 0
            head += bytes([masked | len(data)]) if len(data) < 126 else bytes([masked | 126]) + len(data).to_bytes(2, 'big')
            if row['masked']:
                mask = b'\x01\x02\x03\x04'
                head += mask
                data = bytes(v ^ mask[i % 4] for i, v in enumerate(data))
            row['wire_b64'] = b64(head + data)
    path.write_text(''.join(json.dumps(row, ensure_ascii=False) + '\n' for row in rows))


def hdr(name, value):
    return {'name': name, 'value_b64': b64(value)}


def clock_evidence(start_ms, end_ms):
    from common import INTERVAL_NS, MAX_GAP_NS
    stamps = list(range(start_ms, end_ms, 1000)) + [end_ms]
    samples = [dict(phase='start' if i == 0 else 'end' if i == len(stamps) - 1 else 'periodic',
                    wall_ns=t * 1_000_000, mono_before_ns=(t - start_ms + 1) * 1_000_000,
                    mono_after_ns=(t - start_ms + 1) * 1_000_000 + 100)
               for i, t in enumerate(stamps)]
    return dict(schema=1, status='PASS', interval_ns=INTERVAL_NS, max_gap_ns=MAX_GAP_NS, samples=samples)


def fixture(root):
    enumeration = {'appCaptured': True, 'uniqueRouteCount': 3, 'routes': [
        {'method': 'POST', 'path': '/api/auth/register'},
        {'method': 'POST', 'path': '/api/auth/login'},
        {'method': 'GET', 'path': '/api/uploads/:uploadId/events'}]}
    definitions = [
        {'id': 'auth.register', 'transport': 'http', 'route': 'POST /api/auth/register', 'expected_statuses': [200]},
        {'id': 'auth.login', 'transport': 'http', 'route': 'POST /api/auth/login', 'expected_statuses': [200]},
        {'id': 'upload.events', 'transport': 'http', 'route': 'GET /api/uploads/:uploadId/events', 'expected_statuses': [200]}]
    manifest = {'schema': 1, 'scenarios': [{'id': 'probe'}], 'http_cases': definitions}
    matrix = {'schema': 1, 'routes': [{'route': d['route'], 'cases': [d['id']]} for d in definitions]}
    for name, data in (('enumeration', enumeration), ('manifest', manifest), ('matrix', matrix)):
        write_json(root / f'{name}.json', data)
    for label, mode in (('left', 'direct'), ('right', 'proxy')):
        side = root / label
        side.mkdir()
        (side / 'home').mkdir()
        (side / 'project').mkdir()
        (side / 'home/state.json').write_text('{"status":"completed","count":2}\n')
        (side / 'project/note.txt').write_text('offline fixture\n')
        upload_meta = side / 'home/.tmp/chat-uploads/upload-1'
        upload_meta.mkdir(parents=True)
        write_json(upload_meta / 'metadata.json', {'uploadId': 'upload-1', 'createdAt': '2023-11-14T22:13:20.000Z', 'updatedAt': '2023-11-14T22:13:21.000Z', 'expiresAt': '2023-11-15T22:13:20.000Z', 'status': 'completed'})
        write_json(side / 'snapshot.json', capture(side))
        write_json(side / 'run-meta.json', {'schema': 1, 'mode': mode, 'fixture_path': '/fixture/common/direct',
                                          'run_id': f'offline-{label}', 'observer_port': 13102,
                                          'wall_start_ms': 1700000000000, 'wall_end_ms': 1700000010000, 'synthetic': True,
                                          'clock_regression_check': clock_evidence(1700000000000, 1700000010000)})
        write_json(side / 'scenarios.json', {'probe': {'status': 'PASS'}})
        rows = []
        clock = 0

        def emit(kind, **kw):
            nonlocal clock
            clock += 1_000_000
            rows.append({'kind': kind, 'seq': len(rows) + 1, 'arrival_ns': str(clock), **kw})

        def response_headers(content_type):
            return [hdr('content-type', content_type), hdr('set-cookie', 'a=1'), hdr('set-cookie', 'b=2')] + ([hdr('x-pilotdeck-handler', 'ts')] if mode == 'proxy' else [])

        emit('run_start', schema=1, listen='127.0.0.1:13101', upstream='http://127.0.0.1:' + ('13102' if mode == 'direct' else '13103'))
        cases = []
        for rid, definition in enumerate(definitions, 1):
            case = definition['id']
            method = 'GET' if case == 'upload.events' else 'POST'
            path = '/api/uploads/upload-1/events' if method == 'GET' else '/api/auth/' + case.split('.')[1]
            request_body = b'' if method == 'GET' else b'{"username":"offline","password":"offline-isolated"}'
            request_headers = [hdr('x-pdrs-parity-case', case)] + ([hdr('content-type', 'application/json')] if request_body else [])
            emit('http_request', id=rid, method=method, path=path, query=None, headers=request_headers, case=case)
            if request_body:
                emit('http_chunk', id=rid, direction='request', offset=0, bytes=len(request_body), body_b64=b64(request_body))
            emit('http_end', id=rid, direction='request', offset=len(request_body))
            emit('http_response', id=rid, status=200, handler='ts' if mode == 'proxy' else None,
                 headers=response_headers('text/event-stream' if method == 'GET' else 'application/json'))
            if case == 'upload.events':
                chunks = [b'event: upload_started\nid: 1\ndata: {"uploadId":"upload-1","status":"created","expiresAt":"2023-11-15T22:13:20.000Z"}\n\n',
                          b'event: upload_completed\nid: 2\ndata: {"uploadId":"upload-1","status":"completed","expiresAt":"2023-11-15T22:13:20.000Z"}\n\n']
                offset = 0
                for part in chunks:
                    clock += 1_000_000_000
                    emit('http_chunk', id=rid, direction='response', offset=offset, bytes=len(part), body_b64=b64(part))
                    offset += len(part)
                emit('http_end', id=rid, direction='response', offset=offset)
            else:
                body = json.dumps({'success': True, 'user': {'id': 1, 'username': 'offline'}, 'token': 'offline-token'}).encode()
                emit('http_chunk', id=rid, direction='response', offset=0, bytes=len(body), body_b64=b64(body))
                emit('http_end', id=rid, direction='response', offset=len(body))
            cases.append({'kind': 'case_http', 'side': mode, 'case': case, 'route': definition['route'],
                          'method': method, 'path': path, 'status': 200, 'handler': 'ts' if mode == 'proxy' else None, 'ok': True})
        nonce = b64(b'0123456789abcdef')
        import hashlib
        accept = b64(hashlib.sha1((nonce + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
        emit('http_request', id=4, method='GET', path='/ws', query='token=%3Credacted%3E', case='ws.probe.1',
             headers=[hdr('sec-websocket-key', nonce)])
        emit('http_end', id=4, direction='request', offset=0)
        emit('http_response', id=4, status=101, connection=1, handler='ts' if mode == 'proxy' else None,
             headers=[hdr('sec-websocket-accept', accept)] + ([hdr('x-pilotdeck-handler', 'ts')] if mode == 'proxy' else []))
        activities = [dict(sessionId='session-1', runId='run-1', processing=True, revision=7),
                      dict(sessionId='session-1', runId='run-1', completedRunId='run-1', processing=False, revision=11)]

        def ws(direction, body, opcode=1):
            raw = json.dumps(body).encode() if opcode == 1 else b'\x03\xe8'
            emit('ws_frame', connection=1, leg='downstream-read' if direction == 'c2u' else 'downstream-write',
                 dir=direction, fin=True, rsv=0, opcode=opcode, masked=direction == 'c2u', bytes=len(raw), payload_b64=b64(raw))

        ws('c2u', {'type': 'get-session-activity'})
        ws('u2c', {'type': 'session-activity', 'activity': activities[0]})
        ws('u2c', {'type': 'session-activity', 'activity': activities[1]})
        ws('u2c', {'type': 'session-activity-snapshot', 'activities': [activities[1]]})
        ws('c2u', {}, 8)
        ws('u2c', {}, 8)
        emit('ws_end', connection=1, error=None)
        emit('run_end')
        write_log(side / 'recording.jsonl', rows)
        (side / 'cases.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in cases))
        oob = []
        for seq, phase in enumerate(('before-scenarios', 'after-concurrent-chat', 'after-watch-reconnect', 'after-scenarios'), 1):
            oob.append({'kind': 'activity_anchor', 'out_of_band': True, 'user_id': 1, 'connection': 99,
                        'port': 13102, 'seq': seq, 'arrival_ns': str(seq), 'phase': phase,
                        'frame': {'type': 'session-activity-snapshot', 'activities': activities if seq == 2 else [activities[-1]] if seq > 2 else []}})
        (side / 'activity-direct.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in oob))
    return enumeration, matrix, manifest


def response_body(rows, rid, mutate):
    chunks = [row for row in rows if row['kind'] == 'http_chunk' and row['id'] == rid and row['direction'] == 'response']
    data = b''.join(base64.b64decode(row['body_b64']) for row in chunks)
    data = mutate(data)
    first = chunks[0]
    first.update(body_b64=b64(data), bytes=len(data), offset=0)
    for row in chunks[1:]:
        rows.remove(row)
    end = next(row for row in rows if row['kind'] == 'http_end' and row['id'] == rid and row['direction'] == 'response')
    end['offset'] = len(data)


def change_json(raw, mutate):
    body = json.loads(raw)
    mutate(body)
    return json.dumps(body).encode()


def mutate_frames(rows, fn):
    for row in rows:
        if row['kind'] == 'ws_frame' and row['opcode'] == 1:
            data = json.loads(base64.b64decode(row['payload_b64']))
            fn(data)
            raw = json.dumps(data).encode()
            row.update(payload_b64=b64(raw), bytes=len(raw))


def mutation(name, side):
    path = side / 'recording.jsonl'
    rows = read_jsonl(path)
    response = next(r for r in rows if r['kind'] == 'http_response' and r['id'] == 1)
    ws = [r for r in rows if r['kind'] == 'ws_frame' and r['opcode'] == 1]
    sse_chunks = [r for r in rows if r['kind'] == 'http_chunk' and r['id'] == 3 and r['direction'] == 'response']
    if name == 'http-header':
        response['headers'][0] = hdr('content-type', 'application/json; injected=1')
    elif name == 'set-cookie-drop':
        response['headers'].remove(next(h for h in reversed(response['headers']) if h['name'] == 'set-cookie'))
    elif name == 'http-status':
        response['status'] = 500
    elif name == 'http-body':
        response_body(rows, 1, lambda raw: change_json(raw, lambda body: body.update(success=False)))
    elif name == 'http-request':
        next(r for r in rows if r['kind'] == 'http_request')['query'] = 'changed=1'
    elif name == 'identity-alias':
        response_body(rows, 2, lambda raw: change_json(raw, lambda body: body['user'].update(id=2)))
    elif name == 'ws-swap':
        a, b = ws[1], ws[2]
        for key in ('payload_b64', 'bytes'):
            a[key], b[key] = b[key], a[key]
    elif name == 'ws-drop':
        rows.remove(ws[2])
    elif name == 'ws-structure':
        mutate_frames(rows, lambda body: body.update(injected=True) if body.get('type') == 'get-session-activity' else None)
    elif name == 'sse-drop':
        response_body(rows, 3, lambda raw: raw[raw.index(b'\n\n') + 2:])
    elif name == 'sse-merge':
        response_body(rows, 3, lambda raw: raw.replace(b'\n\n', b'\n', 1))
    elif name == 'sse-truncate':
        response_body(rows, 3, lambda raw: raw[:-1])
    elif name == 'sse-buffer':
        # Keep events and chunk bytes identical, change only arrival times.
        sse_chunks[0]['arrival_ns'] = sse_chunks[-1]['arrival_ns']
    elif name == 'clock-invalid':
        response_body(rows, 3, lambda raw: raw.replace(b'2023-11-15T22:13:20.000Z', b'bad'))
    elif name == 'snapshot-revision-offset':
        def offset(body):
            if body.get('type') == 'session-activity':
                body['activity']['revision'] += 100
            elif body.get('type') == 'session-activity-snapshot':
                for item in body['activities']:
                    item['revision'] += 100
        mutate_frames(rows, offset)
    elif name == 'handler-lie':
        response['handler'] = 'rust'  # original header still says ts
    elif name == 'coverage-case':
        cases = read_jsonl(side / 'cases.jsonl')
        (side / 'cases.jsonl').write_text(''.join(json.dumps(r) + '\n' for r in cases if r['case'] != 'auth.login'))
    elif name == 'disk-change':
        (side / 'project/note.txt').write_text('changed disk\n')
        write_json(side / 'snapshot.json', capture(side))
    elif name == 'both-empty':
        rows = []
    elif name == 'recording-truncate':
        rows.pop()
    elif name == 'both-http-error-body':
        response_body(rows, 1, lambda raw: b'{"error":"same error"}')
    elif name == 'both-sse-errors':
        response_body(rows, 3, lambda raw: raw.replace(b'upload_completed', b'upload_failed'))
    elif name == 'both-sse-empty':
        response_body(rows, 3, lambda raw: b'')
    elif name == 'both-ws-empty':
        rows = [r for r in rows if r['kind'] != 'ws_frame']
    elif name == 'both-disk-empty':
        (side / 'project/note.txt').unlink()
        (side / 'home/state.json').unlink()
        write_json(side / 'snapshot.json', capture(side))
    elif name == 'both-ws-errors':
        mutate_frames(rows, lambda body: body.update(type='error', error='same error') if body.get('type') == 'get-session-activity' else None)
    elif name == 'both-errors':
        response['status'] = 500
        write_json(side / 'scenarios.json', {'probe': {'status': 'FAIL', 'error': 'same error'}})
    else:
        raise AssertionError(name)
    write_log(path, rows)


CONTROLS = [
    ('http-header', 'http.headers', 'headers.content-type'),
    ('set-cookie-drop', 'http.headers', 'headers.set-cookie'),
    ('http-status', 'http.status', 'status'), ('http-body', 'http.body', 'body.success'),
    ('http-request', 'http.request', 'request.query'),
    ('identity-alias', 'identity.consistency', 'body.user.id'),
    ('ws-swap', 'ws.sequence', 'ws.'), ('ws-drop', 'ws.sequence', 'ws.'),
    ('ws-structure', 'ws.structure', 'ws.by_type'),
    ('sse-drop', 'sse.sequence', 'sse['), ('sse-merge', 'sse.sequence', 'sse['),
    ('sse-buffer', 'sse.delay', 'arrival_ns'), ('sse-truncate', 'sse.complete', 'upload.events'),
    ('clock-invalid', 'normalization.field', 'expiresAt'),
    ('disk-change', 'disk.snapshot', 'disk'),
    ('snapshot-revision-offset', 'activity.out-of-band', 'activity'),
    ('handler-lie', 'coverage.handler', 'http['), ('coverage-case', 'coverage.matrix', 'cases.auth.login'),
    ('recording-truncate', 'evidence.integrity', 'right'),
    ('both-http-error-body', 'evidence.integrity', 'body.error'),
    ('both-sse-errors', 'evidence.integrity', '.sse'),
    ('both-sse-empty', 'sse.complete', 'upload.events'),
    ('both-ws-empty', 'evidence.integrity', 'ws['),
    ('both-disk-empty', 'disk.snapshot', 'project'),
    ('both-ws-errors', 'evidence.integrity', 'ws['),
    ('both-empty', 'evidence.integrity', 'left'), ('both-errors', 'evidence.integrity', 'scenarios'),
]


def run_controls(output=None):
    with tempfile.TemporaryDirectory(prefix='b2-controls-') as directory:
        root = Path(directory)
        enumeration, matrix, manifest = fixture(root)
        baseline = compare_sides(root / 'left', root / 'right', enumeration, matrix, manifest)
        results = [{'name': 'equivalence', 'expect': 'PASS', 'actual': baseline['verdict'],
                    'passed': baseline['verdict'] == 'PASS', 'differences': baseline['differences']}]
        for name, rule, location in CONTROLS:
            test = root / name
            test.mkdir()
            for label in ('left', 'right'):
                shutil.copytree(root / label, test / label)
            both = name.startswith('both-')
            mutation(name, test / 'right')
            if both:
                mutation(name, test / 'left')
            report = compare_sides(test / 'left', test / 'right', enumeration, matrix, manifest)
            hit = [d for d in report['differences'] if d['rule'] == rule and location in d['location']]
            results.append({'name': name, 'both_sides': both, 'expect': 'FAIL', 'expected_rule': rule,
                            'expected_location': location, 'actual': report['verdict'],
                            'passed': report['verdict'] == 'FAIL' and bool(hit), 'matched': hit[:3], 'rules': report['rules']})
        results.extend(run_identity_controls())
        results.extend(run_stream_clock_controls(fixture, write_log))
        results.extend(run_multimodal_controls())
        results.extend(run_http_clock_controls())
        results.extend(run_history_clock_controls())
        exercised = {row['expected_rule'] for row in results if row.get('passed') and 'expected_rule' in row}
        missing = sorted(RULES - exercised)
        report = {'verdict': 'PASS' if all(row['passed'] for row in results) and not missing else 'FAIL',
                  'synthetic': True, 'missing_rule_controls': missing, 'controls': results}
        if output:
            write_json(Path(output), report)
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path)
    args = parser.parse_args()
    report = run_controls(args.out)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return int(report['verdict'] != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())
