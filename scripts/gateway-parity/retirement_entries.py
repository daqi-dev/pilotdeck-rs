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


standard_http = _load('retirement_standard_http', _apparatus / 'retirement_standard_http.py')


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


def valid_http(row, key, folder, runtime_project=None):
    method, target, status, body = http_frame(row)
    project = Path(runtime_project) if runtime_project else folder / 'project'
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
        return value == {'ok': True} and (folder / 'project/acceptance-http-write.txt').read_bytes() == b'ENTRY_HTTP_WRITE_42\n'
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


# The first implemented channel profile replays the existing eight-channel
# producer's business assertions. Other profiles retain their own raw evidence
# and stay BLOCKED until their corresponding contract adapter is connected.
EIGHT_CHANNELS = {'telegram', 'discord', 'slack', 'dingtalk', 'feishu', 'weixin', 'qq', 'wecom'}


def channel_basic_observation(raw, channel):
    """Actual inbound -> last user model input -> recipient-bound outbound."""
    def check(condition, rule):
        if not condition:
            raise ValueError(rule)

    check(raw.get('channel') == channel and raw.get('failure') is None, 'CHANNEL_EXECUTION')
    check(raw.get('exit') == {'code': 0, 'signal': None}, 'CHANNEL_EXIT')
    inbound = raw.get('inbound', [])
    check([row.get('generation') for row in inbound] == [0, 1], 'CHANNEL_INBOUND')
    check([row.get('text') for row in inbound] == ['CHANNEL_INPUT_0', 'CHANNEL_INPUT_1'], 'CHANNEL_INBOUND')
    streams = [row for row in raw.get('models', []) if row.get('stream') is True]
    for generation in (0, 1):
        incoming, outgoing = f'CHANNEL_INPUT_{generation}', f'CHANNEL_OUTPUT_{generation}'
        matching = [row for row in streams if
                    [m.get('content') for m in row.get('messages', []) if m.get('role') == 'user'][-1:] == [incoming]]
        check(len(matching) == 1, 'CHANNEL_MODEL_INPUT')
        found = False
        for row in raw.get('http', []):
            if row.get('method') != 'POST' or row.get('path', '').startswith('/v1/'):
                continue
            body, path = row.get('body', {}), row['path']
            if channel == 'telegram':
                found |= path.endswith('/sendMessage') and body.get('chat_id') == '42' and body.get('text') == outgoing
            elif channel == 'discord':
                found |= path == '/channels/42/messages' and body.get('content') == outgoing
            elif channel == 'slack':
                found |= path == '/api/chat.postMessage' and body.get('channel') == 'Droom' and body.get('text') == outgoing
            elif channel == 'dingtalk':
                found |= path == '/dingtalk/send' and body.get('markdown', {}).get('text') == outgoing
            elif channel == 'feishu' and path == '/open-apis/im/v1/messages' and body.get('receive_id') == 'room':
                content = json.loads(body.get('content', '{}'))
                found |= content == {'zh_cn': {'content': [[{'tag': 'text', 'text': outgoing}]]}}
            elif channel == 'weixin' and path == '/ilink/bot/sendmessage':
                message = body.get('msg', {})
                found |= (message.get('to_user_id') == 'chat' and message.get('context_token') == 'ctx' and
                          any(item.get('text_item', {}).get('text') == outgoing for item in message.get('item_list', [])))
            elif channel == 'qq':
                found |= (path == '/v2/groups/group/messages' and body.get('content') == outgoing
                          and body.get('msg_id') == f'event-{generation + 1}')
        if channel == 'wecom':
            found = any(row.get('cmd') == 'aibot_respond_msg'
                        and row.get('headers', {}).get('req_id') == f'in-{generation + 1}'
                        and row.get('body', {}).get('markdown', {}).get('content') == outgoing
                        for row in raw.get('wire', []))
        check(found, 'CHANNEL_OUTBOUND_RECIPIENT')
    frames = raw.get('controlFrames', [])
    for action in ('disable', 'enable'):
        replies = [row for row in frames if row.get('type') == 'response' and row.get('id') == action]
        check(len(replies) == 1 and replies[0].get('ok') is True
              and replies[0].get('result') == {'reloaded': True, 'changedPaths': [f'adapters.{channel}.enabled']},
              'CHANNEL_RELOAD')
    steps = raw.get('steps', [])
    check(any(row.get('event') == 'actual-process-stop' and row.get('exit') == {'code': 0, 'signal': None}
              for row in steps), 'CHANNEL_STOP')
    return {'inboundGenerations': [0, 1], 'actualModelRequests': 2,
            'recipientBoundOutbound': 2, 'publicReload': ['disable', 'enable'], 'processExit': 0}



def channel_applicability(d, baseline, channel):
    """Read the independently reviewed fixed-TS source map, never a producer N/A."""
    registry = d.read(d.SCRIPT / 'fixtures/channel-applicability.json')
    rows = registry['channels']
    expected = {row['id'].partition(':')[2] for row in
                d.read(d.SCRIPT / 'fixtures/retirement-entry-inventory.json')['entries']
                if row['id'].startswith('gateway.channel:')}
    d.require(len(rows) == len(expected) and {row['channel'] for row in rows} == expected,
              'ENTRY_CHANNEL_APPLICABILITY', 'source applicability must retain all original 19 channels')
    found = next(row for row in rows if row['channel'] == channel)
    source = found['sourceFile']
    path = baseline / source['path']
    d.require(found['fixedTS'] == d.RETIREMENT_BASELINE_REVISION and path.resolve().is_relative_to(baseline)
              and sha(path) == source['sha256'], 'ENTRY_CHANNEL_APPLICABILITY', 'fixed-TS source identity changed')
    lines = path.read_text().splitlines()
    for face in ('attachment', 'channelInteraction'):
        info = found[face]
        d.require(type(info['applicable']) is bool and isinstance(info['kinds'], list)
                  and bool(info['kinds']) == info['applicable'],
                  'ENTRY_CHANNEL_APPLICABILITY', 'incomplete applicability source map')
        for anchor in info['sourceAnchors']:
            name, number = anchor['source'].rsplit(':', 1)
            d.require(name == source['path'] and 0 < int(number) <= len(lines)
                      and lines[int(number) - 1].strip() == anchor['text'].strip(),
                      'ENTRY_CHANNEL_APPLICABILITY', 'source anchor changed')
    return {key: found[key] for key in ('fixedTS', 'sourceFile', 'attachment', 'channelInteraction')}


