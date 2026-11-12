"""Strict eight-v2 business replay. Receipts and source identities belong to the caller."""
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
import hashlib
import json
import re
import shutil
from urllib.parse import quote, urlsplit

atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)

CHANNELS = {'telegram', 'discord', 'slack', 'dingtalk', 'feishu', 'weixin', 'qq', 'wecom'}
FIXED_TS = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
contracts = _load('retirement_public_http_contracts', _apparatus.parent.parent /
                  'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/contracts.py')
SCOPE = ('two-text-rounds', 'permission-continuation', 'elicitation-continuation',
         'public-reload', 'current-turn-public-history', 'post-reap-business-files')


def require(ok, rule):
    if not ok:
        raise ValueError('CHANNEL_V2_' + rule)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False)


def indexed(values, index, rule):
    require(type(index) is int and 0 <= index < len(values), rule)
    return values[index]


def payload(value):
    data = bytes.fromhex(value['rawBytesHex'])
    require(value.get('truncated') is False and type(value['bytes']) is int
            and len(data) == value['bytes'], 'TRANSPORT_BYTES')
    return data


def decoded(data):
    try:
        return json.loads(data)
    except (ValueError, UnicodeDecodeError):
        return {'raw': data.decode('utf-8')}


def text(message):
    value = message.get('content')
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return '\n'.join(row.get('text', '') for row in value if row.get('type') == 'text')
    return ''


def _feishu_event(data):
    # The actual fixture protobuf uses field 8 for its JSON event. Parse wire
    # lengths, rather than searching an arbitrary byte string for a marker.
    offset, events = 0, []
    def varint():
        nonlocal offset
        result = 0
        for shift in range(0, 70, 7):
            require(offset < len(data), 'FEISHU_FRAME')
            byte = data[offset]
            offset += 1
            result |= (byte & 127) << shift
            if byte < 128:
                return result
        raise ValueError('CHANNEL_V2_FEISHU_FRAME')
    while offset < len(data):
        key = varint()
        if key & 7 == 0:
            varint()
        else:
            require(key & 7 == 2, 'FEISHU_FRAME')
            size = varint()
            require(offset + size <= len(data), 'FEISHU_FRAME')
            value = data[offset:offset + size]
            offset += size
            if key >> 3 == 8:
                events.append(json.loads(value))
    require(len(events) == 1, 'FEISHU_EVENT')
    return events[0]


def delivery_identity(channel, frame):
    if channel in ('discord', 'qq'):
        value = frame['d']
        require(frame['t'] == ('MESSAGE_CREATE' if channel == 'discord' else 'GROUP_AT_MESSAGE_CREATE'), 'DELIVERY_KIND')
        require(value.get('channel_id' if channel == 'discord' else 'group_openid') == ('42' if channel == 'discord' else 'group'), 'DELIVERY_RECIPIENT')
        return value['id'], value['content']
    if channel == 'wecom':
        require(frame['cmd'] == 'aibot_msg_callback' and frame['body']['chatid'] == 'alice'
                and frame['body']['from']['userid'] == 'alice', 'DELIVERY_RECIPIENT')
        return frame['headers']['req_id'], frame['body']['text']['content']
    if channel == 'telegram':
        require(frame['message']['chat']['id'] == 42 and frame['message']['from']['id'] == 7, 'DELIVERY_RECIPIENT')
        return str(frame['message']['message_id']), frame['message']['text']
    if channel == 'weixin':
        require(frame['from_user_id'] == 'chat' and frame['context_token'] == 'ctx', 'DELIVERY_RECIPIENT')
        return None, frame['item_list'][0]['text_item']['text']
    if channel == 'slack':
        event = frame['payload']['event']
        require(event['channel'] == 'Droom' and event['user'] == 'HUMAN', 'DELIVERY_RECIPIENT')
        return frame['envelope_id'], event['text']
    if channel == 'dingtalk':
        event = json.loads(frame['data'])
        require(event['conversationId'] == 'room' and event['sessionWebhook'] == 'https://api.dingtalk.com/lifecycle', 'DELIVERY_RECIPIENT')
        return event['msgId'], event['text']['content']
    event = _feishu_event(bytes.fromhex(frame['rawBytesHex']))['event']
    require(event['message']['chat_id'] == 'room' and event['sender']['sender_id']['open_id'] == 'human', 'DELIVERY_RECIPIENT')
    return event['message']['message_id'], json.loads(event['message']['content'])['text']


def transport_observation(raw, records):
    require(isinstance(records, list) and records, 'TRANSPORT_MISSING')
    sequences = [row['sequence'] for row in records]
    require(all(type(value) is int for value in sequences)
            and sequences == sorted(set(sequences)), 'TRANSPORT_SEQUENCE')
    requests, responses, channel_sent, channel_received, control_sent, control_received = {}, {}, [], [], [], []
    ws_sockets = {}
    mock, control = raw['runtime']['mockPort'], raw['runtime']['controlPort']
    require(type(mock) is type(control) is int and 0 < mock < 65536 and 0 < control < 65536 and mock != control, 'PORTS')
    for row in records:
        kind = row['kind']
        if 'body' in row:
            payload(row['body'])
        if 'payload' in row:
            payload(row['payload'])
        if kind in ('mock-http-request', 'mock-http-response'):
            target = requests if kind == 'mock-http-request' else responses
            require(type(row['id']) is int and row['id'] not in target, 'HTTP_DUPLICATE')
            target[row['id']] = row
        elif kind in ('fixture-ws-send', 'fixture-ws-receive'):
            socket = row['socket']
            require((socket.get('localPort') == mock) != (socket.get('remotePort') == control), 'WS_SOCKET')
            identity = socket.get('id')
            require(type(identity) is int and identity >= 0, 'WS_SOCKET_IDENTITY')
            endpoint = tuple(socket.get(key) for key in ('localHost', 'localPort', 'remoteHost', 'remotePort'))
            require(ws_sockets.setdefault(identity, endpoint) == endpoint, 'WS_SOCKET_IDENTITY')
            target = ((channel_sent if kind == 'fixture-ws-send' else channel_received)
                      if socket.get('localPort') == mock else
                      (control_sent if kind == 'fixture-ws-send' else control_received))
            target.append(row)
        elif kind.endswith('-error'):
            require(False, 'TRANSPORT_ERROR')
    require(requests.keys() == responses.keys(), 'HTTP_PAIRS')
    http, models, exchanges = [], [], []
    for identity, request in requests.items():
        response = responses[identity]
        require(response.get('finished') is True and type(response['status']) is int
                and request['sequence'] < response['sequence'], 'HTTP_RESPONSE')
        body = decoded(payload(request['body']))
        http.append({'method': request['method'], 'path': urlsplit(request['url']).path,
                     'body': body, 'authorization': request['headers'].get('authorization')})
        if urlsplit(request['url']).path.startswith('/v1/'):
            models.append(body)
        exchanges.append({'request': request, 'response': response})
    require(http == [{key: row[key] for key in ('method', 'path', 'body', 'authorization')}
                     for row in raw['http']], 'HTTP_ORIGINALS')
    require(models == raw['models'], 'MODEL_ORIGINALS')
    actual_wire = [({'binary': base64.b64encode(payload(row['payload'])).decode()}
                    if row.get('binary') else json.loads(payload(row['payload']))) for row in channel_received]
    require(actual_wire == raw['wire'], 'WIRE_ORIGINALS')
    require([json.loads(payload(row['payload'])) for row in control_received] == raw['controlFrames'], 'CONTROL_ORIGINALS')
    return {'exchanges': exchanges, 'channelSent': channel_sent, 'channelReceived': channel_received,
            'controlSent': control_sent, 'controlReceived': control_received}


