"""Per-entry behavioral evidence from the registered 150 inventory.

One passing request does not certify a configurable feature. This face states
which rules were exercised and refuses cross-build or cfg(test) substitutions.
"""
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
import json
import unittest
import types
module = _load('retirement_standard_http', _apparatus / 'retirement_standard_http.py')

def row(body, headers=()):
    raw = json.dumps(body, separators=(',', ':')).encode()
    header = [('Content-Type', 'application/json'), ('Content-Length', str(len(raw))), *headers]
    wire = b'HTTP/1.1 200 OK\r\n' + ''.join(f'{key}: {value}\r\n' for key, value in header).encode() + b'\r\n' + raw
    return {'method': 'GET', 'path': '/health', 'status': 200, 'body': raw.decode(),
            'requestHex': b'GET /health HTTP/1.1\r\nHost: 127.0.0.1:28478\r\n\r\n'.hex(),
            'responseHex': wire.hex()}


def project(value):
    return module.http_projection(value, module.compare.Normalizer())


def text_row(body, headers=(), *, chunked=True):
    raw = body.encode('utf8')
    framing = [('Transfer-Encoding', 'chunked')] if chunked else [('Content-Length', str(len(raw)))]
    payload = raw
    if chunked:
        payload = ((f'{len(raw):x}\r\n'.encode() + raw + b'\r\n') if raw else b'') + b'0\r\n\r\n'
    wire = b'HTTP/1.1 200 OK\r\n' + ''.join(f'{key}: {value}\r\n' for key, value in [*headers, *framing]).encode() + b'\r\n' + payload
    return {'method': 'GET', 'path': '/health', 'status': 200, 'body': body,
            'requestHex': b'GET /health HTTP/1.1\r\nHost: 127.0.0.1:28478\r\n\r\n'.hex(), 'responseHex': wire.hex()}


def a2_project(body, headers, definition=None):
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        (root / 'scenarios/http').mkdir(parents=True)
        (root / 'home').mkdir(); (root / 'project').mkdir()
        (root / 'project/note.txt').write_text('durable fixture')
        if definition is not None:
            (root / 'home/entity.json').write_text(json.dumps(definition))
        (root / 'scenarios/http/http.json').write_text(json.dumps([
            {'path': '/health', 'status': 200, 'headers': headers, 'body': body}]))
        return module.compare.projection(root, {})['http'][0]


def compare_rows(left, right, *, changed_disk=False):
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        runtime = {key: str(root / 'cur' / key) for key in ('home', 'project', 'tmp', 'workspaces')}
        runtime.update(providerUrl='http://127.0.0.1:28531/v1', gatewayPort=28478)
        observed = {}
        for side, response in (('ts', left), ('native', right)):
            folder = root / ('http-' + side); folder.mkdir()
            (folder / 'home').mkdir(); (folder / 'project').mkdir()
            (folder / 'project/note.txt').write_text('complete durable text')
            raw = {'http': [dict(response, id='gateway.http:ANY /health')], 'providerRequests': []}
            (folder / 'result.json').write_text(json.dumps(raw))
            (folder / 'provenance.json').write_text(json.dumps({'runtime': runtime}))
            stat = folder.stat()
            preservation = {'originalRuntime': str(root / 'cur'), 'preservedAt': str(folder),
                            'device': stat.st_dev, 'inode': stat.st_ino, 'exitCode': 0,
                            'originals': {name: module.hashlib.sha256((folder / name).read_bytes()).hexdigest()
                                          for name in ('result.json', 'provenance.json')}}
            (root / (folder.name + '.preservation.json')).write_text(json.dumps(preservation))
            observed[('http', side)] = (folder, raw, None)
        if changed_disk:
            (root / 'http-native/project/note.txt').write_text('unexpected changed durable text')
        d = types.SimpleNamespace(read=lambda path: json.loads(path.read_text()))
        return module.comparison(d, observed, 'http', 'ANY /health')


