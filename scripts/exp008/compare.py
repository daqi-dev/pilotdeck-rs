#!/usr/bin/env python3
"""exp008 / B1 record-replay comparator for the Rust proxy.

Compares two recorded "sides" of the same driving script:

    <side>/
      http.jsonl       one {case, method, path, status, headers, body} per line
      handshake.jsonl  one such record for the upstream WS handshake (101)
      ws.jsonl         one {case, dir, opcode, raw} per WebSocket text frame, in order
      sse.jsonl        one {case, index, sseId, event, data} per SSE event, in order
      close.jsonl      one {case, sentCode, sentReason, observedCode, observedReason}
                       per WebSocket close handshake

`direct` is the TS ui/server (13002); `proxy` is the same server reached through
the Rust relay (13001). Both recordings are produced by `run.py` from the SAME
isolated home and the SAME project path (`specs/parity/normalization.md`: the two
sides must run the same output path).

Normalisation is restricted to explicit channel/event/field paths. Unknown
payloads and all unlisted fields are literal. Only session-activity revisions
with an independently recorded, validated pre-command activity snapshot are
relative; no first observed value may establish its own baseline.

ETags preserve strength and match literally for identical wire bodies. For
bodies differing only in allowed volatile fields, each ETag must validate its
own body. X-Projects-Revision is a wall clock only on successful GET /api/projects:
ASCII decimal, bounded by recorded request times, and proxy >= direct.

Everything else is compared structurally, with a precise location for every
difference:

    http[<case>] status | header.<name> | body.<path>
    handshake[<case>] status | header.<name>
    ws[<case>] #<index>
    sse[<case>] #<index>

Exit code: 0 = PASS, 1 = FAIL, 2 = usage error.

Positive controls (`--controls LEFT RIGHT`): inject one known difference at a
time into a copy of RIGHT and require the comparator to FAIL while naming the
exact location. Equivalence controls (valid evidence compared with itself) must PASS.
"""
import argparse
import base64
from datetime import datetime
import hashlib
import json
import math
import re
import shutil
import sys
import tempfile
from pathlib import Path

CHANNELS = ('http', 'handshake', 'ws', 'sse', 'close')
# HTTP-style channels match records by case name; the rest are ordered sequences.
HTTP_STYLE = ('http', 'handshake')

# Exact event/path policy. No wildcard paths or recursive key-name matching.
CLOCK_KEYS = {'timestamp', 'startedAt'}  # Used only by control mutation selection.
PROCESS_KEYS = {'pid'}
IDENTITY_KEYS = {'id', 'runId', 'sessionId', 'turnId', 'blockId', 'completedRunId'}
RELATIVE_KEYS = {'revision'}
FRAME_KINDS = {'status', 'stream_delta', 'text', 'complete'}
FRAME_IDENTITIES = {('id',), ('sessionId',), ('runId',)}
TIMELINE_IDENTITIES = {('timeline', 'id'), ('timeline', 'turnId'), ('blockId',)}
EVENT_IDENTITIES = {
    'pilotdeck-command': {('options', 'sessionId')},
    'session-activity': {('activity', 'sessionId'), ('activity', 'runId'),
                         ('activity', 'completedRunId')},
    'session-input-accepted': {('sessionId',), ('runId',)},
    'model-selection-changed': {('sessionId',), ('runId',)},
}
HEADER_EXCLUDE = {
    'date', 'content-length', 'connection', 'keep-alive', 'transfer-encoding',
    'x-pilotdeck-handler',
}
CLOCK_HEADER = 'x-projects-revision'
ASCII_REVISION = re.compile(r'[1-9][0-9]*', re.ASCII)
# projects.js uses max(Date.now(), previous + 1). Allow at most one second of
# counter lead, explicitly bounded rather than accepting any positive number.
REVISION_COUNTER_LEAD_MS = 1000


def projects_clock_route(record):
    return (record.get('method') == 'GET' and record.get('path') == '/api/projects'
            and record.get('status') == 200)


def revision_number(value):
    value = str(value)
    return int(value) if ASCII_REVISION.fullmatch(value) and len(value) <= 16 else None