def owned_tcp_streams(raw, records):
    """Keep original bytes; complete unknown endpoints only from real connect."""
    mock, control = raw['runtime']['mockPort'], raw['runtime']['controlPort']
    require(type(mock) is type(control) is int and 0 < mock < 65536
            and 0 < control < 65536 and mock != control, 'PORTS')
    sequences = [row['sequence'] for row in records]
    require(all(type(value) is int for value in sequences)
            and sequences == sorted(set(sequences)), 'TRANSPORT_SEQUENCE')
    fields = ('localHost', 'localPort', 'remoteHost', 'remotePort')
    grouped = {}
    for row in records:
        if row.get('kind') not in ('fixture-tcp-data', 'fixture-tcp-write', 'fixture-tcp-connect'):
            continue
        socket = row['socket']
        require(isinstance(socket, dict), 'TCP_SOCKET_IDENTITY')
        identity = socket.get('id')
        require(type(identity) is int and identity >= 0, 'TCP_SOCKET_IDENTITY')
        require(all(key in socket for key in fields), 'TCP_SOCKET_ENDPOINT')
        for key in fields:
            value = socket[key]
            require(value is None or (type(value) is int and 0 < value < 65536
                    if key.endswith('Port') else isinstance(value, str) and bool(value)), 'TCP_SOCKET_ENDPOINT')
        grouped.setdefault(identity, []).append(row)
    streams = {}
    for identity, rows in grouped.items():
        connected = [row for row in rows if row['kind'] == 'fixture-tcp-connect']
        require(len(connected) <= 1, 'TCP_CONNECT_IDENTITY')
        if connected:
            connect = connected[0]
            require(all(connect['socket'][key] is not None for key in fields), 'TCP_CONNECT_ENDPOINT')
            endpoint = {key: connect['socket'][key] for key in ('id', *fields)}
        else:
            connect = None
            endpoint = {key: rows[0]['socket'][key] for key in ('id', *fields)}
            require(all(endpoint[key] is not None for key in fields), 'TCP_CONNECT_REQUIRED')
        partial = []
        for row in rows:
            socket = row['socket']
            require(all(socket[key] is None or socket[key] == endpoint[key] for key in fields),
                    'TCP_SOCKET_IDENTITY')
            if any(socket[key] is None for key in fields):
                require(connect is not None and row['sequence'] < connect['sequence'], 'TCP_CONNECT_REQUIRED')
                partial.append(row['sequence'])
        server, client = endpoint['localPort'] == mock, endpoint['remotePort'] == control
        require(server != client, 'TCP_SOCKET_SCOPE')
        role = 'mock-server' if server else 'control-client'
        stream = {'socket': endpoint, 'role': role, 'readChunks': [], 'writeChunks': [],
                  'endpointProof': {'connectSequence': None if connect is None else connect['sequence'],
                                    'preConnectSequences': partial}}
        for row in rows:
            if row['kind'] == 'fixture-tcp-connect':
                continue
            direction = 'readChunks' if row['kind'] == 'fixture-tcp-data' else 'writeChunks'
            stream[direction].append((row['sequence'], payload(row['payload'])))
        stream['read'] = b''.join(data for _, data in stream['readChunks'])
        stream['write'] = b''.join(data for _, data in stream['writeChunks'])
        streams[identity] = stream
    return streams


def _tcp_span(chunks, start, end):
    offset, sequences = 0, []
    for sequence, data in chunks:
        if offset < end and offset + len(data) > start:
            sequences.append(sequence)
        offset += len(data)
    require(sequences and start < end, 'HTTP_WIRE_SPAN')
    return min(sequences), max(sequences)


def http_wire_observation(raw, records, trace=None):
    """Bind every mock HTTP record to its actual server socket FIFO exchange.

    Upgrade offsets explicitly hand the untouched remaining bytes to the WS
    verifier. This function does not certify those frames or formal acceptance.
    """
    trace = transport_observation(raw, records) if trace is None else trace
    streams = owned_tcp_streams(raw, records)
    expected, responses = {}, {}
    for pair in trace['exchanges']:
        request, response = pair['request'], pair['response']
        socket = request['socket']
        identity = socket.get('id')
        require(type(identity) is int and identity >= 0 and identity in streams, 'HTTP_WIRE_SOCKET')
        stream = streams[identity]
        require(stream['role'] == 'mock-server'
                and all(socket.get(key) == value for key, value in stream['socket'].items()), 'HTTP_WIRE_SOCKET')
        require(request['id'] not in responses, 'HTTP_DUPLICATE')
        responses[request['id']] = response
        expected.setdefault(identity, []).append(pair)
    bound, upgrades = {}, []
    for identity, stream in streams.items():
        if stream['role'] != 'mock-server':
            continue
        sent, received = stream['read'], stream['write']
        request_offset = response_offset = count = 0
        upgraded = False
        pairs = expected.get(identity, [])
        while request_offset < len(sent):
            request_start = request_offset
            line, headers, body_start = contracts.http_head(sent, request_offset)
            match = re.fullmatch(r"([!#$%&'*+.^_`|~0-9A-Za-z-]+) ([^\s]+) (HTTP/1\.[01])", line)
            require(match is not None, 'HTTP_WIRE_REQUEST_LINE')
            method, target, version = match.groups()
            body, trailers, request_offset = contracts.http_entity(sent, body_start, headers, request=True)
            request_wire = {'line': line, 'method': method, 'target': target, 'version': version,
                            'headers': headers, 'body': body, 'trailers': trailers,
                            'headerBytes': sent[request_start:body_start],
                            'startOffset': request_start, 'endOffset': request_offset,
                            'span': _tcp_span(stream['readChunks'], request_start, request_offset)}
            response_start = response_offset
            informational = []
            while True:
                header_start = response_offset
                response_line, response_headers, response_body_start = contracts.http_head(received, response_offset)
                status_match = re.fullmatch(r'HTTP/1\.[01] ([1-5][0-9]{2}) [^\r\n]*', response_line)
                require(status_match is not None, 'HTTP_WIRE_STATUS_LINE')
                status = int(status_match.group(1))
                if status == 101:
                    require(not informational, 'HTTP_WIRE_INFORMATIONAL_UPGRADE')
                    require(method == 'GET' and version == 'HTTP/1.1' and not body and not trailers
                            and [value.lower() for value in headers.get('upgrade', [])] == ['websocket']
                            and [value.lower() for value in response_headers.get('upgrade', [])] == ['websocket']
                            and count == len(pairs), 'HTTP_WIRE_UNEXPECTED_UPGRADE')
                    response_offset = response_body_start
                    upgrades.append({'socket': identity, 'role': stream['role'], 'request': request_wire,
                                     'response': {'line': response_line, 'status': status, 'headers': response_headers,
                                                  'headerBytes': received[header_start:response_body_start],
                                                  'startOffset': response_start, 'endOffset': response_offset,
                                                  'span': _tcp_span(stream['writeChunks'], response_start, response_offset)},
                                     'requestOffset': request_offset, 'responseOffset': response_offset})
                    upgraded = True
                    break
                response_body, response_trailers, response_offset = contracts.http_entity(
                    received, response_body_start, response_headers,
                    bodyless=method == 'HEAD' or status in (204, 304) or 100 <= status < 200)
                if status < 200:
                    informational.append({'line': response_line, 'headers': response_headers})
                    continue
                break
            if upgraded:
                break
            require(not headers.get('upgrade'), 'HTTP_WIRE_UNEXPECTED_UPGRADE')
            require(count < len(pairs), 'HTTP_WIRE_UNCLAIMED_EXCHANGE')
            pair = pairs[count]
            request, response = pair['request'], pair['response']
            count += 1
            require(method == request['method'] and target == request['url']
                    and body == payload(request['body']), 'HTTP_WIRE_REQUEST')
            raw_headers = request['rawHeaders']
            require(len(raw_headers) % 2 == 0, 'HTTP_REQUEST_HEADERS')
            captured_headers = contracts.header_fields([f'{key}: {value}'.encode('latin-1')
                for key, value in zip(raw_headers[::2], raw_headers[1::2])])
            require(headers == captured_headers, 'HTTP_WIRE_REQUEST_HEADERS')
            # request/end is appended after its parser has received the body;
            # response/finish is appended after Node has queued its raw writes.
            response_span = _tcp_span(stream['writeChunks'], response_start, response_offset)
            require(request_wire['span'][1] < request['sequence'] < response['sequence']
                    and response_span[1] < response['sequence'], 'HTTP_WIRE_EVENT_ORDER')
            actual_head = response['actualHeaderText'].encode('latin-1')
            require(received[header_start:response_body_start] == actual_head
                    and status == response['status'] and response_body == payload(response['body'])
                    and response.get('finished') is True, 'HTTP_WIRE_RESPONSE')
            for key, value in response['headers'].items():
                require(response_headers.get(key.lower()) == [str(item) for item in
                        (value if isinstance(value, list) else [value])], 'HTTP_WIRE_RESPONSE_HEADERS')
            bound[request['id']] = {'socket': identity, 'request': request_wire,
                                   'response': {'line': response_line, 'status': status, 'headers': response_headers,
                                                'headerBytes': received[header_start:response_body_start],
                                                'body': response_body, 'trailers': response_trailers,
                                                'informational': informational, 'startOffset': response_start,
                                                'endOffset': response_offset, 'span': response_span}}
        require(count == len(pairs), 'HTTP_WIRE_MISSING_EXCHANGE')
        require(upgraded or response_offset == len(received), 'HTTP_WIRE_UNCLAIMED_RESPONSE')
    require(len(bound) == len(trace['exchanges']), 'HTTP_WIRE_MISSING_EXCHANGE')
    return {'exchanges': [bound[pair['request']['id']] for pair in trace['exchanges']],
            'upgrades': upgrades, 'streams': streams, 'formalAcceptance': False}


