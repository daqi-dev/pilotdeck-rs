"""Request-bound public HTTP and complete outbound evidence controls."""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is builtin; os is imported only after the frozen-module preflight below.
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import importlib.util
from pathlib import Path
import tempfile

# -B only stops cache writes, so redirect the bytecode prefix too (fix-r5).
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code

# Load the one guard by explicit path, never sys.path: a same-named package or
# extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing the guard is what this block defends.
_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
_guard_path = _apparatus / '_guard.py'
_guard = sys.modules.get('_guard')
if not (getattr(_guard, '__file__', None)
        and Path(_guard.__file__).resolve() == _guard_path
        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):
    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)
    _guard = importlib.util.module_from_spec(_spec)
    sys.modules['_guard'] = _guard
    _guard_source = _guard_path.read_bytes()
    try:
        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)
    except BaseException:
        sys.modules.pop('_guard', None)
        raise
    _guard._apparatus_source = _guard_source
if Path(_guard.__file__).resolve() != _guard_path:
    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')

_load = _guard.load
_has_git_checkout = _guard._has_git_checkout
_verify_cli_imports = _guard.verify_cli_imports
run_entry = _guard.run_entry
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap
import atexit
import base64
import copy
import hashlib
import json
import shutil
import types
import unittest
from unittest.mock import patch

atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
public = _load('retirement_public_http', _apparatus / 'retirement_public_http.py')
contracts = _load('retirement_public_http_contracts', _apparatus.parent.parent /
                  'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/contracts.py')


class Identity:
    def value(self, value):
        return value


def blob(data):
    return {'bytes': len(data), 'rawBytesHex': data.hex(), 'truncated': False}


def exchange(identity=1, *, path='/api/module-host/call', label='case',
             response=b'{"ok":true}', extra=(), status_line='HTTP/1.1 200 OK',
             chunked=False, request=b'{}', method='POST'):
    if method in ('GET', 'HEAD'):
        request = b''
    headers = [('Content-Type', 'application/json'), *extra]
    headers.append(('Transfer-Encoding', 'chunked') if chunked else ('Content-Length', str(len(response))))
    flattened = {}
    for name, value in headers:
        key = name.lower()
        flattened[key] = flattened[key] + ', ' + value if key in flattened else value
    request_headers = {'authorization': 'Bearer test-token', 'content-type': 'application/json'}
    sent = (f'{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:28610\r\n'
            f'Authorization: Bearer test-token\r\nContent-Type: application/json\r\n'
            f'Content-Length: {len(request)}\r\n\r\n').encode() + request
    payload = (f'{len(response):x}\r\n'.encode() + response + b'\r\n0\r\nX-Trailer: retained\r\n\r\n') if chunked else response
    received = (status_line + '\r\n' + ''.join(f'{k}: {v}\r\n' for k, v in headers) + '\r\n').encode() + payload
    socket = {'id': 7, 'remoteHost': '127.0.0.1', 'remotePort': 28610, 'localPort': 42000}
    records = [
        {'kind': 'fixture-http-inbound-request', 'id': identity, 'method': method,
         'url': 'http://127.0.0.1:28610' + path, 'headers': request_headers, 'body': blob(request)},
        {'kind': 'fixture-tcp-write', 'socket': socket, 'payload': blob(sent[:11])},
        {'kind': 'fixture-tcp-write', 'socket': socket, 'payload': blob(sent[11:])},
        {'kind': 'fixture-tcp-data', 'socket': socket, 'payload': blob(received[:17])},
        {'kind': 'fixture-tcp-data', 'socket': socket, 'payload': blob(received[17:])},
        {'kind': 'fixture-http-inbound-response', 'id': identity, 'status': 200,
         'headers': flattened, 'body': blob(response)},
    ]
    row = {'method': method, 'path': path, 'label': label, 'status': 200, 'body': response.decode(),
           'headers': flattened, 'requestBody': request.decode(), 'requestBodyHex': request.hex(), 'responseBodyHex': response.hex()}
    return row, records


def project(rows, records):
    return public.http_projection(contracts, rows, records, Identity())