def valid_clock_revision(record, value):
    number = revision_number(value)
    bounds = record.get('requestTimeMs')
    if number is None or not isinstance(bounds, list) or len(bounds) != 2:
        return False
    start, end = bounds
    return (all(isinstance(v, (int, float)) and not isinstance(v, bool)
                and 0 < v <= 2**53 and math.isfinite(v) for v in bounds)
            and 0 < start <= end and start <= number <= end + REVISION_COUNTER_LEAD_MS)


UUID_RE = re.compile(r'[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-'
                     r'[0-9a-fA-F]{4}-[0-9a-fA-F]{12}')


def valid_etag(value, body):
    """True when `value` is the Express etag of `body` (weak sha1 of the bytes)."""
    raw = body.encode('utf-8', 'replace')
    digest = base64.b64encode(hashlib.sha1(raw).digest()).decode('ascii').rstrip('=')
    return value in (f'"{len(raw):x}-{digest}"', f'W/"{len(raw):x}-{digest}"')


def normalise_header(name, value, body, normalizer, record=None):
    value = str(value)
    if name == 'etag' and valid_etag(value, body):
        return '<ETAG-WEAK>' if value.startswith('W/') else '<ETAG-STRONG>'
    if (name == CLOCK_HEADER and record is not None and projects_clock_route(record)
            and valid_clock_revision(record, value)):
        return '<REVISION>'
    return normalizer.text(value)


def activity_anchor(snapshot):
    """Derive the counter from a separately observed pre-command snapshot.

    The isolated B1 fixture has one user, so its latest activity is the global
    registry counter. A missing or malformed snapshot provides no exemption.
    """
    if not isinstance(snapshot, dict) or snapshot.get('type') != 'session-activity-snapshot':
        return None
    activities = snapshot.get('activities')
    if not isinstance(activities, list):
        return None
    revisions = []
    sessions = set()
    for item in activities:
        if not isinstance(item, dict):
            return None
        revision = item.get('revision')
        session = item.get('sessionId')
        if (type(revision) is not int or revision <= 0 or not isinstance(session, str)
                or not session or session in sessions or revision in revisions
                or item.get('processing') is not False):
            return None
        revisions.append(revision)
        sessions.add(session)
    return max(revisions, default=0)


def clock_value(value):
    if not isinstance(value, str):
        return False
    try:
        return datetime.fromisoformat(value.replace('Z', '+00:00')).tzinfo is not None
    except ValueError:
        return False


class Normalizer:
    """Per-side UUID references with explicit event and complete-path policy."""

    def __init__(self, paths=()):
        self.ids = {}
        self.activity_bases = {}
        self.errors = []
        self.paths = sorted([str(p) for p in paths if p], key=len, reverse=True)

    def ref(self, category, value):
        key = (category, str(value))
        if key not in self.ids:
            self.ids[key] = len(self.ids)
        return f'<{category}:{self.ids[key]}>'

    def text(self, value):
        for path in self.paths:
            value = value.replace(path, '<PATH>')
        return value

    def identity(self, value):
        if isinstance(value, str):
            return UUID_RE.sub(lambda match: self.ref('uuid', match.group(0)), self.text(value))
        return value

    def value(self, value, case=None, path=(), *, identities=frozenset(),
              volatile=frozenset(), relative_base=None):
        if path in identities:
            return self.identity(value)
        if path in volatile and (clock_value(value) or (path == ('pid',)
                                                       and type(value) is int and value > 0)):
            return '<VOLATILE>'
        if (path == ('activity', 'revision') and relative_base is not None
                and type(value) is int and value > relative_base):
            return value - relative_base
        if isinstance(value, dict):
            return {name: self.value(item, case, path + (name,), identities=identities,
                                     volatile=volatile, relative_base=relative_base)
                    for name, item in sorted(value.items())}
        if isinstance(value, list):
            return [self.value(item, case, path + (index,), identities=identities,
                               volatile=volatile, relative_base=relative_base)
                    for index, item in enumerate(value)]
        if isinstance(value, str):
            return self.text(value)
        return value


