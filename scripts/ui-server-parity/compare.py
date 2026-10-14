#!/usr/bin/env python3
"""B2 schema-1 fail-closed comparator. See specs/ui-server-parity (DRAFT).

python3 scripts/ui-server-parity/compare.py LEFT RIGHT --enumeration ENUM --out JSON
Exit 0 PASS; 1 FAIL; 2 REFUSED. Explicit TS pool required; --paired-only is diagnostic. No transport chunk-boundary or cross-connection comparison.
"""
import argparse
import base64
import binascii
from collections import defaultdict
from datetime import datetime, timezone
import hashlib
import hmac
import subprocess
import sqlite3
import json
import math
from pathlib import Path
import re
import sys

from check_coverage import check as check_matrix, matches
from policy import (HEADER_OMIT, SSE_ABSOLUTE_NS, SSE_DELAY_NS, field_key, fields, pattern,
                    UUID_VALUE, BLOCK_VALUE, MESSAGE_VALUE, CHAT_FIXTURE_SESSIONS,
                    CLOCK_FORMATS, CLOCK_PAIRS, CLOCK_RESPONSE_ORDER, ISO_MS, SQL_S, EPOCH_MS, MTIME_MS)
from snapshot import capture
from common import apparatus_hashes

HERE = Path(__file__).resolve().parent
NOT_JSON = object()
RULES = {'evidence.integrity', 'coverage.matrix', 'coverage.handler', 'http.request',
         'http.status', 'http.headers', 'http.body', 'identity.consistency',
         'normalization.field', 'ws.sequence', 'ws.structure', 'activity.out-of-band',
         'sse.sequence', 'sse.complete', 'sse.delay', 'disk.snapshot', 'multimodal.membership'}


def read_json(path):
    return json.loads(Path(path).read_text())


def read_jsonl(path):
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]


def decode(value):
    if not isinstance(value, str):
        raise ValueError('base64 must be a string')
    return base64.b64decode(value, validate=True)


def parse(raw):
    try:
        return json.loads(raw, parse_constant=lambda v: (_ for _ in ()).throw(ValueError('nonfinite JSON ' + v)))
    except (ValueError, UnicodeError):
        return NOT_JSON


def issue(out, rule, location, message, **detail):
    out.append({'rule': rule, 'location': location, 'message': message, **detail})


def diff(left, right, out, rule, location):
    """JSON structural diff; bool is not an int; missing differs from null."""
    if type(left) in (int, float) and type(right) in (int, float):
        if left != right:
            issue(out, rule, location, 'value differs', left=left, right=right)
    elif type(left) is not type(right):
        issue(out, rule, location, 'type differs', left=left, right=right)
    elif isinstance(left, dict):
        for key in sorted(left.keys() | right.keys()):
            if key not in left or key not in right:
                issue(out, rule, f'{location}.{field_key(key)}', 'field missing', missing='left' if key not in left else 'right')
            else:
                diff(left[key], right[key], out, rule, f'{location}.{field_key(key)}')
    elif isinstance(left, list):
        if len(left) != len(right):
            issue(out, rule, location, 'length differs', left=len(left), right=len(right))
        for index, (a, b) in enumerate(zip(left, right)):
            diff(a, b, out, rule, f'{location}[{index}]')
    elif left != right:
        issue(out, rule, location, 'value differs', left=left, right=right)
        if isinstance(left, str) and isinstance(right, str) and re.fullmatch(r'<[a-z]+:[a-z]+:[0-9]+>', left) and re.fullmatch(r'<[a-z]+:[a-z]+:[0-9]+>', right):
            issue(out, 'identity.consistency', location, 'identity alias/reference differs', left=left, right=right)


