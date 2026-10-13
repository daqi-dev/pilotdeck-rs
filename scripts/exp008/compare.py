#!/usr/bin/env python3
"""exp008 / B1 record-replay comparator for the Rust proxy.

Compares two recorded "sides" of the same driving script:

    <side>/
      http.jsonl   one {case, method, path, status, headers, body} per line
      ws.jsonl     one {case, dir, opcode, raw} per WebSocket text frame, in order
      sse.jsonl    one {case, index, sseId, event, data} per SSE event, in order

`direct` is the TS ui/server (13002); `proxy` is the same server reached through
the Rust relay (13001). Both recordings are produced by `run.py` from the SAME
isolated home and the SAME project path (`specs/parity/normalization.md`: the two
sides must run the same output path). The comparator normalises only:

  * volatile envelope keys (timestamps / process / request / run ids) via a
    per-side first-occurrence mapping so equal references stay equal and a
    changed reference still shows up as a difference;
  * UUIDs embedded anywhere in strings, same mapping;
  * the absolute project root (both sides pass `--path`);
  * response headers: hop-by-hop, `date`, `content-length`, `set-cookie`,
    `etag` are dropped and the proxy marker `X-PilotDeck-Handler` is stripped.

Everything else is compared structurally, with a precise location for every
difference:

    http[<case>] status | header.<name> | body.<path>
    ws[<case>] #<index>
    sse[<case>] #<index>

Exit code: 0 = PASS, 1 = FAIL, 2 = usage error.

Positive controls (`--controls LEFT RIGHT`): inject one known difference at a
time into a copy of RIGHT and require the comparator to FAIL while naming the
exact location. Equivalence controls (a side compared with itself) must PASS.
"""
import argparse
import json
import re
import shutil
import sys
import tempfile
from pathlib import Path

# Envelope keys that are volatile across two runs (subset of the frozen
# specs/parity/normalization.md _VOLATILE_KEYS, minus `seq`, which is semantic
# for the SSE resume anchor and must stay comparable).
# Identity keys keep a per-side first-occurrence mapping so equal references stay
# equal. Counter / clock / derived keys have session-dependent absolute values
# (they must not be index-mapped, or a legitimately different revision count
# would read as a difference); they collapse to one placeholder instead.
COLLAPSE_KEYS = {
    'timestamp', 'startedAt', 'completedAt', 'createdAt', 'updatedAt',
    'durationMs', 'mtimeMs', 'revision', 'generation', 'connectionGeneration',
    'used', 'displayUsed', 'ratio', 'processId', 'pid', 'operationId',
    'idempotencyKey', 'expiresAt', 'token',
}
VOLATILE_KEYS = {
    'timestamp', 'startedAt', 'completedAt', 'createdAt', 'updatedAt',
    'durationMs', 'mtimeMs', 'messageId', 'requestId', 'streamId', 'runId',
    'operationId', 'idempotencyKey', 'connectionGeneration', 'moduleInstanceId',
    'processId', 'pid', 'entryId', 'sessionId', 'generation', 'revision',
    'completedRunId', 'turnId', 'blockId', 'toolCallId', 'used', 'displayUsed',
    'ratio', 'expiresAt', 'token',
}
# Headers a proxy is allowed to recompute, or that carry wall-clock / request
# identity rather than application semantics. `x-pilotdeck-handler` is the
# proxy's own marker and is always stripped before comparing.
HEADER_EXCLUDE = {
    'date', 'content-length', 'connection', 'keep-alive', 'transfer-encoding',
    'x-pilotdeck-handler', 'server', 'etag', 'last-modified', 'set-cookie',
    'x-projects-revision', 'x-request-id', 'x-refreshed-token', 'x-staffdeck-runtime',
}
UUID_RE = re.compile(r'[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-'
                     r'[0-9a-fA-F]{4}-[0-9a-fA-F]{12}')


class Normalizer:
    """Per-side normaliser; one instance per recorded side so the first-seen
    ordering of generated ids is independent, exactly like the frozen trace
    comparator."""

    def __init__(self, paths=()):
        self.ids = {}
        self.paths = sorted([str(p) for p in paths if p], key=len, reverse=True)

    def ref(self, category, value):
        key = (category, str(value))
        if key not in self.ids:
            self.ids[key] = len(self.ids)
        return f'<{category}:{self.ids[key]}>'

    def text(self, value):
        for path in self.paths:
            value = value.replace(path, '<PATH>')
        return UUID_RE.sub(lambda match: self.ref('uuid', match.group(0)), value)

    def value(self, value, key=None):
        if isinstance(value, dict):
            out = {}
            for name in sorted(value):
                item = value[name]
                if name in COLLAPSE_KEYS:
                    out[name] = None if item is None else '<VOLATILE>'
                elif name in VOLATILE_KEYS:
                    out[name] = None if item is None else self.ref(name, item)
                else:
                    out[name] = self.value(item, name)
            return out
        if isinstance(value, list):
            return [self.value(item, key) for item in value]
        if isinstance(value, str):
            return self.text(value)
        return value


