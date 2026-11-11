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
import atexit
import shutil

atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
from collections import Counter
import hashlib
from http.client import HTTPResponse
from io import BytesIO
import json
from urllib.parse import unquote, urlsplit


COUNTS = {'gateway.method': 101, 'gateway.http': 14, 'gateway.channel-http': 13,
          'gateway.channel': 19, 'gateway.background': 2, 'gateway.ws': 1}


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def expected_rows(d, baseline, methods):
    path = d.SCRIPT / 'fixtures/retirement-entry-inventory.json'
    inv = d.read(path)
    d.require(inv['baseline'] == d.RETIREMENT_BASELINE_REVISION, 'ENTRY_INVENTORY', 'baseline identity changed')
    rows = inv['entries']
    identities = {row['id'] for row in rows}
    d.require(len(rows) == len(identities) == 150 and Counter(r['id'].partition(':')[0] for r in rows) == COUNTS
              and {key.partition(':')[2] for key in identities if key.startswith('gateway.method:')} == set(methods),
              'ENTRY_INVENTORY', 'the expected 150-entry census is incomplete or duplicated')
    d.require(all(isinstance(row.get('sources'), list) and row['sources'] and
                  all(isinstance(pointer, str) and ':' in pointer and pointer.rsplit(':', 1)[-1].isdecimal()
                      and int(pointer.rsplit(':', 1)[-1]) > 0 for pointer in row['sources'])
                  for row in rows), 'ENTRY_INVENTORY', 'every entry requires a nonempty concrete source location')
    origins = {pointer.rsplit(':', 1)[0] for row in rows for pointer in row['sources']}
    d.require(origins == inv['sources'].keys() and all(not Path(p).is_absolute() and '..' not in Path(p).parts
              and sha(baseline / p) == digest for p, digest in inv['sources'].items()),
              'ENTRY_INVENTORY', 'independent fixed TS source changed')
    return rows, sha(path)


class Wire:
    def __init__(self, data):
        self.data = data

    def makefile(self, *_):
        return BytesIO(self.data)


def http_frame(row):
    wire_request = bytes.fromhex(row['requestHex'])
    wire_response = bytes.fromhex(row['responseHex'])
    method, target, version = wire_request.split(b'\r\n', 1)[0].decode('ascii').split(' ')
    if (method, target, version) != (row['method'], row['path'], 'HTTP/1.1'):
        raise ValueError('HTTP request metadata differs from the retained wire request')
    response = HTTPResponse(Wire(wire_response))
    response.begin()
    body = response.read().decode('utf-8')
    if response.status != row['status'] or body != row['body']:
        raise ValueError('HTTP status/body metadata differs from retained response')
    return method, unquote(urlsplit(target).path), response.status, body


def valid_http(row, key, folder):
    method, target, status, body = http_frame(row)
    project = folder / 'project'
    base = f'/api/web/projects/{project}'
    if key == '<unmatched-404>':
        return method == 'GET' and target == '/entry-missing-42.txt' and status == 404 and body == 'not found'
    if key == 'static-fallback':
        return method == 'GET' and target == '/acceptance.js' and status == 200 and body == 'ENTRY_STATIC_42\n'
    verb, route = key.split(' ', 1)
    actual = route.replace('/api/web/projects/:projectKey', base)
    if (verb != 'ANY' and method != verb) or target != actual:
        return False
    if route == '/feishu/webhook':
        # Without configured callback authentication this HTTP route can fall
        # through to the SPA. The optional probe is retained but is no hit.
        return None
    try:
        value = json.loads(body)
    except ValueError:
        return False
    if route.startswith('/api/module-host/'):
        # Compare the real fixed TS error face separately. These inputs do
        # not exercise registered success, admission or configured capabilities.
        return status, value
    if status != 200:
        return False
    if route == '/health':
        return value == {'ok': True}
    if route == '/auth/local-token':
        return value == {'token': 'entry-owned-token'}
    if route == '/api/web/projects':
        return any(item.get('projectKey') == str(project) for item in value['projects'])
    if route.endswith('/files/read'):
        return value.get('projectKey') == str(project) and 'ENTRY_NOTE_42' in json.dumps(value)
    if route.endswith('/files/write'):
        return value == {'ok': True} and (project / 'acceptance-http-write.txt').read_bytes() == b'ENTRY_HTTP_WRITE_42\n'
    if route.endswith('/files/tree'):
        return value.get('projectKey') == str(project) and 'note.txt' in json.dumps(value)
    return value.get('projectKey') == str(project)