class Normalizer:
    def __init__(self, side, out):
        self.side, self.out = side, out
        self.identities = defaultdict(dict)
        self.activities = {}
        self.revisions = {}
        self.final_snapshot = None
        self.auth_tokens = {}
        self.password_hashes = set()
        self.auth_secret = None
        self.auth_secret_rows = set()
        self.vapid_keys = set()
        self.expiries = {}

    def ref(self, category, value, location):
        if value is None:
            return None
        if not ((isinstance(value, str) and value) or (type(value) is int and value > 0)):
            issue(self.out, 'normalization.field', location, 'invalid identity', side=self.side)
            return value
        if isinstance(value, str):
            # One UUID namespace across HTTP/WS/disk and semantic categories:
            # runId == turnId and blockId == timeline.id remain observable.
            if category != 'uuid' and UUID_VALUE.fullmatch(value):
                return self.ref('uuid', value, location)
            if category in ('block', 'timeline'):
                derived = BLOCK_VALUE.fullmatch(value)
                if derived:
                    return self.ref('uuid', derived[1], location) + derived[2]
                # Non-UUID block keys are semantic identifiers, not random IDs.
                return value
            if category == 'run':
                message = MESSAGE_VALUE.fullmatch(value)
                if message:
                    return message[1] + '_' + self.ref('uuid', message[2], location)
        # Type is part of identity: numeric 1 is not textual "1".
        key = (type(value).__name__, value)
        mapping = self.identities[category]
        if key not in mapping:
            mapping[key] = len(mapping) + 1
        return f'<{category}:{type(value).__name__}:{mapping[key]}>'

    def clock_instant(self, value, representation, location, *, wall=True):
        """Validate the producer's exact representation before interpreting it."""
        try:
            if representation in (ISO_MS, SQL_S):
                regex = (r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}Z'
                         if representation == ISO_MS else
                         r'[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}')
                if not isinstance(value, str) or not re.fullmatch(regex, value):
                    raise ValueError('representation must be ' + representation)
                dt = datetime.strptime(value, '%Y-%m-%dT%H:%M:%S.%fZ' if representation == ISO_MS
                                       else '%Y-%m-%d %H:%M:%S').replace(tzinfo=timezone.utc)
                delta = dt - datetime(1970, 1, 1, tzinfo=timezone.utc)
                instant = (delta.days * 86400 + delta.seconds) * 1000 + delta.microseconds // 1000
            elif representation in (EPOCH_MS, MTIME_MS):
                if (type(value) not in (int, float) or not math.isfinite(value)
                        or not 0 < value < 2**53
                        or representation == EPOCH_MS and value != int(value)):
                    raise ValueError('representation must be ' + representation)
                instant = value
            else:
                raise ValueError('unregistered clock representation')
            if wall:
                start, end = self.wall_bounds
                # CURRENT_TIMESTAMP truncates to whole UTC seconds. Compare
                # in that source precision, not an invented timezone/tolerance.
                if representation == SQL_S:
                    start = start // 1000 * 1000
                if not start <= instant <= end + 1000:
                    raise ValueError('outside capture wall interval (unit: ms)')
            return instant
        except (ValueError, OverflowError, AttributeError) as error:
            issue(self.out, 'normalization.field', location, 'invalid clock field: ' + str(error),
                  side=self.side, representation=representation)
            return None

    def time(self, value, location, representation):
        if value is None:
            return None
        if self.clock_instant(value, representation, location) is None:
            return value
        return f'<TIME:{type(value).__name__}>'

    def ordered_clocks(self, raw, result, channel, context, path, location):
        """Preserve ranks without sorting arrays or changing the field set."""
        pair_rules = CLOCK_PAIRS.get((channel, context), {}).get(pattern(path), ())
        whole_response = not path and (channel, context) in CLOCK_RESPONSE_ORDER
        if not pair_rules and not whole_response:
            return result
        clocks = []
        def collect(value, current, relative=()):
            representation = CLOCK_FORMATS.get((channel, context), {}).get(pattern(current))
            if representation and value is not None:
                # The leaf dispatcher already emitted validation errors.
                mark = result
                for part in relative:
                    mark = mark[part]
                if isinstance(mark, str) and mark.startswith('<TIME:'):
                    instant = self.clock_instant(value, representation, location + '.' + pattern(relative))
                    if instant is not None:
                        clocks.append((relative, instant))
            elif isinstance(value, dict):
                for key, item in sorted(value.items()):
                    collect(item, current + (key,), relative + (key,))
            elif isinstance(value, list):
                for i, item in enumerate(value):
                    collect(item, current + (i,), relative + (i,))
        # Whole-response ranking is performed only once, at the root. Pair
        # validation still runs on each exact object on the way up.
        if whole_response:
            collect(raw, path)
        elif isinstance(raw, dict):
            for name in sorted({n for pair in pair_rules for n in pair}):
                if name in raw:
                    collect(raw[name], path + (name,), (name,))
        values = dict(clocks)
        for earlier, later in pair_rules:
            a, b = values.get((earlier,)), values.get((later,))
            if a is not None and b is not None and a > b:
                issue(self.out, 'normalization.field', location + '.' + later,
                      'clock order invalid: ' + earlier + ' follows ' + later, side=self.side)
        # Independent SQLite CURRENT_TIMESTAMP calls quantize to seconds:
        # equality can change across captures. Enforce <= above, but do not
        # treat quantization ties as source-level alias equality.
        sql_pair = bool(pair_rules) and all(
            CLOCK_FORMATS[channel, context][pattern(path + (name,))] == SQL_S
            for pair in pair_rules for name in pair)
        if whole_response or ((channel, context) not in CLOCK_RESPONSE_ORDER and not sql_pair):
            ranks = {v: i for i, v in enumerate(sorted({v for _, v in clocks}))}
            for relative, instant in clocks:
                target = result
                for part in relative[:-1]:
                    target = target[part]
                target[relative[-1]] = {'clock': target[relative[-1]], 'order': ranks[instant]}
        return result

    def upload_url(self, value, location):
        found = re.fullmatch(r'/api/uploads/([^/]+)/(content|events)', value) if isinstance(value, str) else None
        if not found:
            issue(self.out, 'normalization.field', location, 'invalid upload URL', side=self.side)
            return value
        return f'/api/uploads/{self.ref("upload", found[1], location)}/{found[2]}'

    def seed_chat_identities(self, entries):
        """Name persisted entities before reading interleaved live references.

        Only the two driver-owned session files and registered @chat ID paths.
        This does not sort, drop, or replace any compared list/event/file.
        """
        ids, _ = fields('disk', '@chat')

        def visit(value, path=()):
            if pattern(path) in ids:
                self.ref(ids[pattern(path)], value, 'identity.seed.' + pattern(path))
            elif isinstance(value, dict):
                for key, item in sorted(value.items()):
                    visit(item, path + (key,))
            elif isinstance(value, list):
                for index, item in enumerate(value):
                    visit(item, path + (index,))

        for session in CHAT_FIXTURE_SESSIONS:
            self.ref('session', session, 'identity.seed.session')
        for session in CHAT_FIXTURE_SESSIONS:
            path = f'home/projects/exp008-fixture/chats/{session}.jsonl'
            for entry in entries:
                if entry['path'] == path:
                    visit(entry.get('data'))

    def value(self, value, channel, context, path=(), location='body', upload_id=None):
        ids, clocks = fields(channel, context)
        field = pattern(path)
        if not path and isinstance(value, dict) and 'expiresAt' in clocks:
            upload_id = value.get('uploadId')
        if field == 'expiresAt' and field in clocks:
            if self.clock_instant(value, CLOCK_FORMATS[channel, context][field], location, wall=False) is None:
                return value
            trusted = self.expiries.get(upload_id)
            if not trusted or type(value) is not type(trusted['raw']) or value != trusted['raw']:
                issue(self.out, 'normalization.field', location, 'expiry differs from persisted backend upload witness', side=self.side)
                return value
            return {'type': type(value).__name__, 'ttl_ms': trusted['ttl_ms']}
        if channel == 'http' and context == '_probe:GET:/health' and field == 'pid':
            if type(value) is not int or value != self.ui_pid:
                issue(self.out, 'normalization.field', location, 'health PID is not the owned UI process', side=self.side)
                return value
            return '<UI-PID:VERIFIED>'
        if channel == 'disk' and context == '@upload' and field == 'attachments.*.path':
            match = re.fullmatch(re.escape(self.fixture_path) + r'/home/\.tmp/chat-uploads/([^/]+)/files/(.+)', value) if isinstance(value, str) else None
            if not match:
                issue(self.out, 'normalization.field', location, 'invalid attachment path', side=self.side)
                return value
            return self.fixture_path + '/home/.tmp/chat-uploads/' + self.ref('upload', match[1], location) + '/files/' + match[2]
        if channel == 'disk' and context == '@registry' and field == 'records.*.ownerPid':
            if type(value) is not int or value != self.gateway_pid:
                issue(self.out, 'disk.snapshot', location, 'registry ownerPid is not the owned gateway process', side=self.side)
                return value
            return '<GATEWAY-PID:VERIFIED>'
        if channel == 'http' and context in ('auth.register', 'auth.login') and field == 'token' and value in self.auth_tokens:
            return self.auth_tokens[value]
        if channel == 'disk' and context == 'home/auth.db':
            if field == 'tables.users.rows.*.password_hash' and value in self.password_hashes:
                return '<BCRYPT12:VERIFIED>'
            if field == 'tables.app_config.rows.*.value' and path[-2] in self.auth_secret_rows and value == self.auth_secret:
                return '<JWT-SECRET:VERIFIED>'
            if field in ('tables.vapid_keys.rows.*.public_key', 'tables.vapid_keys.rows.*.private_key') and value in self.vapid_keys:
                return '<VAPID:VERIFIED:' + field.rsplit('.', 1)[-1] + '>'
        if field in ids:
            return self.upload_url(value, location) if ids[field] == 'upload-url' else self.ref(ids[field], value, location)
        if field in clocks:
            return self.time(value, location, CLOCK_FORMATS[channel, context][field])
        if isinstance(value, dict):
            # Sorted object keys do not depend on serialization key order.
            result = {key: self.value(item, channel, context, path + (key,), f'{location}.{field_key(key)}', upload_id=upload_id)
                      for key, item in sorted(value.items())}
            return self.ordered_clocks(value, result, channel, context, path, location)
        if isinstance(value, list):
            result = [self.value(item, channel, context, path + (index,), f'{location}[{index}]', upload_id=upload_id)
                      for index, item in enumerate(value)]
            return self.ordered_clocks(value, result, channel, context, path, location)
        return value

    def activity(self, item, location):
        if not isinstance(item, dict):
            issue(self.out, 'activity.out-of-band', location, 'malformed activity', side=self.side)
            return item
        key = (item.get('sessionId'), item.get('revision'))
        # Exact raw per-session revision AND complete activity must match OOB.
        if key not in self.activities or item != self.activities[key]:
            issue(self.out, 'activity.out-of-band', location, 'DUT activity absent/different in direct observer', side=self.side,
                  session=item.get('sessionId'), revision=item.get('revision'))
        result = self.value(item, 'ws', 'session-activity', ('activity',), location)
        result['revision'] = self.revisions.get(key, item.get('revision'))
        return result

    def ws(self, body, location):
        if not isinstance(body, dict):
            return body
        context = body.get('type', body.get('kind', ''))
        result = self.value(body, 'ws', context, location=location)
        if context == 'session-activity':
            result['activity'] = self.activity(body.get('activity'), f'{location}.activity')
        if context == 'session-activity-snapshot':
            if not isinstance(body.get('activities'), list):
                issue(self.out, 'activity.out-of-band', location, 'snapshot missing activities', side=self.side)
            else:
                if body['activities'] != self.final_snapshot:
                    issue(self.out, 'activity.out-of-band', location + '.activities', 'snapshot differs from post-scenario direct anchor', side=self.side)
                result['activities'] = [self.activity(item, f'{location}.activities[{i}]') for i, item in enumerate(body['activities'])]
        return result