def channel_transport_observation(raw, channel, records):
    """Reconstruct basic observations from full actual transport bytes."""
    def body(row, key='body'):
        value = row[key]
        payload = bytes.fromhex(value['rawBytesHex'])
        if value.get('truncated') is not False or len(payload) != value['bytes']:
            raise ValueError('CHANNEL_TRANSPORT_TRUNCATED')
        return payload

    sequences = [row['sequence'] for row in records]
    if len(sequences) != len(set(sequences)) or sequences != sorted(sequences):
        raise ValueError('CHANNEL_TRANSPORT_SEQUENCE')
    requests, responses = {}, {}
    sent, received, inbound_bytes = [], [], []
    for row in records:
        kind = row['kind']
        if kind == 'mock-http-request':
            if row['id'] in requests:
                raise ValueError('CHANNEL_TRANSPORT_DUPLICATE')
            requests[row['id']] = row
        elif kind == 'mock-http-response':
            if row['id'] in responses or row.get('finished') is not True:
                raise ValueError('CHANNEL_TRANSPORT_RESPONSE')
            responses[row['id']] = row
            inbound_bytes.append(body(row))
        elif kind in ('fixture-ws-send', 'fixture-ws-receive'):
            payload = body(row, 'payload')
            if kind == 'fixture-ws-send':
                inbound_bytes.append(payload)
            try:
                decoded = json.loads(payload)
            except (UnicodeDecodeError, ValueError):
                continue
            (sent if kind == 'fixture-ws-send' else received).append(decoded)
    actual_http, models = [], []
    for identity, row in requests.items():
        payload = body(row)
        try:
            decoded = json.loads(payload)
        except (UnicodeDecodeError, ValueError):
            continue
        path = urlsplit(row['url']).path
        if path.startswith('/v1/'):
            models.append(decoded)
        else:
            response = responses.get(identity)
            if response is None or not 200 <= response['status'] < 300:
                raise ValueError('CHANNEL_TRANSPORT_RESPONSE')
            body(response)
            actual_http.append({'method': row['method'], 'path': path, 'body': decoded})
    for generation in (0, 1):
        marker = f'CHANNEL_INPUT_{generation}'.encode()
        if not any(marker in payload for payload in inbound_bytes):
            raise ValueError('CHANNEL_TRANSPORT_INBOUND')
    # Replay the same basic contract over the actual model and protocol bytes.
    actual = dict(raw, models=models, http=actual_http, wire=received)
    result = channel_basic_observation(actual, channel)
    if channel == 'wecom':
        for generation in (0, 1):
            request_id = f'in-{generation + 1}'
            if not any(isinstance(row, dict) and row.get('headers', {}).get('req_id') == request_id
                       and row.get('body', {}).get('errcode') == 0 for row in sent):
                raise ValueError('CHANNEL_TRANSPORT_ACK')
    return dict(result, actualTransportRecords=len(records), actualHttpPairs=len(responses))

def channel_artifact(d, ref):
    path = Path(ref['path'])
    d.require(path.is_absolute() and path.is_file() and not path.is_symlink() and path.resolve() == path
              and path.is_relative_to(d.ROOT / '_tmp') and sha(path) == ref['sha256'],
              'ENTRY_CHANNEL_RAW', 'missing, aliased, or changed channel raw artifact')
    return path