def raw_background(d, folder, mode, result):
    messages = result['providerRequests']
    if not any(row['path'] == '/v1/chat/completions' and row['body'].get('stream') is True for row in messages):
        return False
    if mode == 'cron':
        raw = d.read(folder / 'cron.json')
        wire = [json.loads(line) for line in (folder / 'wire.jsonl').read_text().splitlines()]
        key = raw['created']['task']['sessionKey']
        durable = list((folder / 'home/projects').rglob('*.jsonl'))
        texts = [p.read_text() for p in durable if 'ENTRY_BACKGROUND_CRON' in p.read_text()]
        return (key.startswith('cron:') and raw == result['background'] and bool(wire)
                and any('ENTRY_BACKGROUND_CRON' in json.dumps(row['body']) for row in messages)
                and not raw['history'].get('stream', {}).get('active')
                and 'ENTRY_RUNTIME_REPLY_cron' in json.dumps(raw['history'])
                and any('turn_result' in text and 'ENTRY_RUNTIME_REPLY_cron' in text for text in texts))
    history = list((folder / 'home/always-on/projects').glob('*/run-history.jsonl'))
    events = list((folder / 'home/always-on/projects').glob('*/events.jsonl'))
    background = result.get('background') or {}
    return (len(history) == len(events) == 1 and history[0].parent == events[0].parent
            and background.get('snapshotAfterReap') is True
            and background.get('sha256') == {'runHistory': sha(history[0]), 'events': sha(events[0])}
            and 'no_plan' in history[0].read_text() and 'discovery_started' in events[0].read_text()
            and background['runHistory'] == history[0].read_text() and background['events'] == events[0].read_text())


def production_results(d, native, baseline, entry_root):
    """Require a single source/candidate identity and actual isolated product exits."""
    preflight = d.read(entry_root / 'inside/network-isolation.json')
    cleanup = d.read(entry_root / 'container-cleanup.json')
    tests = preflight['negativeControls']
    d.require(preflight['interfaces'] == [[1, 'lo']] and preflight['loopbackConnect'] == 'PASS'
              and len(tests) == 4 and all(x['blocked'] is True for x in tests)
              and {(x['family'], x['kind']) for x in tests} == {(2, 1), (2, 2), (10, 1), (10, 2)}
              and cleanup['ownedContainerStillRunning'] == '',
              'ENTRY_PRODUCTION_SOURCE', 'entry consumer lacks same isolation and cleanup proof')
    result = {}
    for mode in ('http', 'cron', 'alwayson'):
        for engine in ('ts', 'native'):
            folder = entry_root / f'{mode}-{engine}'
            if not folder.is_dir():
                continue
            raw, provenance = d.read(folder / 'result.json'), d.read(folder / 'provenance.json')
            d.require(raw['engine'] == provenance['engine'] == engine and raw['mode'] == provenance['mode'] == mode
                      and provenance['sourceManifest'] == {'path': str(native.source_manifest), 'sha256': native.source_sha}
                      and provenance['candidateManifest'] == {'path': str(native.manifest), 'sha256': native.manifest_sha}
                      and provenance['binaries'] == {'pilotdeck': native.identities['host']['sha256'],
                                                     'sidecar': native.identities['sidecar']['sha256']}
                      and provenance['networkNamespace'] == preflight['networkNamespace'],
                      'ENTRY_PRODUCTION_SOURCE', f'{mode}/{engine} comes from a different build or isolated run')
            argv = provenance['command']
            listener = ['server', '--host', '127.0.0.1', '--port', '28479' if engine == 'native' else '28478']
            d.require(argv[-len(listener):] == listener and
                      (argv[:-len(listener)] == [native.identities['host']['path']] if engine == 'native'
                       else len(argv) == len(listener) + 2 and Path(argv[0]).name == 'node'
                       and provenance['node']['node'] == '22.22.0' and provenance['node']['icu'] == '78.2'
                       and Path(argv[1]) == baseline / 'dist/src/cli/pilotdeck.js'
                       and provenance['baselineEntrySha256'] == sha(baseline / 'dist/src/cli/pilotdeck.js')),
                      'ENTRY_PRODUCTION_SOURCE', f'{mode}/{engine} used a different production startup')
            fixture = d.ROOT / 'src/pilotdeck-gateway/tests/fixtures/retirement-production-entries'
            d.require(provenance['producerSha256'] == sha(fixture / 'probe.py')
                      and provenance['cronDriverSha256'] == sha(fixture / 'cron.mts'),
                      'ENTRY_PRODUCTION_SOURCE', f'{mode}/{engine} used a different fixture')
            result[(mode, engine)] = (folder, raw, sha(folder / 'result.json'))
    return result