class FullProjection(unittest.TestCase):
    def test_extra_body_field_is_not_hidden_by_success_marker(self):
        self.assertNotEqual(project(row({'ok': True})), project(row({'ok': True, 'unexpected': 1})))

    def test_content_length_is_retained(self):
        projected = project(row({'ok': True}))
        self.assertEqual(projected['headers']['content-length'], ['11'])

    def test_status_line_protocol_and_reason_are_retained(self):
        for replacement in (b'HTTP/1.0 200 OK', b'HTTP/1.1 200 Different'):
            original = row({'ok': True})
            changed = dict(original, responseHex=bytes.fromhex(original['responseHex']).replace(
                b'HTTP/1.1 200 OK', replacement, 1).hex())
            self.assertNotEqual(project(original), project(changed))

    def test_extra_header_is_retained(self):
        self.assertNotEqual(project(row({'ok': True})), project(row({'ok': True}, [('X-Contract', 'changed')])))

    def test_only_registered_response_headers_are_omitted(self):
        self.assertEqual(project(row({'ok': True}, [('Date', 'one'), ('Connection', 'close')])),
                         project(row({'ok': True}, [('Date', 'two'), ('Connection', 'keep-alive')])))

    def test_body_metadata_must_match_wire(self):
        value = row({'ok': True}); value['body'] = '{"ok":false}'
        with self.assertRaises(ValueError): project(value)

    def test_request_metadata_must_match_wire(self):
        value = row({'ok': True}); value['path'] = '/different'
        with self.assertRaises(ValueError): project(value)

    def test_business_timing_and_paths_remain_exact(self):
        self.assertNotEqual(project(row({'updatedAt': 1, 'path': '/one'})),
                            project(row({'updatedAt': 2, 'path': '/two'})))

    def test_diagnostic_background_cannot_certify_parity(self):
        present = {('cron', side): object() for side in ('ts', 'native')}
        self.assertEqual(module.comparison(None, present, 'cron', 'cron')['status'], 'BLOCKED')

    def test_empty_project_summary_created_at_is_metadata(self):
        n = module.compare.Normalizer()
        value = {'projectKey': '/project', 'name': 'project', 'fullPath': '/project',
                 'sessionCount': 0, 'createdAt': 1}
        self.assertEqual(n.value(value), n.value(dict(value, createdAt=2)))
        for key in ('projectKey', 'name', 'fullPath'):
            incomplete = {k: v for k, v in value.items() if k != key}
            self.assertNotEqual(n.value(incomplete), n.value(dict(incomplete, createdAt=2)))
        business = dict(value, runAt=1, nextRunAt=2, durationMs=3, usage=4)
        self.assertNotEqual(n.value(business), n.value(dict(business, runAt=99)))

    def test_full_comparison_accepts_equal_originals_and_rejects_changed_disk(self):
        self.assertEqual(compare_rows(row({'ok': True}), row({'ok': True}))['status'], 'PASS')
        self.assertEqual(compare_rows(row({'ok': True}), row({'ok': True}), changed_disk=True)['status'], 'FAIL')

    def test_json_semantics_survive_shared_body_projection(self):
        left, right = '{"a":1,"b":2}', '{ "b":2, "a":1 }'
        headers = [('Content-Type', 'application/json')]
        self.assertEqual(project(text_row(left, headers)), project(text_row(right, headers)))
        self.assertEqual(a2_project(left, dict(headers)), a2_project(right, dict(headers)))
        self.assertEqual(compare_rows(text_row(left, headers), text_row(right, headers))['status'], 'PASS')
        # JSON semantics do not permit discarding a different Content-Length.
        self.assertEqual(compare_rows(text_row(left, headers, chunked=False),
                                      text_row(right, headers, chunked=False))['status'], 'FAIL')

    def test_json_strings_and_plain_text_are_disjoint_in_full_comparison(self):
        pairs = [('"not found"', 'not found'), ('{"kind":"text","value":"X"}', 'X'),
                 ('"中文"', '中文'), ('""', ''), ('null', '')]
        for left, right in pairs:
            with self.subTest(left=left, right=right):
                self.assertNotEqual(project(text_row(left)), project(text_row(right)))
                self.assertNotEqual(a2_project(left, {}), a2_project(right, {}))
                result = compare_rows(text_row(left), text_row(right))
                self.assertEqual(result['status'], 'FAIL')
                self.assertTrue(any(item['path'].startswith('$.http.body') for item in result['differences']))
                self.assertEqual(compare_rows(text_row(right), text_row(right))['status'], 'PASS')

    def test_sse_and_single_line_ndjson_keep_the_complete_text(self):
        cases = [('Text/Event-Stream; charset=utf-8', 'data: first\n\ndata: last\n\n'),
                 ('Application/X-NDJSON; charset=UTF-8', '{"first":1,"last":2}\n'),
                 ('application/ndjson', '{"first":1,"last":2}\n')]
        for content_type, body in cases:
            headers = [('cOnTeNt-TyPe', content_type)]
            for changed in (json.dumps(body), body.rstrip('\n'), body + ' '):
                with self.subTest(content_type=content_type, changed=changed):
                    self.assertNotEqual(project(text_row(body, headers)), project(text_row(changed, headers)))
                    self.assertNotEqual(a2_project(body, dict(headers)), a2_project(changed, dict(headers)))
                    self.assertEqual(compare_rows(text_row(body, headers), text_row(changed, headers))['status'], 'FAIL')
            self.assertEqual(project(text_row(body, headers))['body'], {'kind': 'text', 'value': body})

    def test_duplicate_content_types_are_detected_without_hiding_header_order(self):
        headers = [('Content-Type', 'application/json'), ('Content-Type', 'Application/X-NDJSON; charset=utf-8')]
        left, right = '{"a":1}', '{ "a":1 }'
        self.assertNotEqual(project(text_row(left, headers)), project(text_row(right, headers)))
        self.assertNotEqual(a2_project(left, {'CONTENT-TYPE': [value for _, value in headers]}),
                            a2_project(right, {'CONTENT-TYPE': [value for _, value in headers]}))
        # The A2 Fetch recorder joins repeated Content-Type values into one str.
        joined = ', '.join(value for _, value in headers)
        self.assertNotEqual(a2_project(left, {'content-type': joined}), a2_project(right, {'content-type': joined}))
        self.assertNotEqual(project(text_row(left, [('Content-Type', joined)])),
                            project(text_row(right, [('Content-Type', joined)])))
        quoted = 'application/json; note="a, text/event-stream, x"'
        self.assertEqual(a2_project(left, {'content-type': quoted}), a2_project(right, {'content-type': quoted}))
        self.assertEqual(project(text_row(left, [('Content-Type', quoted)])),
                         project(text_row(right, [('Content-Type', quoted)])))
        self.assertEqual(project(text_row(left, headers))['headers']['content-type'], [value for _, value in headers])
        self.assertNotEqual(project(text_row(left, headers)), project(text_row(left, list(reversed(headers)))))
        # Classification does not normalize or omit the original header values.
        self.assertNotEqual(project(text_row(left, [('Content-Type', 'application/json')])),
                            project(text_row(left, [('Content-Type', 'Application/JSON')])))

    def test_json_entity_metadata_is_normalized_inside_the_type_envelope(self):
        ids = ['00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002']
        values, a2_values = [], []
        for index, identity in enumerate(ids):
            normalizer = module.compare.Normalizer()
            normalizer.index({'id': identity, 'name': 'same entity'})
            body = {'id': identity, 'summary': {'projectKey': '/p', 'name': 'p', 'fullPath': '/p',
                                              'sessionCount': 0, 'createdAt': index + 1}, 'runAt': 3}
            values.append(module.http_projection(text_row(json.dumps(body)), normalizer)['body'])
            a2_values.append(a2_project(json.dumps(body), {}, {'id': identity, 'name': 'same entity'})['body'])
        self.assertEqual(values[0], values[1])
        self.assertEqual(a2_values[0], a2_values[1])
        self.assertEqual(values, a2_values)
        changed = dict(body, runAt=4)
        self.assertNotEqual(values[1], module.http_projection(text_row(json.dumps(changed)), normalizer)['body'])
        unknown = dict(body, unknownId='00000000-0000-4000-8000-000000000003')
        self.assertNotEqual(module.http_projection(text_row(json.dumps(unknown)), normalizer)['body'],
                            module.http_projection(text_row(json.dumps(dict(unknown, unknownId='00000000-0000-4000-8000-000000000004'))), normalizer)['body'])

    def test_text_preserves_known_uuid_mapping_but_no_other_changes(self):
        ids = ['00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002']
        unknown = '00000000-0000-4000-8000-000000000003'
        for content_type, template in [('text/plain', 'UUID'), ('text/event-stream', 'data: UUID\n\n'),
                                       ('application/x-ndjson', '{"id":"UUID"}\n')]:
            headers = [('Content-Type', content_type)]
            with self.subTest(content_type=content_type):
                values = []
                for identity in ids:
                    definition = {'id': identity, 'name': 'entity'}
                    normalizer = module.compare.Normalizer(); normalizer.index(definition)
                    body = template.replace('UUID', identity)
                    actual = module.http_projection(text_row(body, headers), normalizer)['body']
                    self.assertEqual(actual, {'kind': 'text', 'value': template.replace('UUID', normalizer.ids[identity])})
                    self.assertEqual(actual, a2_project(body, dict(headers), definition)['body'])
                    values.append(actual)
                    changed = body + ' '
                    self.assertNotEqual(actual, module.http_projection(text_row(changed, headers), normalizer)['body'])
                    self.assertNotEqual(actual, a2_project(changed, dict(headers), definition)['body'])
                    unbound = template.replace('UUID', unknown)
                    self.assertEqual(module.http_projection(text_row(unbound, headers), normalizer)['body'],
                                     {'kind': 'text', 'value': unbound})
                    self.assertEqual(a2_project(unbound, dict(headers), definition)['body'],
                                     {'kind': 'text', 'value': unbound})
                    self.assertNotEqual(actual, {'kind': 'text', 'value': unbound})
                self.assertEqual(values[0], values[1])

    def test_http_json_numbers_keep_value_but_not_boolean_type(self):
        cases = [('true', '1   ', False), ('false', '0    ', False),
                 ('{"items":[true,{"ok":false}]}', '{"items":[1   ,{"ok":0    }]}', False),
                 ('1  ', '1.0', True), ('{"items":[1,{"n":2.0}]}', '{"items":[1.0,{"n":2}]}', True)]
        for left, right, equal in cases:
            with self.subTest(left=left, right=right):
                self.assertEqual(module.compare.native_evidence.same_json(
                    a2_project(left, {}), a2_project(right, {})), equal)

    def test_missing_side_cannot_certify_parity(self):
        self.assertEqual(module.comparison(None, {}, 'http', '/health')['status'], 'BLOCKED')


if __name__ == '__main__':
    unittest.main()