def verify_owned_process(d, native, receipt_ref, raw_ref, channel, *,
                         executable_role='pilotdeck-gateway', argv_prefix=('--native',),
                         deployment_ref=None, node_ref=None, baseline=None, preload_refs=()):
    """Verify shared observation originals, without assigning business coverage."""
    fixed_bins = {row['path']: row['sha256'] for row in d.read(native.manifest)}
    tracked = {str(d.ROOT / row['path']): row['sha256'] for row in native.source['files']}
    originals, missing = [], []
    def artifact(ref):
        path = channel_artifact(d, ref)
        if ref not in originals:
            originals.append(ref)
        return path
    receipt_path = artifact(receipt_ref)
    receipt = d.read(receipt_path)
    d.require(receipt.get('schemaVersion') == 1 and receipt.get('channel') == channel
              and receipt.get('rawEvidence') == raw_ref
              and receipt.get('sourceManifest') == {'path': str(native.source_manifest), 'sha256': native.source_sha}
              and receipt.get('candidateManifest') == {'path': str(native.manifest), 'sha256': native.manifest_sha}
              and receipt.get('runtimeLibraries') == native.source.get('runtimeLibraries'),
              'ENTRY_CHANNEL_SOURCE', 'process receipt is not bound to the same raw/source/bin/runtime')
    expected_path = str(native.manifest.parent / executable_role)
    expected_host = Path(expected_path)
    expected_sha = fixed_bins.get(executable_role)
    deployment = None
    if deployment_ref is not None:
        public = d._load('retirement_public_http', d.SCRIPT / 'retirement_public_http.py')
        _verify_cli_imports(_apparatus)
        deployment = public.verify_deployment(d, native, deployment_ref)
        mapped = deployment['executables'][executable_role]
        expected_path = mapped['path']
        expected_host = Path(mapped.get('hostPath', mapped['path']))
        if executable_role != 'ts-node':
            d.require(mapped['sha256'] == expected_sha, 'ENTRY_CHANNEL_PROCESS', 'deployment binary differs from fixed candidate')
        expected_sha = mapped['sha256']
        originals.extend(deployment['originals'])
    if executable_role == 'ts-node':
        d.require(node_ref is not None and baseline is not None,
                  'ENTRY_CHANNEL_PROCESS', 'fixed TS process requires its Node and baseline identity')
        if deployment is None:
            expected_path = node_ref['path']
            expected_host = Path(expected_path)
            expected_sha = node_ref['sha256']
        d.require(str(expected_host) == node_ref['path'] and expected_sha == node_ref['sha256'],
                  'ENTRY_CHANNEL_PROCESS', 'observed TS runtime differs from the fixed Node reference')
    d.require(expected_host.is_absolute() and expected_host.is_file() and not expected_host.is_symlink()
              and expected_host.resolve() == expected_host and sha(expected_host) == expected_sha,
              'ENTRY_CHANNEL_PROCESS', 'fixed executable identity changed')
    if executable_role == 'ts-node':
        versions = json.loads(d.subprocess.check_output(
            [str(expected_host), '-p', 'JSON.stringify(process.versions)'], timeout=5))
        d.require([versions.get(key) for key in ('node', 'icu', 'uv')] == ['22.22.0', '78.2', '1.52.1']
                  and d.subprocess.check_output(['git', '-C', str(baseline), 'rev-parse', 'HEAD'], timeout=10).decode().strip()
                      == d.RETIREMENT_BASELINE_REVISION
                  and not d.subprocess.check_output(['git', '-C', str(baseline), 'status', '--porcelain', '--untracked-files=no'], timeout=10).strip(),
                  'ENTRY_CHANNEL_PROCESS', 'fixed TS process source or runtime changed')
        runtime_baseline = str(baseline) if deployment is None else deployment['baseline']['path']
        imports = []
        for preload in preload_refs:
            path = Path(preload['path'])
            d.require(path.is_absolute() and path.resolve() == path and path.is_file() and not path.is_symlink()
                      and tracked.get(str(path)) == preload['sha256'] == sha(path),
                      'ENTRY_CHANNEL_PRODUCER', 'TS preload is not an authenticated source input')
            runtime_path = str(path)
            if deployment is not None:
                choices = [mapped for mapped in deployment.get('preloads', [])
                           if mapped['hostPath'] == str(path) and mapped['sha256'] == preload['sha256']]
                d.require(len(choices) == 1, 'ENTRY_CHANNEL_PRODUCER', 'preload has no unique verified mount mapping')
                runtime_path = choices[0]['path']
            imports.extend(('--import', runtime_path))
        runtime_baseline = Path(runtime_baseline)
        argv_prefix = (*imports, '--import', str(runtime_baseline / 'node_modules/tsx/dist/loader.mjs'),
                       str(runtime_baseline / 'src/cli/pilotdeck.ts'), 'server')
    producer = receipt['producer']
    d.require(tracked.get(producer['path']) == producer['sha256']
              and sha(Path(producer['path'])) == producer['sha256'],
              'ENTRY_CHANNEL_PRODUCER', 'producer is not an authenticated source input')
    d.require(receipt.get('bindingErrors') == [], 'ENTRY_CHANNEL_PROCESS', 'observer reported binding errors')
    observer_path = artifact(receipt['observer'])
    observer = d.read(observer_path)
    d.require(tracked.get(observer['path']) == observer['sha256']
              and sha(Path(observer['path'])) == observer['sha256'],
              'ENTRY_CHANNEL_PRODUCER', 'observer is not an authenticated source input')
    if 'finalizer' in receipt:
        finalizer = receipt['finalizer']
        d.require(tracked.get(finalizer['path']) == finalizer['sha256']
                  and sha(Path(finalizer['path'])) == finalizer['sha256'],
                  'ENTRY_CHANNEL_PRODUCER', 'finalizer is not an authenticated source input')
    else:
        missing.append('source-bound-process-finalizer')
    observation_path = artifact(receipt['observationTrace'])
    observations = d.read(observation_path)
    d.require(len(observations) == 1 and observations[0].get('errors') == [],
              'ENTRY_CHANNEL_PROCESS', 'missing or erroneous actual process observations')
    process = receipt['process']
    observed_process = observations[0]
    actual_listeners = {json.dumps(listener, sort_keys=True) for observation in
                        observed_process.get('observations', [])
                        for listener in observation.get('readyListeners', [])}
    d.require({json.dumps(listener, sort_keys=True) for listener in process['readyListeners']}
              == actual_listeners, 'ENTRY_CHANNEL_PROCESS', 'ready listener receipt diverges from observations')
    d.require(all(process.get(field) == observed_process.get(field) for field in
                  ('pid', 'argv', 'executable', 'birth', 'cwd', 'exit', 'remainingOwnedIdentities',
                   'networkNamespace', 'pidNamespace'))
              and observed_process.get('closeObservedAt') is not None
              and observed_process.get('identityStillPresent') is False
              and observed_process.get('portsStillListening') == [],
              'ENTRY_CHANNEL_PROCESS', 'process receipt diverges from actual observation trace')
    executable = process['executable']
    d.require(type(process['pid']) is int and process['pid'] > 0
              and executable['path'] == expected_path
              and executable['sha256'] == expected_sha
              and process['argv'][:len(argv_prefix) + 1] == [executable['path'], *argv_prefix]
              and isinstance(process['birth']['bootId'], str) and bool(process['birth']['bootId'])
              and str(process['birth']['startTimeTicks']).isdigit()
              and process['readyListeners'] and all(listener['host'] == '127.0.0.1'
                 and listener['ownerPid'] == process['pid'] and type(listener['port']) is int
                 and 0 < listener['port'] < 65536 for listener in process['readyListeners'])
              and process['exit'] == {'code': 0, 'signal': None}
              and process['reaped'] is True and process['portsClosed'] is True
              and process['remainingOwnedIdentities'] == [],
              'ENTRY_CHANNEL_PROCESS', 'owned production lifecycle is incomplete')
    preflight = d.read(artifact(receipt['networkPreflight']))
    cleanup = d.read(artifact(receipt['containerCleanup']))
    tests = preflight['negativeControls']
    d.require(isinstance(process.get('networkNamespace'), str)
              and process['networkNamespace'].startswith('net:[')
              and preflight['networkNamespace'] == process['networkNamespace']
              and isinstance(process.get('pidNamespace'), str) and process['pidNamespace'].startswith('pid:[')
              and preflight['interfaces'] == [[1, 'lo']] and preflight['loopbackConnect'] == 'PASS'
              and len(tests) == 4 and all(test['blocked'] is True for test in tests)
              and {(test['family'], test['kind']) for test in tests} == {(2, 1), (2, 2), (10, 1), (10, 2)}
              and cleanup['ownedContainerStillRunning'] == '',
              'ENTRY_CHANNEL_ISOLATION', 'network blocking or owned container cleanup is unproven')
    if 'containerInspection' in receipt:
        inspections = d.read(artifact(receipt['containerInspection']))
        d.require(len(inspections) == 1, 'ENTRY_CHANNEL_ISOLATION', 'one owned container inspection required')
        inspection = inspections[0]
        d.require(inspection['Id'] == cleanup.get('containerId')
                  and inspection['Name'] == '/' + cleanup['name']
                  and inspection['Config']['Hostname'] == preflight.get('hostname')
                  and preflight.get('pidNamespace') == process['pidNamespace']
                  and inspection['HostConfig']['NetworkMode'] == 'none'
                  and inspection['State']['Running'] is True and inspection['State']['Pid'] > 0,
                  'ENTRY_CHANNEL_ISOLATION', 'preflight, process and cleanup belong to different containers')
    elif deployment is None:
        missing.append('source-bound-owned-container-cleanup')
    files = receipt['afterReapFiles']
    d.require(files and all(artifact(ref).stat().st_size == ref['bytes'] for ref in files),
              'ENTRY_CHANNEL_DURABLE', 'post-reap business bytes are missing')
    if 'preservation' in receipt:
        preserved = d.read(artifact(receipt['preservation']))
        preserver = preserved['preserver']
        d.require(preserved.get('schemaVersion') == 1
                  and preserved.get('method') == 'rename actual stopped cur directory'
                  and preserved.get('processObservation') == receipt['observationTrace']
                  and tracked.get(preserver['path']) == preserver['sha256'] == sha(Path(preserver['path'])),
                  'ENTRY_CHANNEL_DURABLE', 'preservation is not bound to the observed process and registered producer')
        original_root, preserved_root = Path(preserved['originalRoot']), Path(preserved['preservedRoot'])
        d.require(original_root.is_absolute() and original_root.name == 'cur' and '..' not in original_root.parts
                  and preserved_root.is_absolute() and preserved_root.resolve() == preserved_root,
                  'ENTRY_CHANNEL_DURABLE', 'invalid original or preserved business root')
        by_path = {row['path']: row for row in preserved['files']}
        d.require(len(by_path) == len(preserved['files']), 'ENTRY_CHANNEL_DURABLE', 'duplicate preserved business file')
        for row in files:
            path = artifact(row)
            original = by_path.get(str(path))
            info = path.stat()
            d.require(original is not None and path.is_relative_to(preserved_root)
                      and original['originalPath'] == str(original_root / path.relative_to(preserved_root))
                      and row.get('originalPath') == original['originalPath']
                      and original['relativePath'] == str(path.relative_to(preserved_root))
                      and original['sha256'] == row['sha256'] and original['bytes'] == row['bytes']
                      and original['device'] == info.st_dev and original['inode'] == info.st_ino,
                      'ENTRY_CHANNEL_DURABLE', 'preserved file is not the same original business inode and bytes')
    records = []
    if 'transportTrace' in receipt:
        path = artifact(receipt['transportTrace'])
        records = [json.loads(line) for line in path.read_text().splitlines()]
    else:
        missing.append('production-inbound-and-outbound-response-originals')
    return {'receipt': receipt, 'transportRecords': records, 'afterReapFiles': files,
            'originals': originals, 'missing': missing}


