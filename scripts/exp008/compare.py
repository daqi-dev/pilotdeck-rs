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

Normalisation is scoped to a field path, never global. A key is only touched
where its name marks it as session identity or a clock:

  * identity keys (`runId`, `sessionId`, `id`, `blockId`, `timeline.turnId`, ...)
    get every embedded UUID replaced by a per-side first-occurrence reference, so
    equal references stay equal and a changed reference still shows as a
    difference while any non-UUID prefix (`status_`, `text_`, `web:`) is kept;
  * clock keys (`timestamp`, `startedAt`, ...) and process keys (`pid`, ...)
    collapse to `<VOLATILE>`;
  * counters that are session/ordering dependent (`revision`, `generation`) are
    compared as deltas from their first value in the same case, so a proxy that
    skips or doubles a bump still shows up;
  * everything else -- `used`, `displayUsed`, `ratio`, token budgets, arbitrary
    strings and any UUID inside them -- is compared literally.

Response headers: only the recomputable / hop-by-hop whitelist is dropped
(`date`, `content-length`, `connection`, `keep-alive`, `transfer-encoding`,
`x-pilotdeck-handler`). `set-cookie`, `etag`, `x-refreshed-token` and every
other end-to-end header must be present and equal. `etag` is value-checked
against the body it is supposed to summarise (`<ETAG-VALID>` when it is a real
weak strong-hash of that body), so dropping or rewriting it is a difference
rather than a pass. `x-projects-revision` is a wall-clock revision minted per
request (`projects.js`); it is recorded as `<REVISION>` only when it is a
positive integer, so a dropped or mangled revision is still a difference.

Everything else is compared structurally, with a precise location for every
difference:

    http[<case>] status | header.<name> | body.<path>
    handshake[<case>] status | header.<name>
    ws[<case>] #<index>
    sse[<case>] #<index>

Exit code: 0 = PASS, 1 = FAIL, 2 = usage error.