def report_entries(d, native, baseline, methods, rounds, coverage, outcome, *, product_capture=None):
    inventory, manifest_sha = expected_rows(d, baseline, methods)
    observed = production_results(d, native, baseline, product_capture.resolve(strict=True)) if product_capture else {}
    result = []
    for expected in inventory:
        key = expected['id']
        group, _, identity = key.partition(':')
        state = 'BLOCKED'
        pointers = []
        if group == 'gateway.method':
            records = [v['methodVerdicts'].get(identity) for v in coverage]
            original = outcome.get('report') or {}
            violations = [row for row in original.get('violations', []) if row.get('method') == identity]
            if violations or any(state_ not in ('ok', 'error') for state_ in records):
                state = 'FAIL'
            elif outcome['classification'] == 'PASS' and all(r['status'] == 'PASS' for r in coverage):
                state = 'PASS'
            detail = {'methodOutcomes': records, 'outcomeRules': violations,
                      'scope': 'registered wire and #1477 method-outcome profile'}
            pointers = [row['folder'] + '/driver/methods.json' for row in rounds]
        elif group == 'gateway.ws':
            valid = []
            for row in rounds:
                frames = row['frames']
                valid.append(bool(len(frames) >= 2 and frames[0]['direction'] == 'send' and frames[0]['frame']['type'] == 'hello'
                                  and frames[1]['direction'] == 'receive' and frames[1]['frame']['type'] == 'hello_ok'
                                  and frames[0]['connection'] == frames[1]['connection']
                                  and row['nativeWitness']['status'] == 'PASS'))
                pointers.append(row['folder'] + '/driver/frames.jsonl')
            state = 'PASS' if all(valid) else 'BLOCKED'
            detail = {'handshakeAndNativeWitnessPerRound': valid}
        elif group in ('gateway.http', 'gateway.background'):
            mode = 'http' if group == 'gateway.http' else {'cron': 'cron', 'always-on': 'alwayson'}[identity]
            profiles = []
            for engine in ('ts', 'native'):
                found = observed.get((mode, engine))
                if found is None:
                    profiles.append(None)
                    continue
                folder, raw, digest = found
                pointers.append({'path': str(folder / 'result.json'), 'sha256': digest})
                port = 28478 if engine == 'ts' else 28479
                owner = raw.get('listener') or {}
                if (type(raw['serverExitCode']) is not int or raw['serverExitCode'] != 0 or raw['errors']
                        or raw['binariesUnchanged'] is not True or raw.get('ownedCleanup', {}).get('allStopped') is not True
                        or type(raw.get('serverPid')) is not int or raw['serverPid'] <= 0
                        or owner.get('addresses') != [f'127.0.0.1:{port}'] or owner.get('ownerPid') != raw['serverPid']):
                    profiles.append(False)
                elif mode != 'http':
                    profiles.append(raw_background(d, folder, mode, raw))
                else:
                    record = [r for r in raw['http'] if r['id'] == key]
                    profiles.append(valid_http(record[0], identity, folder) if len(record) == 1 else False)
            if any(profile is False for profile in profiles):
                state = 'FAIL'
            elif all(profile is True for profile in profiles):
                state = 'PASS'
            elif all(isinstance(profile, tuple) for profile in profiles) and profiles[0] != profiles[1]:
                state = 'FAIL'
            detail = {'profileResults': profiles, 'scope': 'production HTTP data or timer/durable behavior',
                      'missing': ('configured success/admission or Feishu callback' if mode == 'http' else
                                  'same-head positive timer and persistence on both sides') if state == 'BLOCKED' else None}
        else:
            detail = {'missing': 'same-head production scoped channel mock evidence; old/cfg(test) evidence is not substituted'}
        result.append({**expected, 'status': state, 'detail': detail, 'originals': pointers})
    status = d.common.aggregate_status(*(row['status'] for row in result))[0]
    return {'status': status, 'expectedCount': 150, 'inventorySha256': manifest_sha,
            'counts': dict(Counter(row['status'] for row in result)), 'entries': result}