def load_jsonl(path):
    records = []
    if not Path(path).exists():
        return records
    for line in Path(path).read_text().splitlines():
        if line.strip():
            records.append(json.loads(line))
    return records


def write_jsonl(path, records):
    Path(path).write_text(''.join(json.dumps(item, ensure_ascii=False) + '\n' for item in records))


def parse_json(text):
    try:
        return json.loads(text)
    except Exception:
        return None


def http_key(record):
    return record['case']


def normalise_http(record, normalizer):
    body = record.get('body', '')
    headers = {k.lower(): v for k, v in record.get('headers', {}).items()
               if k.lower() not in HEADER_EXCLUDE}
    parsed = parse_json(body)
    health = (record.get('method') == 'GET' and record.get('path') == '/health'
              and record.get('status') == 200)
    normalized = normalizer.value(
        parsed, record['case'], identities={('instanceId',)} if health else set(),
        volatile={('timestamp',), ('startedAt',), ('pid',)} if health else set())
    return {'case': record['case'], 'method': record.get('method'),
            'path': record.get('path'), 'status': record.get('status'),
            'headers': {k: normalise_header(k, v, body, normalizer, record)
                        for k, v in sorted(headers.items())},
            '_wireBody': body, '_wireEtag': headers.get('etag'),
            '_etagValid': valid_etag(str(headers.get('etag', '')), body),
            '_clockRoute': projects_clock_route(record),
            '_revision': revision_number(headers.get(CLOCK_HEADER, '')),
            '_clockValid': valid_clock_revision(record, headers.get(CLOCK_HEADER, '')),
            'body': normalized if parsed is not None else normalizer.text(body)}


def normalise_ws(record, normalizer):
    raw = record.get('raw')
    case = record.get('case', 'ws')
    parsed = parse_json(raw) if isinstance(raw, str) else None
    identities, volatile, base = set(), set(), None
    if isinstance(parsed, dict) and record.get('opcode', 'text') == 'text':
        event = parsed.get('type')
        direction = record.get('dir', 'u2c')
        if direction == 'c2u' and event == 'pilotdeck-command':
            identities = EVENT_IDENTITIES[event]
            # Anchor is separate evidence, never the first revision under test.
            anchor = activity_anchor(record.get('activityBaseline'))
            options = parsed.get('options')
            session = options.get('sessionId') if isinstance(options, dict) else None
            if 'activityBaseline' in record and anchor is None:
                normalizer.errors.append({'location': f'ws[{case}] activityBaseline',
                                          'message': 'invalid or unsettled pre-command snapshot'})
            if anchor is not None and isinstance(session, str):
                normalizer.activity_bases[(case, session)] = anchor
        elif direction == 'u2c':
            identities = EVENT_IDENTITIES.get(event, set()) if isinstance(event, str) else set()
            kind = parsed.get('kind')
            if event is None and isinstance(kind, str) and kind in FRAME_KINDS:
                identities = FRAME_IDENTITIES
                volatile = {('timestamp',)}
                if parsed['kind'] in {'stream_delta', 'text'}:
                    identities = identities | TIMELINE_IDENTITIES
            if event == 'session-activity' and isinstance(parsed.get('activity'), dict):
                session = parsed['activity'].get('sessionId')
                base = normalizer.activity_bases.get((case, session)) if isinstance(session, str) else None
                revision = parsed['activity'].get('revision')
                if base is not None and (type(revision) is not int or revision <= base):
                    normalizer.errors.append({'location': f'ws[{case}] body.activity.revision',
                                              'message': 'activity revision must advance the snapshot',
                                              'revision': revision, 'baseline': base})
    body = normalizer.value(parsed, case, identities=identities, volatile=volatile,
                            relative_base=base) if parsed is not None else normalizer.text(str(raw))
    return {'case': case, 'opcode': record.get('opcode', 'text'),
            'dir': record.get('dir', 'u2c'), 'body': body}