def shell_stream(messages, frames, out, location, side):
    """PTY output is a directional byte stream; other frames retain direction order.

    Only used for a verified /shell handshake. Output envelopes are exact so
    new metadata can never disappear under the stream concatenation rule.
    """
    result = {direction: {'output_b64': '', 'frames': []} for direction in ('c2u', 'u2c')}
    streams = {direction: bytearray() for direction in result}
    diagnostics = {direction: [] for direction in result}
    output_frames = set()
    for message in messages:
        direction, body = message['dir'], message['body']
        if message['opcode'] == 1 and isinstance(body, dict) and body.get('type') == 'output':
            if set(body) != {'type', 'data'} or not isinstance(body['data'], str):
                issue(out, 'ws.structure', location + '.' + direction + '.output',
                      'PTY output must have exactly type and string data', side=side)
                continue
            raw = body['data'].encode('utf-8')
            streams[direction].extend(raw)
            diagnostics[direction].append({'bytes': len(raw), 'end_offset': len(streams[direction]),
                                           'wire_frames': len(message['frames'])})
            output_frames.update(id(frame) for frame in message['frames'])
    # Use the original frame order, including control frames inserted inside a
    # fragmented non-output message. Completion order must not reorder those.
    for frame in frames:
        if id(frame) not in output_frames:
            result[frame['dir']]['frames'].append(frame)
    for direction, raw in streams.items():
        result[direction]['output_b64'] = base64.b64encode(raw).decode()
    return result, diagnostics


def headers(record):
    result = defaultdict(list)
    raw = record.get('headers')
    if not isinstance(raw, list):
        raise ValueError('headers must preserve raw multi-values as a list')
    for header in raw:
        name = header['name'].lower()
        if not re.fullmatch(r"[!#$%&'*+.^_`|~0-9a-z-]+", name):
            raise ValueError('invalid header name')
        result[name].append(decode(header['value_b64']))
    return dict(result)


def literal_headers(raw_headers):
    return {name: [base64.b64encode(v).decode() for v in values]
            for name, values in headers({'headers': raw_headers}).items()}


def valid_etag(value, body):
    tag = base64.b64encode(hashlib.sha1(body).digest()).rstrip(b'=')
    suffix = b'"' + f'{len(body):x}'.encode() + b'-' + tag + b'"'
    return value in (suffix, b'W/' + suffix)


def normalize_headers(record, body, norm, location, *, request=False, case='', handshake=False):
    result = {}
    for name, values in sorted(headers(record).items()):
        if name in HEADER_OMIT and (not request or name != 'x-pilotdeck-handler'):
            continue
        normalized = []
        for value in values:
            if name == 'date' and not request:
                # HTTP Date is a known response header path, no substring scrubbing.
                from email.utils import parsedate_to_datetime
                try:
                    parsedate_to_datetime(value.decode('ascii'))
                except (ValueError, TypeError, UnicodeError):
                    issue(norm.out, 'normalization.field', f'{location}.date', 'invalid HTTP Date', side=norm.side)
                normalized.append('<HTTP-DATE>')
            elif name == 'sec-websocket-key' and request and handshake:
                try:
                    if len(decode(value.decode('ascii'))) != 16:
                        raise ValueError('WS key length')
                except (ValueError, UnicodeError):
                    issue(norm.out, 'normalization.field', location, 'invalid WS key', side=norm.side)
                normalized.append('<WS-KEY>')
            elif name == 'sec-websocket-accept' and not request and handshake:
                # Validated against the request key separately, never blindly omitted.
                normalized.append('<WS-ACCEPT>')
            elif name == 'etag' and not request and valid_etag(value, body):
                normalized.append('<ETAG-WEAK>' if value.startswith(b'W/') else '<ETAG-STRONG>')
            elif name == 'x-projects-revision' and not request and case == 'readonly.projects' and record.get('status') == 200:
                try:
                    clock = int(value) if re.fullmatch(rb'[1-9][0-9]{0,15}', value) else 0
                    if not norm.wall_bounds[0] <= clock <= norm.wall_bounds[1] + 1000:
                        raise ValueError('out of run wall clock bounds')
                except (ValueError, AttributeError):
                    issue(norm.out, 'normalization.field', f'{location}.{name}', 'invalid projects clock', side=norm.side)
                    normalized.append(base64.b64encode(value).decode())
                else:
                    normalized.append('<PROJECTS-CLOCK>')
            else:
                normalized.append(base64.b64encode(value).decode())
        result[name] = normalized
    return result


def normalize_path(path, norm, location):
    found = re.fullmatch(r'(/api/uploads/)([^/]+)(/.*)?', path)
    if found:
        return found[1] + norm.ref('upload', found[2], location) + (found[3] or '')
    found = re.fullmatch(r'(/api/sessions/)([^/]+)(/messages)', path)
    if found:
        from urllib.parse import unquote
        return found[1] + norm.ref('session', unquote(found[2]), location) + found[3]
    return path


def complete_body(records, request_id, direction, metadata):
    events = [r for r in records if r.get('id') == request_id and r.get('direction') == direction
              and r['kind'] in ('http_chunk', 'http_end')]
    if not events or events[-1]['kind'] != 'http_end' or sum(r['kind'] == 'http_end' for r in events) != 1:
        raise ValueError(f'{request_id}/{direction}: missing unique http_end')
    raw, chunks, trailers = bytearray(), [], []
    previous_ns = int(metadata['arrival_ns'])
    for record in events:
        ns = int(record['arrival_ns'])
        if ns < previous_ns:
            raise ValueError(f'{request_id}/{direction}: backwards body arrival clock')
        previous_ns = ns
        if type(record.get('offset')) is not int or record['offset'] != len(raw):
            raise ValueError(f'{request_id}/{direction}: offset mismatch')
        if record['kind'] == 'http_chunk':
            if 'body_b64' in record:
                data = decode(record['body_b64'])
                if type(record.get('bytes')) is not int or record['bytes'] != len(data):
                    raise ValueError('chunk bytes mismatch')
                raw.extend(data)
                chunks.append((len(raw), int(record['arrival_ns'])))
            if 'trailers' in record:
                trailers.extend(record['trailers'])
    for length in headers(metadata).get('content-length', []):
        if not re.fullmatch(rb'[0-9]+', length) or int(length) != len(raw):
            raise ValueError('Content-Length disagrees with complete body')
    return bytes(raw), chunks, trailers


def sse_events(raw, chunks, start_ns, out, location, side):
    """Parse SSE lines; every dispatch remains separate. BOM/CR/LF per SSE grammar."""
    try:
        text = raw.decode('utf-8')
    except UnicodeError:
        issue(out, 'sse.complete', location, 'invalid UTF-8 event stream', side=side)
        return []
    # Retain byte offsets: an event arrives only when its final blank line arrives.
    cursor = 3 if text.startswith('\ufeff') else 0
    raw = raw[cursor:]
    lines = list(re.finditer(rb'([^\r\n]*)(\r\n|\r|\n)', raw))
    consumed, data, event, last_id, retry, extensions = 0, [], '', '', None, []
    events = []
    for line in lines:
        consumed = line.end()
        value = line[1].decode('utf-8')
        if value == '':
            if data:
                offset = cursor + consumed
                arrival = next((time for end, time in chunks if end >= offset), None)
                if arrival is None:
                    issue(out, 'sse.complete', location, 'event has no arrival evidence', side=side)
                    arrival = start_ns
                events.append({'event': event or 'message', 'id': last_id, 'retry': retry,
                               'data': '\n'.join(data), 'extensions': extensions, 'arrival_ns': arrival - start_ns})
            data, event, extensions = [], '', []
            continue
        if value.startswith(':'):
            continue  # comments are heartbeat diagnostics, not dispatches
        name, separator, content = value.partition(':')
        content = content[1:] if content.startswith(' ') else content
        if not separator:
            content = ''
        if name == 'data':
            data.append(content)
        elif name == 'event':
            event = content
        elif name == 'id':
            if '\0' in content:
                issue(out, 'sse.complete', location, 'NUL in SSE resume id', side=side)
            last_id = content
        elif name == 'retry':
            if not re.fullmatch(r'[0-9]+', content):
                issue(out, 'sse.complete', location, 'invalid retry', side=side)
            retry = content
        else:
            extensions.append([name, content])
    if consumed != len(raw) or data or event or extensions:
        issue(out, 'sse.complete', location, 'unterminated SSE event/line at EOF', side=side)
    if not events:
        issue(out, 'sse.complete', location, 'empty SSE stream', side=side)
    return events