def mock_evidence():
    raw = {'http': [], 'models': []}
    records = []
    inputs = []
    for generation in (0, 1):
        inputs.append(('/v1/chat/completions', {'model': 'mock', 'stream': True,
                       'messages': [{'role': 'user', 'content': f'CHANNEL_INPUT_{generation}'}]}))
        inputs.append(('/open-apis/im/v1/messages', {'receive_id': 'room', 'msg_type': 'post',
                       'content': json.dumps({'zh_cn': {'content': [[{'tag': 'text', 'text': f'CHANNEL_OUTPUT_{generation}'}]]}})}))
    for identity, (path, value) in enumerate(inputs):
        body = json.dumps(value).encode()
        reply = b'{"code":0}'
        headers = {'content-type': 'application/json', 'content-length': str(len(reply))}
        raw['http'].append({'method': 'POST', 'path': path, 'body': value, 'authorization': 'Bearer mock'})
        if path.startswith('/v1/'):
            raw['models'].append(value)
        records.extend([
            {'kind': 'mock-http-request', 'id': identity, 'method': 'POST', 'url': path,
             'socket': {'id': 1},
             'headers': {'authorization': 'Bearer mock'},
             'rawHeaders': ['Authorization', 'Bearer mock'], 'body': blob(body)},
            {'kind': 'mock-http-response', 'id': identity, 'status': 200, 'finished': True,
             'headers': headers, 'body': blob(reply),
             'actualHeaderText': 'HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 10\r\n\r\n'},
        ])
    return raw, records