def normalise_sse(record, normalizer):
    data = record.get('data', '')
    parsed = parse_json(data) if isinstance(data, str) else None
    return {'case': record['case'], 'index': record.get('index'),
            # the SSE `id:` line is the resume anchor: semantic, never volatile.
            'sseId': record.get('sseId'), 'event': record.get('event'),
            'data': normalizer.value(parsed, record['case']) if parsed is not None
            else normalizer.text(str(data))}


def normalise_close(record, normalizer):
    """Both directions of a WebSocket close: what the client sent and the
    code/reason the peer replied with."""
    return {'case': record.get('case', 'ws-close'),
            'sent': [record.get('sentCode'),
                     normalizer.text(str(record.get('sentReason', '')))],
            'observed': [record.get('observedCode'),
                         normalizer.text(str(record.get('observedReason', '')))],
            'clean': record.get('clean')}


# --------------------------------------------------------------- structural diff

def diff_value(location, left, right, out):
    number_left = isinstance(left, (int, float)) and not isinstance(left, bool)
    number_right = isinstance(right, (int, float)) and not isinstance(right, bool)
    if number_left and number_right:
        # JSON does not distinguish 1 from 1.0; Python's int/float split must not
        # manufacture a false difference.
        if left != right:
            out.append({'location': location, 'left': left, 'right': right,
                        'message': 'value differs'})
        return
    if type(left) is not type(right):
        out.append({'location': location, 'left': left, 'right': right,
                    'message': 'type mismatch'})
        return
    if isinstance(left, dict):
        for key in sorted(set(left) | set(right)):
            if key not in left:
                out.append({'location': f'{location}.{key}', 'left': None, 'right': right[key],
                            'message': 'missing on left'})
            elif key not in right:
                out.append({'location': f'{location}.{key}', 'left': left[key], 'right': None,
                            'message': 'extra on left'})
            else:
                diff_value(f'{location}.{key}', left[key], right[key], out)
        return
    if isinstance(left, list):
        for index in range(max(len(left), len(right))):
            item_left = left[index] if index < len(left) else None
            item_right = right[index] if index < len(right) else None
            if index >= len(left):
                out.append({'location': f'{location}[{index}]', 'left': None, 'right': item_right,
                            'message': 'missing on left'})
            elif index >= len(right):
                out.append({'location': f'{location}[{index}]', 'left': item_left, 'right': None,
                            'message': 'extra on left'})
            else:
                diff_value(f'{location}[{index}]', item_left, item_right, out)
        return
    if left != right:
        out.append({'location': location, 'left': left, 'right': right, 'message': 'value differs'})


def diff_sequences(channel, left, right, location):
    out = []
    length = max(len(left), len(right))
    for index in range(length):
        item_left = left[index] if index < len(left) else None
        item_right = right[index] if index < len(right) else None
        case = (item_left or item_right).get('case')
        base = f'{channel}[{case}] #{index}'
        if item_left is None:
            out.append({'location': base, 'left': None, 'right': item_right,
                        'message': 'missing frame on left'})
        elif item_right is None:
            out.append({'location': base, 'left': item_left, 'right': None,
                        'message': 'extra frame on left'})
        else:
            diff_value(base, item_left, item_right, out)
    return out