def validate_wire(record):
    raw = decode(record['wire_b64'])
    if len(raw) < 2:
        raise ValueError('incomplete WS wire header')
    length, cursor = raw[1] & 127, 2
    if length in (126, 127):
        size = 2 if length == 126 else 8
        if len(raw) < cursor + size:
            raise ValueError('incomplete WS extended length')
        length = int.from_bytes(raw[cursor:cursor + size], 'big')
        cursor += size
    masked = bool(raw[1] & 128)
    if masked:
        if len(raw) < cursor + 4:
            raise ValueError('incomplete WS mask')
        mask = raw[cursor:cursor + 4]
        cursor += 4
    if length > 16 * 1024 * 1024 or len(raw) != cursor + length:
        raise ValueError('WS wire length mismatch')
    payload = raw[cursor:]
    if masked:
        payload = bytes(value ^ mask[i % 4] for i, value in enumerate(payload))
    if (record['fin'] != bool(raw[0] & 128) or record['rsv'] != ((raw[0] >> 4) & 7)
            or record['opcode'] != (raw[0] & 15) or record['masked'] != masked
            or decode(record['payload_b64']) != payload):
        raise ValueError('WS wire contradicts decoded frame')
    if record['opcode'] == 8:
        if len(payload) < 2 or int.from_bytes(payload[:2], 'big') != 1000:
            raise ValueError('six-scenario WS close is not clean 1000')
        payload[2:].decode('utf-8')
    if (record['opcode'] >= 8 and (not record['fin'] or length > 125)
            or (record['dir'] == 'c2u') != masked):
        raise ValueError('invalid control frame/masking at client boundary')