def _ws_tokens(headers, key):
    values = [value.strip() for line in headers.get(key, []) for value in line.split(',')]
    require(all(values), 'WS_WIRE_HEADER_TOKEN')
    return values


def _ws_upgrade(stream, request_start, response_start):
    """Read the actual two HTTP prefixes, preserving all handshake bytes."""
    server = stream['role'] == 'mock-server'
    request_bytes, response_bytes = ((stream['read'], stream['write']) if server
                                     else (stream['write'], stream['read']))
    request_chunks, response_chunks = ((stream['readChunks'], stream['writeChunks']) if server
                                       else (stream['writeChunks'], stream['readChunks']))
    request_line, request_headers, request_end = contracts.http_head(request_bytes, request_start)
    response_line, response_headers, response_end = contracts.http_head(response_bytes, response_start)
    match = re.fullmatch(r'GET ([^\s]+) HTTP/1\.1', request_line)
    require(match is not None and re.fullmatch(r'HTTP/1\.1 101 [^\r\n]*', response_line), 'WS_WIRE_UPGRADE_LINE')
    body, trailers, entity_end = contracts.http_entity(request_bytes, request_end, request_headers, request=True)
    require(not body and not trailers and entity_end == request_end
            and not request_headers.get('transfer-encoding')
            and not response_headers.get('transfer-encoding')
            and not response_headers.get('content-length'), 'WS_WIRE_UPGRADE_BODY')
    require(len(request_headers.get('host', [])) == 1 and bool(request_headers['host'][0])
            and request_headers.get('sec-websocket-version') == ['13'], 'WS_WIRE_UPGRADE_REQUEST')
    for headers in (request_headers, response_headers):
        require([value.lower() for value in headers.get('upgrade', [])] == ['websocket']
                and 'upgrade' in [value.lower() for value in _ws_tokens(headers, 'connection')],
                'WS_WIRE_UPGRADE_CONNECTION')
    keys = request_headers.get('sec-websocket-key', [])
    require(len(keys) == 1, 'WS_WIRE_UPGRADE_KEY')
    try:
        key_bytes = base64.b64decode(keys[0], validate=True)
        accept = base64.b64encode(hashlib.sha1(
            (keys[0] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode('ascii')).digest()).decode('ascii')
    except (ValueError, UnicodeError):
        require(False, 'WS_WIRE_UPGRADE_KEY')
    require(len(key_bytes) == 16, 'WS_WIRE_UPGRADE_KEY')
    require(response_headers.get('sec-websocket-accept') == [accept], 'WS_WIRE_UPGRADE_ACCEPT')
    protocols = _ws_tokens(response_headers, 'sec-websocket-protocol')
    require(not protocols or (len(protocols) == 1
            and protocols[0] in _ws_tokens(request_headers, 'sec-websocket-protocol')), 'WS_WIRE_UPGRADE_PROTOCOL')
    # The owned fixtures do not enable a server extension. A selected extension
    # needs its actual negotiated codec, never an invented decompression step.
    require(not response_headers.get('sec-websocket-extensions'), 'WS_WIRE_UNSUPPORTED_EXTENSION')
    request_span = _tcp_span(request_chunks, request_start, request_end)
    response_span = _tcp_span(response_chunks, response_start, response_end)
    require(request_span[1] < response_span[0], 'WS_WIRE_UPGRADE_ORDER')
    return {'socket': stream['socket']['id'], 'role': stream['role'],
            'request': {'line': request_line, 'target': match.group(1), 'headers': request_headers,
                        'headerBytes': request_bytes[request_start:request_end],
                        'startOffset': request_start, 'endOffset': request_end, 'span': request_span},
            'response': {'line': response_line, 'headers': response_headers,
                         'headerBytes': response_bytes[response_start:response_end],
                         'startOffset': response_start, 'endOffset': response_end, 'span': response_span},
            'requestOffset': request_end, 'responseOffset': response_end}


def _ws_frames(data, chunks, offset, masked, direction):
    """Decode complete TCP bytes without replacing their original frame headers."""
    frames, messages, pending = [], [], None
    closed = False
    while offset < len(data):
        start = offset
        require(len(data) - offset >= 2, 'WS_WIRE_TRUNCATED_HEADER')
        first, second = data[offset:offset + 2]
        offset += 2
        fin, opcode, mask, length_code = bool(first & 128), first & 15, bool(second & 128), second & 127
        require(first & 112 == 0, 'WS_WIRE_RSV')
        require(opcode in (0, 1, 2, 8, 9, 10), 'WS_WIRE_OPCODE')
        require(mask == masked, 'WS_WIRE_MASK')
        size = length_code
        if length_code in (126, 127):
            count = 2 if length_code == 126 else 8
            require(len(data) - offset >= count, 'WS_WIRE_TRUNCATED_LENGTH')
            size = int.from_bytes(data[offset:offset + count], 'big')
            require(length_code != 127 or size < 1 << 63, 'WS_WIRE_LENGTH')
            require(size >= (126 if length_code == 126 else 65536), 'WS_WIRE_NONMINIMAL_LENGTH')
            offset += count
        if opcode >= 8:
            require(fin and size <= 125, 'WS_WIRE_CONTROL')
        mask_key = b''
        if mask:
            require(len(data) - offset >= 4, 'WS_WIRE_TRUNCATED_MASK')
            mask_key = data[offset:offset + 4]
            offset += 4
        header_end = offset
        require(size <= len(data) - offset, 'WS_WIRE_TRUNCATED_PAYLOAD')
        wire_payload = data[offset:offset + size]
        value = (bytes(byte ^ mask_key[index % 4] for index, byte in enumerate(wire_payload))
                 if mask else wire_payload)
        offset += size
        span = _tcp_span(chunks, start, offset)
        frame = {'direction': direction, 'opcode': opcode, 'fin': fin, 'rsv': (first >> 4) & 7,
                 'masked': mask, 'lengthCode': length_code, 'payloadLength': size,
                 'wireHeaderHex': data[start:header_end].hex(), 'maskKeyHex': mask_key.hex(),
                 'wireHex': data[start:offset].hex(),
                 'wirePayloadHex': wire_payload.hex(), 'payload': value,
                 'startOffset': start, 'endOffset': offset, 'span': span}
        frames.append(frame)
        if opcode >= 8:
            if opcode == 8:
                require(len(value) != 1, 'WS_WIRE_CLOSE')
                if value:
                    code = int.from_bytes(value[:2], 'big')
                    require((1000 <= code <= 1014 and code not in (1004, 1005, 1006))
                            or 3000 <= code <= 4999, 'WS_WIRE_CLOSE')
                    try:
                        value[2:].decode('utf-8')
                    except UnicodeError:
                        require(False, 'WS_WIRE_CLOSE_UTF8')
                closed = True
            continue
        require(not closed, 'WS_WIRE_DATA_AFTER_CLOSE')
        if opcode in (1, 2):
            require(pending is None, 'WS_WIRE_FRAGMENT_START')
            pending = {'binary': opcode == 2, 'parts': [], 'frames': [], 'startSpan': span}
        else:
            require(pending is not None, 'WS_WIRE_CONTINUATION')
        pending['parts'].append(value)
        pending['frames'].append(len(frames) - 1)
        if fin:
            message = b''.join(pending['parts'])
            if not pending['binary']:
                try:
                    message.decode('utf-8')
                except UnicodeError:
                    require(False, 'WS_WIRE_TEXT_UTF8')
            messages.append({'payload': message, 'binary': pending['binary'], 'frames': pending['frames'],
                             'span': (pending['startSpan'][0], span[1])})
            pending = None
    require(pending is None, 'WS_WIRE_FRAGMENT_INCOMPLETE')
    return {'frames': frames, 'messages': messages}


def ws_wire_observation(raw, records, http_state=None):
    """Bind every WS API event to an authenticated upgrade and actual TCP frames.

    Raw mask bytes, extended lengths, fragmentation and all control frames are
    retained. This scratch verifier does not change normalization or acceptance.
    """
    actual_http = http_wire_observation(raw, records)
    require(http_state is None or http_state == actual_http, 'WS_WIRE_HTTP_BINDING')
    streams, upgrades = actual_http['streams'], {}
    for deferred in actual_http['upgrades']:
        identity = deferred['socket']
        require(identity not in upgrades and identity in streams, 'WS_WIRE_UPGRADE_SOCKET')
        upgrade = _ws_upgrade(streams[identity], deferred['request']['startOffset'],
                              deferred['response']['startOffset'])
        require(upgrade['requestOffset'] == deferred['requestOffset']
                and upgrade['responseOffset'] == deferred['responseOffset']
                and upgrade['request']['headerBytes'] == deferred['request']['headerBytes']
                and upgrade['response']['headerBytes'] == deferred['response']['headerBytes'], 'WS_WIRE_HTTP_BINDING')
        upgrades[identity] = upgrade
    for identity, stream in streams.items():
        if stream['role'] == 'control-client':
            require(identity not in upgrades, 'WS_WIRE_UPGRADE_SOCKET')
            upgrades[identity] = _ws_upgrade(stream, 0, 0)
    api = {identity: {'send': [], 'receive': [], 'url': None} for identity in upgrades}
    for row in records:
        if row['kind'] not in ('fixture-ws-send', 'fixture-ws-receive'):
            continue
        identity = row['socket'].get('id')
        require(identity in upgrades and all(row['socket'].get(key) == value
                for key, value in streams[identity]['socket'].items()), 'WS_WIRE_API_SOCKET')
        if streams[identity]['role'] == 'control-client':
            value = row['socket'].get('url')
            require(isinstance(value, str) and bool(value), 'WS_WIRE_API_TARGET')
            url = urlsplit(value)
            target = (url.path or '/') + ('?' + url.query if url.query else '')
            require(url.scheme == 'ws' and url.hostname in ('127.0.0.1', 'localhost', '::1')
                    and url.port == streams[identity]['socket']['remotePort']
                    and not url.username and not url.password and not url.fragment
                    and target == upgrades[identity]['request']['target'], 'WS_WIRE_API_TARGET')
            require(api[identity]['url'] is None or api[identity]['url'] == value, 'WS_WIRE_API_TARGET')
            api[identity]['url'] = value
        api[identity]['send' if row['kind'] == 'fixture-ws-send' else 'receive'].append(row)
    observations = []
    for identity, upgrade in upgrades.items():
        stream = streams[identity]
        server = stream['role'] == 'mock-server'
        directions = {}
        for direction in ('read', 'write'):
            client_to_server = (direction == 'read') == server
            offset = upgrade['requestOffset'] if client_to_server else upgrade['responseOffset']
            parsed = _ws_frames(stream[direction], stream[direction + 'Chunks'], offset,
                                client_to_server, direction)
            if parsed['frames']:
                first_span = parsed['frames'][0]['span']
                require(first_span[0] > upgrade['response']['span'][1] if client_to_server else
                        first_span[0] >= upgrade['response']['span'][1], 'WS_WIRE_FRAME_BEFORE_UPGRADE')
            directions[direction] = parsed
        sent = [frame for frame in directions['write']['frames'] if frame['opcode'] < 8]
        received = directions['read']['messages']
        require(len(sent) == len(api[identity]['send'])
                and len(received) == len(api[identity]['receive']), 'WS_WIRE_API_COUNT')
        bindings = []
        for frame, event in zip(sent, api[identity]['send']):
            require(payload(event['payload']) == frame['payload'], 'WS_WIRE_API_PAYLOAD')
            require(upgrade['response']['span'][1] < event['sequence'] < frame['span'][0], 'WS_WIRE_API_ORDER')
            bindings.append({'kind': 'send', 'sequence': event['sequence'], 'span': frame['span']})
        for message, event in zip(received, api[identity]['receive']):
            require(payload(event['payload']) == message['payload'], 'WS_WIRE_API_PAYLOAD')
            require(type(event.get('binary')) is bool and event['binary'] == message['binary'], 'WS_WIRE_API_BINARY')
            require(message['span'][1] < event['sequence'], 'WS_WIRE_API_ORDER')
            bindings.append({'kind': 'receive', 'sequence': event['sequence'], 'span': message['span']})
        # A TCP event can hold several frames. Preserve its within-stream byte
        # offsets as well as cross-direction event order; do not invent timing.
        interleaving = sorted([frame for parsed in directions.values() for frame in parsed['frames']],
                              key=lambda frame: (frame['span'][1], frame['span'][0], frame['endOffset']))
        observations.append({'socket': stream['socket'], 'role': stream['role'], 'url': api[identity]['url'], 'upgrade': upgrade,
                             'directions': directions, 'interleaving': interleaving,
                             'apiBindings': sorted(bindings, key=lambda item: item['sequence'])})
    return {'connections': observations, 'formalAcceptance': False}


def _deliveries(raw, channel, trace):
    rows, seen, identities, sequences = raw['deliveries'], set(), [], []
    require([row for row in rows if row.get('initial') is True] == raw['inbound'], 'INITIAL_DELIVERIES')
    require([row['generation'] for row in raw['inbound']] == [0, 1], 'INITIAL_GENERATIONS')
    for row in rows:
        require(type(row['generation']) is int and row['generation'] in (0, 1)
                and type(row['initial']) is bool and isinstance(row['text'], str)
                and isinstance(row['frames'], list) and len(row['frames']) == 1, 'DELIVERY_SHAPE')
        frame = row['frames'][0]
        identity, incoming = delivery_identity(channel, frame['frame'])
        require(incoming == row['text'], 'DELIVERY_TEXT')
        if row['initial']:
            require(incoming == f"CHANNEL_INPUT_{row['generation']}", 'INITIAL_TEXT')
        else:
            require(incoming == '1', 'ANSWER_TEXT')
        if channel in ('telegram', 'weixin'):
            require(frame['transport'] == 'poll', 'DELIVERY_TRANSPORT')
            pair = indexed(trace['exchanges'], frame['httpIndex'], 'DELIVERY_HTTP_INDEX')
            response = json.loads(payload(pair['response']['body']))
            values = response['result'] if channel == 'telegram' else response['msgs']
            require(values == [frame['frame']], 'DELIVERY_POLL_ORIGINAL')
            sequence = pair['response']['sequence']
        else:
            require(frame['transport'] == 'websocket', 'DELIVERY_TRANSPORT')
            expected = (bytes.fromhex(frame['frame']['rawBytesHex']) if channel == 'feishu'
                        else canonical(frame['frame']))
            matches = [item for item in trace['channelSent']
                       if (payload(item['payload']) if channel == 'feishu' else canonical(json.loads(payload(item['payload'])))) == expected]
            require(len(matches) == 1, 'DELIVERY_WIRE_ORIGINAL')
            sequence = matches[0]['sequence']
        require(sequence not in seen, 'DELIVERY_REUSED')
        seen.add(sequence)
        identities.append(identity)
        sequences.append(sequence)
        if channel in ('discord', 'qq'):
            require(row.get('messageId') == identity, 'DELIVERY_ID')
        if channel == 'wecom':
            require(row.get('reqId') == identity, 'DELIVERY_ID')
    require(sequences == sorted(sequences), 'DELIVERY_ORDER')
    return identities, sequences


def _feishu_output(raw, trace, index, reply_id):
    """An edit inherits its recipient from its observed successful creation."""
    row = raw['http'][index]
    pair = trace['exchanges'][index]
    body, path = row['body'], row['path']
    base = '/open-apis/im/v1/messages'
    deliveries = [delivery for delivery in raw['deliveries']
                  if any(delivery_identity('feishu', frame['frame'])[0] == reply_id for frame in delivery['frames'])]
    require(len(deliveries) == 1 and type(row.get('generation')) is int
            and row['generation'] == deliveries[0]['generation'], 'FEISHU_OUTBOUND_GENERATION')
    reply = json.loads(payload(pair['response']['body']))
    require(type(reply.get('code')) is int and reply['code'] == 0, 'FEISHU_RESPONSE')
    if row['method'] == 'POST':
        recipient = (path == base and body.get('receive_id') == 'room'
                     and urlsplit(pair['request']['url']).query == 'receive_id_type=chat_id')
    else:
        require(row['method'] == 'PUT' and path.startswith(base + '/'), 'FEISHU_EDIT_PATH')
        creators = []
        for previous_index, previous in enumerate(raw['http']):
            if previous.get('method') != 'POST' or previous.get('path') != base:
                continue
            original = trace['exchanges'][previous_index]
            if original['response']['sequence'] >= pair['request']['sequence']:
                continue
            response = json.loads(payload(original['response']['body']))
            message_id = response.get('data', {}).get('message_id')
            if (200 <= original['response']['status'] < 300 and type(response.get('code')) is int
                    and response['code'] == 0 and isinstance(message_id, str) and message_id
                    and path == base + '/' + quote(message_id, safe="~()*!.'-")):
                creators.append((previous, original))
        # The fixed fixture historically reused a message ID. Every observed
        # definition must agree on recipient, and this round must have its own
        # successful definition; neither a foreign chat nor an old round can
        # supply authority for an update.
        require(creators and all(previous['body'].get('receive_id') == 'room'
                                and urlsplit(original['request']['url']).query == 'receive_id_type=chat_id'
                                for previous, original in creators)
                and any(type(previous.get('generation')) is int
                        and previous['generation'] == row['generation'] for previous, _ in creators), 'FEISHU_EDIT_RECIPIENT')
        recipient = (not urlsplit(pair['request']['url']).query
                     and ('receive_id' not in body or body['receive_id'] == 'room'))
    content = json.loads(body.get('content', '{}'))
    if body.get('msg_type') == 'text':
        output = content.get('text')
    elif body.get('msg_type') == 'post':
        output = '\n'.join(item.get('text', '') for line in content.get('zh_cn', {}).get('content', [])
                           for item in line if item.get('tag') == 'text')
    else:
        output = None
    return recipient, output


def _outbound(raw, channel, trace, reference, marker, reply_id=None):
    source, index = reference['source'], reference['index']
    if source == 'http':
        row = indexed(raw['http'], index, 'OUTBOUND_INDEX')
        pair = trace['exchanges'][index]
        require((row['method'] == 'POST' or channel == 'feishu' and row['method'] == 'PUT')
                and not row['path'].startswith('/v1/')
                and 200 <= pair['response']['status'] < 300, 'OUTBOUND_RESPONSE')
        body, path = row['body'], row['path']
        if channel == 'telegram':
            recipient, output = path.endswith('/sendMessage') and str(body.get('chat_id')) == '42', body.get('text')
        elif channel == 'discord':
            recipient, output = path == '/api/v10/channels/42/messages', body.get('content')
        elif channel == 'slack':
            recipient, output = path == '/api/chat.postMessage' and body.get('channel') == 'Droom', body.get('text')
        elif channel == 'dingtalk':
            recipient, output = path == '/lifecycle', body.get('markdown', {}).get('text')
        elif channel == 'feishu':
            recipient, output = _feishu_output(raw, trace, index, reply_id)
        elif channel == 'weixin':
            message = body.get('msg', {})
            recipient = path == '/ilink/bot/sendmessage' and message.get('to_user_id') == 'chat' and message.get('context_token') == 'ctx'
            output = '\n'.join(item.get('text_item', {}).get('text', '') for item in message.get('item_list', []))
        elif channel == 'qq':
            recipient = path == '/v2/groups/group/messages' and body.get('msg_id') == reply_id
            output = body.get('content')
        else:
            require(False, 'OUTBOUND_TRANSPORT')
        sequence = pair['request']['sequence']
    else:
        require(source == 'wire' and channel == 'wecom', 'OUTBOUND_TRANSPORT')
        row = indexed(raw['wire'], index, 'OUTBOUND_INDEX')
        recipient = row.get('cmd') == 'aibot_respond_msg' and row.get('headers', {}).get('req_id') == reply_id
        output = row.get('body', {}).get('markdown', {}).get('content')
        sequence = trace['channelReceived'][index]['sequence']
        socket_id = trace['channelReceived'][index]['socket']['id']
        later = [item['sequence'] for item in trace['channelReceived'] if item['sequence'] > sequence
                 and item['socket']['id'] == socket_id
                 and json.loads(payload(item['payload'])).get('headers', {}).get('req_id') == reply_id]
        upper = min(later) if later else float('inf')
        acknowledgements = [item for item in trace['channelSent'] if sequence < item['sequence'] < upper
                            and item['socket']['id'] == socket_id
                            and (value := json.loads(payload(item['payload']))).get('headers', {}).get('req_id') == reply_id
                            and value.get('body', {}).get('errcode') == 0]
        require(len(acknowledgements) == 1, 'OUTBOUND_ACK')
    require(recipient and isinstance(output, str) and marker in output, 'OUTBOUND_RECIPIENT')
    return sequence


def _rpc(raw, trace, receipt, method):
    request, response = receipt['request'], receipt['response']
    frame = indexed(raw['controlFrames'], receipt['frameIndex'], 'HISTORY_FRAME_INDEX')
    require(request.get('type') == 'request' and request['method'] == method
            and response == frame and response.get('type') == 'response'
            and response['id'] == request['id'] and response.get('ok') is True, 'HISTORY_RPC')
    sent = [row for row in trace['controlSent'] if json.loads(payload(row['payload'])) == request]
    received = trace['controlReceived'][receipt['frameIndex']]
    require(len(sent) == 1 and sent[0]['sequence'] < received['sequence']
            and sent[0]['socket']['id'] == received['socket']['id'], 'HISTORY_REQUEST_ORIGINAL')
    return response['result']


def context_observation(raw, trace):
    contexts = raw['roundContexts']
    require([row['generation'] for row in contexts] == [0, 1]
            and all(type(row['generation']) is int for row in contexts), 'ROUND_CONTEXTS')
    output = []
    for context in contexts:
        generation, project, key = context['generation'], context['projectKey'], context['sessionKey']
        require(project == raw['runtime']['home'] == raw['runtime']['pilotHome'] == raw['runtime']['defaultProject']
                and key.startswith(raw['channel'] + ':'), 'HISTORY_PROJECT')
        catalog = indexed(raw['persistedSessions'], context['listIndex'], 'HISTORY_LIST_INDEX')
        history = indexed(raw['persistedMessages'], context['messageIndex'], 'HISTORY_MESSAGE_INDEX')
        require(catalog['generation'] == history['generation'] == generation
                and history['listIndex'] == context['listIndex'], 'HISTORY_GENERATION')
        listing, result = _rpc(raw, trace, catalog, 'list_sessions'), _rpc(raw, trace, history, 'read_session_messages')
        require(catalog['request']['params']['projectKey'] == project
                and history['request']['params'] == {'projectKey': project, 'sessionKey': key, 'limit': 0}
                and history['catalogSession'] in listing['sessions']
                and (history['catalogSession'].get('sessionKey') or history['catalogSession'].get('sessionId')) == key
                and result['session']['sessionKey'] == key and result['session']['cwd'] == project, 'HISTORY_IDENTITY')
        messages, turn = result['messages'], context['turnId']
        require(isinstance(turn, str) and turn, 'TURN_ID')
        anchors = [i for i, row in enumerate(messages) if row.get('source') == 'history' and row.get('role') == 'user'
                   and row.get('kind') == 'text' and f'CHANNEL_INPUT_{generation}' in str(row.get('text', ''))]
        require(anchors and {messages[i].get('turnId') for i in anchors} == {turn}, 'TURN_ANCHOR')
        users = [i for i, row in enumerate(messages) if row.get('source') == 'history' and row.get('turnId') == turn
                 and row.get('role') == 'user' and row.get('kind') == 'text']
        outputs = [i for i, row in enumerate(messages) if row.get('source') == 'history' and row.get('turnId') == turn
                   and row.get('role') == 'assistant' and row.get('kind') == 'text'
                   and f'CHANNEL_OUTPUT_{generation}' in str(row.get('text', ''))]
        require(all(type(index) is int and index >= 0 for field in ('anchorIndices', 'userIndices', 'outputIndices') for index in context[field]), 'TURN_INDEX_TYPES')
        require(outputs and context['anchorIndices'] == anchors and context['userIndices'] == users
                and context['outputIndices'] == outputs, 'TURN_INDICES')
        if generation:
            previous = contexts[0]
            require(key == previous['sessionKey'] and project == previous['projectKey'] and turn != previous['turnId'], 'RELOAD_SESSION')
            for role, marker in [('user', 'CHANNEL_INPUT_0'), ('assistant', 'CHANNEL_OUTPUT_0')]:
                require(any(row.get('source') == 'history' and row.get('turnId') == previous['turnId']
                            and row.get('role') == role and marker in str(row.get('text', '')) for row in messages), 'RELOAD_HISTORY')
        output.append({'catalog': listing, 'history': result})
    return output


def observe(raw, channel, transport_records, durable_files):
    """Replay one capture. durable_files maps ORIGINAL runtime paths to bytes.

    The caller authenticates both capture/process receipts, every artifact hash,
    helper/producer, fixed Node/TS dependencies and same candidate first. This
    function never converts CAPTURED into overall channel or retirement PASS.
    """
    require(channel in CHANNELS and raw.get('channel') == channel
            and raw.get('schemaVersion') == 2 and raw.get('profile') == 'native-eight-v2', 'PROFILE')
    require(raw.get('engine') in ('ts', 'native') and raw.get('fixedTS') == FIXED_TS
            and raw.get('status') == 'CAPTURED' and raw.get('formalAcceptance') is False
            and raw.get('failure') is None and raw.get('exit') == {'code': 0, 'signal': None}
            and type(raw['exit']['code']) is int, 'EXECUTION')
    trace = transport_observation(raw, transport_records)
    identities, delivery_sequences = _deliveries(raw, channel, trace)
    require(isinstance(durable_files, dict) and durable_files
            and all(isinstance(path, str) and isinstance(value, bytes) for path, value in durable_files.items()), 'DURABLE_FILES')
    runtime = raw['runtime']
    require(runtime['cwd'] == runtime['project'] and runtime['home'] != runtime['project'], 'RUNTIME_PROJECT')
    contexts = context_observation(raw, trace)
    model_pairs = [pair for pair in trace['exchanges'] if urlsplit(pair['request']['url']).path.startswith('/v1/')]
    bindings = raw['modelRoundInputs']
    require(all(type(row.get('stream', False)) is bool for row in raw['models']), 'MODEL_STREAM_FLAG')
    require(all(type(row['generation']) is int and row['generation'] in (0, 1)
                and type(row['modelIndex']) is int and type(row['userMessageIndex']) is int
                and row['userMessageIndex'] >= 0 for row in bindings), 'MODEL_BINDING_TYPES')
    require([row['generation'] for row in raw['permissionGate']] == [0, 1]
            and all(type(row['generation']) is int for row in raw['permissionGate']), 'PERMISSION_GENERATIONS')
    require(all(type(row['generation']) is int and row['generation'] in (0, 1) for row in raw['answerSent']), 'ANSWER_GENERATIONS')
    stream_indices = [i for i, body in enumerate(raw['models']) if body.get('stream') is True]
    require([row['modelIndex'] for row in bindings] == stream_indices, 'MODEL_BINDINGS')
    require([row['generation'] for row in bindings] == sorted(row['generation'] for row in bindings), 'MODEL_GENERATION_ORDER')
    final_round_sequences = []
    claimed_deliveries = {i for i, row in enumerate(raw['deliveries']) if row['initial']}
    for generation in (0, 1):
        current = [row for row in bindings if row['generation'] == generation]
        require(len(current) >= 3, 'MODEL_CONTINUATION')
        for binding in current:
            require(type(binding['generation']) is int and binding['marker'] == f'CHANNEL_INPUT_{generation}', 'MODEL_GENERATION')
            body = raw['models'][binding['modelIndex']]
            matches = [i for i, row in enumerate(body['messages']) if row.get('role') == 'user' and binding['marker'] in text(row)]
            require(matches == [binding['userMessageIndex']], 'MODEL_USER_INDEX')
        # Permission and elicitation must actually return into later model
        # requests, rather than just producing an unrelated final message.
        for phase, call_id in [(1, f'permission-{generation}'), (2, f'question-{generation}')]:
            messages = raw['models'][current[phase]['modelIndex']]['messages']
            require(any(row.get('role') == 'tool' and row.get('tool_call_id') == call_id for row in messages), 'MODEL_TOOL_RESULT')
        initial = next(i for i, row in enumerate(raw['deliveries']) if row['generation'] == generation and row['initial'])
        initial_id = identities[initial]
        gates = [row for row in raw['permissionGate'] if row['generation'] == generation]
        require(len(gates) == 1, 'PERMISSION_GATE')
        gate = gates[0]
        file = str(Path(runtime['project']) / f'permission-{generation}.txt')
        require(gate['path'] == file and gate['fileExistsBeforeAnswer'] is False
                and durable_files.get(file) == b'approved', 'PERMISSION_SIDE_EFFECT')
        prompt_sequence = _outbound(raw, channel, trace, gate['promptReceipt'], '回复 1 允许一次', initial_id)
        require(prompt_sequence > delivery_sequences[initial], 'PERMISSION_PROMPT_ORDER')
        answers = [row for row in raw['answerSent'] if row['generation'] == generation]
        permissions = [row for row in answers if row['kind'] == 'permission']
        questions = [row for row in answers if row['kind'] == 'elicitation']
        require(1 <= len(permissions) <= 3 and len(questions) == 1 and answers == permissions + questions, 'ANSWERS')
        for attempt, answer in enumerate(permissions, 1):
            index = answer['deliveryIndex']
            delivery = indexed(raw['deliveries'], index, 'ANSWER_DELIVERY_INDEX')
            require(index not in claimed_deliveries and delivery['generation'] == generation and not delivery['initial']
                    and answer['text'] == delivery['text'] == '1' and type(answer['attempt']) is int and answer['attempt'] == attempt
                    and answer['decision'] == 'allow_once' and answer['fileExistsBeforeAnswer'] is False
                    and answer['promptReceipt'] == gate['promptReceipt'], 'PERMISSION_ANSWER')
            claimed_deliveries.add(index)
            require(delivery_sequences[index] > prompt_sequence, 'ANSWER_ORDER')
            if attempt > 1:
                previous = permissions[attempt - 2]
                retry = _outbound(raw, channel, trace, answer['retryAfterReceipt'], '权限提示发送中，请稍候', identities[previous['deliveryIndex']])
                require(delivery_sequences[previous['deliveryIndex']] < retry < delivery_sequences[index], 'RETRY_ORDER')
        answer = questions[0]
        index = answer['deliveryIndex']
        delivery = indexed(raw['deliveries'], index, 'ANSWER_DELIVERY_INDEX')
        require(index not in claimed_deliveries and delivery['generation'] == generation and not delivery['initial']
                and answer['text'] == delivery['text'] == '1', 'ELICITATION_ANSWER')
        claimed_deliveries.add(index)
        question_sequence = _outbound(raw, channel, trace, answer['promptReceipt'], 'Choose mock option', initial_id)
        require(delivery_sequences[permissions[-1]['deliveryIndex']] < question_sequence < delivery_sequences[index], 'ELICITATION_ORDER')
        final_sequences = []
        for source in ('http', 'wire'):
            for i, row in enumerate(raw[source]):
                if f'CHANNEL_OUTPUT_{generation}' not in canonical(row) or (source == 'http' and row['path'].startswith('/v1/')):
                    continue
                final_sequences.append(_outbound(raw, channel, trace, {'source': source, 'index': i}, f'CHANNEL_OUTPUT_{generation}', initial_id))
        require(final_sequences and min(final_sequences) > delivery_sequences[index], 'FINAL_ORDER')
        final_round_sequences.append(max(final_sequences))
        pairs = [model_pairs[row['modelIndex']] for row in current]
        require(delivery_sequences[initial] < pairs[0]['request']['sequence'] < prompt_sequence
                and delivery_sequences[permissions[-1]['deliveryIndex']] < pairs[1]['request']['sequence'] < question_sequence
                and all(delivery_sequences[index] < pair['request']['sequence'] < min(final_sequences) for pair in pairs[2:]), 'MODEL_CONTINUATION_ORDER')
        for phase, pair in enumerate(pairs):
            require(pair['response']['status'] == 200, 'MODEL_RESPONSE_STATUS')
            lines = payload(pair['response']['body']).decode('utf-8').splitlines()
            data = [line[6:] for line in lines if line.startswith('data: ')]
            require(data and data[-1] == '[DONE]', 'MODEL_STREAM_COMPLETE')
            chunks = [json.loads(value) for value in data[:-1]]
            deltas = [choice.get('delta', {}) for chunk in chunks for choice in chunk.get('choices', [])]
            if phase < 2:
                expected = f'permission-{generation}' if phase == 0 else f'question-{generation}'
                name = 'bash' if phase == 0 else 'ask_user_question'
                require(any(call.get('id') == expected and call.get('function', {}).get('name') == name
                            for delta in deltas for call in delta.get('tool_calls', [])), 'MODEL_TOOL_CALL')
            else:
                require(''.join(delta.get('content', '') for delta in deltas) == f'CHANNEL_OUTPUT_{generation}', 'MODEL_FINAL')
        for action in ('disable', 'enable'):
            replies = [(i, row) for i, row in enumerate(raw['controlFrames']) if row.get('type') == 'response' and row.get('id') == action]
            require(len(replies) == 1 and replies[0][1].get('ok') is True
                    and replies[0][1].get('result') == {'reloaded': True, 'changedPaths': [f'adapters.{channel}.enabled']}, 'RELOAD_RESPONSE')
            sent = [row for row in trace['controlSent'] if json.loads(payload(row['payload'])) == {'type': 'request', 'id': action, 'method': 'reload_config', 'params': {}}]
            received = trace['controlReceived'][replies[0][0]]
            require(len(sent) == 1 and sent[0]['sequence'] < received['sequence']
                    and sent[0]['socket']['id'] == received['socket']['id'], 'RELOAD_REQUEST')
        turn = raw['roundContexts'][generation]['turnId']
        durable_rounds = []
        for path, data in durable_files.items():
            if path.startswith(runtime['home'] + '/') and path.endswith('.jsonl'):
                require(not data or data.endswith(b'\n'), 'DURABLE_JSONL')
                rows = [json.loads(line) for line in data.splitlines() if line.strip()]
                selected = [row for row in rows if row.get('turnId') == turn]
                if any(row.get('type') == 'accepted_input' and f'CHANNEL_INPUT_{generation}' in canonical(row) for row in selected):
                    require(any(row.get('type') == 'turn_result' for row in selected)
                            and f'CHANNEL_OUTPUT_{generation}' in canonical(selected), 'DURABLE_TURN_RESULT')
                    durable_rounds.append(path)
        require(len(durable_rounds) == 1, 'DURABLE_CURRENT_TURN')
    require(claimed_deliveries == set(range(len(raw['deliveries']))), 'UNCLAIMED_DELIVERY')
    disable = next(row['sequence'] for row in trace['controlReceived'] if json.loads(payload(row['payload'])).get('id') == 'disable')
    enable = next(row['sequence'] for row in trace['controlReceived'] if json.loads(payload(row['payload'])).get('id') == 'enable')
    second_input = next(i for i, row in enumerate(raw['deliveries']) if row['initial'] and row['generation'] == 1)
    require(final_round_sequences[0] < disable < enable < delivery_sequences[second_input], 'RELOAD_ORDER')
    require(any(row.get('event') == 'actual-process-stop' and row.get('exit') == raw['exit'] for row in raw['steps']), 'PROCESS_STOP')
    return {'profile': 'native-eight-v2', 'channel': channel, 'scope': list(SCOPE),
            **transport_projection(trace),
            'inputs': [{'generation': row['generation'], 'text': row['text'],
                        'frames': [frame['frame'] for frame in row['frames']]} for row in raw['deliveries']],
            'contexts': contexts}


def transport_projection(trace):
    """Retain each socket's complete stream, not cross-socket scheduling.

    Socket identity is used only to partition original records. Sorting whole
    streams removes observer numbering; events within a stream are never sorted
    by content, deduplicated or dropped. The same policy is already specified
    in normalization.md and used by the registered HTTP outbound comparator.
    """
    exchanges, models = {}, {}
    for pair in trace['exchanges']:
        request, response = pair['request'], pair['response']
        identity = request['socket']['id']
        require(type(identity) is int, 'HTTP_SOCKET')
        head = response['actualHeaderText'].encode('latin-1')
        status, headers, end = contracts.http_head(head, 0)
        match = re.fullmatch(r'HTTP/1\.[01] ([1-5][0-9]{2}) [^\r\n]*', status)
        require(end == len(head) and match is not None and int(match.group(1)) == response['status'], 'HTTP_HEAD')
        for name, value in response['headers'].items():
            require(headers.get(name.lower()) == [str(x) for x in (value if isinstance(value, list) else [value])], 'HTTP_HEADERS')
        if 'content-length' in headers:
            require(headers['content-length'] == [str(len(payload(response['body'])))], 'HTTP_LENGTH')
        require(len(request['rawHeaders']) % 2 == 0, 'HTTP_REQUEST_HEADERS')
        request_headers = contracts.header_fields([f'{k}: {v}'.encode('latin-1')
                          for k, v in zip(request['rawHeaders'][::2], request['rawHeaders'][1::2])])
        require(request_headers.get('authorization', [None]) == [request['headers'].get('authorization')], 'HTTP_AUTHORIZATION')
        body = _wire_value(payload(request['body']))
        exchanges.setdefault(identity, []).append({'method': request['method'], 'target': request['url'],
                          'requestHeaders': request_headers, 'request': body,
                          'statusLine': status, 'responseHeaders': {key: value for key, value in headers.items()
                          if key not in {'date', 'connection', 'keep-alive', 'transfer-encoding'}},
                          'response': _wire_value(payload(response['body']))})
        if urlsplit(request['url']).path.startswith('/v1/'):
            models.setdefault(identity, []).append(body)
    channels, endpoints = {}, {}
    events = [(row, direction) for field, direction in [('channelSent', 'send'), ('channelReceived', 'receive')]
              for row in trace[field]]
    for row, direction in sorted(events, key=lambda item: item[0]['sequence']):
        socket = row['socket']
        identity = socket.get('id')
        require(type(identity) is int and identity >= 0, 'WS_SOCKET_IDENTITY')
        require(type(socket.get('localPort')) is int and type(socket.get('remotePort')) is int, 'WS_SOCKET')
        endpoint = tuple(socket.get(key) for key in ('localHost', 'localPort', 'remoteHost', 'remotePort'))
        require(endpoints.setdefault(identity, endpoint) == endpoint, 'WS_SOCKET_IDENTITY')
        data = payload(row['payload'])
        value = ({'wireKind': 'bytes', 'value': data.hex()} if row.get('binary') is True
                 else _wire_value(data))
        channels.setdefault(identity, []).append({'direction': direction, 'binary': row.get('binary'), 'payload': value})
    return {'models': sorted(models.values(), key=canonical),
            'exchanges': sorted(exchanges.values(), key=canonical),
            'channelStreams': sorted(channels.values(), key=canonical)}


def _non_json_constant(value):
    raise ValueError('non-standard JSON constant: ' + value)


def _wire_value(data):
    # A user's JSON object must never impersonate the non-JSON fallback.
    # Keep oracle decoding separate: only comparison payloads carry this tag.
    try:
        value = json.loads(data, parse_constant=_non_json_constant)
    except (ValueError, UnicodeDecodeError):
        try:
            return {'wireKind': 'text', 'value': data.decode('utf-8')}
        except UnicodeDecodeError:
            return {'wireKind': 'bytes', 'value': data.hex()}
    return {'wireKind': 'json', 'value': value}


def wire_projection(raw, records):
    """Compare only authenticated wire data, retaining headers and control frames.

    Socket numbers and capture offsets locate originals; they are not protocol
    fields. Whole streams may be reordered across sockets, never within one.
    Original TCP bytes remain in the source-bound transport receipt.
    #3062 authorizes only the validated WebSocket key/accept/mask exception
    below for channel-v2; business IDs, heartbeat and stream order stay exact.
    """
    http = http_wire_observation(raw, records)
    websocket = ws_wire_observation(raw, records, http)
    http_streams = {}
    excluded = {'date', 'connection', 'keep-alive', 'transfer-encoding'}
    for pair in http['exchanges']:
        request, response = pair['request'], pair['response']
        http_streams.setdefault(pair['socket'], []).append({
            'requestLine': request['line'], 'requestHeaders': request['headers'],
            'request': _wire_value(request['body']), 'requestTrailers': request['trailers'],
            'statusLine': response['line'],
            'responseHeaders': {key: value for key, value in response['headers'].items()
                                if key not in excluded},
            'response': _wire_value(response['body']), 'responseTrailers': response['trailers'],
            'informational': [{**item, 'headers': {key: value for key, value in item['headers'].items()
                                                  if key not in excluded}}
                              for item in response['informational']]})
    websocket_streams = []
    for connection in websocket['connections']:
        upgrade = connection['upgrade']
        frames = []
        for frame in connection['interleaving']:
            frames.append({key: frame[key] for key in (
                'direction', 'opcode', 'fin', 'rsv', 'masked', 'lengthCode', 'payloadLength')})
            # #3062: the parser validates each original mask, binds
            # data payloads to API events, and retains full control payloads.
            # Keep non-mask header bytes; full originals stay in wire proof.
            frames[-1]['wireHeaderPrefixHex'] = (frame['wireHeaderHex'][:-8] if frame['masked']
                                                  else frame['wireHeaderHex'])
            frames[-1]['maskKeyBytes'] = 4 if frame['masked'] else 0
            frames[-1]['payload'] = (_wire_value(frame['payload']) if frame['opcode'] in (0, 1)
                                      else {'rawBytesHex': frame['payload'].hex()})
        websocket_streams.append({
            'role': connection['role'],
            'priorHttp': http_streams.pop(connection['socket']['id'], []),
            'requestLine': upgrade['request']['line'],
            'requestHeaders': {**upgrade['request']['headers'],
                               'sec-websocket-key': ['<validated-16-byte-key>']},
            'statusLine': upgrade['response']['line'],
            'responseHeaders': {**upgrade['response']['headers'],
                                'sec-websocket-accept': ['<validated-derived-accept>']},
            'frames': frames,
            'messages': {direction: [
                {'binary': message['binary'], 'frames': message['frames'],
                 'payload': ({'rawBytesHex': message['payload'].hex()} if message['binary']
                             else _wire_value(message['payload']))}
                for message in connection['directions'][direction]['messages']]
                for direction in ('read', 'write')}})
    return {'wireHttpStreams': sorted(http_streams.values(), key=canonical),
            'wireWebsocketStreams': sorted(websocket_streams, key=canonical)}


def compare_pair(ts_raw, native_raw, ts_records, native_records, ts_files, native_files, *, ts_root, native_root):
    """Compare authenticated captures under the approved wire and entity rules.

    Roots must be the verified preserved directories with their complete home
    and project bytes. The caller checks their receipt/inode/hash provenance.
    Equality is restricted to SCOPE and never certifies missing media profiles.
    """
    require(ts_raw.get('engine') == 'ts' and native_raw.get('engine') == 'native', 'PAIR_ENGINES')
    require(ts_raw.get('channel') == native_raw.get('channel') and ts_raw.get('runtime') == native_raw.get('runtime'), 'PAIR_INPUT_LAYOUT')
    compare = _load('compare', _apparatus / 'compare.py')
    _verify_cli_imports(_apparatus)
    channel = ts_raw['channel']
    projections = []
    for raw, records, files, root in [(ts_raw, ts_records, ts_files, ts_root),
                                       (native_raw, native_records, native_files, native_root)]:
        root = Path(root)
        require(root.is_absolute() and root.resolve() == root and root.is_dir()
                and (root / 'home').is_dir() and (root / 'project').is_dir(), 'PAIR_PRESERVED_ROOT')
        actual_files = {}
        for name in ('home', 'project'):
            for path in (root / name).rglob('*'):
                require(not path.is_symlink(), 'PAIR_DURABLE_SYMLINK')
                if path.is_file():
                    actual_files[str(Path(raw['runtime'][name]) / path.relative_to(root / name))] = path.read_bytes()
        require(actual_files == files, 'PAIR_DURABLE_ORIGINALS')
        observation = observe(raw, channel, records, files)
        observation.update(wire_projection(raw, records))
        normalizer = compare.entity_normalizer(root, {})
        normalizer.index(raw)
        projection = normalizer.value(observation)
        # Entity normalization can change a stream's lexical key; it must not
        # restore an accidental cross-connection ordering constraint.
        for field in ('models', 'exchanges', 'channelStreams', 'wireHttpStreams', 'wireWebsocketStreams'):
            projection[field].sort(key=canonical)
        projection['disk'] = compare.disk(root, normalizer)
        projections.append(projection)
    left, right = projections
    differences = [key for key in left if canonical(left[key]) != canonical(right[key])]
    return {'equal': not differences, 'differentFields': differences, 'scope': list(SCOPE),
            'formalAcceptance': False,
            'wireBindings': [{'engine': engine, 'httpSameSocket': True, 'websocketFrames': True}
                             for engine in ('ts', 'native')],
            'ts': left, 'native': right}