Positive controls (`--controls LEFT RIGHT`): inject one known difference at a
time into a copy of RIGHT and require the comparator to FAIL while naming the
exact location. Equivalence controls (a side compared with itself) must PASS.
"""
import argparse
import base64
import hashlib
import json
import re
import shutil
import sys
import tempfile
from pathlib import Path

CHANNELS = ('http', 'handshake', 'ws', 'sse', 'close')
# HTTP-style channels match records by case name; the rest are ordered sequences.
HTTP_STYLE = ('http', 'handshake')

# Truly wall-clock / process values. Collapsed to a placeholder, never indexed.
CLOCK_KEYS = {
    'timestamp', 'startedAt', 'completedAt', 'createdAt', 'updatedAt',
    'durationMs', 'mtimeMs', 'lastActivity', 'expiresAt',
}
PROCESS_KEYS = {'processId', 'pid'}
# Session identity. UUIDs embedded in these values are mapped per side; the rest
# of the string is compared as-is.
IDENTITY_KEYS = {
    'id', 'runId', 'sessionId', 'messageId', 'requestId', 'streamId', 'turnId',
    'blockId', 'toolCallId', 'completedRunId', 'entryId', 'moduleInstanceId',
    'instanceId', 'operationId', 'idempotencyKey',
}
# Counters whose absolute value depends on how many runs preceded this side.
# Compared as deltas from the first value seen for the same (case, path).
RELATIVE_KEYS = {'revision', 'generation', 'connectionGeneration'}

# Headers a proxy may legitimately recompute or that are hop-by-hop. Everything
# else is an end-to-end header and must participate in the comparison.
HEADER_EXCLUDE = {
    'date', 'content-length', 'connection', 'keep-alive', 'transfer-encoding',
    'x-pilotdeck-handler',
}
# Per-request wall-clock revision header; validated by form, not by equality.
CLOCK_HEADER = 'x-projects-revision'

UUID_RE = re.compile(r'[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-'
                     r'[0-9a-fA-F]{4}-[0-9a-fA-F]{12}')


def valid_etag(value, body):
    """True when `value` is the Express etag of `body` (weak sha1 of the bytes)."""
    raw = body.encode('utf-8', 'replace')
    digest = base64.b64encode(hashlib.sha1(raw).digest()).decode('ascii').rstrip('=')
    return value in (f'"{len(raw):x}-{digest}"', f'W/"{len(raw):x}-{digest}"')


def normalise_header(name, value, body, normalizer):
    if name == 'etag':
        return '<ETAG-VALID>' if valid_etag(str(value), body) else normalizer.text(str(value))
    if name == CLOCK_HEADER:
        text = str(value)
        return '<REVISION>' if text.isdigit() and int(text) > 0 else normalizer.text(text)
    return normalizer.text(str(value))


class Normalizer:
    """Per-side normaliser; one instance per recorded side so the first-seen
    ordering of generated ids is independent, exactly like the frozen trace
    comparator."""

    def __init__(self, paths=()):
        self.ids = {}
        self.bases = {}
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
        """Identity value: keep non-UUID content, map embedded UUIDs per side."""
        if value is None:
            return None
        if isinstance(value, str):
            return UUID_RE.sub(lambda match: self.ref('uuid', match.group(0)), self.text(value))
        return self.value(value)

    def relative(self, case, path, value):
        key = (case, path)
        base = self.bases.setdefault(key, value)
        return value - base

    def value(self, value, case=None, path=()):
        if isinstance(value, dict):
            out = {}
            for name in sorted(value):
                item = value[name]
                if name in CLOCK_KEYS or name in PROCESS_KEYS:
                    out[name] = None if item is None else '<VOLATILE>'
                elif name in IDENTITY_KEYS:
                    out[name] = self.identity(item)
                elif name in RELATIVE_KEYS and isinstance(item, (int, float)) \
                        and not isinstance(item, bool):
                    out[name] = self.relative(case, path + (name,), item)
                else:
                    out[name] = self.value(item, case, path + (name,))
            return out
        if isinstance(value, list):
            return [self.value(item, case, path) for item in value]
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
    return {'case': record['case'], 'method': record.get('method'),
            'path': record.get('path'), 'status': record.get('status'),
            'headers': {k: normalise_header(k, v, body, normalizer)
                        for k, v in sorted(headers.items())},
            'body': normalizer.value(parsed, record['case']) if parsed is not None
            else normalizer.text(body)}


def normalise_ws(record, normalizer):
    raw = record.get('raw')
    case = record.get('case', 'ws')
    parsed = parse_json(raw) if isinstance(raw, str) else None
    body = normalizer.value(parsed, case) if parsed is not None else normalizer.text(str(raw))
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
            if entry_left['status'] != entry_right['status']:
                diffs.append({'location': f'{channel}[{case}] status', 'left': entry_left['status'],
                              'right': entry_right['status'], 'message': 'status differs'})
            for header in sorted(set(entry_left['headers']) | set(entry_right['headers'])):
                if entry_left['headers'].get(header) != entry_right['headers'].get(header):
                    diffs.append({'location': f'{channel}[{case}] header.{header}',
                                  'left': entry_left['headers'].get(header),
                                  'right': entry_right['headers'].get(header),
                                  'message': 'header differs'})
            diff_value(f'{channel}[{case}] body', entry_left['body'], entry_right['body'], diffs)
    return diffs


def compare_sides(left_dir, right_dir, paths=()):
    left_norm = {channel: [] for channel in CHANNELS}
    right_norm = {channel: [] for channel in CHANNELS}
    loaders = {'http': normalise_http, 'handshake': normalise_http,
               'ws': normalise_ws, 'sse': normalise_sse, 'close': normalise_close}
    for side, directory in (('left', left_dir), ('right', right_dir)):
        normalizer = Normalizer(paths)
        for channel in CHANNELS:
            target = left_norm if side == 'left' else right_norm
            for record in load_jsonl(Path(directory) / f'{channel}.jsonl'):
                target[channel].append(loaders[channel](record, normalizer))

    diffs = []
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
                  'etag': 'validated against the response body (weak sha1)',
                  'clockHeader': CLOCK_HEADER + ' (positive integer -> <REVISION>)',
                  'collapseKeys': sorted(CLOCK_KEYS | PROCESS_KEYS),
                  'identityKeys': sorted(IDENTITY_KEYS),
                  'relativeKeys': sorted(RELATIVE_KEYS),
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
    controls = []
    baseline = compare_sides(left_dir, right_dir, paths)
    controls.append({'name': 'equivalence:left-vs-right', 'expect': 'PASS',
                     'mutated': None, 'actual': 'PASS' if baseline['verdict'] == 'PASS' else 'FAIL',
                     'differences': baseline['differences'][:3],
                     'passed': baseline['verdict'] == 'PASS'})
    # A side compared with itself must always PASS, independent of the baseline.
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