class PublicHttpControls(unittest.TestCase):
    def test_provider_preserves_complete_model_and_response(self):
        model = {'model': 'mock', 'messages': [{'role': 'user', 'content': 'other input'}]}
        body = b'{"choices":[]}'
        wire = b'HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\n\r\n' + body
        # Construct the declared length from bytes, so this control is about
        # projection coverage and not an unrelated malformed response.
        wire = wire.replace(b'Content-Length: 14', f'Content-Length: {len(body)}'.encode())
        row = {'method': 'POST', 'path': '/v1/chat/completions',
               'headers': [['Authorization', 'Bearer mock']],
               'requestBodyHex': json.dumps(model).encode().hex(),
               'responseWireHex': wire.hex(), 'responseTruncated': False}
        expected = contracts.provider_wire([model], [], [row], expected_public=0)
        other_model = dict(model, temperature=0.7)
        changed = dict(row, requestBodyHex=json.dumps(other_model).encode().hex())
        self.assertNotEqual(expected, contracts.provider_wire([other_model], [], [changed], expected_public=0))
        changed = dict(row, responseWireHex=wire.replace(b'200 OK', b'200 Different').hex())
        self.assertNotEqual(expected, contracts.provider_wire([model], [], [changed], expected_public=0))

    def test_provider_rejects_response_tail_outside_declared_body(self):
        row = {'method': 'GET', 'path': '/api/auth/me', 'headers': [['Authorization', 'Bearer actor-only']],
               'requestBodyHex': '', 'responseTruncated': False,
               'responseWireHex': b'HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}extra'.hex()}
        with self.assertRaisesRegex(ValueError, 'PROVIDER_RESPONSE_BODY_BOUND'):
            contracts.provider_wire([], [{'path': '/api/auth/me', 'bearerKind': 'actor'}], [row], expected_public=0)

    def test_finished_http10_provider_stream_is_close_delimited(self):
        model = {'model': 'mock', 'stream': True, 'messages': []}
        response = b'data: [DONE]\n\n'
        row = {'method': 'POST', 'path': '/v1/chat/completions', 'headers': [],
               'requestBodyHex': json.dumps(model).encode().hex(), 'responseTruncated': False,
               'responseWireHex': (b'HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n' + response).hex()}
        actual = contracts.provider_wire([model], [], [row], expected_public=0)
        self.assertEqual(actual[0]['response'], response.decode())

    def test_equal_bodies_keep_the_headers_of_their_own_request(self):
        a, ar = exchange(extra=[('X-Case', 'first')])
        b, br = exchange(2, path='/api/module-host/describe', extra=[('X-Case', 'second')])
        result = project([a, b], ar + br)
        self.assertEqual(result[0]['headers']['x-case'], ['first'])
        self.assertEqual(result[1]['headers']['x-case'], ['second'])

    def test_identical_repeated_requests_bind_in_distinct_event_windows(self):
        a, ar = exchange()
        b, br = exchange(2, status_line='HTTP/1.1 200 Different')
        result = project([a, b], ar + br)
        self.assertEqual([r['statusLine'] for r in result], ['HTTP/1.1 200 OK', 'HTTP/1.1 200 Different'])

    def test_status_reason_and_protocol_remain_visible(self):
        row, records = exchange()
        for line in ('HTTP/1.0 200 OK', 'HTTP/1.1 200 Different'):
            changed, trace = exchange(status_line=line)
            self.assertNotEqual(project([row], records), project([changed], trace))

    def test_duplicate_headers_and_trailers_are_retained(self):
        row, records = exchange(extra=[('X-Repeat', 'one'), ('X-Repeat', 'two')], chunked=True)
        result = project([row], records)[0]
        self.assertEqual(result['headers']['x-repeat'], ['one', 'two'])
        self.assertEqual(result['trailers'], {'x-trailer': ['retained']})

    def test_health_on_same_socket_consumes_its_response(self):
        _, health = exchange(path='/health', method='GET', extra=[('X-Case', 'health')])
        row, records = exchange(2, extra=[('X-Case', 'business')])
        self.assertEqual(project([row], health + records)[0]['headers']['x-case'], ['business'])

    def test_decoded_fetch_without_original_wire_is_rejected(self):
        row, records = exchange()
        with self.assertRaisesRegex(ValueError, 'HTTP_RAW_PAIR_MISSING_OR_AMBIGUOUS'):
            project([row], [records[0], records[-1]])

    def test_extra_raw_business_exchange_cannot_be_hidden_by_omitting_fetch_events(self):
        row, records = exchange()
        _, extra = exchange(2, path='/api/module-host/describe')
        with self.assertRaisesRegex(ValueError, 'HTTP_RAW_UNEXPECTED_EXCHANGE'):
            project([row], records + extra[1:-1])

    def test_upgrade_header_cannot_hide_a_business_http_exchange(self):
        row, records = exchange()
        _, extra = exchange(2, path='/api/module-host/describe')
        sent = b''.join(bytes.fromhex(item['payload']['rawBytesHex']) for item in extra[1:3])
        sent = sent.replace(b'\r\n\r\n', b'\r\nUpgrade: websocket\r\n\r\n', 1)
        raw = [dict(extra[1], payload=blob(sent)), *extra[3:5]]
        with self.assertRaisesRegex(ValueError, 'HTTP_RAW_UNEXPECTED_UPGRADE'):
            project([row], records + raw)

    def test_only_a_verified_ws_handshake_ends_http_parsing(self):
        row, records = exchange()
        key = base64.b64encode(bytes(range(16))).decode()
        accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        sent = (f'GET /ws HTTP/1.1\r\nHost: 127.0.0.1:28610\r\nUpgrade: websocket\r\n'
                f'Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n').encode()
        received = (f'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n'
                    f'Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n').encode()
        socket = {'id': 8, 'remoteHost': '127.0.0.1', 'remotePort': 28610}
        trace = [{'kind': 'fixture-tcp-write', 'socket': socket, 'payload': blob(sent + b'\x81\x00')},
                 {'kind': 'fixture-tcp-data', 'socket': socket, 'payload': blob(received + b'\x81\x00')}]
        self.assertEqual(project([row], records), project([row], records + trace))
        trace[1]['payload'] = blob(received.replace(b'101 Switching Protocols', b'200 OK'))
        with self.assertRaisesRegex(ValueError, 'HTTP_RAW_UPGRADE_HANDSHAKE'):
            project([row], records + trace)

    def test_declared_request_and_response_must_match_actual_bytes(self):
        for field, value in (('requestBody', '{"unexpected":true}'),
                             ('body', '{"ok":false}')):
            row, records = exchange()
            row[field] = value
            with self.assertRaisesRegex(ValueError, 'HTTP_REQUEST_HEX_BODY|HTTP_HEX_BODY'):
                project([row], records)
        row, records = exchange(method='GET')
        row['requestBody'] = '{"notActuallySent":true}'
        with self.assertRaisesRegex(ValueError, 'HTTP_REQUEST_BODY_NOT_SENT'):
            project([row], records)

    def test_duplicate_or_ambiguous_wire_is_rejected(self):
        row, records = exchange()
        duplicate = copy.deepcopy(records[1:-1])
        for item in duplicate:
            item['socket']['id'] = 8
        with self.assertRaisesRegex(ValueError, 'HTTP_RAW_PAIR_MISSING_OR_AMBIGUOUS'):
            project([row], records[:-1] + duplicate + records[-1:])

    def test_truncated_and_inconsistent_observation_are_rejected(self):
        for field, value in (('truncated', True), ('bytes', 999), ('bytes', True)):
            row, records = exchange()
            records[1]['payload'][field] = value
            with self.assertRaisesRegex(ValueError, 'HTTP_TRACE_BODY_BOUND'):
                project([row], records)

    def test_incomplete_and_conflicting_framing_are_rejected(self):
        row, records = exchange()
        data = bytes.fromhex(records[-2]['payload']['rawBytesHex'])
        records[-2]['payload'] = blob(data[:-1])
        with self.assertRaisesRegex(ValueError, 'HTTP_RAW_INCOMPLETE_BODY'):
            project([row], records)
        row, records = exchange(extra=[('Transfer-Encoding', 'chunked')])
        with self.assertRaisesRegex(ValueError, 'HTTP_RAW_AMBIGUOUS_FRAMING'):
            project([row], records)

    def test_frozen_headers_only_are_omitted(self):
        a, ar = exchange(extra=[('Date', 'first'), ('Connection', 'close')])
        b, br = exchange(extra=[('Date', 'second'), ('Connection', 'keep-alive')])
        self.assertEqual(project([a], ar), project([b], br))
        self.assertIn('content-length', project([a], ar)[0]['headers'])

    def test_json_boolean_and_number_and_nonfinite_values_are_not_conflated(self):
        self.assertNotEqual(contracts.canonical({'v': True}), contracts.canonical({'v': 1}))
        for value in (float('nan'), float('inf')):
            with self.assertRaises(ValueError):
                contracts.canonical({'v': value})

    def test_feishu_checks_every_model_request_not_only_expected_text(self):
        raw, records = mock_evidence()
        expected = contracts.feishu_outbound_wire(raw, records)
        changed = copy.deepcopy(raw)
        changed['models'][0]['temperature'] = 0.7
        with self.assertRaisesRegex(ValueError, 'FEISHU_MOCK_REQUEST_ORIGINALS|FEISHU_MOCK_MODEL_ORIGINALS'):
            contracts.feishu_outbound_wire(changed, records)
        raw, records = mock_evidence()
        raw['models'][0]['temperature'] = 0.7
        records[0]['body'] = blob(json.dumps(raw['models'][0]).encode())
        self.assertNotEqual(expected, contracts.feishu_outbound_wire(raw, records))

    def test_feishu_rejects_duplicate_ids_or_missing_response(self):
        raw, records = mock_evidence()
        with self.assertRaisesRegex(ValueError, 'FEISHU_MOCK_DUPLICATE_ID'):
            contracts.feishu_outbound_wire(raw, records + [records[0]])
        with self.assertRaisesRegex(ValueError, 'FEISHU_MOCK_INCOMPLETE_PAIRS'):
            contracts.feishu_outbound_wire(raw, records[:-1])

    def test_feishu_extra_call_is_retained_even_when_fully_recorded(self):
        raw, records = mock_evidence()
        expected = contracts.feishu_outbound_wire(raw, records)
        raw['http'].append(copy.deepcopy(raw['http'][0]))
        raw['models'].append(copy.deepcopy(raw['models'][0]))
        extra = copy.deepcopy(records[:2])
        for item in extra:
            item['id'] = 99
        self.assertNotEqual(expected, contracts.feishu_outbound_wire(raw, records + extra))

    def test_feishu_keeps_same_socket_order_but_not_cross_socket_scheduling(self):
        raw, records = mock_evidence()
        expected = contracts.feishu_outbound_wire(raw, records)
        reordered = records[4:6] + records[2:4] + records[:2] + records[6:]
        self.assertNotEqual(expected, contracts.feishu_outbound_wire(raw, reordered))
        for index in range(0, len(records), 2):
            records[index]['socket']['id'] = index
        expected = contracts.feishu_outbound_wire(raw, records)
        reordered = records[4:6] + records[2:4] + records[:2] + records[6:]
        self.assertEqual(expected, contracts.feishu_outbound_wire(raw, reordered))

    def test_restoring_contract_file_does_not_authenticate_cached_mutant(self):
        source = Path(contracts.__file__).read_bytes()
        native = types.SimpleNamespace(source={'files': [{
            'path': 'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/contracts.py',
            'sha256': hashlib.sha256(source).hexdigest()}]})
        def require(ok, rule, detail):
            if not ok:
                raise ValueError(rule)
        d = types.SimpleNamespace(ROOT=_apparatus.parent.parent, require=require,
                                  _load=lambda *args: contracts)
        self.assertIs(public.authenticated_contracts(d, native), contracts)
        mutant = types.SimpleNamespace(__file__=contracts.__file__, _apparatus_source=source + b'\n# mutant')
        d._load = lambda *args: mutant
        with self.assertRaisesRegex(ValueError, 'PUBLIC_HTTP_CONTRACT_SOURCE'):
            public.authenticated_contracts(d, native)

    def test_late_report_loader_verifies_before_calling_loaded_code(self):
        entries = _load('retirement_entries', _apparatus / 'retirement_entries.py')
        called = []
        mutant = types.SimpleNamespace(report=lambda *args: called.append('mutant'))
        with patch.object(entries, 'expected_rows', return_value=([], 'inventory')), \
                patch.object(entries, 'channel_results', return_value={}), \
                patch.object(entries, '_load', return_value=mutant), \
                patch.object(entries, '_verify_cli_imports', side_effect=ValueError('reject loaded mutant')):
            with self.assertRaisesRegex(ValueError, 'reject loaded mutant'):
                entries.report_entries(None, None, None, None, [], None, None)
        self.assertEqual(called, [])

    def test_late_deployment_loader_verifies_before_calling_loaded_code(self):
        entries = _load('retirement_entries', _apparatus / 'retirement_entries.py')
        native = types.SimpleNamespace(manifest=Path('/candidate/manifest.json'), manifest_sha='binary',
                                       source_manifest=Path('/candidate/source.json'), source_sha='source',
                                       source={'files': [], 'runtimeLibraries': {}})
        receipt = {'schemaVersion': 1, 'channel': 'test', 'rawEvidence': {'path': '/raw'},
                   'sourceManifest': {'path': str(native.source_manifest), 'sha256': 'source'},
                   'candidateManifest': {'path': str(native.manifest), 'sha256': 'binary'},
                   'runtimeLibraries': {}}
        called = []
        mutant = types.SimpleNamespace(verify_deployment=lambda *args: called.append('mutant'))
        def require(ok, rule, detail):
            if not ok:
                raise ValueError(rule)
        d = types.SimpleNamespace(ROOT=Path('/repo'), SCRIPT=_apparatus, require=require,
                                  _load=lambda *args: mutant,
                                  read=lambda path: [{'path': 'pilotdeck-gateway', 'sha256': 'exe'}]
                                  if path == native.manifest else receipt)
        with patch.object(entries, 'channel_artifact', return_value=Path('/receipt')), \
                patch.object(entries, '_verify_cli_imports', side_effect=ValueError('reject loaded mutant')):
            with self.assertRaisesRegex(ValueError, 'reject loaded mutant'):
                entries.verify_owned_process(d, native, {'path': '/receipt'}, {'path': '/raw'}, 'test',
                                             deployment_ref={'path': '/deployment'})
        self.assertEqual(called, [])


if __name__ == '__main__':
    unittest.main()
