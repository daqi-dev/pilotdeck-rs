#!/usr/bin/env python3
"""Fail-closed gateway ruler: complete recordings, independent oracles, three surfaces."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import sys
import tempfile

# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# Load apparatus modules by explicit path, never sys.path: a same-named package
# or extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing common.py is what this block defends.  -B only
# stops cache writes, so redirect the bytecode prefix too (fix-r5).
import importlib.util, sys, tempfile
from pathlib import Path

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before loading anything and print the command.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {Path(__file__).resolve()}')

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        if not isinstance(getattr(existing, '_apparatus_source', None), bytes):
            raise RuntimeError(f'#166 unverified apparatus module cache: {path}')
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        source = path.read_bytes()
        exec(compile(source, str(path), 'exec'), module.__dict__)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    module._apparatus_source = source
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module



def _verify_cli_imports():
    # This trust root uses only stdlib + Git, never a helper from disk common:
    # that module's verifier can itself be the active negative-control mutant.
    import subprocess
    from types import ModuleType
    root = _apparatus.parent.parent
    head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', '--verify', 'HEAD'],
                                   timeout=30).decode().strip()
    source = subprocess.check_output(['git', '-C', str(root), 'show',
                                      f'{head}:scripts/gateway-parity/common.py'], timeout=30)
    trusted = ModuleType('pristine_cli_verifier')
    trusted.__file__ = str(_apparatus / 'common.py')
    exec(compile(source, trusted.__file__, 'exec'), trusted.__dict__)
    trusted.verify_loaded_apparatus(head)


_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

DEFAULT_OUT = common.DEFAULT_OUT
READ_METHODS = common.READ_METHODS
SCENARIOS = common.SCENARIOS
save = common.save

UUID = re.compile(r'\b[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\b', re.I)


class Normalizer:
    def __init__(self):
        self.ids = {}
        self.entities = {}
        self.generations = {}
        self.generation_owners = {}

    def value(self, value, path=()):
        if isinstance(value, dict):
            result = {}
            for key, item in sorted(value.items()):
                # Binding generation is a server-generated connection epoch.
                if key == 'generation' and 'connectionId' in value:
                    connection = value['connectionId']
                    if connection in self.generations and self.generations[connection] != item:
                        raise ValueError('same connectionId has conflicting binding generations')
                    if item in self.generation_owners and self.generation_owners[item] != connection:
                        raise ValueError('distinct connectionIds share a binding generation')
                    self.generation_owners[item] = connection
                    self.generations[connection] = item
                    item = f'<generation:{self.value(connection)}>'
                # Explicit protocol/transcript wall clock metadata. Business
                # schedule fields (runAt, nextRunAt), budgets, seq remain exact.
                elif self.clock_field(value, key) and isinstance(item, (str, int, float)):
                    item = '<wall-clock>'
                result[key] = self.value(item, path + (key,))
            return result
        if isinstance(value, list):
            return [self.value(item, path + ('[]',)) for item in value]
        if isinstance(value, str):
            return UUID.sub(lambda match: self.ids.get(match.group().lower(), match.group()), value)
        return value

    def bind(self, value, key):
        """Only entity definitions assign identities; reference/list order never does."""
        if not isinstance(value, str):
            return
        match = UUID.search(value)
        if not match:
            return
        identity = match.group().lower()
        label = '<uuid:' + hashlib.sha256(json.dumps(key, sort_keys=True, ensure_ascii=False).encode()).hexdigest() + '>'
        if key in self.entities and self.entities[key] != identity:
            raise ValueError(f'ambiguous stable entity key: {key}')
        if identity in self.ids and self.ids[identity] != label:
            raise ValueError(f'UUID has conflicting entity definitions: {key}')
        self.entities[key] = identity
        self.ids[identity] = label

    def index(self, value):
        if isinstance(value, list):
            for item in value:
                self.index(item)
        elif isinstance(value, dict):
            if {'entryId', 'turnId', 'sequence', 'type'} <= value.keys():
                self.bind(value['entryId'], ('entry', value['turnId'], value['sequence']))
                if value['type'].endswith('_started') and 'operationId' in value:
                    self.bind(value.get('operationId'), ('operation', value['turnId'], value['sequence']))
            if {'taskId', 'projectKey', 'originSessionKey', 'message', 'schedule'} <= value.keys():
                self.bind(value['taskId'], ('task', value['projectKey'], value['originSessionKey'], value['message'],
                                            json.dumps(value['schedule'], sort_keys=True)))
            if value.get('type') in ('permission_request', 'elicitation_request') and 'runId' in value:
                self.bind(value.get('requestId'), ('pending', value['type'], value['runId'], value.get('toolCallId'), value.get('toolName')))
            if {'turnId', 'order', 'id', 'version'} <= value.keys():
                self.bind(value['id'], ('block', value['turnId'], value['order']))
            # Named entity definitions, including future list fixtures. Names
            # identify the entity independently of its place in a list.
            if 'id' in value and 'name' in value and value.get('type') not in ('request', 'response', 'event'):
                self.bind(value['id'], ('named', value.get('type'), value.get('scope'), value['name']))
            for item in value.values():
                self.index(item)

    @staticmethod
    def clock_field(parent, key):
        # Exact documented metadata containers, not single-name heuristics.
        project = 'name' in parent and 'projectKey' in parent and ('path' in parent or 'fullPath' in parent)
        cron = {'taskId', 'schedule', 'message', 'status'} <= parent.keys()
        transcript = {'entryId', 'sequence', 'turnId', 'type'} <= parent.keys()
        pending = 'requestId' in parent and {'ownerId', 'kind', 'toolCallId'} <= parent.keys()
        registry = {'runId', 'ownerPid', 'acceptedAt'} <= parent.keys()
        if key == 'createdAt':
            return transcript or cron or pending or (project and 'lastActivity' in parent)
        if key == 'updatedAt':
            return cron or {'firstPrompt', 'lastPrompt'} <= parent.keys() or 'isSnapshot' in parent or registry
        if key in ('startedAt', 'completedAt'):
            # Baseline agent/protocol/result.ts and tool/protocol/result.ts.
            # A business object carrying only an ID is never clock metadata.
            turn = ({'type', 'sessionId', 'turnId', 'stopReason', 'usage', 'turns',
                     'startedAt', 'completedAt'} <= parent.keys()
                    and parent['type'] in ('success', 'error', 'aborted', 'max_turns'))
            tool = ({'type', 'toolCallId', 'toolName', 'content', 'startedAt',
                     'completedAt'} <= parent.keys()
                    and parent['type'] in ('success', 'error'))
            return turn or tool
        if key in ('acceptedAt', 'ownerPid'):
            return registry
        if key == 'lastActivity':
            return project
        if key == 'mtimeMs':
            return ('path' in parent or 'relativePath' in parent) and ('kind' in parent or 'name' in parent or 'isDirectory' in parent)
        if key == 'mtime':
            return 'slug' in parent and 'scope' in parent
        return False


def fingerprint(request):
    return hashlib.sha256(json.dumps(dict(request, stream=True), ensure_ascii=False,
                                     separators=(',', ':')).encode()).hexdigest()


def transcript_projection(entries, normalizer):
    """Verify derived hashes, then recompute after clock normalization.

    Model request content itself is still compared in full. A changed raw
    fingerprint cannot be silently ignored even when both inputs are broken.
    """
    requests = {}
    result = []
    for row in entries:
        normalized = normalizer.value(row)
        if row.get('type') == 'model_request':
            requests[row.get('step')] = (row['request'], normalized['request'])
        if row.get('type') == 'model_stream_event' and row.get('event', {}).get('type') == 'request_started':
            original, canonical = requests[row.get('step')]
            if row['event'].get('requestFingerprint') != fingerprint(original):
                raise ValueError('raw requestFingerprint disagrees with recorded canonical request')
            normalized['event']['requestFingerprint'] = fingerprint(canonical)
        result.append(normalized)
    return result


def load(path, expected=dict):
    value = json.loads(path.read_text())
    if not isinstance(value, expected):
        raise ValueError(f'{path.name}: expected {expected}')
    return value


def wire_frame(row):
    data = bytes.fromhex(row['wireHex'])
    if len(data) < 2:
        raise ValueError('truncated wire header')
    opcode, length_code, offset = data[0] & 15, data[1] & 127, 2
    size = length_code
    if size in (126, 127):
        count = 2 if size == 126 else 8
        if len(data) < offset + count:
            raise ValueError('truncated extended length')
        size = int.from_bytes(data[offset:offset + count], 'big')
        offset += count
    masked = bool(data[1] & 128)
    mask = data[offset:offset + 4] if masked else b''
    offset += 4 if masked else 0
    if len(data) < offset:
        raise ValueError('truncated mask')
    payload = data[offset:]
    if len(payload) != size or opcode != row['wire']['opcode']:
        raise ValueError('wire bytes/header disagree')
    metadata = {'opcode': opcode, 'fin': bool(data[0] & 128), 'rsv': data[0] & 0x70,
                'masked': masked, 'lengthCode': length_code, 'payloadLength': size}
    if metadata != row['wire']:
        raise ValueError('wire FIN/RSV/mask/length metadata disagree')
    if masked:
        payload = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
    value = payload.decode('utf8', errors='replace')
    if row['direction'] == 'receive' and opcode == 1:
        value = json.loads(value)
    elif row['direction'] == 'receive' and opcode == 8:
        value = {'code': int.from_bytes(payload[:2], 'big') if len(payload) >= 2 else 1005,
                 'reason': payload[2:].decode('utf8', errors='replace')}
    if value != row.get('frame'):
        raise ValueError('wire bytes/decoded frame disagree')
    return data[:offset].hex()


def validate(path, local_methods=()):
    errors = []

    def fail(rule, detail):
        errors.append({'rule': rule, 'detail': detail})

    frames = {}
    try:
        run = load(path / 'run.json')
        if run.get('status') != 'DONE' or not run.get('audit', {}).get('allStopped'):
            fail('G0.round', 'unfinished round or cleanup failure')
        if set(run.get('scenarios', {})) != set(SCENARIOS):
            fail('G1.coverage', 'run scenario set must equal fixed manifest')
        if set(p.name for p in (path / 'scenarios').iterdir()) != set(SCENARIOS):
            fail('G1.coverage', 'scenario evidence directories missing/extra')
        for name in SCENARIOS:
            folder = path / 'scenarios' / name
            if not folder.is_dir():
                fail('G1.coverage', f'{name}: missing scenario')
                continue
            try:
                result, complete = load(folder / 'result.json'), load(folder / 'complete.json')
                if result.get('scenario') != name or complete.get('scenario') != name:
                    fail('G1.coverage', f'{name}: wrong scenario identity')
                if result.get('status') != 'PASS' or run.get('scenarios', {}).get(name, {}).get('exitCode') != 0:
                    fail('G3.oracle', f'{name}: driver failed its independent expectations')
                rows = []
                if name != 'http':
                    raw = (folder / 'frames.jsonl').read_text()
                    if not raw.endswith('\n'):
                        fail('G2.recording', f'{name}: truncated final line')
                    rows = [json.loads(line) for line in raw.splitlines()]
                    if any(not isinstance(row, dict) for row in rows):
                        raise ValueError('recorder rows must be objects')
                    if not rows:
                        fail('G2.recording', f'{name}: empty log')
                if complete.get('complete') is not True or complete.get('records') != len(rows):
                    fail('G2.recording', f'{name}: count/footer mismatch')
                connections = {row.get('connection') for row in rows}
                if name != 'http' and connections != set(range(1, complete.get('connections', 0) + 1)):
                    fail('G2.recording', f'{name}: connection count/identity mismatch')
                pending = {}
                for number, row in enumerate(rows, 1):
                    if type(row.get('seq')) is not int or row['seq'] != number:
                        fail('G2.recording', f'{name}: non-contiguous recorder seq at {number}')
                    if type(row.get('connection')) is not int or row['connection'] < 1 or row.get('direction') not in ('send', 'receive'):
                        fail('G2.recording', f'{name}: invalid row at {number}')
                    if 'raw' in row:
                        try:
                            parsed = json.loads(row['raw'])
                        except ValueError:
                            parsed = row['raw']  # auth deliberately sends invalid JSON.
                        if parsed != row.get('frame'):
                            fail('G2.recording', f'{name}: raw/parsed disagreement at {number}')
                    if 'wire' in row:
                        wire_frame(row)
                    elif 'raw' in row:
                        frame = row.get('frame')
                        if not isinstance(frame, dict):
                            if name != 'auth' or row['connection'] != 4 or frame != '{invalid json':
                                fail('G2.recording', f'{name}: unexpected non-JSON application frame')
                            continue
                        kind = frame.get('type')
                        if kind not in ('hello', 'hello_ok', 'request', 'response', 'event', 'notification'):
                            fail('G2.recording', f'{name}: unknown frame type')
                        if kind == 'hello_ok':
                            binding = frame.get('interactionBinding', {})
                            if frame.get('protocolVersion') != '1.2' or not isinstance(binding.get('connectionId'), str) or type(binding.get('generation')) is not int or binding['generation'] < 1:
                                fail('G3.oracle', f'{name}: invalid hello version / server binding')
                        key = (row['connection'], frame.get('id'))
                        if kind == 'request':
                            if row['direction'] != 'send' or not isinstance(frame.get('id'), str) or key in pending:
                                fail('G2.link', f'{name}: invalid/duplicate request id')
                            pending[key] = frame.get('method')
                        elif kind in ('response', 'event'):
                            if row['direction'] != 'receive' or key not in pending:
                                fail('G2.link', f'{name}: reply refers to unknown request')
                            elif kind == 'event' and pending[key] != 'submit_turn':
                                fail('G2.link', f'{name}: event belongs to non-stream request')
                            if kind == 'event' and (type(frame.get('final')) is not bool or not isinstance(frame.get('event'), dict)):
                                fail('G2.recording', f'{name}: malformed event/final')
                for conn in {row['connection'] for row in rows}:
                    last = [row for row in rows if row['connection'] == conn][-1]
                    if 'close' not in last and not last.get('transportEnd'):
                        fail('G2.recording', f'{name}: connection {conn} missing close/end')
                    if sum('close' in row or row.get('transportEnd', False) for row in rows if row['connection'] == conn) != 1:
                        fail('G2.recording', f'{name}: connection {conn} must have exactly one close/end')
                stream_seq = {}
                for row in rows:
                    frame = row.get('frame')
                    if isinstance(frame, dict) and frame.get('type') == 'event':
                        key = (row['connection'], frame.get('id'))
                        expected = stream_seq.get(key, 0)
                        if type(frame.get('seq')) is not int or frame['seq'] != expected:
                            fail('G2.event-sequence', f'{name}: stream seq gap/duplicate')
                        stream_seq[key] = expected + 1
                frames[name] = rows
            except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
                fail('G2.recording', f'{name}: {error}')
        # Oracles derived from raw observations, not just driver status flags.
        def received(name):
            return [row['frame'] for row in frames.get(name, []) if row.get('direction') == 'receive' and isinstance(row.get('frame'), dict)]

        auth = [row['close']['code'] for row in frames.get('auth', []) if 'close' in row]
        if auth != [4001, 4001, 4003, 4002, 1000]:
            fail('G3.oracle', 'auth close codes / valid hello missing')
        for name in ('turn', 'permission'):
            events = [frame for frame in received(name) if frame.get('type') == 'event']
            if not events or sum(frame.get('final') is True for frame in events) != 1:
                fail('G3.oracle', f'{name}: exactly one final required')
            elif events[-1].get('event', {}).get('type') != 'turn_completed':
                fail('G3.oracle', f'{name}: final turn_completed required')
            if name == 'turn' and 'GP_MOCK_OK' not in json.dumps(events):
                fail('G3.oracle', 'turn: fixed model text missing')
            if name == 'permission' and not any(frame.get('event', {}).get('type') == 'permission_request' for frame in events):
                fail('G3.oracle', 'permission: real pending prompt missing')
        reads = frames.get('readonly', [])
        methods = [row['frame']['method'] for row in reads if isinstance(row.get('frame'), dict) and row['frame'].get('type') == 'request']
        if methods != READ_METHODS:
            fail('G1.coverage', 'all ten readonly methods in fixed order required')
        responses = [frame for frame in received('readonly') if frame.get('type') == 'response']
        if len(responses) != 10 or any(frame.get('ok') is not True for frame in responses):
            fail('G3.oracle', 'all ten readonly responses must succeed')
        for name, required in [('reconnect', ['submit_turn', 'reconnect_interaction', 'active_turn_snapshot', 'elicitation_respond', 'abort_turn']),
                               ('permission', ['submit_turn', 'permission_decide']), ('writes', ['cron_create', 'skill_create', 'skill_write'])]:
            observed = [row['frame'].get('method') for row in frames.get(name, []) if isinstance(row.get('frame'), dict) and row['direction'] == 'send']
            if not all(method in observed for method in required):
                fail('G1.coverage', f'{name}: required methods missing')
        def response_map(name):
            rows = frames.get(name, [])
            requests = {row['frame']['id']: row['frame']['method'] for row in rows
                        if isinstance(row.get('frame'), dict) and row['frame'].get('type') == 'request'}
            result = {}
            for frame in received(name):
                if frame.get('type') == 'response':
                    method = requests.get(frame.get('id'))
                    if method is None or method in result or frame.get('ok') is not True:
                        fail('G3.oracle', f'{name}: unmatched/duplicate/failed response')
                    result[method] = frame.get('result', {})
            return result

        reconnect = response_map('reconnect')
        question_ids = [frame['event'].get('requestId') for frame in received('reconnect')
                        if frame.get('event', {}).get('type') == 'elicitation_request']
        replay = reconnect.get('reconnect_interaction', {})
        if not question_ids or replay.get('outcome') != 'reconnected' or not all(
                any(item.get('requestId') == value for item in replay.get('requests', [])) for value in question_ids):
            fail('G3.oracle', 'reconnect must replay the actual pending question')
        if reconnect.get('active_turn_snapshot', {}).get('active') is not True or reconnect.get('elicitation_respond', {}).get('delivered') is not False:
            fail('G3.oracle', 'reconnect active snapshot / baseline rejected answer missing')
        if response_map('permission').get('permission_decide', {}).get('delivered') is not True:
            fail('G3.oracle', 'permission decision must reach its pending owner')
        writes = response_map('writes')
        if not writes.get('cron_create', {}).get('task', {}).get('taskId') or not all(
                writes.get(method, {}).get('ok') is True for method in ('skill_create', 'skill_write')):
            fail('G3.oracle', 'cron/skill writes must succeed')
        read_results = response_map('readonly')
        if len(read_results) != len(READ_METHODS):
            fail('G3.oracle', 'readonly replies must match all ten request IDs')
        if read_results.get('project_file_read', {}).get('content') != 'gateway parity fixture\n':
            fail('G3.oracle', 'project_file_read must return the fixture')
        artifact = read_results.get('native_archive_artifact', {})
        if artifact.get('content') != base64.b64encode(b'gateway parity fixture artifact\n').decode() or artifact.get('encoding') != 'base64' or artifact.get('truncated') is not False:
            fail('G3.oracle', 'native artifact must succeed with the complete fixture bytes')
        if not read_results.get('supported_agents', {}).get('agents') or not read_results.get('model_catalog_list', {}).get('items'):
            fail('G3.oracle', 'agent / model catalog cannot be empty')
        if read_results.get('session_model_get', {}).get('effective', {}).get('model') != 'deterministic':
            fail('G3.oracle', 'session model must resolve the configured model')
        if not read_results.get('native_archive_entries', {}).get('entries') or read_results.get('native_archive_manifest', {}).get('entryCount', 0) < 1:
            fail('G3.oracle', 'native archive must contain the completed text turn')
        # Independent disk oracles prevent both-sides-the-same failed execution.
        if (path / 'project/note.txt').read_text() != 'gateway parity fixture\n' or (path / 'project/written.txt').read_text() != 'HTTP written fixture\n':
            fail('G3.oracle', 'fixture / HTTP durable write incorrect')
        transcripts = list((path / 'home').rglob('*gateway-parity-text*.jsonl'))
        if len(transcripts) != 1 or 'GP_MOCK_OK' not in transcripts[0].read_text():
            fail('G3.oracle', 'successful reply must persist in exactly one transcript')
        skills = path / 'project/.pilotdeck/skills/parity-fixture/SKILL.md'
        if not skills.is_file() or not skills.read_text().endswith('Written\n'):
            fail('G3.oracle', 'skill write must persist')
        tasks = list((path / 'home/cron').rglob('tasks.json'))
        if len(tasks) != 1 or 'GP_CRON' not in tasks[0].read_text():
            fail('G3.oracle', 'cron task must persist')
        http = load(path / 'scenarios/http/http.json', list)
        expected = [200, 200, 200, 200, 200, 401, 200, 200]
        if [row.get('status') for row in http] != expected:
            fail('G3.oracle', 'HTTP status or endpoint coverage incorrect')
        for row in http:
            body = b''.join(base64.b64decode(chunk['base64'], validate=True) for chunk in row['chunks']).decode('utf8')
            if body != row['body']:
                fail('G2.recording', 'HTTP raw chunk bytes / body disagree')
        if json.loads(http[0]['body']) != {'ok': True} or json.loads(http[1]['body']).get('token') != (path / 'home/server-token').read_text().strip():
            fail('G3.oracle', 'health / local token body incorrect')
        if not json.loads(http[2]['body']).get('projects') or json.loads(http[3]['body']).get('content') != 'gateway parity fixture\n':
            fail('G3.oracle', 'HTTP projects / file read fixture missing')
        for row in http[-2:]:
            chunks = row['chunks']
            if len(chunks) < 2 or chunks[-1]['elapsedMs'] - chunks[0]['elapsedMs'] < 300 or 'first' not in row['body'] or 'last' not in row['body']:
                fail('G3.oracle', 'stream buffered/truncated')
        quirks = load(path / 'scenarios/quirks/result.json').get('results', [])
        if [row.get('kind') for row in quirks] != ['rsv', 'continuation', 'client-close', 'unmasked'] or not all(row.get('ok') is True for row in quirks):
            fail('G3.oracle', 'four WS quirks must meet baseline oracles')
        raw = frames.get('quirks', [])
        for conn in (1, 2):
            if not any(row['connection'] == conn and isinstance(row.get('frame'), dict) and row['frame'].get('type') == 'hello_ok' for row in raw):
                fail('G3.oracle', 'RSV/continuation must preserve valid hello')
        if not any(row['connection'] == 4 and row.get('exceptionalShutdown') is True for row in raw):
            fail('G3.oracle', 'unmasked must trigger exceptional TS listener shutdown')
        if run.get('side') == 'rust':
            try:
                raw_upstream = (path / 'upstream.jsonl').read_text()
                if not raw_upstream or not raw_upstream.endswith('\n'):
                    raise ValueError('empty/truncated upstream recording')
                upstream = [json.loads(line) for line in raw_upstream.splitlines()]
                if any(not isinstance(row, dict) or ('frame' in row and not isinstance(row['frame'], dict)) for row in upstream):
                    raise ValueError('invalid upstream row')
            except (OSError, ValueError, TypeError) as error:
                fail('G2.observer', str(error))
                return frames, errors
            upstream_methods = {row.get('frame', {}).get('method') for row in upstream}
            expected = (set(READ_METHODS) - set(local_methods)) | {'submit_turn', 'permission_decide', 'cron_create'}
            if not expected.issubset(upstream_methods):
                fail('G2.observer', 'required passthrough requests missing from independently recorded upstream')
            if set(local_methods) & upstream_methods:
                fail('G7.no-forward', 'declared local method was forwarded to TS upstream')
            if any(row.get('seq') != index for index, row in enumerate(upstream, 1)):
                fail('G2.observer', 'upstream recorder seq must be contiguous')
    except (OSError, ValueError, KeyError, TypeError, IndexError, AttributeError) as error:
        fail('G0.round', str(error))
    return frames, errors


def disk(path, normalize):
    result = []
    for root in ('home', 'project'):
        for item in sorted((path / root).rglob('*')):
            relative = f'{root}/{item.relative_to(path / root).as_posix()}'
            # Operational diagnostics are retained raw, excluded from durable contract.
            if relative.startswith(('home/logs/', 'home/telemetry/')):
                continue
            if item.is_symlink():
                raise ValueError(f'unsupported symlink in isolated disk: {relative}')
            if item.is_dir():
                result.append({'path': normalize.value(relative), 'kind': 'directory'})
                continue
            data = item.read_bytes()
            if data.startswith(b'SQLite format 3'):
                connection = sqlite3.connect(f'file:{item}?mode=ro', uri=True)
                try:
                    content = list(connection.iterdump())
                finally:
                    connection.close()
            else:
                try:
                    text = data.decode('utf8')
                except UnicodeError:
                    content = {'base64': base64.b64encode(data).decode()}
                else:
                    if item.suffix == '.jsonl':
                        if text and not text.endswith('\n'):
                            raise ValueError(f'truncated durable jsonl: {relative}')
                        content = transcript_projection([json.loads(line) for line in text.splitlines()], normalize)
                    elif item.suffix == '.json':
                        content = json.loads(text)
                    else:
                        content = text
            result.append({'path': normalize.value(relative), 'kind': 'file',
                           'content': content if item.suffix == '.jsonl' else normalize.value(content)})
    if not result:
        raise ValueError('empty durable snapshot')
    return result


def entity_normalizer(path, frames):
    normalizer = Normalizer()
    for name in SCENARIOS:
        for row in frames.get(name, []):
            frame = row.get('frame')
            if isinstance(frame, dict):
                if frame.get('type') == 'hello_ok':
                    normalizer.bind(frame.get('interactionBinding', {}).get('connectionId'),
                                    ('connection', name, row['connection']))
                if frame.get('type') == 'request':
                    normalizer.bind(frame.get('id'), ('request', name, row['connection'], frame.get('method')))
                normalizer.index(frame)
    for root in ('home', 'project'):
        for item in sorted((path / root).rglob('*')):
            relative = item.relative_to(path).as_posix()
            if relative.startswith(('home/logs/', 'home/telemetry/')) or not item.is_file():
                continue
            if item.suffix == '.json':
                normalizer.index(load(item, (dict, list)))
            elif item.suffix == '.jsonl':
                for line in item.read_text().splitlines():
                    normalizer.index(json.loads(line))
    return normalizer


def projection(path, frames):
    normalizer = entity_normalizer(path, frames)
    projected = {}
    for name in SCENARIOS:
        # Preserve order per connection, including notifications; never reorder
        # interleaved event/response frames on a connection. Cross-connection
        # order is explicitly outside contract. Recorder seq is checked above.
        connections = {}
        for row in frames.get(name, []):
            semantic = {key: value for key, value in row.items()
                        if key not in ('seq', 'monotonicNs', 'connection', 'raw', 'wireHex')}
            if 'wire' in row:
                semantic['wireHeaderHex'] = wire_frame(row)
            connections.setdefault(str(row['connection']), []).append(normalizer.value(semantic))
        projected[name] = connections
    http = []
    for row in load(path / 'scenarios/http/http.json', list):
        headers = {key: val for key, val in row['headers'].items() if key not in
                   ('date', 'connection', 'keep-alive', 'transfer-encoding')}
        body = row['body']
        try:
            body = json.loads(body)
        except ValueError:
            pass
        http.append(normalizer.value({'path': row['path'], 'status': row['status'], 'headers': headers, 'body': body}))
    return {'frames': projected, 'http': http, 'disk': disk(path, normalizer)}


def compare(left, right, local_methods=()):
    failures = []
    recording = {}
    runs = {}
    for side, path in [('left', left), ('right', right)]:
        recording[side], errors = validate(path, local_methods)
        failures.extend({'side': side, **error} for error in errors)
        try:
            runs[side] = load(path / 'run.json')
        except (OSError, ValueError):
            pass
    if len(runs) == 2:
        for field in ('runtime', 'ports', 'baseline', 'baselineSourceSha256', 'baselineDistSha256'):
            if runs['left'].get(field) != runs['right'].get(field):
                failures.append({'rule': 'G0.paths', 'detail': f'{field} differs; no post-hoc path alignment allowed'})
    if not failures:
        try:
            a, b = projection(left, recording['left']), projection(right, recording['right'])
            for surface, rule in [('frames', 'G4.frames'), ('http', 'G5.http'), ('disk', 'G6.disk')]:
                if a[surface] != b[surface]:
                    failures.append({'rule': rule, 'left': a[surface], 'right': b[surface]})
        except (OSError, ValueError, KeyError, TypeError, IndexError, AttributeError, sqlite3.Error) as error:
            failures.append({'rule': 'G2.recording', 'detail': str(error)})
    return {'status': 'FAIL' if failures else 'PASS', 'failures': failures,
            'scenarios': len(SCENARIOS), 'left': str(left), 'right': str(right), 'localMethods': list(local_methods)}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('left', type=Path)
    ap.add_argument('right', type=Path)
    ap.add_argument('--out', type=Path)
    ap.add_argument('--local-method', choices=READ_METHODS, action='append', default=[],
                    help='A4: require successful matching response and absence in independent upstream recording')
    args = ap.parse_args()
    left = args.left if args.left.exists() else DEFAULT_OUT / args.left
    right = args.right if args.right.exists() else DEFAULT_OUT / args.right
    result = compare(left, right, args.local_method)
    if args.out:
        save(args.out, result)
    print(json.dumps({'status': result['status'], 'rules': [row['rule'] for row in result['failures']]}, ensure_ascii=False))
    return int(result['status'] != 'PASS')


if __name__ == '__main__':
    try:
        _verify_cli_imports()
    except (Exception, KeyboardInterrupt) as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        raise SystemExit(2)
    raise SystemExit(main())