def channel_v2_preserved(d, raw, process_evidence):
    """Use the stopped directory's actual files, retaining its runtime names."""
    receipt = process_evidence['receipt']
    preserved = d.read(channel_artifact(d, receipt['preservation']))
    layout_ref = {'path': raw['layout']['path'], 'sha256': raw['layout']['sha256']}
    layout = d.read(channel_artifact(d, layout_ref))
    d.require(raw['layout'] == dict(layout, **layout_ref)
              and preserved['layout'] == layout_ref
              and preserved['originalRoot'] == layout['root']
              and set(preserved['actualClosedPorts']) == {layout['mockPort'], layout['controlPort']},
              'ENTRY_CHANNEL_LAYOUT', 'capture and preserved directory use different layouts')
    original = Path(layout['root'])
    expected_runtime = {'node': 'v22.22.0', 'osHome': str(original / 'os-home'),
                        'pilotHome': str(original / 'home'), 'home': str(original / 'home'),
                        'project': str(original / 'project'), 'defaultProject': str(original / 'home'),
                        'cwd': str(original / 'project'), 'tmp': str(original / 'tmp'),
                        'mockPort': layout['mockPort'], 'controlPort': layout['controlPort']}
    d.require(raw['runtime'] == expected_runtime
              and receipt['originalBusinessRoots'] == {key: expected_runtime[key] for key in ('home', 'project')}
              and receipt['process']['cwd'] == expected_runtime['cwd']
              and {row['port'] for row in receipt['process']['readyListeners']} == {layout['controlPort']},
              'ENTRY_CHANNEL_LAYOUT', 'actual process home/project/cwd/listener differs')
    root = Path(preserved['preservedRoot'])
    d.require(root.is_dir() and not root.is_symlink() and root.resolve() == root
              and root.is_relative_to(d.ROOT / '_tmp'),
              'ENTRY_CHANNEL_DURABLE', 'preserved root is not an owned actual directory')
    actual = set()
    def walk_error(error):
        raise error
    for name in ('home', 'project'):
        folder = root / name
        d.require(folder.is_dir() and not folder.is_symlink(),
                  'ENTRY_CHANNEL_DURABLE', 'missing ordinary business directory')
        for directory, dirs, names in os.walk(folder, followlinks=False, onerror=walk_error):
            for name_ in dirs + names:
                path = Path(directory) / name_
                d.require(not path.is_symlink() and (path.is_dir() or path.is_file()),
                          'ENTRY_CHANNEL_DURABLE', 'aliased or non-regular business entry')
            actual.update(str(Path(directory) / name_) for name_ in names)
    files = process_evidence['afterReapFiles']
    d.require(len(files) == len(actual) and {row['path'] for row in files} == actual,
              'ENTRY_CHANNEL_DURABLE', 'receipt does not enumerate every stopped business file')
    contents = {}
    for row in files:
        path = channel_artifact(d, row)
        runtime_path = str(original / path.relative_to(root))
        d.require(row['originalPath'] == runtime_path and runtime_path not in contents,
                  'ENTRY_CHANNEL_DURABLE', 'duplicate or different original business path')
        contents[runtime_path] = path.read_bytes()
    return root, contents