def diff_http_style(channel, left_records, right_records):
    """Match HTTP-style records by case name so a missing case is named."""
    diffs = []
    left_by_case = {}
    for item in left_records:
        left_by_case.setdefault(item['case'], []).append(item)
    right_by_case = {}
    for item in right_records:
        right_by_case.setdefault(item['case'], []).append(item)
    for case in sorted(set(left_by_case) | set(right_by_case)):
        if case not in left_by_case:
            diffs.append({'location': f'{channel}[{case}]', 'left': None, 'right': 'present',
                          'message': 'case missing on left'})
            continue
        if case not in right_by_case:
            diffs.append({'location': f'{channel}[{case}]', 'left': 'present', 'right': None,
                          'message': 'case missing on right'})
            continue
        for index in range(max(len(left_by_case[case]), len(right_by_case[case]))):
            entry_left = left_by_case[case][index] if index < len(left_by_case[case]) else None
            entry_right = right_by_case[case][index] if index < len(right_by_case[case]) else None
            if entry_left is None or entry_right is None:
                diffs.append({'location': f'{channel}[{case}]', 'left': entry_left,
                              'right': entry_right, 'message': 'record count differs'})
                continue
            for field in ('method', 'path'):
                diff_value(f'{channel}[{case}] {field}', entry_left[field], entry_right[field], diffs)
            for label, entry in (('left', entry_left), ('right', entry_right)):
                if entry['_clockRoute'] and not entry['_clockValid']:
                    diffs.append({'location': f'{channel}[{case}] header.{CLOCK_HEADER}',
                                  'message': f'{label} revision outside recorded request time range',
                                  'left': entry_left['_revision'], 'right': entry_right['_revision']})
            if (entry_left['_clockRoute'] and entry_right['_clockRoute']
                    and entry_left['_clockValid'] and entry_right['_clockValid']
                    and entry_right['_revision'] < entry_left['_revision']):
                diffs.append({'location': f'{channel}[{case}] header.{CLOCK_HEADER}',
                              'message': 'proxy revision precedes direct revision (stale replay)',
                              'left': entry_left['_revision'], 'right': entry_right['_revision']})
            if entry_left['status'] != entry_right['status']:
                diffs.append({'location': f'{channel}[{case}] status', 'left': entry_left['status'],
                              'right': entry_right['status'], 'message': 'status differs'})
            headers_left, headers_right = dict(entry_left['headers']), dict(entry_right['headers'])
            if entry_left['_wireBody'] == entry_right['_wireBody']:
                if entry_left['_wireEtag'] is not None:
                    headers_left['etag'] = entry_left['_wireEtag']
                if entry_right['_wireEtag'] is not None:
                    headers_right['etag'] = entry_right['_wireEtag']
            else:
                for label, entry in (('left', entry_left), ('right', entry_right)):
                    if entry['_wireEtag'] is not None and not entry['_etagValid']:
                        diffs.append({'location': f'{channel}[{case}] header.etag',
                                      'message': f'{label} ETag does not validate differing wire body',
                                      'left': entry_left['_wireEtag'], 'right': entry_right['_wireEtag']})
            for header in sorted(set(headers_left) | set(headers_right)):
                if headers_left.get(header) != headers_right.get(header):
                    diffs.append({'location': f'{channel}[{case}] header.{header}',
                                  'left': headers_left.get(header),
                                  'right': headers_right.get(header),
                                  'message': 'header differs'})
            diff_value(f'{channel}[{case}] body', entry_left['body'], entry_right['body'], diffs)
    return diffs


def compare_sides(left_dir, right_dir, paths=()):
    left_norm = {channel: [] for channel in CHANNELS}
    right_norm = {channel: [] for channel in CHANNELS}
    loaders = {'http': normalise_http, 'handshake': normalise_http,
               'ws': normalise_ws, 'sse': normalise_sse, 'close': normalise_close}
    validation_errors = []
    for side, directory in (('left', left_dir), ('right', right_dir)):
        normalizer = Normalizer(paths)
        for channel in CHANNELS:
            target = left_norm if side == 'left' else right_norm
            for record in load_jsonl(Path(directory) / f'{channel}.jsonl'):
                target[channel].append(loaders[channel](record, normalizer))
        validation_errors.extend({**error, 'side': side} for error in normalizer.errors)

    diffs = validation_errors
    for channel in HTTP_STYLE:
        diffs.extend(diff_http_style(channel, left_norm[channel], right_norm[channel]))
    diffs.extend(diff_sequences('ws', left_norm['ws'], right_norm['ws'], 'ws'))
    diffs.extend(diff_sequences('sse', left_norm['sse'], right_norm['sse'], 'sse'))
    diffs.extend(diff_sequences('close', left_norm['close'], right_norm['close'], 'close'))
    report = {'verdict': 'PASS' if not diffs else 'FAIL',
              'left': str(left_dir), 'right': str(right_dir),
              'counts': {'left': {k: len(v) for k, v in left_norm.items()},
                         'right': {k: len(v) for k, v in right_norm.items()}},
              # Self-documenting coverage: exactly what was normalised and what
              # was required to match literally.
              'normalisation': {
                  'headerExclude': sorted(HEADER_EXCLUDE),
                  'etag': 'same wire body: literal; otherwise body-valid sha1, preserving strength',
                  'clockHeader': CLOCK_HEADER + ' (GET /api/projects, ASCII, requestTimeMs, proxy >= direct)',
                  'httpHealthVolatilePaths': ['timestamp', 'startedAt', 'pid'],
                  'wsKinds': sorted(FRAME_KINDS),
                  'wsEventIdentityPaths': {k: sorted('.'.join(p) for p in v)
                                           for k, v in EVENT_IDENTITIES.items()},
                  'relative': 'session-activity.activity.revision with pre-command snapshot only',
                  'literal': 'every other key and end-to-end header',
              },
              'differences': diffs}
    return report