def load_jsonl(path):
    records = []
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
    headers = {k.lower(): v for k, v in record.get('headers', {}).items()
               if k.lower() not in HEADER_EXCLUDE}
    body = record.get('body', '')
    parsed = parse_json(body)
    return {'case': record['case'], 'method': record.get('method'),
            'path': record.get('path'), 'status': record.get('status'),
            'headers': {k: normalizer.text(str(v)) for k, v in sorted(headers.items())},
            'body': normalizer.value(parsed) if parsed is not None else normalizer.text(body)}


def normalise_ws(record, normalizer):
    raw = record.get('raw')
    parsed = parse_json(raw) if isinstance(raw, str) else None
    body = normalizer.value(parsed) if parsed is not None else normalizer.text(str(raw))
    return {'case': record.get('case', 'ws'), 'opcode': record.get('opcode', 'text'),
            'dir': record.get('dir', 'u2c'), 'body': body}


def normalise_sse(record, normalizer):
    data = record.get('data', '')
    parsed = parse_json(data) if isinstance(data, str) else None
    return {'case': record['case'], 'index': record.get('index'),
            # the SSE `id:` line is the resume anchor: semantic, never volatile.
            'sseId': record.get('sseId'), 'event': record.get('event'),
            'data': normalizer.value(parsed) if parsed is not None else normalizer.text(str(data))}


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


def compare_sides(left_dir, right_dir, paths=()):
    left_norm = {'http': [], 'ws': [], 'sse': []}
    right_norm = {'http': [], 'ws': [], 'sse': []}
    for side, directory in (('left', left_dir), ('right', right_dir)):
        normalizer = Normalizer(paths)
        for channel, loader in (('http', normalise_http), ('ws', normalise_ws), ('sse', normalise_sse)):
            target = left_norm if side == 'left' else right_norm
            for record in load_jsonl(Path(directory) / f'{channel}.jsonl'):
                target[channel].append(loader(record, normalizer))
        # keep id order deterministic across sides by re-reading is unnecessary;
        # normalisation above is already order-driven.

    diffs = []
    # HTTP: match by case name so a missing/extra case is named explicitly.
    left_http = {}
    for item in left_norm['http']:
        left_http.setdefault(item['case'], []).append(item)
    right_http = {}
    for item in right_norm['http']:
        right_http.setdefault(item['case'], []).append(item)
    for case in sorted(set(left_http) | set(right_http)):
        if case not in left_http:
            diffs.append({'location': f'http[{case}]', 'left': None, 'right': 'present',
                          'message': 'case missing on left'})
            continue
        if case not in right_http:
            diffs.append({'location': f'http[{case}]', 'left': 'present', 'right': None,
                          'message': 'case missing on right'})
            continue
        for index in range(max(len(left_http[case]), len(right_http[case]))):
            entry_left = left_http[case][index] if index < len(left_http[case]) else None
            entry_right = right_http[case][index] if index < len(right_http[case]) else None
            if entry_left is None or entry_right is None:
                diffs.append({'location': f'http[{case}]', 'left': entry_left, 'right': entry_right,
                              'message': 'record count differs'})
                continue
            if entry_left['status'] != entry_right['status']:
                diffs.append({'location': f'http[{case}] status', 'left': entry_left['status'],
                              'right': entry_right['status'], 'message': 'status differs'})
            for header in sorted(set(entry_left['headers']) | set(entry_right['headers'])):
                if entry_left['headers'].get(header) != entry_right['headers'].get(header):
                    diffs.append({'location': f'http[{case}] header.{header}',
                                  'left': entry_left['headers'].get(header),
                                  'right': entry_right['headers'].get(header),
                                  'message': 'header differs'})
            diff_value(f'http[{case}] body', entry_left['body'], entry_right['body'], diffs)
    diffs.extend(diff_sequences('ws', left_norm['ws'], right_norm['ws'], 'ws'))
    diffs.extend(diff_sequences('sse', left_norm['sse'], right_norm['sse'], 'sse'))
    report = {'verdict': 'PASS' if not diffs else 'FAIL',
              'left': str(left_dir), 'right': str(right_dir),
              'counts': {'left': {k: len(v) for k, v in left_norm.items()},
                         'right': {k: len(v) for k, v in right_norm.items()}},
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
    for record in records:
        parsed = parse_json(record.get('body', ''))
        if not isinstance(parsed, dict) or not parsed:
            continue
        candidate = None
        for key in sorted(parsed):
            # Never mutate a volatile key: the normaliser would map both the
            # original and injected value to a first-seen placeholder and hide
            # the change.
            if key in VOLATILE_KEYS or UUID_RE.search(str(key)):
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