def verify_channel_ts_sources(d, baseline):
    """Reject source aliases and extra resolution candidates, including ignored files."""
    roots = ('src', 'packages/sdk/src')
    fixed = ('package.json', 'pnpm-lock.yaml', 'tsconfig.json')
    tree = d.subprocess.check_output(['git', '-C', str(baseline), 'ls-tree', '-r', '-z',
                                     d.RETIREMENT_BASELINE_REVISION, '--', *roots, *fixed], timeout=10)
    expected = {}
    for entry in tree.split(b'\0'):
        if not entry:
            continue
        metadata, name = entry.split(b'\t', 1)
        mode, kind, identity = metadata.split()
        d.require(mode in (b'100644', b'100755') and kind == b'blob',
                  'ENTRY_CHANNEL_SOURCE', 'unsupported fixed TS source alias')
        expected[name.decode()] = identity.decode()
    actual = set()
    def walk_error(error):
        raise error
    for name in roots:
        folder = baseline / name
        d.require(folder.is_dir() and folder.resolve() == folder,
                  'ENTRY_CHANNEL_SOURCE', 'fixed TS source root is missing or aliased')
        for directory, dirs, names in os.walk(folder, followlinks=False, onerror=walk_error):
            for name_ in dirs + names:
                path = Path(directory) / name_
                d.require(not path.is_symlink() and (path.is_dir() or path.is_file()),
                          'ENTRY_CHANNEL_SOURCE', 'fixed TS source contains an alias or non-regular entry')
            actual.update(str((Path(directory) / name_).relative_to(baseline)) for name_ in names)
    actual.update(name for name in fixed if (baseline / name).exists())
    d.require(actual == set(expected), 'ENTRY_CHANNEL_SOURCE', 'fixed TS source has extra or missing files')
    for name, identity in expected.items():
        path = baseline / name
        d.require(path.resolve() == path, 'ENTRY_CHANNEL_SOURCE', 'fixed TS file is aliased')
        content = path.read_bytes()
        obj = b'blob ' + str(len(content)).encode() + b'\0' + content
        actual_hash = hashlib.sha1(obj).hexdigest() if len(identity) == 40 else hashlib.sha256(obj).hexdigest()
        d.require(actual_hash == identity, 'ENTRY_CHANNEL_SOURCE', 'fixed TS source bytes differ: ' + name)