# --------------------------------------------------------------- controls

def _copy_side(source):
    temp = Path(tempfile.mkdtemp(prefix='exp008-control-'))
    target = temp / 'side'
    shutil.copytree(source, target)
    return temp, target


def _mut_http_body(records):
    """Change one non-volatile scalar field (or add one) in a JSON HTTP body."""
    skip = CLOCK_KEYS | PROCESS_KEYS | IDENTITY_KEYS | RELATIVE_KEYS
    for record in records:
        parsed = parse_json(record.get('body', ''))
        if not isinstance(parsed, dict) or not parsed:
            continue
        candidate = None
        for key in sorted(parsed):
            # Never mutate a volatile key: the normaliser would map both the
            # original and injected value to the same placeholder and hide it.
            if key in skip or UUID_RE.search(str(key)):
                continue
            if isinstance(parsed[key], (str, int, float, bool)):
                candidate = key
                break
        if candidate is None:
            candidate = 'exp008_control_injected'
            parsed[candidate] = 'INJECTED'
        else:
            parsed[candidate] = f'INJECTED_{parsed[candidate]}'
        record['body'] = json.dumps(parsed, ensure_ascii=False)
        return record['case'], f'http[{record["case"]}] body.{candidate}'
    raise AssertionError('no JSON-object HTTP case available to mutate')


def _mut_http_status(records):
    assert records, 'no HTTP records to mutate'
    record = records[0]
    record['status'] = record['status'] + 1
    return record['case'], f'http[{record["case"]}] status'


def _pick_compared_header(record, preferred=()):
    headers = record.get('headers', {})
    for name in preferred:
        if name in headers and name not in HEADER_EXCLUDE:
            return name
    for name in sorted(headers):
        if name not in HEADER_EXCLUDE:
            return name
    raise AssertionError('no comparable HTTP header available to mutate')


def _mut_http_header_change(records):
    for record in records:
        name = _pick_compared_header(record, preferred=('content-type',))
        record['headers'][name] = f'{record["headers"][name]}-INJECTED'
        return record['case'], f'http[{record["case"]}] header.{name}'
    raise AssertionError('no HTTP record available to mutate')


def _mut_http_header_drop(records):
    for record in records:
        name = _pick_compared_header(record, preferred=('etag',))
        del record['headers'][name]
        return record['case'], f'http[{record["case"]}] header.{name}'
    raise AssertionError('no HTTP record available to mutate')


def _mut_http_add_semantic_header(records):
    """Inject a `Set-Cookie` on one side: proves it is compared, not stripped."""
    for record in records:
        record['headers']['set-cookie'] = 'exp008-control=1; Path=/'
        return record['case'], f'http[{record["case"]}] header.set-cookie'
    raise AssertionError('no HTTP record available to mutate')


def _mut_http_add_refresh_header(records):
    """Inject an `X-Refreshed-Token` on one side: proves it is compared."""
    for record in records:
        record['headers']['x-refreshed-token'] = 'exp008-control-token'
        return record['case'], f'http[{record["case"]}] header.x-refreshed-token'
    raise AssertionError('no HTTP record available to mutate')