class Side:
    def __init__(self, directory, label, enumeration, matrix, manifest, out, recorded_apparatus=None):
        self.directory, self.label, self.out = Path(directory), label, out
        self.norm = Normalizer(label, out)
        self.http, self.ws, self.sse, self.disk = {}, {}, {}, []
        self.types = {}
        self.pty_diagnostics = {}
        self.meta = read_json(self.directory / 'run-meta.json')
        if (type(self.meta.get('schema')) is not int or self.meta.get('schema') != 1 or self.meta.get('observer_port') != 13102 or self.meta.get('mode') not in ('direct', 'proxy')
                or not isinstance(self.meta.get('fixture_path'), str) or not self.meta['fixture_path'].startswith('/')
                or not isinstance(self.meta.get('run_id'), str) or not self.meta['run_id']):
            raise ValueError('missing/invalid run-meta')
        expected_apparatus = apparatus_hashes() if recorded_apparatus is None else recorded_apparatus
        if self.meta.get('synthetic') is not True and self.meta.get('apparatus_sha256') != expected_apparatus:
            raise ValueError('real recording apparatus/spec hash mismatch')
        self.norm.wall_bounds = [self.meta['wall_start_ms'], self.meta['wall_end_ms']]
        if not all(type(v) is int and v > 0 for v in self.norm.wall_bounds) or self.norm.wall_bounds[0] > self.norm.wall_bounds[1]:
            raise ValueError('invalid run wall bounds')
        self.records = read_jsonl(self.directory / 'recording.jsonl')
        self.validate_log()
        scenarios = read_json(self.directory / 'scenarios.json')
        required = {s['id'] for s in manifest['scenarios']}
        if set(scenarios) != required or not required or any(s.get('status') != 'PASS' for s in scenarios.values()):
            issue(out, 'evidence.integrity', label + '.scenarios', 'missing/failed scenario')
        self.norm.fixture_path = self.meta['fixture_path']
        audit_path = self.directory / 'audit.json'
        audit = read_json(audit_path) if audit_path.exists() else {'processes': []}
        self.norm.ui_pid = next((p['pid'] for p in audit['processes'] if p.get('name') == 'ui'), None)
        self.norm.gateway_pid = next((p['pid'] for p in audit['processes'] if p.get('name') == 'gateway'), None)
        self.load_activities()
        self.actual_snapshot = capture(self.directory)
        self.norm.seed_chat_identities(self.actual_snapshot['entries'])
        self.verify_auth()
        self.load_upload_expiries()
        self.load_http(enumeration, matrix, manifest)
        self.load_ws()
        saved = read_json(self.directory / 'snapshot.json')
        actual = self.actual_snapshot
        diff(saved, actual, out, 'disk.snapshot', label + '.snapshot.archive')
        if saved.get('schema') != 1 or saved.get('roots') != ['home', 'project'] or not saved.get('entries'):
            issue(out, 'disk.snapshot', label, 'missing/empty snapshot')
        for root in ('home', 'project'):
            if not any(e['path'].startswith(root + '/') and e['kind'] != 'directory' for e in actual['entries']):
                issue(out, 'disk.snapshot', label + '.' + root, 'empty snapshot root')
        paths = set()
        for item in actual['entries']:
            path = item['path']
            if path in paths:
                issue(out, 'disk.snapshot', label, 'duplicate snapshot path')
            paths.add(path)
            entry = dict(item)
            context = path
            match = re.fullmatch(r'home/\.tmp/chat-uploads/([^/]+)(/.*)?', path)
            if match:
                entry['path'] = 'home/.tmp/chat-uploads/' + self.norm.ref('upload', match[1], 'disk.path') + (match[2] or '')
                if match[2] == '/metadata.json':
                    context = '@upload'
            if re.fullmatch(r'home/projects/exp008-fixture/chats/[^/]+\.jsonl', path):
                context = '@chat'
            if re.fullmatch(r'home/projects/exp008-fixture/chats/[^/]+\.jsonl\.projections\.json', path):
                context = '@projection'
            if path == 'home/runs/registry.json':
                context = '@registry'
                entry['data'] = self.registry(entry['data'])
            if 'data' in entry:
                entry['data'] = self.norm.value(entry['data'], 'disk', context, location=f'disk.{path}.data')
            self.disk.append(entry)

    def registry(self, data):
        result = dict(data)
        events = []
        records = data.get('records', [])
        for key, entries in data.get('events', {}).items():
            parts = key.split('\0')
            if len(parts) != 3 or not any(parts == [r.get('projectKey'), r.get('sessionKey'), r.get('runId')] for r in records):
                raise ValueError('registry event key contradicts recorded project/session/run')
            mapped = parts[0] + '\0' + self.norm.ref('session', parts[1], 'disk.registry.events.key.session') + '\0' + self.norm.ref('run', parts[2], 'disk.registry.events.key.run')
            events.append({'key': mapped, 'entries': entries})
        result['events'] = sorted(events, key=lambda e: e['key'])
        return result

    def validate_log(self):
        records = self.records
        if (not records or records[0].get('kind') != 'run_start' or records[-1].get('kind') != 'run_end'
                or type(records[0].get('schema')) is not int or records[0].get('schema') != 1
                or sum(r.get('kind') == 'run_start' for r in records) != 1 or sum(r.get('kind') == 'run_end' for r in records) != 1):
            raise ValueError('missing unique schema-1 start/end recording')
        allowed = {'run_start', 'run_end', 'http_request', 'http_response', 'http_chunk', 'http_end',
                   'http_body_error', 'http_aborted', 'recorder_error', 'ws_frame', 'ws_end', 'ws_partial'}
        expected_upstream = 'http://127.0.0.1:' + ('13102' if self.meta['mode'] == 'direct' else '13103')
        if records[0].get('listen') != '127.0.0.1:13101' or records[0].get('upstream') != expected_upstream:
            raise ValueError('recorded topology disagrees with run mode')
        previous_ns = -1
        for seq, record in enumerate(records, 1):
            if type(record.get('seq')) is not int or record['seq'] != seq:
                issue(self.out, 'ws.sequence' if record.get('kind', '').startswith('ws_') else 'evidence.integrity',
                      f'{self.label}.recording[{seq - 1}].seq', 'noncontinuous/out-of-order seq')
            if record.get('kind') not in allowed:
                raise ValueError('unknown recorder kind')
            ns = record.get('arrival_ns')
            if not isinstance(ns, str) or not re.fullmatch(r'[0-9]+', ns):
                raise ValueError('invalid arrival_ns')
            previous_ns = int(ns)
            if record.get('kind') in ('recorder_error', 'http_body_error', 'http_aborted', 'ws_partial'):
                issue(self.out, 'evidence.integrity', f'{self.label}.recording[{seq - 1}]', record['kind'])
            if record.get('kind') == 'ws_end' and record.get('error'):
                issue(self.out, 'evidence.integrity', f'{self.label}.ws_end', 'WS transport error')

    def load_activities(self):
        observations = read_jsonl(self.directory / 'activity-direct.jsonl')
        if not observations:
            raise ValueError('empty out-of-band observer')
        anchors, previous_seq, previous_ns = [], -1, -1
        users, connections = set(), set()
        for record in observations:
            if (record.get('out_of_band') is not True or record.get('port') != self.meta['observer_port']
                    or record.get('port') in (13101, 13103) or record.get('user_id') is None):
                raise ValueError('observer does not prove bypass/user scope')
            if type(record.get('seq')) is not int or record['seq'] <= previous_seq:
                raise ValueError('invalid observer seq')
            previous_seq = record['seq']
            ns = record.get('arrival_ns')
            if not isinstance(ns, str) or not re.fullmatch(r'[0-9]+', ns) or int(ns) < previous_ns:
                raise ValueError('invalid observer arrival clock')
            previous_ns = int(ns)
            users.add(record['user_id'])
            connections.add(record['connection'])
            if record['kind'] in ('ws_error', 'parse_error'):
                raise ValueError('observer error')
            frame = None
            if record['kind'] == 'activity_anchor':
                anchors.append(record.get('phase'))
                frame = record['frame']
                if record.get('phase') == 'after-scenarios':
                    self.norm.final_snapshot = frame.get('activities')
            elif record['kind'] == 'ws_frame' and record.get('dir') == 'u2c' and record.get('opcode') == 'text':
                frame = parse(decode(record['payload_b64']))
            if not isinstance(frame, dict):
                continue
            activities = ([frame.get('activity')] if frame.get('type') == 'session-activity'
                          else frame.get('activities', []) if frame.get('type') == 'session-activity-snapshot' else [])
            for item in activities:
                if (not isinstance(item, dict) or not isinstance(item.get('sessionId'), str)
                        or type(item.get('revision')) is not int or item['revision'] <= 0 or type(item.get('processing')) is not bool):
                    raise ValueError('malformed out-of-band activity')
                key = (item['sessionId'], item['revision'])
                if key in self.norm.activities and self.norm.activities[key] != item:
                    raise ValueError('observer contradicts same per-session revision')
                self.norm.activities[key] = item
        # Old archives retain their original phase name for explicit replay.
        chat_phase = 'after-serial-chat' if 'after-serial-chat' in anchors else 'after-concurrent-chat'
        expected_phases = {'before-scenarios', chat_phase, 'after-watch-reconnect', 'after-scenarios'}
        if set(anchors) != expected_phases or len(anchors) != len(expected_phases) or len(users) != 1 or len(connections) != 1:
            raise ValueError('missing/duplicated observer phases or ambiguous user/connection scope')
        self.observer_user = next(iter(users))
        by_session = defaultdict(list)
        for session, revision in self.norm.activities:
            by_session[session].append(revision)
        if not by_session:
            raise ValueError('observer has no activity evidence')
        for session, revisions in by_session.items():
            for ordinal, revision in enumerate(sorted(revisions), 1):
                self.norm.revisions[(session, revision)] = ordinal

    def load_upload_expiries(self):
        for entry in self.actual_snapshot['entries']:
            match = re.fullmatch(r'home/\.tmp/chat-uploads/([^/]+)/metadata\.json', entry['path'])
            if not match:
                continue
            data = entry.get('data')
            if not isinstance(data, dict) or data.get('uploadId') != match[1]:
                raise ValueError('upload metadata ID does not match directory')
            created = self.norm.clock_instant(data.get('createdAt'), ISO_MS, 'disk.upload.createdAt')
            expiry = self.norm.clock_instant(data.get('expiresAt'), ISO_MS, 'disk.upload.expiresAt', wall=False)
            if created is None or expiry is None:
                continue
            if not self.norm.wall_bounds[0] <= created <= self.norm.wall_bounds[1] or expiry <= created:
                raise ValueError('upload creation/lifetime outside real fixture bounds')
            self.norm.expiries[match[1]] = {'raw': data['expiresAt'], 'ttl_ms': expiry - created}

    def verify_auth(self):
        db_entries = [e for e in self.actual_snapshot['entries'] if e['path'] == 'home/auth.db']
        if not db_entries:
            if self.meta.get('synthetic') is not True:
                raise ValueError('real six-scenario fixture missing auth.db')
            return
        tables = db_entries[0]['data']['tables']
        secrets = [r['value'] for r in tables['app_config']['rows'] if r['key'] == 'jwt_secret']
        if len(secrets) != 1 or not re.fullmatch(r'[0-9a-f]{128}', secrets[0]):
            raise ValueError('missing/malformed JWT secret')
        secret = secrets[0]
        self.jwt_secret = secret
        credentials = {}
        for request in self.records:
            if request['kind'] == 'http_request' and request.get('case') == 'auth.register':
                raw, _, _ = complete_body(self.records, request['id'], 'request', request)
                data = parse(raw)
                credentials[data['username']] = data['password']
        passwords = [{'hash': row['password_hash'], 'password': credentials.get(row['username'], '')}
                     for row in tables['users']['rows']]
        vapid = tables.get('vapid_keys', {}).get('rows', [])
        baseline = self.meta.get('baseline_path')
        if not baseline or not passwords:
            raise ValueError('auth snapshot lacks verifier baseline/registered users')
        proc = subprocess.run(['node', str(HERE / 'verify_auth.mjs'), baseline],
                              input=json.dumps({'passwords': passwords, 'vapid': vapid}),
                              text=True, capture_output=True, timeout=15)
        if proc.returncode:
            raise ValueError('offline bcrypt/VAPID verifier failed (no secrets emitted)')
        flags = json.loads(proc.stdout)
        if flags.get('passwords') != [True] * len(passwords) or flags.get('vapid') != [True] * len(vapid):
            issue(self.out, 'disk.snapshot', self.label + '.auth.db', 'password/VAPID cryptographic validation failed')
        else:
            self.norm.password_hashes = {r['hash'] for r in passwords}
            self.norm.vapid_keys = {r[k] for r in vapid for k in ('public_key', 'private_key')}
        self.norm.auth_secret = secret
        self.norm.auth_secret_rows = {i for i, row in enumerate(tables['app_config']['rows']) if row['key'] == 'jwt_secret'}

    def validate_token(self, value, user, location):
        if not hasattr(self, 'jwt_secret'):
            return  # synthetic minimal fixture; check_ruler forbids synthetic calibration
        try:
            a, b, signature = value.split('.')
            decoded = lambda s: base64.b64decode(s + '=' * (-len(s) % 4), altchars=b'-_', validate=True)
            header, claims = json.loads(decoded(a)), json.loads(decoded(b))
            expected = hmac.new(self.jwt_secret.encode(), (a + '.' + b).encode(), hashlib.sha256).digest()
            if (header != {'alg': 'HS256', 'typ': 'JWT'} or not hmac.compare_digest(decoded(signature), expected)
                    or claims.get('userId') != user['id'] or claims.get('username') != user['username']
                    or type(claims.get('iat')) is not int or type(claims.get('exp')) is not int
                    or claims['exp'] - claims['iat'] != 604800
                    or not self.norm.wall_bounds[0] // 1000 - 1 <= claims['iat'] <= self.norm.wall_bounds[1] // 1000 + 1):
                raise ValueError('JWT signature/claims/lifetime invalid')
            normalized = dict(claims)
            normalized['userId'] = self.norm.ref('user', claims['userId'], location + '.claims.userId')
            normalized['iat'] = '<JWT-IAT>'
            normalized['exp'] = '<JWT-IAT+604800>'
            self.norm.auth_tokens[value] = {'header': header, 'claims': normalized}
        except (ValueError, KeyError, TypeError, AttributeError, binascii.Error):
            issue(self.out, 'normalization.field', location, 'JWT signature/claims/lifetime validation failed', side=self.label)

    def load_http(self, enumeration, matrix, manifest):
        self.requests, responses = {}, {}
        for record in self.records:
            table = self.requests if record['kind'] == 'http_request' else responses if record['kind'] == 'http_response' else None
            if table is not None:
                if record['id'] in table:
                    raise ValueError('duplicate request/response id')
                table[record['id']] = record
        if not self.requests or set(self.requests) != set(responses):
            raise ValueError('empty or unmatched HTTP requests/responses')
        for record in self.records:
            if record['kind'] in ('http_chunk', 'http_end') and (record.get('id') not in self.requests or record.get('direction') not in ('request', 'response')):
                raise ValueError('orphan/invalid-direction HTTP body record')
        definitions = {c['id']: c for c in manifest['http_cases']}
        evidence, observed_rust = [], set()
        occurrences = defaultdict(int)
        self.connection_keys = {}
        self.shell_connections = set()
        clients = read_jsonl(self.directory / 'cases.jsonl')
        for request_id, request in self.requests.items():
            response = responses[request_id]
            if (type(request_id) is not int or request_id <= 0 or type(response.get('status')) is not int
                    or not 100 <= response['status'] < 600 or not isinstance(request.get('method'), str)
                    or not re.fullmatch(r'[A-Z]+', request['method']) or not isinstance(request.get('path'), str)
                    or not request['path'].startswith('/') or request.get('query') is not None and not isinstance(request['query'], str)):
                raise ValueError('malformed HTTP metadata')
            if response['seq'] <= request['seq']:
                raise ValueError('HTTP response precedes request')
            case = request.get('case') or f'_probe:{request["method"]}:{request["path"]}'
            ordinal = occurrences[case]
            occurrences[case] += 1
            key = f'{case}#{ordinal}'
            location = f'http[{key}]'
            for metadata in (request, response):
                headers(metadata)  # validate lossless header schema
            raw_handler = headers(response).get('x-pilotdeck-handler', [])
            expected_handler = response.get('handler')
            if (len(raw_handler) > 1 or raw_handler not in ([], [b'ts'], [b'rust'])
                    or expected_handler != (raw_handler[0].decode() if raw_handler else None)):
                issue(self.out, 'coverage.handler', location, 'marker metadata/header inconsistent', side=self.label)
            if self.meta['mode'] == 'proxy' and expected_handler not in ('ts', 'rust'):
                issue(self.out, 'coverage.handler', location, 'proxy missing handler proof', side=self.label)
            if self.meta['mode'] == 'direct' and expected_handler is not None:
                issue(self.out, 'coverage.handler', location, 'direct fixture unexpectedly handled by proxy', side=self.label)
            is_ws = response['status'] == 101
            if is_ws:
                if type(response.get('connection')) is not int or response['connection'] <= 0 or response['connection'] in self.connection_keys:
                    raise ValueError('missing/duplicate WS connection handshake')
                self.connection_keys[response['connection']] = key
                if request['path'] == '/shell':
                    self.shell_connections.add(key)
                req_headers, res_headers = headers(request), headers(response)
                nonce = req_headers.get('sec-websocket-key', [])
                accept = res_headers.get('sec-websocket-accept', [])
                if len(nonce) != 1 or accept != [base64.b64encode(hashlib.sha1(nonce[0] + b'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest())]:
                    issue(self.out, 'http.headers', location, 'invalid WS accept challenge', side=self.label)
                req_raw, _, req_trailers = complete_body(self.records, request_id, 'request', request)
                if any(r['kind'] in ('http_chunk', 'http_end') and r.get('id') == request_id and r.get('direction') == 'response' for r in self.records):
                    raise ValueError('WS 101 response unexpectedly contains HTTP body records')
                res_raw, res_trailers, chunks = b'', [], []
            else:
                req_raw, _, req_trailers = complete_body(self.records, request_id, 'request', request)
                res_raw, chunks, res_trailers = complete_body(self.records, request_id, 'response', response)
            if case in definitions:
                definition = definitions[case]
                status_ok = response['status'] in definition['expected_statuses']
                if not status_ok or not matches(definition['route'], request['method'], request['path']):
                    issue(self.out, 'coverage.matrix', location, 'recorder case does not match manifest/status', side=self.label)
                found = [r for r in clients if r.get('kind') == 'case_http' and r.get('case') == case]
                if not any(r.get('status') == response['status'] and r.get('method') == request['method']
                           and r.get('path', '').split('?', 1)[0] == request['path'] and r.get('handler') == expected_handler
                           and r.get('ok') is True and r.get('route') == definition['route'] for r in found):
                    issue(self.out, 'coverage.matrix', location, 'client evidence does not corroborate recorder', side=self.label)
                evidence.append({'kind': 'case_http', 'side': 'direct' if self.label == 'left' else 'proxy',
                                 'case': case, 'route': definition['route'], 'method': request['method'],
                                 'path': request['path'], 'status': response['status'], 'handler': expected_handler, 'ok': status_ok})
            if expected_handler == 'rust':
                routes = {row['route'] for row in matrix['routes'] if matches(row['route'], request['method'], request['path'])}
                if case in definitions:
                    routes = {definitions[case]['route']}
                if not routes:
                    issue(self.out, 'coverage.handler', location, 'Rust request absent from matrix', side=self.label)
                observed_rust.update(routes)
            req_body = parse(req_raw)
            res_body = parse(res_raw)
            if case == '_probe:GET:/health' and (response['status'] != 200 or not isinstance(res_body, dict) or res_body.get('status') != 'ok'):
                issue(self.out, 'evidence.integrity', location, 'health readiness failed', side=self.label)
            if response['status'] < 400 and isinstance(res_body, dict) and res_body.get('error'):
                issue(self.out, 'evidence.integrity', location + '.body.error', 'unexpected success-status application error', side=self.label)
            if case in ('auth.register', 'auth.login') and response['status'] == 200:
                if not isinstance(res_body, dict) or res_body.get('success') is not True or not res_body.get('token') or res_body.get('user', {}).get('id') != self.observer_user:
                    issue(self.out, 'activity.out-of-band', location, 'auth user does not match observer', side=self.label)
            for metadata, value in ((request, req_body), (response, res_body)):
                if any(v.split(b';', 1)[0].strip().lower() == b'application/json' for v in headers(metadata).get('content-type', [])) and value is NOT_JSON:
                    issue(self.out, 'evidence.integrity', location, 'invalid declared JSON body', side=self.label)
            if case in ('auth.register', 'auth.login') and response['status'] == 200 and isinstance(res_body, dict):
                self.validate_token(res_body.get('token'), res_body.get('user', {}), location + '.body.token')
            content_types = headers(response).get('content-type', [])
            is_sse = any(value.split(b';', 1)[0].strip().lower() == b'text/event-stream' for value in content_types)
            if is_sse:
                events = sse_events(res_raw, chunks, int(request['arrival_ns']), self.out, location, self.label)
                normalized_events = []
                if case == 'upload.events' and (len(events) < 2 or events[0]['event'] != 'upload_started' or events[-1]['event'] != 'upload_completed' or any(e['event'] not in ('upload_started', 'upload_progress', 'upload_completed') for e in events)):
                    issue(self.out, 'evidence.integrity', location + '.sse', 'upload lifecycle did not start/complete successfully', side=self.label)
                for index, item in enumerate(events):
                    value = parse(item['data'])
                    if case == 'upload.events':
                        upload_id = request['path'].split('/')[3]
                        if not isinstance(value, dict) or value.get('uploadId') != upload_id:
                            issue(self.out, 'evidence.integrity', f'{location}.sse[{index}]', 'upload event JSON/id does not match request', side=self.label)
                    entry = {**item, 'data': self.norm.value(value, 'sse', case + ':' + item['event'], location=f'{location}.sse[{index}].data') if value is not NOT_JSON else item['data']}
                    if not 0 <= item['arrival_ns'] <= SSE_ABSOLUTE_NS:
                        issue(self.out, 'sse.delay', f'{location}.sse[{index}]', 'event outside absolute arrival bound', side=self.label)
                    normalized_events.append(entry)
                self.sse[key] = normalized_events
                res_body = '<SSE:compared-separately>'
            self.http[key] = {
                'request': {'method': request['method'], 'path': normalize_path(request['path'], self.norm, location + '.request.path'),
                            'query': request.get('query'), 'headers': normalize_headers(request, req_raw, self.norm, location + '.request.headers', request=True, case=case, handshake=is_ws),
                            'body': req_body if req_body is not NOT_JSON else {'raw_b64': base64.b64encode(req_raw).decode()}, 'trailers': literal_headers(req_trailers)},
                'status': response['status'],
                'headers': normalize_headers(response, res_raw, self.norm, location + '.headers', case=case, handshake=is_ws),
                'body': self.norm.value(res_body, 'http', case, location=location + '.body') if res_body is not NOT_JSON else {'raw_b64': base64.b64encode(res_raw).decode()},
                'trailers': literal_headers(res_trailers),
                '_raw': res_raw, '_etag': headers(response).get('etag', []),
            }
        for case in definitions:
            count = occurrences[case]
            if not count or sum(r.get('kind') == 'case_http' and r.get('case') == case for r in clients) != count:
                issue(self.out, 'coverage.matrix', f'{self.label}.cases.{case}', 'missing/duplicate recorder or client HTTP evidence')
        self.evidence = evidence
        for row in matrix['routes']:
            route = row['route']
            if (row.get('handler') == 'rust' and self.meta['mode'] == 'proxy') or route in observed_rust:
                waiver = row.get('exemption')
                if waiver and 'AI 代批' in waiver.get('approver', ''):
                    issue(self.out, 'coverage.handler', route, 'Rust exemption requires human approval', side=self.label)
                if not row.get('cases') and not waiver:
                    issue(self.out, 'coverage.handler', route, 'Rust route has no case/exemption', side=self.label)
                for case in row.get('cases', []):
                    if not any(e['case'] == case and e['handler'] == 'rust' for e in evidence):
                        issue(self.out, 'coverage.handler', f'{route}[{case}]', 'declared Rust route lacks Rust-handled direct HTTP case', side=self.label)

    def load_ws(self):
        previous = {}
        pending = {}
        messages = defaultdict(list)
        # Connection traversal is canonical, not global arrival order. That
        # keeps shared ID assignment independent of cross-connection scheduling.
        frames = [r for r in self.records if r['kind'] == 'ws_frame' and r.get('leg') in ('downstream-read', 'downstream-write')]
        for frame in frames:
            if type(frame.get('connection')) is not int or frame['connection'] not in self.connection_keys:
                raise ValueError('WS frame has no valid connection handshake')
        frames.sort(key=lambda r: (self.connection_keys[r['connection']], r['seq']))
        for record in frames:
            connection = record['connection']
            if connection not in self.connection_keys:
                raise ValueError('WS frame has no matching handshake')
            key = self.connection_keys[connection]
            handshake = next(r for r in self.records if r['kind'] == 'http_response' and r.get('connection') == connection)
            if record['seq'] <= handshake['seq']:
                raise ValueError('WS frame precedes handshake')
            if record['seq'] <= previous.get(connection, -1):
                issue(self.out, 'ws.sequence', f'ws[{key}].seq', 'connection seq does not strictly increase')
            previous[connection] = record['seq']
            validate_wire(record)
            raw = decode(record['payload_b64'])
            expected_dir = 'c2u' if record['leg'] == 'downstream-read' else 'u2c'
            if record.get('dir') != expected_dir or type(record.get('bytes')) is not int or record['bytes'] != len(raw):
                raise ValueError('WS direction/byte count contradicts boundary')
            if (type(record.get('fin')) is not bool or type(record.get('masked')) is not bool
                    or type(record.get('rsv')) is not int or record['rsv'] != 0
                    or type(record.get('opcode')) is not int or record['opcode'] not in (0, 1, 2, 8, 9, 10)):
                raise ValueError('malformed WS frame metadata')
            body = parse(raw) if record['opcode'] == 1 and record['fin'] else NOT_JSON
            if isinstance(body, dict) and (body.get('kind') == 'error' or body.get('type') == 'error'):
                issue(self.out, 'evidence.integrity', f'ws[{key}]', 'unexpected WS application error', side=self.label)
            if isinstance(body, dict) and body.get('kind') == 'complete' and (body.get('success') is not True or body.get('finishReason') != 'completed'):
                issue(self.out, 'evidence.integrity', f'ws[{key}]', 'mock turn completion is not successful', side=self.label)
            entry = {name: record[name] for name in ('dir', 'opcode', 'fin', 'rsv', 'masked')}
            entry['body'] = self.norm.ws(body, f'ws[{key}][{len(self.ws.get(key, []))}].body') if body is not NOT_JSON else {'raw_b64': record['payload_b64']}
            entry['type'] = body.get('type', body.get('kind')) if isinstance(body, dict) else None
            self.ws.setdefault(key, []).append(entry)
            message_key = (connection, record['dir'])
            if record['opcode'] in (1, 2):
                if message_key in pending:
                    issue(self.out, 'ws.sequence', f'ws[{key}]', 'new data message interrupts continuation')
                pending[message_key] = (record['opcode'], bytearray(raw), [entry])
            elif record['opcode'] == 0:
                if message_key not in pending:
                    issue(self.out, 'ws.sequence', f'ws[{key}]', 'continuation has no initial data frame')
                else:
                    pending[message_key][1].extend(raw)
                    pending[message_key][2].append(entry)
            else:
                messages[key].append({'dir': record['dir'], 'opcode': record['opcode'],
                                      'body': entry['body'], 'frames': [entry]})
            if record['opcode'] in (0, 1, 2) and record['fin'] and message_key in pending:
                opcode, message, wire_frames = pending.pop(message_key)
                parsed = parse(bytes(message)) if opcode == 1 else NOT_JSON
                normalized = entry['body'] if record['opcode'] == 1 else self.norm.ws(parsed, f'ws[{key}].message') if parsed is not NOT_JSON else {'raw_b64': base64.b64encode(message).decode()}
                kind = parsed.get('type', parsed.get('kind')) if isinstance(parsed, dict) else None
                if record['opcode'] == 0 and isinstance(parsed, dict) and (kind == 'error' or kind == 'complete' and (parsed.get('success') is not True or parsed.get('finishReason') != 'completed')):
                    issue(self.out, 'evidence.integrity', f'ws[{key}]', 'fragmented application message is an error/failed completion', side=self.label)
                self.types.setdefault(key, {}).setdefault(str(kind), []).append({'dir': record['dir'], 'body': normalized})
                messages[key].append({'dir': record['dir'], 'opcode': opcode,
                                      'body': normalized, 'frames': wire_frames})
        if pending:
            issue(self.out, 'ws.sequence', self.label + '.ws', 'unfinished fragmented message')
        for connection, key in self.connection_keys.items():
            ends = [r for r in self.records if r['kind'] == 'ws_end' and r.get('connection') == connection]
            sequence = self.ws.get(key, [])
            if len(ends) == 1 and ends[0]['seq'] <= previous.get(connection, -1):
                issue(self.out, 'evidence.integrity', f'ws[{key}]', 'WS end precedes final frame', side=self.label)
            if len(ends) != 1 or not sequence or not any(f['dir'] == 'u2c' and f['opcode'] in (1, 2) for f in sequence):
                issue(self.out, 'evidence.integrity', f'ws[{key}]', 'missing WS completion/application evidence', side=self.label)
            for direction in ('c2u', 'u2c'):
                if not any(f['opcode'] == 8 and f['dir'] == direction for f in sequence):
                    issue(self.out, 'evidence.integrity', f'ws[{key}]', 'missing bidirectional close', side=self.label)
        if not self.connection_keys:
            issue(self.out, 'evidence.integrity', self.label + '.ws', 'empty WS evidence')
        for key in self.shell_connections:
            view, diagnostics = shell_stream(messages[key], self.ws.get(key, []), self.out, f'ws[{key}]', self.label)
            self.ws[key] = view
            # Never reintroduce output chunk counts in the by-type hard gate.
            self.types[key] = view
            self.pty_diagnostics[key] = diagnostics


def compare_sides(left, right, enumeration, matrix=None, scenarios=None, *, recorded_apparatus=None,
                  _prepared=None, _registered=False):
    out = []
    sides = []
    try:
        enumeration = read_json(enumeration) if not isinstance(enumeration, dict) else enumeration
        matrix = read_json(matrix or HERE / 'coverage.json') if not isinstance(matrix, dict) else matrix
        manifest = read_json(scenarios or HERE / 'scenarios.json') if not isinstance(scenarios, dict) else scenarios
        declaration = check_matrix(enumeration, matrix, manifest)
        for error in declaration['errors']:
            issue(out, 'coverage.matrix', 'matrix', error)
        for index, row in enumerate(matrix['routes']):
            if 'handler' in row and row['handler'] not in ('ts', 'rust'):
                issue(out, 'coverage.handler', f'matrix.routes[{index}].handler', 'invalid declared handler (expected ts/rust)')
        sides = []
        for label, directory in (('left', left), ('right', right)):
            try:
                if _prepared is None:
                    sides.append(Side(directory, label, enumeration, matrix, manifest, out, recorded_apparatus))
                else:
                    side, errors = _prepared[len(sides)]
                    sides.append(side)
                    out.extend(errors)
            except (OSError, ValueError, TypeError, KeyError, AttributeError, binascii.Error, subprocess.TimeoutExpired, sqlite3.Error) as error:
                issue(out, 'evidence.integrity', label, f'{type(error).__name__}: {error}')
        if len(sides) == 2:
            a, b = sides
            if a.meta['mode'] != 'direct':
                issue(out, 'evidence.integrity', 'left.mode', 'left reference must be TS direct')
            diff(a.meta['fixture_path'], b.meta['fixture_path'], out, 'evidence.integrity', 'fixture_path')
            coverage = check_matrix(enumeration, matrix, manifest,
                                    [dict(e, side='direct') for e in a.evidence] +
                                    [dict(e, side='proxy') for e in b.evidence])
            for error in coverage['errors']:
                issue(out, 'coverage.matrix', 'matrix.evidence', error)
            for key in sorted(a.http.keys() | b.http.keys()):
                location = f'http[{key}]'
                if key not in a.http or key not in b.http:
                    issue(out, 'http.request', location, 'request missing')
                    continue
                x, y = a.http[key], b.http[key]
                diff(x['request'], y['request'], out, 'http.request', location + '.request')
                diff(x['status'], y['status'], out, 'http.status', location + '.status')
                h1, h2 = dict(x['headers']), dict(y['headers'])
                if x['_raw'] == y['_raw']:
                    # Equal bytes => equal ETag literally, even a body-valid one.
                    if x['_etag'] or y['_etag']:
                        h1['etag'] = [base64.b64encode(v).decode() for v in x['_etag']]
                        h2['etag'] = [base64.b64encode(v).decode() for v in y['_etag']]
                if x['_raw'] != y['_raw']:
                    for side_label, entry in (('left', x), ('right', y)):
                        if any(not valid_etag(tag, entry['_raw']) for tag in entry['_etag']):
                            issue(out, 'http.headers', location + '.headers.etag', 'differing wire body requires body-valid ETag', side=side_label)
                diff(h1, h2, out, 'http.headers', location + '.headers')
                diff(x['body'], y['body'], out, 'http.body', location + '.body')
                diff(x['trailers'], y['trailers'], out, 'http.headers', location + '.trailers')
            # /shell uses directional output streams; other WS retain full ordering.
            diff(a.ws, b.ws, out, 'ws.sequence', 'ws')
            diff(a.types, b.types, out, 'ws.structure', 'ws.by_type')
            for key in sorted(a.sse.keys() | b.sse.keys()):
                if key not in a.sse or key not in b.sse:
                    issue(out, 'sse.sequence', f'sse[{key}]', 'stream missing')
                    continue
                left_events, right_events = a.sse[key], b.sse[key]
                diff([{k: v for k, v in event.items() if k != 'arrival_ns'} for event in left_events],
                     [{k: v for k, v in event.items() if k != 'arrival_ns'} for event in right_events], out, 'sse.sequence', f'sse[{key}]')
                for index, (x, y) in enumerate(zip(left_events, right_events)):
                    # Direct/direct calibration is symmetric; direct/proxy checks added latency.
                    delta = y['arrival_ns'] - x['arrival_ns']
                    if b.meta['mode'] == 'direct':
                        delta = abs(delta)
                    if delta > SSE_DELAY_NS:
                        issue(out, 'sse.delay', f'sse[{key}][{index}].arrival_ns', 'event arrival exceeds direct + 250ms',
                              left=x['arrival_ns'], right=y['arrival_ns'], bound_ns=SSE_DELAY_NS)
            if not a.sse or not b.sse:
                issue(out, 'sse.complete', 'sse', 'empty required SSE evidence')
            diff(a.disk, b.disk, out, 'disk.snapshot', 'disk')
            # Shared mapping is checked by every normalized reference; report maps for diagnostics.
    except (OSError, ValueError, TypeError, KeyError, AttributeError, sqlite3.Error) as error:
        issue(out, 'evidence.integrity', 'inputs', f'{type(error).__name__}: {error}')
    registered_differences = []
    if _registered and len(sides) == 2:
        from multimodal import registered_fields, UNIT_LOCATION
        try:
            units = None if _registered is True else _registered
            _, pa = registered_fields(sides[0], units)
            _, pb = registered_fields(sides[1], units)
            registered_differences = [d for d in out if (d['rule'], d['location']) in pa & pb
                                      and d['message'] == 'value differs']
            out = [d for d in out if d not in registered_differences]
        except (ValueError, TypeError, KeyError, AttributeError) as error:
            issue(out, 'multimodal.membership', UNIT_LOCATION, str(error))
    return {'verdict': 'FAIL' if out else 'PASS', 'spec_status': 'DRAFT',
            'registered_differences': registered_differences,
            'calibration_eligible': recorded_apparatus is None,
            'comparison_mode': 'calibration' if recorded_apparatus is None else 'historical-replay',
            'left': str(left), 'right': str(right), 'differences': out,
            'rules': sorted({item['rule'] for item in out}),
            'pty_chunk_diagnostics': {label: side.pty_diagnostics for label, side in zip(('left', 'right'), sides)},
            'sse_bounds_ns': {'added': SSE_DELAY_NS, 'absolute': SSE_ABSOLUTE_NS}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('left', type=Path)
    parser.add_argument('right', type=Path)
    parser.add_argument('--enumeration', type=Path, required=True)
    parser.add_argument('--matrix', type=Path)
    parser.add_argument('--scenarios', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--reference-pool', nargs='+', type=Path, help='Required same-day TS-only reference pool (>=20)')
    parser.add_argument('--paired-only', action='store_true', help='Strict paired diagnostic without class acceptance')
    parser.add_argument('--replay-pool-of', type=Path, help='Offline pool replay bound to an original ruler report; never current calibration')
    args = parser.parse_args()
    if ((args.reference_pool is None and (not args.paired_only or args.replay_pool_of))
            or (args.reference_pool is not None and args.paired_only)):
        report = {'verdict': 'REFUSED', 'spec_status': 'DRAFT', 'calibration_eligible': False,
                  'errors': ['requires --reference-pool; --paired-only is mutually exclusive and diagnostic only']}
    elif args.reference_pool is not None:
        from multimodal import compare_with_pool
        report = compare_with_pool(args.left, args.right, args.enumeration, args.reference_pool, args.matrix, args.scenarios,
                                   replay_pool_of=args.replay_pool_of)
    else:
        report = compare_sides(args.left, args.right, args.enumeration, args.matrix, args.scenarios)
        report.update(calibration_eligible=False, comparison_mode='paired-diagnostic')
    raw = json.dumps(report, ensure_ascii=False, indent=2) + '\n'
    if args.out:
        args.out.write_text(raw)
    print(raw, end='')
    return 2 if report['verdict'] == 'REFUSED' else int(report['verdict'] != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())