def channel_v2_comparison(d, native, baseline, row, raw, process_evidence):
    """Authenticate both real processes before comparing the eight-v2 scope."""
    adapter = _load('retirement_channel_v2', _apparatus / 'retirement_channel_v2.py')
    _verify_cli_imports(_apparatus)
    tracked = {str(d.ROOT / item['path']): item['sha256'] for item in native.source['files']}
    fixture = d.ROOT / 'src/pilotdeck-gateway/tests/fixtures/native-channel-production'
    bins = {item['path']: item['sha256'] for item in d.read(native.manifest)}
    originals, missing = [], list(process_evidence['missing'])
    d.require(raw['channel'] == row['id'].partition(':')[2],
              'ENTRY_CHANNEL_PROFILE', 'v2 raw channel differs from its inventory entry')

    def source(path, digest):
        d.require(tracked.get(str(path)) == digest == sha(path),
                  'ENTRY_CHANNEL_PRODUCER', 'v2 source is not the authenticated build input')

    source(d.ROOT / 'scripts/gateway-parity/retirement_channel_v2.py',
           hashlib.sha256(adapter._apparatus_source).hexdigest())

    def header(value, engine):
        d.require(value.get('schemaVersion') == 2 and value.get('profile') == 'native-eight-v2'
                  and value.get('engine') == engine and value.get('channel') == raw['channel']
                  and value.get('sourceManifestSha256') == native.source_sha
                  and value.get('gatewaySha256') == bins['pilotdeck-gateway']
                  and value.get('sidecarSha256') == bins['pilotdeck-agent-loop-sidecar']
                  and value.get('fixedTS') == d.RETIREMENT_BASELINE_REVISION,
                  'ENTRY_CHANNEL_SOURCE', 'v2 capture belongs to a different source/binary/profile')
        source(fixture / 'eight.mjs', value['fixtureSha256'])
        source(fixture / 'reference-round-context.mjs', value['contextHelperSha256'])

    header(raw, 'native')
    if missing or 'preservation' not in process_evidence['receipt']:
        return {'missing': [*missing, 'v2-stopped-layout-and-originals'], 'originals': originals}
    native_root, native_files = channel_v2_preserved(d, raw, process_evidence)
    proc = process_evidence['receipt']['process']
    d.require(raw['actualExecutable'] == proc['executable'] and proc['argv'] == [
        proc['executable']['path'], '--native', '--home', raw['runtime']['home'],
        '--project', raw['runtime']['home'], '--listen', '127.0.0.1:' + str(raw['runtime']['controlPort']),
        '--token-file', str(Path(raw['runtime']['home']) / 'token'),
        '--sidecar', str(native.manifest.parent / 'pilotdeck-agent-loop-sidecar'), '--drain-ms', '2000'],
        'ENTRY_CHANNEL_PROCESS', 'v2 native argv or executable differs')
    observed = adapter.observe(raw, raw['channel'], process_evidence['transportRecords'], native_files)
    if 'reference' not in row:
        return {'observations': observed, 'missing': ['fixed-ts-v2-reference'], 'originals': originals}
    reference = row['reference']
    ts_raw = d.read(channel_artifact(d, reference['rawEvidence']))
    originals.append(reference['rawEvidence'])
    if ts_raw.get('status') == 'BLOCKED':
        d.require(ts_raw.get('schemaVersion') == 2 and ts_raw.get('profile') == 'native-eight-v2'
                  and ts_raw.get('engine') == 'ts' and ts_raw.get('channel') == raw['channel']
                  and ts_raw.get('fixedTS') == d.RETIREMENT_BASELINE_REVISION
                  and ts_raw.get('formalAcceptance') is False
                  and isinstance(ts_raw.get('prerequisiteFailures'), list) and ts_raw['prerequisiteFailures'],
                  'ENTRY_CHANNEL_REFERENCE', 'invalid TS prerequisite failure record')
        return {'observations': observed, 'missing': ['fixed-ts-v2-prerequisites'],
                'referenceBlockers': ts_raw['prerequisiteFailures'], 'originals': originals}
    header(ts_raw, 'ts')
    verify_channel_ts_sources(d, baseline)
    d.require(ts_raw['runtime'] == raw['runtime'] and ts_raw['layout'] == raw['layout'],
              'ENTRY_CHANNEL_LAYOUT', 'TS/native must run at the same original paths and ports')
    preload = ts_raw['tsPreload']
    d.require(preload['path'] == str(fixture / 'media-ts-loopback.mjs'),
              'ENTRY_CHANNEL_PRODUCER', 'unexpected TS preload')
    source(Path(preload['path']), preload['sha256'])
    for key, path in [('tsEntrypoint', baseline / 'src/cli/pilotdeck.ts'),
                      ('tsLoader', baseline / 'node_modules/tsx/dist/loader.mjs')]:
        d.require(ts_raw[key] == {'path': str(path), 'sha256': sha(path)},
                  'ENTRY_CHANNEL_SOURCE', 'TS entry or loader differs from the reference capture')
    sdk = {'telegram': 'grammy', 'discord': 'discord.js', 'slack': '@slack/bolt', 'dingtalk': 'dingtalk-stream'}
    roots = ('tsx', 'ws', '@pilotdeck/sdk', '@larksuiteoapi/node-sdk', 'weixin-ilink', 'undici')
    if raw['channel'] in sdk:
        roots += (sdk[raw['channel']],)
    dependencies = common.baseline_dependency_hashes(baseline, roots=roots)
    installed = (baseline / 'node_modules').resolve(strict=True)
    d.require(installed.is_relative_to(baseline)
              and (baseline / 'node_modules/tsx/dist/loader.mjs').resolve(strict=True).is_relative_to(installed)
              and reference['baseline'] == {'path': str(baseline), 'revision': d.RETIREMENT_BASELINE_REVISION}
              and reference['baselineDependencies'] == {
                  'path': str(installed), 'sha256': common.tree_digest(installed), 'packages': dependencies},
              'ENTRY_CHANNEL_SOURCE', 'TS dependency graph changed or omits linked package bytes')
    ts_process = verify_owned_process(d, native, reference['processReceipt'], reference['rawEvidence'],
                                      raw['channel'], executable_role='ts-node', node_ref=reference['node'],
                                      baseline=baseline, preload_refs=(preload,))
    originals.extend(ts_process['originals'])
    missing.extend(ts_process['missing'])
    d.require(ts_raw['actualExecutable'] == ts_process['receipt']['process']['executable']
              and ts_process['receipt']['process']['argv'] == [reference['node']['path'], '--import',
                  preload['path'], '--import', ts_raw['tsLoader']['path'], ts_raw['tsEntrypoint']['path'], 'server'],
              'ENTRY_CHANNEL_PROCESS', 'v2 TS argv or executable differs')
    if missing or 'preservation' not in ts_process['receipt']:
        return {'observations': observed, 'missing': [*missing, 'fixed-ts-v2-stopped-originals'], 'originals': originals}
    ts_root, ts_files = channel_v2_preserved(d, ts_raw, ts_process)
    comparison = adapter.compare_pair(ts_raw, raw, ts_process['transportRecords'],
                                      process_evidence['transportRecords'], ts_files, native_files,
                                      ts_root=ts_root, native_root=native_root)
    d.require(comparison['equal'], 'ENTRY_CHANNEL_BEHAVIOR',
              'TS/native eight-v2 business differs: ' + str(comparison['differentFields']))
    return {'observations': observed, 'comparison': comparison, 'missing': missing, 'originals': originals}