def _mut_ws_drop_frame(records):
    assert records, 'no WS frames to mutate'
    index = min(3, len(records) - 1)
    case = records[index].get('case', 'ws')
    del records[index]
    return case, f'ws[{case}] #{index}'


def _mut_sse_event(records):
    assert records, 'no SSE events to mutate'
    record = records[0]
    parsed = parse_json(record.get('data', ''))
    if isinstance(parsed, dict):
        parsed['exp008_control_injected'] = True
        record['data'] = json.dumps(parsed, ensure_ascii=False)
    else:
        record['data'] = str(record.get('data', '')) + '\ncontrol-injected\n'
    return record['case'], f'sse[{record["case"]}] #0'


MUTATIONS = (
    ('inject:http-body-field', 'http', _mut_http_body),
    ('inject:http-status-code', 'http', _mut_http_status),
    ('inject:http-header-change', 'http', _mut_http_header_change),
    ('inject:http-header-drop', 'http', _mut_http_header_drop),
    ('inject:http-set-cookie', 'http', _mut_http_add_semantic_header),
    ('inject:http-refreshed-token', 'http', _mut_http_add_refresh_header),
    ('inject:delete-ws-frame', 'ws', _mut_ws_drop_frame),
    ('inject:change-sse-event', 'sse', _mut_sse_event),
)


def run_controls(left_dir, right_dir, paths=()):
    from controls import run_regressions
    controls = run_regressions(include_rust=True)
    baseline = compare_sides(left_dir, right_dir, paths)
    controls.append({'name': 'equivalence:left-vs-right', 'expect': 'PASS',
                     'mutated': None, 'actual': 'PASS' if baseline['verdict'] == 'PASS' else 'FAIL',
                     'differences': baseline['differences'][:3],
                     'passed': baseline['verdict'] == 'PASS'})
    # Valid evidence compared with itself must PASS; invalid anchors/clocks still FAIL.
    for label, directory in (('left', left_dir), ('right', right_dir)):
        temp, copied = _copy_side(directory)
        try:
            report = compare_sides(directory, copied, paths)
        finally:
            shutil.rmtree(temp, ignore_errors=True)
        controls.append({'name': f'equivalence:{label}-vs-itself', 'expect': 'PASS', 'mutated': None,
                         'actual': report['verdict'], 'differences': report['differences'][:3],
                         'passed': report['verdict'] == 'PASS'})
    for name, channel, mutator in MUTATIONS:
        temp, copied = _copy_side(right_dir)
        try:
            records = load_jsonl(copied / f'{channel}.jsonl')
            case, expected = mutator(records)
            write_jsonl(copied / f'{channel}.jsonl', records)
            report = compare_sides(left_dir, copied, paths)
        finally:
            shutil.rmtree(temp, ignore_errors=True)
        locations = [item['location'] for item in report['differences']]
        named = any(expected in location for location in locations)
        controls.append({'name': name, 'expect': 'FAIL', 'mutated': case,
                         'expectedLocation': expected, 'actual': report['verdict'],
                         'namedLocation': named, 'differences': report['differences'][:3],
                         'passed': report['verdict'] == 'FAIL' and named})
    summary = {'verdict': 'PASS' if all(item['passed'] for item in controls) else 'FAIL',
               'controls': controls}
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('left', type=Path)
    parser.add_argument('right', type=Path)
    parser.add_argument('--out', type=Path, default=None)
    parser.add_argument('--controls', action='store_true',
                        help='run equivalence + injected-difference positive controls')
    parser.add_argument('--path', action='append', default=[],
                        help='absolute path to replace with <PATH> on both sides (repeatable)')
    args = parser.parse_args()
    if args.controls:
        report = run_controls(args.left, args.right, args.path)
    else:
        report = compare_sides(args.left, args.right, args.path)
    text = json.dumps(report, indent=2, ensure_ascii=False) + '\n'
    if args.out:
        args.out.write_text(text)
    print(text)
    return 0 if report['verdict'] == 'PASS' else 1


if __name__ == '__main__':
    sys.exit(main())