def channel_results(d, native, entry_root, baseline=None):
    """Consume the 19 + 13 original IDs; incomplete production faces stay explicit."""
    baseline = baseline or d.BASELINE
    expected = [row['id'] for row in d.read(d.SCRIPT / 'fixtures/retirement-entry-inventory.json')['entries']
                if row['id'].startswith(('gateway.channel:', 'gateway.channel-http:'))]
    d.require(len(expected) == len(set(expected)) == 32, 'ENTRY_CHANNEL_INVENTORY', 'original 19 + 13 IDs required')
    result = {key: {'status': 'BLOCKED', 'covered': [], 'missing': ['same-head-production-capture'],
                    'originals': []} for key in expected}
    if entry_root is None or not (entry_root / 'channels.json').is_file():
        return result
    index = d.read(entry_root / 'channels.json')
    rows = index['entries']
    ids = [row['id'] for row in rows]
    d.require(index.get('schemaVersion') == 1 and len(ids) == len(set(ids)) and set(ids) <= result.keys(),
              'ENTRY_CHANNEL_INVENTORY', 'duplicate, unknown, or unversioned channel evidence')
    fixed_bins = {row['path']: row['sha256'] for row in d.read(native.manifest)}
    tracked = {str(d.ROOT / row['path']): row['sha256'] for row in native.source['files']}

    def artifact(ref):
        path = Path(ref['path'])
        d.require(path.is_absolute() and path.is_file() and not path.is_symlink() and path.resolve() == path
                  and path.is_relative_to(d.ROOT / '_tmp') and sha(path) == ref['sha256'],
                  'ENTRY_CHANNEL_RAW', 'missing, aliased, or changed channel raw artifact')
        return path

    for row in rows:
        key = row['id']
        cell = {'status': 'BLOCKED', 'covered': [], 'missing': [], 'originals': [], 'observations': None}
        result[key] = cell
        try:
            raw_path = artifact(row['rawEvidence'])
            raw = d.read(raw_path)
            cell['originals'].append({'path': str(raw_path), 'sha256': sha(raw_path)})
            source_ok = (raw.get('sourceManifestSha256') == native.source_sha
                         and raw.get('gatewaySha256') == fixed_bins['pilotdeck-gateway']
                         and raw.get('sidecarSha256') == fixed_bins['pilotdeck-agent-loop-sidecar']
                         and raw.get('fixedTS') == d.RETIREMENT_BASELINE_REVISION)
            if not source_ok:
                cell['missing'].append('matching-source-binary-baseline-generation')
            else:
                cell['covered'].append('same-build-raw-header')
            if key.startswith('gateway.channel:') and row.get('profile') == 'native-eight-v1':
                channel = key.partition(':')[2]
                d.require(channel in EIGHT_CHANNELS, 'ENTRY_CHANNEL_PROFILE', 'eight-channel profile assigned outside its domain')
                try:
                    cell['observations'] = channel_basic_observation(raw, channel)
                    cell['covered'].extend(['basic-text-model-outbound', 'public-reload', 'reported-process-stop'])
                except (ValueError, KeyError, TypeError, IndexError) as error:
                    cell.update(status='FAIL', rule=str(error))
            elif key.startswith('gateway.channel:') and row.get('profile') == 'native-eight-v2':
                d.require(key.partition(':')[2] in EIGHT_CHANNELS,
                          'ENTRY_CHANNEL_PROFILE', 'eight-channel profile assigned outside its domain')
            elif key.startswith('gateway.channel-http:'):
                probes = raw.get('http', [])
                retained = [probe for probe in probes if 'requestHex' in probe and 'responseHex' in probe]
                cell['observations'] = {'wireRequests': [http_frame(probe)[:3] for probe in retained]}
                cell['missing'].append('registered-route-contract-and-all-applicable-configurations')
            else:
                cell['missing'].append('registered-producer-profile-business-adapter')
            if 'processReceipt' not in row:
                cell['missing'].append('owned-process-and-listener-reap-receipt')
            else:
                identity = key.partition(':')[2]
                adapter = identity if key.startswith('gateway.channel:') else identity.partition(':')[0]
                adapter = {'api-server': 'apiServer', 'wecom-callback': 'wecomCallback'}.get(adapter, adapter)
                process_evidence = verify_owned_process(d, native, row['processReceipt'], row['rawEvidence'], adapter)
                receipt = process_evidence['receipt']
                cell['originals'].extend(process_evidence['originals'])
                cell['missing'].extend(process_evidence['missing'])
                cell['covered'].append('source-bound-owned-process-network-and-after-reap-files')
            identity = key.partition(':')[2]
            channel = identity if key.startswith('gateway.channel:') else identity.partition(':')[0]
            channel = {'api-server': 'apiServer', 'wecom-callback': 'wecomCallback'}.get(channel, channel)
            cell['applicability'] = channel_applicability(d, baseline, channel)
            cell['covered'].append('fixed-ts-source-applicability')
            for face in ('attachment', 'channelInteraction'):
                info = cell['applicability'][face]
                cell['missing'].extend(f'{face}:{kind}:production-scenario' for kind in info['kinds'])
            if 'processReceipt' in row and 'transportTrace' in receipt:
                trace_path = artifact(receipt['transportTrace'])
                records = [json.loads(line) for line in trace_path.read_text().splitlines()]
                cell['originals'].append({'path': str(trace_path), 'sha256': sha(trace_path)})
                if row.get('profile') == 'native-eight-v1':
                    cell['transportObservations'] = channel_transport_observation(raw, channel, records)
                    cell['covered'].append('actual-basic-inbound-model-outbound-response-bytes')
                elif key.startswith('gateway.channel:') and row.get('profile') == 'native-eight-v2':
                    paired = channel_v2_comparison(d, native, baseline, row, raw, process_evidence)
                    cell['missing'].extend(paired['missing'])
                    cell['originals'].extend(paired['originals'])
                    if 'referenceBlockers' in paired:
                        cell['referenceBlockers'] = paired['referenceBlockers']
                    if 'observations' in paired:
                        cell['observations'] = paired['observations']
                        cell['covered'].append('v2-current-turn-interaction-reload-and-durable-originals')
                    if 'comparison' in paired:
                        cell['comparison'] = paired['comparison']
                        cell['covered'].append('fixed-ts-v2-business-contract-comparison')
                else:
                    cell['missing'].append('registered-profile-transport-contract-adapter')
            else:
                cell['missing'].append('production-inbound-and-outbound-response-originals')
            if 'comparison' not in cell:
                cell['missing'].append('fixed-ts-business-contract-comparison')
        except (d.EvidenceError, OSError, ValueError, KeyError, TypeError, IndexError, AttributeError) as error:
            cell.update(status='FAIL', rule=getattr(error, 'rule', 'ENTRY_CHANNEL_EVIDENCE'), detail=str(error))
    return result


def production_results(d, native, baseline, entry_root):
    """Require a single source/candidate identity and actual isolated product exits."""
    if not any((entry_root / f'{mode}-{engine}').is_dir()
               for mode in ('http', 'cron', 'alwayson') for engine in ('ts', 'native')):
        return {}
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
            port = provenance.get('runtime', {}).get('gatewayPort', 28479 if engine == 'native' else 28478)
            d.require(type(port) is int and 28000 <= port < 29000, 'ENTRY_PRODUCTION_SOURCE', 'invalid isolated listener')
            listener = ['server', '--host', '127.0.0.1', '--port', str(port)]
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
    channels = channel_results(d, native, product_capture, baseline)
    public = _load('retirement_public_http', _apparatus / 'retirement_public_http.py')
    _verify_cli_imports(_apparatus)
    public_http = public.report(
        d, native, baseline, product_capture, rounds)
    result = []
    for expected in inventory:
        key = expected['id']
        group, _, identity = key.partition(':')
        state = 'BLOCKED'
        pointers = []
        if key in public_http:
            cell = public_http[key]
            state, detail, pointers = cell['status'], cell['detail'], cell['originals']
        elif group == 'gateway.method':
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
                runtime = d.read(folder / 'provenance.json').get('runtime', {})
                port = runtime.get('gatewayPort', 28478 if engine == 'ts' else 28479)
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
                    profiles.append(valid_http(record[0], identity, folder, runtime.get('project')) if len(record) == 1 else False)
            if any(profile is False for profile in profiles):
                state = 'FAIL'
            elif all(profile is True for profile in profiles):
                state = 'BLOCKED'  # Successful business invariants alone are not parity.
            elif all(isinstance(profile, tuple) for profile in profiles) and profiles[0] != profiles[1]:
                state = 'FAIL'
            try:
                full = standard_http.comparison(d, observed, mode, identity)
            except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
                full = {'status': 'FAIL', 'rule': 'ENTRY_FULL_COMPARISON_EVIDENCE', 'detail': str(error)}
            if state != 'FAIL' and all(profile is True for profile in profiles):
                state = full['status']
            detail = {'profileResults': profiles, 'scope': 'full production HTTP/model/durable comparison',
                      'fullComparison': full,
                      'missing': ('configured success/admission or Feishu callback' if mode == 'http' else
                                  'full-background-wire-model-disk-comparison') if state == 'BLOCKED' else None}
        else:
            cell = channels[key]
            state = cell['status']
            detail = {name: value for name, value in cell.items() if name not in ('status', 'originals')}
            pointers = cell['originals']
        result.append({**expected, 'status': state, 'detail': detail, 'originals': pointers})
    status = d.common.aggregate_status(*(row['status'] for row in result))[0]
    return {'status': status, 'expectedCount': 150, 'inventorySha256': manifest_sha,
            'counts': dict(Counter(row['status'] for row in result)), 'entries': result}
