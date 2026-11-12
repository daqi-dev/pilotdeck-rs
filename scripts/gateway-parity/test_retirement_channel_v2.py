"""Adversarial eight-v2 replay controls; these are not production acceptance evidence."""
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
import copy
import base64
import hashlib
import json
import shutil
import unittest

atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
v2 = _load('retirement_channel_v2', _apparatus / 'retirement_channel_v2.py')


def blob(data):
    return {'bytes': len(data), 'rawBytesHex': data.hex(), 'truncated': False}


def fixture(channel='qq', *, update_final=False):
    """Synthetic transport originals for rejection controls, never a capture."""
    raw = {'schemaVersion': 2, 'profile': 'native-eight-v2', 'channel': channel, 'engine': 'native',
           'fixedTS': v2.FIXED_TS, 'status': 'CAPTURED', 'formalAcceptance': False,
           'failure': None, 'exit': {'code': 0, 'signal': None},
           'runtime': {'node': 'v22.22.0', 'osHome': '/test/cur/os-home', 'home': '/test/cur/home',
                       'pilotHome': '/test/cur/home', 'defaultProject': '/test/cur/home',
                       'project': '/test/cur/project', 'cwd': '/test/cur/project',
                       'tmp': '/test/cur/tmp', 'mockPort': 28690, 'controlPort': 28610},
           'http': [], 'models': [], 'wire': [], 'controlFrames': [], 'deliveries': [], 'inbound': [],
           'permissionGate': [], 'answerSent': [], 'modelRoundInputs': [], 'roundContexts': [],
           'persistedSessions': [], 'persistedMessages': [],
           'steps': [{'event': 'actual-process-stop', 'exit': {'code': 0, 'signal': None}}]}
    records, files, history, durable = [], {}, [], []
    channel_socket = {'id': 2, 'localPort': 28690, 'remotePort': 41000}
    control_socket = {'id': 3, 'localPort': 42000, 'remotePort': 28610}
    def record(kind, **value):
        row = {'kind': kind, 'sequence': len(records), **value}
        records.append(row)
        return row
    def ws(kind, value, socket):
        data = value if isinstance(value, bytes) else json.dumps(value).encode()
        return record(kind, socket=socket, payload=blob(data), binary=isinstance(value, bytes))
    def http(path, body, response=None, *, method='POST'):
        index = len(raw['http'])
        raw['http'].append({'generation': generation, 'method': method, 'path': path, 'body': copy.deepcopy(body), 'authorization': 'Bearer fixture'})
        target = path + '?receive_id_type=chat_id' if channel == 'feishu' and method == 'POST' and path == '/open-apis/im/v1/messages' else path
        request = record('mock-http-request', id=len(records), method=method, url=target,
                         headers={'authorization': 'Bearer fixture'}, rawHeaders=['Authorization', 'Bearer fixture'],
                         body=blob(json.dumps(body).encode()), socket={'id': 1})
        default = json.dumps({'code': 0, 'data': {'message_id': f'reply-{index}'}}).encode() if channel == 'feishu' else b'{"id":"reply"}'
        reply = response if response is not None else default
        headers = {'content-type': 'text/event-stream' if path.startswith('/v1/') else 'application/json',
                   'content-length': str(len(reply))}
        record('mock-http-response', id=request['id'], status=200, finished=True, headers=headers,
               actualHeaderText='HTTP/1.1 200 OK\r\n' + ''.join(f'{key}: {value}\r\n' for key, value in headers.items()) + '\r\n', body=blob(reply))
        return index
    def deliver(generation, value, initial=False):
        identity = 'event-' + str(len(raw['deliveries']) + 1)
        if channel == 'feishu':
            event = {'event': {'sender': {'sender_id': {'open_id': 'human'}},
                              'message': {'message_id': identity, 'chat_id': 'room', 'content': json.dumps({'text': value})}}}
            data = json.dumps(event).encode()
            length, size = bytearray(), len(data)
            while size >= 128:
                length.append((size & 127) | 128)
                size >>= 7
            length.append(size)
            encoded = b'\x42' + bytes(length) + data
            ws('fixture-ws-send', encoded, channel_socket)
            frame = {'rawBytesHex': encoded.hex()}
        else:
            frame = {'op': 0, 't': 'MESSAGE_CREATE' if channel == 'discord' else 'GROUP_AT_MESSAGE_CREATE',
                     's': len(raw['deliveries']) + 2,
                     'd': {'id': identity, 'channel_id' if channel == 'discord' else 'group_openid': '42' if channel == 'discord' else 'group',
                           'author': {'id': 'human', 'bot': False} if channel == 'discord' else {'member_openid': 'human'}, 'content': value}}
            ws('fixture-ws-send', frame, channel_socket)
        row = {'generation': generation, 'text': value, 'initial': initial,
               'frames': [{'transport': 'websocket', 'frame': frame}]}
        if channel != 'feishu':
            row['messageId'] = identity
        index = len(raw['deliveries'])
        raw['deliveries'].append(row)
        if initial:
            raw['inbound'].append(row)
        return index, identity
    def outbound(value, identity):
        if channel == 'discord':
            index = http('/api/v10/channels/42/messages', {'content': value})
        elif channel == 'feishu':
            final = value.startswith('CHANNEL_OUTPUT_')
            content = {'zh_cn': {'content': [[{'tag': 'text', 'text': value}]]}} if final else {'text': value}
            body = {'msg_type': 'post' if final else 'text', 'content': json.dumps(content)}
            if final and update_final:
                create = http('/open-apis/im/v1/messages', {'receive_id': 'room', 'msg_type': 'post',
                              'content': json.dumps({'zh_cn': {'content': [[{'tag': 'text', 'text': 'working'}]]}})})
                index = http(f'/open-apis/im/v1/messages/reply-{create}', body, method='PUT')
            else:
                index = http('/open-apis/im/v1/messages', {'receive_id': 'room', **body})
        else:
            index = http('/v2/groups/group/messages', {'content': value, 'msg_id': identity})
        return {'source': 'http', 'index': index}
    def model(generation, phase):
        messages = [{'role': 'user', 'content': f'CHANNEL_INPUT_{generation}'}]
        if phase >= 1:
            messages.append({'role': 'tool', 'tool_call_id': f'permission-{generation}', 'content': 'success'})
        if phase >= 2:
            messages.append({'role': 'tool', 'tool_call_id': f'question-{generation}', 'content': 'One'})
        body = {'stream': True, 'model': 'mock', 'messages': messages, 'temperature': 0.2}
        delta = ({'role': 'assistant', 'tool_calls': [{'index': 0, 'id': f"{'permission' if phase == 0 else 'question'}-{generation}",
                  'type': 'function', 'function': {'name': 'bash' if phase == 0 else 'ask_user_question', 'arguments': '{}'}}]}
                 if phase < 2 else {'role': 'assistant', 'content': f'CHANNEL_OUTPUT_{generation}'})
        reply = ('data: ' + json.dumps({'choices': [{'delta': delta}]}) + '\n\ndata: [DONE]\n\n').encode()
        index = len(raw['models'])
        raw['models'].append(copy.deepcopy(body))
        http('/v1/chat/completions', body, reply)
        raw['modelRoundInputs'].append({'generation': generation, 'modelIndex': index, 'userMessageIndex': 0,
                                       'marker': f'CHANNEL_INPUT_{generation}'})
    def rpc(identity, method, params, result):
        request = {'type': 'request', 'id': identity, 'method': method, 'params': params}
        response = {'type': 'response', 'id': identity, 'ok': True, 'result': copy.deepcopy(result)}
        ws('fixture-ws-send', request, control_socket)
        ws('fixture-ws-receive', response, control_socket)
        frame_index = len(raw['controlFrames'])
        raw['controlFrames'].append(response)
        return {'request': request, 'response': response, 'frameIndex': frame_index}
    for generation in (0, 1):
        _, identity = deliver(generation, f'CHANNEL_INPUT_{generation}', True)
        model(generation, 0)
        prompt = outbound('回复 1 允许一次', identity)
        path = f'/test/cur/project/permission-{generation}.txt'
        raw['permissionGate'].append({'generation': generation, 'path': path, 'fileExistsBeforeAnswer': False, 'promptReceipt': prompt})
        answer_index, _ = deliver(generation, '1')
        raw['answerSent'].append({'generation': generation, 'kind': 'permission', 'deliveryIndex': answer_index,
                                 'text': '1', 'attempt': 1, 'decision': 'allow_once',
                                 'fileExistsBeforeAnswer': False, 'promptReceipt': prompt})
        model(generation, 1)
        prompt = outbound('Choose mock option', identity)
        answer_index, _ = deliver(generation, '1')
        raw['answerSent'].append({'generation': generation, 'kind': 'elicitation', 'deliveryIndex': answer_index,
                                 'text': '1', 'promptReceipt': prompt})
        model(generation, 2)
        outbound(f'CHANNEL_OUTPUT_{generation}', identity)
        turn = f'turn-{generation}'
        history.extend([{'source': 'history', 'role': role, 'kind': 'text', 'turnId': turn, 'text': value}
                        for role, value in [('user', f'CHANNEL_INPUT_{generation}'), ('assistant', f'CHANNEL_OUTPUT_{generation}')]])
        entry = {'sessionKey': channel + ':group', 'cwd': '/test/cur/home'}
        catalog = rpc(f'catalog-{generation}', 'list_sessions', {'projectKey': '/test/cur/home', 'limit': 100}, {'sessions': [entry]})
        messages = rpc(f'history-{generation}', 'read_session_messages', {'projectKey': '/test/cur/home', 'sessionKey': channel + ':group', 'limit': 0}, {'session': entry, 'messages': history})
        raw['persistedSessions'].append({'generation': generation, **catalog})
        raw['persistedMessages'].append({'generation': generation, 'listIndex': generation, 'catalogSession': entry, **messages})
        raw['roundContexts'].append({'generation': generation, 'projectKey': '/test/cur/home', 'sessionKey': channel + ':group',
                                     'turnId': turn, 'listIndex': generation, 'messageIndex': generation,
                                     'anchorIndices': [generation * 2], 'userIndices': [generation * 2],
                                     'outputIndices': [generation * 2 + 1]})
        files[path] = b'approved'
        durable.extend([{'type': 'accepted_input', 'turnId': turn, 'messages': [{'content': f'CHANNEL_INPUT_{generation}'}]},
                        {'type': 'turn_result', 'turnId': turn, 'result': {'content': f'CHANNEL_OUTPUT_{generation}'}}])
        if not generation:
            for action in ('disable', 'enable'):
                rpc(action, 'reload_config', {}, {'reloaded': True, 'changedPaths': ['adapters.' + channel + '.enabled']})
    files['/test/cur/home/projects/test/session.jsonl'] = b''.join(json.dumps(row).encode() + b'\n' for row in durable)
    return raw, records, files


def observe(data):
    raw, records, files = data
    return v2.observe(raw, raw['channel'], records, files)


def sync_request(data, index):
    raw, records, _ = data
    row = raw['http'][index]
    request = [record for record in records if record['kind'] == 'mock-http-request'][index]
    query = v2.urlsplit(request['url']).query
    request.update(method=row['method'], url=row['path'] + ('?' + query if query else ''),
                   body=blob(json.dumps(row['body']).encode()))


def change_response(data, index, value):
    _, records, _ = data
    request = [record for record in records if record['kind'] == 'mock-http-request'][index]
    response = next(record for record in records if record['kind'] == 'mock-http-response' and record['id'] == request['id'])
    before = response['headers']['content-length']
    response['body'] = blob(json.dumps(value).encode())
    response['headers']['content-length'] = str(response['body']['bytes'])
    response['actualHeaderText'] = response['actualHeaderText'].replace('content-length: ' + before + '\r\n',
                                                                     'content-length: ' + str(response['body']['bytes']) + '\r\n')


class ChannelV2Controls(unittest.TestCase):
    def rejected(self, data, rule):
        with self.assertRaisesRegex(ValueError, 'CHANNEL_V2_' + rule):
            observe(data)

    def test_complete_two_rounds_keep_full_business_and_no_pass(self):
        result = observe(fixture())
        self.assertEqual(sum(map(len, result['models'])), 6)
        self.assertEqual(len(result['contexts']), 2)
        self.assertNotIn('status', result)
        self.assertNotIn('attachment', result['scope'])
        self.assertEqual(result['models'][0][0]['wireKind'], 'json')
        self.assertEqual(result['models'][0][0]['value']['temperature'], 0.2)

    def test_old_profile_cannot_be_relabelled_by_consumer(self):
        data = fixture()
        data[0]['profile'] = 'native-eight-v1'
        self.rejected(data, 'PROFILE')

    def test_captured_failure_is_not_a_successful_business_observation(self):
        data = fixture()
        data[0]['status'] = 'BLOCKED'
        self.rejected(data, 'EXECUTION')

    def test_mutated_model_sidecar_cannot_replace_observed_request(self):
        data = fixture()
        data[0]['models'][0]['temperature'] = 0.8
        self.rejected(data, 'MODEL_ORIGINALS')

    def test_missing_extra_model_request_binding_is_rejected(self):
        data = fixture()
        data[0]['modelRoundInputs'].pop(1)
        self.rejected(data, 'MODEL_BINDINGS')

    def test_boolean_and_negative_indices_are_rejected(self):
        for index in (-1, True):
            data = fixture()
            data[0]['roundContexts'][0]['messageIndex'] = index
            self.rejected(data, 'HISTORY_MESSAGE_INDEX')

    def test_boolean_model_user_index_and_unknown_generation_are_rejected(self):
        for field, value in [('userMessageIndex', False), ('generation', 2)]:
            data = fixture()
            data[0]['modelRoundInputs'][0][field] = value
            self.rejected(data, 'MODEL_BINDING_TYPES')

    def test_boolean_turn_index_is_not_a_numeric_history_index(self):
        data = fixture()
        data[0]['roundContexts'][0]['anchorIndices'] = [False]
        self.rejected(data, 'TURN_INDEX_TYPES')

    def test_pair_requires_same_actual_paths_and_ports(self):
        raw, records, files = fixture()
        ts_raw = copy.deepcopy(raw)
        ts_raw['engine'] = 'ts'
        ts_raw['runtime']['controlPort'] += 1
        with self.assertRaisesRegex(ValueError, 'PAIR_INPUT_LAYOUT'):
            v2.compare_pair(ts_raw, raw, records, records, files, files,
                            ts_root=Path('/absent/ts'), native_root=Path('/absent/native'))

    def test_prior_round_model_user_cannot_satisfy_current_round(self):
        data = fixture()
        data[0]['modelRoundInputs'][3]['userMessageIndex'] = 1
        self.rejected(data, 'MODEL_USER_INDEX')

    def test_reused_answer_delivery_is_rejected(self):
        data = fixture()
        data[0]['answerSent'][1]['deliveryIndex'] = data[0]['answerSent'][0]['deliveryIndex']
        self.rejected(data, 'ELICITATION_ANSWER')

    def test_permission_file_must_be_actual_post_reap_bytes(self):
        data = fixture()
        data[2]['/test/cur/project/permission-1.txt'] = b'not approved'
        self.rejected(data, 'PERMISSION_SIDE_EFFECT')

    def test_previous_turn_cannot_be_claimed_as_second_turn(self):
        data = fixture()
        data[0]['roundContexts'][1]['turnId'] = 'turn-0'
        self.rejected(data, 'TURN_ANCHOR')

    def test_raw_history_cannot_override_observed_control_response(self):
        data = fixture()
        data[0]['persistedMessages'][1]['response'] = copy.deepcopy(data[0]['persistedMessages'][1]['response'])
        data[0]['persistedMessages'][1]['response']['result']['messages'][3]['text'] = 'invented'
        self.rejected(data, 'HISTORY_RPC')

    def test_history_response_without_real_request_is_rejected(self):
        data = fixture()
        target = next(row for row in data[1] if row['kind'] == 'fixture-ws-send'
                      and json.loads(bytes.fromhex(row['payload']['rawBytesHex'])).get('id') == 'history-1')
        target['payload'] = blob(b'{"type":"request","id":"wrong"}')
        self.rejected(data, 'HISTORY_REQUEST_ORIGINAL')

    def test_old_generation_plus_one_reply_id_is_rejected(self):
        data = fixture()
        index = next(i for i, row in enumerate(data[0]['http']) if row['body'].get('content') == 'CHANNEL_OUTPUT_1')
        data[0]['http'][index]['body']['msg_id'] = 'event-2'
        requests = [row for row in data[1] if row['kind'] == 'mock-http-request']
        requests[index]['body'] = blob(json.dumps(data[0]['http'][index]['body']).encode())
        self.rejected(data, 'OUTBOUND_RECIPIENT')

    def test_missing_transport_response_is_rejected(self):
        data = fixture()
        data[1].remove(next(row for row in data[1] if row['kind'] == 'mock-http-response'))
        self.rejected(data, 'HTTP_PAIRS')

    def test_duplicate_sequence_and_truncation_are_rejected(self):
        data = fixture()
        data[1][1]['sequence'] = data[1][0]['sequence']
        self.rejected(data, 'TRANSPORT_SEQUENCE')
        data = fixture()
        next(row for row in data[1] if row['kind'] == 'mock-http-request')['body']['truncated'] = True
        self.rejected(data, 'TRANSPORT_BYTES')

    def test_wrong_socket_cannot_supply_control_evidence(self):
        data = fixture()
        row = next(row for row in data[1] if row['kind'] == 'fixture-ws-receive')
        row['socket'] = {'localPort': 41000, 'remotePort': 9999}
        self.rejected(data, 'WS_SOCKET')

    def test_durable_current_turn_must_exist_after_reap(self):
        data = fixture()
        path = '/test/cur/home/projects/test/session.jsonl'
        lines = data[2][path].splitlines()
        data[2][path] = b'\n'.join(lines[:2]) + b'\n'
        self.rejected(data, 'DURABLE_CURRENT_TURN')

    def test_durable_turn_requires_terminal_output(self):
        data = fixture()
        path = '/test/cur/home/projects/test/session.jsonl'
        data[2][path] = data[2][path].replace(b'CHANNEL_OUTPUT_1', b'wrong output')
        self.rejected(data, 'DURABLE_TURN_RESULT')

    def test_stream_response_must_retain_actual_final_output(self):
        data = fixture()
        row = next(row for row in data[1] if row['kind'] == 'mock-http-response' and b'CHANNEL_OUTPUT_1' in bytes.fromhex(row['body']['rawBytesHex']))
        row['body'] = blob(bytes.fromhex(row['body']['rawBytesHex']).replace(b'CHANNEL_OUTPUT_1', b'CHANNEL_OUTPUT_0'))
        self.rejected(data, 'MODEL_FINAL')

    def test_complete_model_business_value_changes_projection(self):
        original = fixture()
        changed = copy.deepcopy(original)
        changed[0]['models'][0]['temperature'] = 0.7
        changed[0]['http'][0]['body']['temperature'] = 0.7
        row = next(row for row in changed[1] if row['kind'] == 'mock-http-request')
        row['body'] = blob(json.dumps(changed[0]['models'][0]).encode())
        self.assertNotEqual(observe(original), observe(changed))

    def test_response_status_reason_and_headers_remain_visible(self):
        original = fixture()
        changed = copy.deepcopy(original)
        row = next(row for row in changed[1] if row['kind'] == 'mock-http-response')
        row['actualHeaderText'] = row['actualHeaderText'].replace('200 OK', '200 Different')
        self.assertNotEqual(observe(original), observe(changed))

    def test_wecom_cannot_borrow_acknowledgement_for_later_reply(self):
        first = {'cmd': 'aibot_respond_msg', 'headers': {'req_id': 'same'}, 'body': {'markdown': {'content': 'prompt'}}}
        second = {'cmd': 'aibot_respond_msg', 'headers': {'req_id': 'same'}, 'body': {'markdown': {'content': 'final'}}}
        ack = {'headers': {'req_id': 'same'}, 'body': {'errcode': 0}}
        trace = {'channelReceived': [{'sequence': 10, 'socket': {'id': 4}, 'payload': blob(json.dumps(first).encode())},
                                     {'sequence': 20, 'socket': {'id': 4}, 'payload': blob(json.dumps(second).encode())}],
                 'channelSent': [{'sequence': 21, 'socket': {'id': 4}, 'payload': blob(json.dumps(ack).encode())}]}
        with self.assertRaisesRegex(ValueError, 'OUTBOUND_ACK'):
            v2._outbound({'wire': [first, second]}, 'wecom', trace, {'source': 'wire', 'index': 0}, 'prompt', 'same')
        trace['channelSent'].insert(0, {'sequence': 11, 'socket': {'id': 4}, 'payload': blob(json.dumps(ack).encode())})
        self.assertEqual(v2._outbound({'wire': [first, second]}, 'wecom', trace, {'source': 'wire', 'index': 0}, 'prompt', 'same'), 10)

    def test_feishu_text_dialog_and_post_final_are_real_outbound_shapes(self):
        result = observe(fixture('feishu'))
        calls = [row for stream in result['exchanges'] for row in stream if row['target'].startswith('/open-apis/')]
        self.assertEqual([row['request']['value']['msg_type'] for row in calls], ['text', 'text', 'post', 'text', 'text', 'post'])

    def test_feishu_live_post_edits_bind_successful_create_responses(self):
        result = observe(fixture('feishu', update_final=True))
        self.assertEqual(sum(row['method'] == 'PUT' for stream in result['exchanges'] for row in stream), 2)

    def test_feishu_text_fallback_edit_keeps_bound_recipient(self):
        data = fixture('feishu', update_final=True)
        for index, row in enumerate(data[0]['http']):
            if row['method'] == 'PUT':
                row['body'] = {'msg_type': 'text', 'content': json.dumps({'text': f"CHANNEL_OUTPUT_{row['generation']}"})}
                sync_request(data, index)
        observe(data)

    def test_feishu_edit_rejects_uncreated_and_previous_round_ids(self):
        for target in ('uncreated', 'previous'):
            data = fixture('feishu', update_final=True)
            edits = [index for index, row in enumerate(data[0]['http']) if row['method'] == 'PUT']
            index = edits[1]
            data[0]['http'][index]['path'] = ('/open-apis/im/v1/messages/unknown' if target == 'uncreated'
                                            else data[0]['http'][edits[0]]['path'])
            sync_request(data, index)
            self.rejected(data, 'FEISHU_EDIT_RECIPIENT')

    def test_feishu_edit_rejects_foreign_recipient_and_query(self):
        for field in ('recipient', 'query'):
            data = fixture('feishu', update_final=True)
            edit = next(index for index, row in enumerate(data[0]['http']) if row['method'] == 'PUT')
            create = edit - 1
            if field == 'recipient':
                data[0]['http'][create]['body']['receive_id'] = 'other-room'
                sync_request(data, create)
            else:
                request = [row for row in data[1] if row['kind'] == 'mock-http-request'][create]
                request['url'] = '/open-apis/im/v1/messages?receive_id_type=open_id'
            self.rejected(data, 'FEISHU_EDIT_RECIPIENT')

    def test_feishu_edit_rejects_failed_or_missing_create_identity(self):
        for response in ({'code': 1, 'data': {'message_id': 'matching'}}, {'code': 0, 'data': {}}):
            data = fixture('feishu', update_final=True)
            edit = next(index for index, row in enumerate(data[0]['http']) if row['method'] == 'PUT')
            create = edit - 1
            response = copy.deepcopy(response)
            if response.get('data', {}).get('message_id'):
                response['data']['message_id'] = f'reply-{create}'
            change_response(data, create, response)
            self.rejected(data, 'FEISHU_EDIT_RECIPIENT')

    def test_feishu_edit_cannot_relabel_generation_or_use_future_create(self):
        data = fixture('feishu', update_final=True)
        edit = next(index for index, row in enumerate(data[0]['http']) if row['method'] == 'PUT')
        data[0]['http'][edit]['generation'] = 1
        self.rejected(data, 'FEISHU_OUTBOUND_GENERATION')
        data = fixture('feishu', update_final=True)
        raw, records, _ = data
        trace = v2.transport_observation(raw, records)
        trace['exchanges'][edit - 1]['response']['sequence'] = trace['exchanges'][edit]['request']['sequence'] + 1
        with self.assertRaisesRegex(ValueError, 'FEISHU_EDIT_RECIPIENT'):
            v2._outbound(raw, 'feishu', trace, {'source': 'http', 'index': edit}, 'CHANNEL_OUTPUT_0', 'event-1')

    def test_discord_uses_real_api_v10_path_and_recipient(self):
        observe(fixture('discord'))
        for path in ('/channels/42/messages', '/api/v10/channels/other/messages'):
            data = fixture('discord')
            index = data[0]['permissionGate'][0]['promptReceipt']['index']
            data[0]['http'][index]['path'] = path
            sync_request(data, index)
            self.rejected(data, 'OUTBOUND_RECIPIENT')

    def test_http_projection_ignores_socket_numbers_and_cross_socket_schedule(self):
        raw, records, _ = fixture()
        seed = v2.transport_observation(raw, records)['exchanges'][0]
        def pair(label, socket):
            value = copy.deepcopy(seed)
            value['request'].update(socket={'id': socket}, url='/business/' + label, body=blob(json.dumps({'value': label}).encode()))
            return value
        a1, a2, b1, b2 = pair('a1', 1), pair('a2', 1), pair('b1', 2), pair('b2', 2)
        trace = {'exchanges': [a1, b1, a2, b2], 'channelSent': [], 'channelReceived': []}
        expected = v2.transport_projection(trace)
        reordered = copy.deepcopy(trace)
        reordered['exchanges'] = [reordered['exchanges'][index] for index in (1, 0, 3, 2)]
        for value in reordered['exchanges']:
            value['request']['socket']['id'] += 100
        self.assertEqual(expected, v2.transport_projection(reordered))
        reordered['exchanges'] = [reordered['exchanges'][index] for index in (0, 3, 2, 1)]
        self.assertNotEqual(expected, v2.transport_projection(reordered), 'same-socket order remains significant')
        extra = copy.deepcopy(trace)
        extra['exchanges'].append(copy.deepcopy(a2))
        self.assertNotEqual(expected, v2.transport_projection(extra), 'repeated business calls remain significant')

    def test_websocket_projection_preserves_duplex_order_without_cross_socket_order(self):
        def frame(sequence, port, message):
            return {'sequence': sequence, 'socket': {'id': port, 'localPort': 28690, 'remotePort': port},
                    'payload': blob(json.dumps({'business': message}).encode()), 'binary': False}
        trace = {'exchanges': [], 'channelSent': [frame(1, 41000, 'a1'), frame(2, 41001, 'b1'), frame(5, 41000, 'a3')],
                 'channelReceived': [frame(3, 41000, 'a2'), frame(4, 41001, 'b2')]}
        expected = v2.transport_projection(trace)
        changed = copy.deepcopy(trace)
        schedule = {1: 2, 2: 1, 3: 4, 4: 3, 5: 5}
        for key in ('channelSent', 'channelReceived'):
            for row in changed[key]:
                row['sequence'] = schedule[row['sequence']]
                row['socket']['remotePort'] += 100
                row['socket']['id'] += 500
        self.assertEqual(expected, v2.transport_projection(changed))
        changed['channelReceived'][0]['sequence'] = 6
        self.assertNotEqual(expected, v2.transport_projection(changed), 'send/receive interleaving on one socket remains significant')
        changed = copy.deepcopy(trace)
        changed['channelReceived'][0]['payload'] = blob(b'{"business":"changed"}')
        self.assertNotEqual(expected, v2.transport_projection(changed))

    def test_missing_socket_identity_is_not_inferred_from_endpoint_tuple(self):
        data = fixture()
        row = next(row for row in data[1] if row['kind'] == 'fixture-ws-send')
        row['socket'] = {key: value for key, value in row['socket'].items() if key != 'id'}
        self.rejected(data, 'WS_SOCKET_IDENTITY')

    def test_reused_endpoint_tuple_does_not_merge_distinct_websocket_connections(self):
        def frame(sequence, identity):
            return {'sequence': sequence, 'socket': {'id': identity, 'localHost': '127.0.0.1',
                    'localPort': 28690, 'remoteHost': '127.0.0.1', 'remotePort': 41000},
                    'payload': blob(b'{"business":"same"}'), 'binary': False}
        trace = {'exchanges': [], 'channelSent': [frame(1, 10), frame(2, 11)], 'channelReceived': []}
        expected = v2.transport_projection(trace)
        self.assertEqual(len(expected['channelStreams']), 2)
        renamed = copy.deepcopy(trace)
        for row in renamed['channelSent']:
            row['socket']['id'] += 100
        self.assertEqual(expected, v2.transport_projection(renamed))
        merged = copy.deepcopy(trace)
        merged['channelSent'][1]['socket']['id'] = 10
        self.assertNotEqual(expected, v2.transport_projection(merged))
        missing = copy.deepcopy(trace)
        del missing['channelSent'][0]['socket']['id']
        with self.assertRaisesRegex(ValueError, 'WS_SOCKET_IDENTITY'):
            v2.transport_projection(missing)

    def test_control_response_cannot_move_to_another_socket_with_the_same_tuple(self):
        data = fixture()
        row = next(row for row in data[1] if row['kind'] == 'fixture-ws-receive'
                   and json.loads(bytes.fromhex(row['payload']['rawBytesHex'])).get('id') == 'history-0')
        row['socket'] = dict(row['socket'], id=999)
        self.rejected(data, 'HISTORY_REQUEST_ORIGINAL')

    def test_reload_response_must_use_the_request_connection(self):
        data = fixture()
        row = next(row for row in data[1] if row['kind'] == 'fixture-ws-receive'
                   and json.loads(bytes.fromhex(row['payload']['rawBytesHex'])).get('id') == 'disable')
        row['socket'] = dict(row['socket'], id=999)
        self.rejected(data, 'RELOAD_REQUEST')

    def test_wecom_acknowledgement_must_use_the_reply_connection(self):
        reply = {'cmd': 'aibot_respond_msg', 'headers': {'req_id': 'same'}, 'body': {'markdown': {'content': 'final'}}}
        ack = {'headers': {'req_id': 'same'}, 'body': {'errcode': 0}}
        trace = {'channelReceived': [{'sequence': 10, 'socket': {'id': 4}, 'payload': blob(json.dumps(reply).encode())}],
                 'channelSent': [{'sequence': 11, 'socket': {'id': 5}, 'payload': blob(json.dumps(ack).encode())}]}
        with self.assertRaisesRegex(ValueError, 'OUTBOUND_ACK'):
            v2._outbound({'wire': [reply]}, 'wecom', trace, {'source': 'wire', 'index': 0}, 'final', 'same')

    def test_fixed_normalizer_pair_requires_both_complete_preserved_roots(self):
        raw, records, files = full_wire_fixture()
        ts_raw = copy.deepcopy(raw)
        ts_raw['engine'] = 'ts'
        with tempfile.TemporaryDirectory() as temp:
            roots = [Path(temp) / name for name in ('ts', 'native')]
            for root in roots:
                for original, data in files.items():
                    path = root / Path(original).relative_to('/test/cur')
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
            result = v2.compare_pair(ts_raw, raw, records, records, files, files, ts_root=roots[0], native_root=roots[1])
            self.assertTrue(result['equal'])
            self.assertFalse(result['formalAcceptance'])
            (roots[1] / 'project/extra.txt').write_text('unregistered business file')
            with self.assertRaisesRegex(ValueError, 'PAIR_DURABLE_ORIGINALS'):
                v2.compare_pair(ts_raw, raw, records, records, files, files, ts_root=roots[0], native_root=roots[1])


def http_wire_fixture(*, second_socket=False, chunked=False):
    """Synthetic original bytes in the real observer's server-side row shape."""
    raw = {'runtime': {'mockPort': 28690, 'controlPort': 28610},
           'http': [], 'models': [], 'wire': [], 'controlFrames': []}
    records = []

    def add(kind, **fields):
        records.append({'kind': kind, 'sequence': len(records), **fields})

    for index in range(2):
        socket = {'id': index + 1 if second_socket else 1, 'localHost': '127.0.0.1',
                  'localPort': 28690, 'remoteHost': '127.0.0.1',
                  'remotePort': 41000 + index if second_socket else 41000}
        target = '/channel/out?part=' + str(index)
        body = b'{"text":"unit"}'
        response_body = b'{"ok":true}'
        request_headers = ['Host', '127.0.0.1:28690', 'Authorization', 'Bearer fixture',
                           'Content-Length', str(len(body)), 'X-Repeat', 'first', 'X-Repeat', 'second']
        request_head = ('POST ' + target + ' HTTP/1.1\r\n' + ''.join(
            f'{key}: {value}\r\n' for key, value in zip(request_headers[::2], request_headers[1::2])) + '\r\n').encode()
        request_wire = request_head + body
        for chunk in (request_wire[:11], request_wire[11:]):
            add('fixture-tcp-data', socket=copy.deepcopy(socket), payload=blob(chunk))
        raw['http'].append({'method': 'POST', 'path': '/channel/out', 'body': json.loads(body),
                            'authorization': 'Bearer fixture'})
        add('mock-http-request', id=index, socket=copy.deepcopy(socket), method='POST', url=target,
            headers={'host': '127.0.0.1:28690', 'authorization': 'Bearer fixture',
                     'content-length': str(len(body)), 'x-repeat': 'first, second'},
            rawHeaders=request_headers, body=blob(body))
        response_headers = {'content-type': 'application/json',
                            'transfer-encoding' if chunked else 'content-length':
                                'chunked' if chunked else str(len(response_body))}
        response_head = 'HTTP/1.1 200 Kept reason\r\n' + ''.join(
            f'{key}: {value}\r\n' for key, value in response_headers.items()) + '\r\n'
        response_wire = response_head.encode() + (f'{len(response_body):x}\r\n'.encode() + response_body + b'\r\n0\r\nX-Trailer: kept\r\n\r\n'
                                                    if chunked else response_body)
        for chunk in (response_wire[:9], response_wire[9:]):
            add('fixture-tcp-write', socket=copy.deepcopy(socket), payload=blob(chunk))
        add('mock-http-response', id=index, status=200, finished=True, headers=response_headers,
            actualHeaderText=response_head, body=blob(response_body))
    return raw, records


class ChannelHttpWireControls(unittest.TestCase):
    def _preconnect_rows(self):
        raw = {'runtime': {'mockPort': 28690, 'controlPort': 28610}}
        partial = {'id': 4, 'localHost': '127.0.0.1', 'localPort': 41000,
                   'remoteHost': None, 'remotePort': None}
        complete = {**partial, 'remoteHost': '127.0.0.1', 'remotePort': 28610}
        return raw, [
            {'sequence': 0, 'kind': 'fixture-tcp-write', 'socket': partial,
             'payload': blob(b'GET /ws HTTP/1.1\r\n\r\n')},
            {'sequence': 1, 'kind': 'fixture-tcp-connect', 'socket': complete},
            {'sequence': 2, 'kind': 'fixture-tcp-data', 'socket': copy.deepcopy(complete),
             'payload': blob(b'HTTP/1.1 101 Switching Protocols\r\n\r\n')}]

    def test_preconnect_bytes_bind_only_to_same_socket_connect(self):
        raw, records = self._preconnect_rows()
        stream = v2.owned_tcp_streams(raw, records)[4]
        self.assertEqual(stream['socket'], records[1]['socket'])
        self.assertEqual(stream['write'], v2.payload(records[0]['payload']))
        self.assertEqual(stream['endpointProof'], {'connectSequence': 1, 'preConnectSequences': [0]})
        self.assertIsNone(records[0]['socket']['remotePort'], 'original capture must stay untouched')

    def test_later_tcp_or_url_cannot_replace_missing_connect(self):
        raw, records = self._preconnect_rows()
        records[0]['socket']['url'] = 'ws://127.0.0.1:28610/ws'
        records.pop(1)
        with self.assertRaisesRegex(ValueError, 'TCP_CONNECT_REQUIRED'):
            v2.owned_tcp_streams(raw, records)

    def test_connect_cannot_change_known_endpoint_or_socket_identity(self):
        for change, rule in [('known', 'TCP_SOCKET_IDENTITY'), ('id', 'TCP_CONNECT_REQUIRED'),
                             ('incomplete', 'TCP_CONNECT_ENDPOINT')]:
            with self.subTest(change=change):
                raw, records = self._preconnect_rows()
                if change == 'known':
                    records[0]['socket']['localPort'] += 1
                elif change == 'id':
                    records[1]['socket']['id'] += 1
                else:
                    records[1]['socket']['remoteHost'] = None
                with self.assertRaisesRegex(ValueError, rule):
                    v2.owned_tcp_streams(raw, records)

    def test_duplicate_connect_and_partial_after_connect_are_rejected(self):
        for change, rule in [('duplicate', 'TCP_CONNECT_IDENTITY'), ('late', 'TCP_CONNECT_REQUIRED')]:
            with self.subTest(change=change):
                raw, records = self._preconnect_rows()
                if change == 'duplicate':
                    records.append({**copy.deepcopy(records[1]), 'sequence': 3})
                else:
                    records[0], records[1] = records[1], records[0]
                    for index, row in enumerate(records):
                        row['sequence'] = index
                with self.assertRaisesRegex(ValueError, rule):
                    v2.owned_tcp_streams(raw, records)

    def test_real_server_row_shape_binds_split_chunks_and_fifo(self):
        raw, records = http_wire_fixture()
        result = v2.http_wire_observation(raw, records)
        self.assertEqual([pair['request']['target'] for pair in result['exchanges']],
                         ['/channel/out?part=0', '/channel/out?part=1'])
        self.assertEqual(result['exchanges'][0]['request']['headers']['x-repeat'], ['first', 'second'])
        self.assertEqual(result['exchanges'][0]['response']['line'], 'HTTP/1.1 200 Kept reason')
        self.assertEqual(result['exchanges'][0]['response']['body'], b'{"ok":true}')
        self.assertFalse(result['formalAcceptance'])

    def test_chunked_bytes_and_trailers_are_retained(self):
        raw, records = http_wire_fixture(chunked=True)
        result = v2.http_wire_observation(raw, records)
        self.assertEqual(result['exchanges'][0]['response']['trailers'], {'x-trailer': ['kept']})

    def test_coalesced_pipelined_messages_still_bind_fifo(self):
        raw, original = http_wire_fixture()
        requests = [copy.deepcopy(row) for row in original if row['kind'] == 'mock-http-request']
        responses = [copy.deepcopy(row) for row in original if row['kind'] == 'mock-http-response']
        incoming = b''.join(v2.payload(row['payload']) for row in original if row['kind'] == 'fixture-tcp-data')
        outgoing = b''.join(v2.payload(row['payload']) for row in original if row['kind'] == 'fixture-tcp-write')
        socket = copy.deepcopy(requests[0]['socket'])
        records = [{'kind': 'fixture-tcp-data', 'socket': socket, 'payload': blob(incoming)}, *requests,
                   {'kind': 'fixture-tcp-write', 'socket': socket, 'payload': blob(outgoing)}, *responses]
        for sequence, row in enumerate(records):
            row['sequence'] = sequence
        result = v2.http_wire_observation(raw, records)
        self.assertEqual([pair['request']['target'] for pair in result['exchanges']],
                         ['/channel/out?part=0', '/channel/out?part=1'])

    def test_same_body_cannot_supply_another_socket_request(self):
        raw, records = http_wire_fixture(second_socket=True)
        requests = [row for row in records if row['kind'] == 'mock-http-request']
        requests[0]['socket'], requests[1]['socket'] = requests[1]['socket'], requests[0]['socket']
        with self.assertRaisesRegex(ValueError, 'HTTP_WIRE_REQUEST|HTTP_WIRE_EVENT_ORDER'):
            v2.http_wire_observation(raw, records)

    def test_unclaimed_raw_business_connection_is_rejected(self):
        raw, records = http_wire_fixture(second_socket=True)
        extra = copy.deepcopy([row for row in records if row['kind'].startswith('fixture-tcp')
                               and row['socket']['id'] == 2])
        for row in extra:
            row['socket']['id'] = 99
            row['socket']['remotePort'] = 41999
            row['sequence'] = len(records)
            records.append(row)
        with self.assertRaisesRegex(ValueError, 'HTTP_WIRE_UNCLAIMED_EXCHANGE'):
            v2.http_wire_observation(raw, records)

    def test_missing_bytes_are_not_reconstructed_from_json(self):
        raw, records = http_wire_fixture()
        records = [row for row in records if not row['kind'].startswith('fixture-tcp')]
        with self.assertRaisesRegex(ValueError, 'HTTP_WIRE_SOCKET'):
            v2.http_wire_observation(raw, records)

    def test_raw_query_and_status_reason_cannot_disagree_with_logical_rows(self):
        for change in ('query', 'status'):
            with self.subTest(change=change):
                raw, records = http_wire_fixture()
                if change == 'query':
                    row = next(row for row in records if row['kind'] == 'mock-http-request')
                    row['url'] = '/channel/out?part=other'
                else:
                    row = next(row for row in records if row['kind'] == 'mock-http-response')
                    row['actualHeaderText'] = row['actualHeaderText'].replace('Kept reason', 'Invented reason')
                with self.assertRaisesRegex(ValueError, 'HTTP_WIRE_REQUEST|HTTP_WIRE_RESPONSE'):
                    v2.http_wire_observation(raw, records)

    def test_truncation_endpoint_reuse_and_response_tail_fail(self):
        for change, rule in [('truncated', 'TRANSPORT_BYTES'), ('endpoint', 'TCP_SOCKET_IDENTITY'),
                             ('tail', 'HTTP_RAW_|HTTP_WIRE_')]:
            with self.subTest(change=change):
                raw, records = http_wire_fixture()
                if change == 'truncated':
                    records[0]['payload']['truncated'] = True
                elif change == 'endpoint':
                    records[1]['socket']['remotePort'] += 1
                else:
                    row = next(row for row in reversed(records) if row['kind'] == 'fixture-tcp-write')
                    row['payload'] = blob(v2.payload(row['payload']) + b'unregistered tail')
                with self.assertRaisesRegex(ValueError, rule):
                    v2.http_wire_observation(raw, records)

    def test_mock_upgrade_preserves_exact_offsets_and_unparsed_frame_bytes(self):
        raw = {'runtime': {'mockPort': 28690, 'controlPort': 28610},
               'http': [], 'models': [], 'wire': [], 'controlFrames': []}
        socket = {'id': 8, 'localHost': '127.0.0.1', 'localPort': 28690,
                  'remoteHost': '127.0.0.1', 'remotePort': 41000}
        request = b'GET /feishu/stream?service_id=77 HTTP/1.1\r\nUpgrade: websocket\r\n\r\n'
        response = b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n'
        records = [{'sequence': 0, 'kind': 'fixture-tcp-data', 'socket': socket,
                    'payload': blob(request + b'\x81\x80abcd')},
                   {'sequence': 1, 'kind': 'fixture-tcp-write', 'socket': socket,
                    'payload': blob(response + b'\x81\x00')}]
        result = v2.http_wire_observation(raw, records)
        upgrade = result['upgrades'][0]
        self.assertEqual(upgrade['requestOffset'], len(request))
        self.assertEqual(upgrade['responseOffset'], len(response))
        self.assertEqual(upgrade['request']['headerBytes'], request)
        self.assertEqual(result['streams'][8]['read'][upgrade['requestOffset']:], b'\x81\x80abcd')
        self.assertFalse(result['formalAcceptance'], 'HTTP boundary is not a WS handshake/frame verdict')

    def test_informational_before_upgrade_is_explicitly_unsupported(self):
        raw = {'runtime': {'mockPort': 28690, 'controlPort': 28610},
               'http': [], 'models': [], 'wire': [], 'controlFrames': []}
        socket = {'id': 8, 'localHost': '127.0.0.1', 'localPort': 28690,
                  'remoteHost': '127.0.0.1', 'remotePort': 41000}
        request = b'GET /stream HTTP/1.1\r\nUpgrade: websocket\r\n\r\n'
        upgrade = b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n'
        for informational in (b'HTTP/1.1 100 Continue\r\n\r\n',
                              b'HTTP/1.1 103 Early Hints\r\nLink: </kept>\r\n\r\n'):
            with self.subTest(informational=informational):
                records = [{'sequence': 0, 'kind': 'fixture-tcp-data', 'socket': socket,
                            'payload': blob(request)},
                           {'sequence': 1, 'kind': 'fixture-tcp-write', 'socket': socket,
                            'payload': blob(informational + upgrade)}]
                with self.assertRaisesRegex(ValueError, 'HTTP_WIRE_INFORMATIONAL_UPGRADE'):
                    v2.http_wire_observation(raw, records)


def ws_wire_frame(value, *, masked=False, opcode=1, fin=True, length_code=None, mask_key=b'\x12\x34\x56\x78'):
    """Independent byte fixture; no production encoder or decoded-only evidence."""
    size = len(value)
    code = length_code if length_code is not None else (size if size < 126 else (126 if size < 65536 else 127))
    head = bytes([(128 if fin else 0) | opcode, (128 if masked else 0) | code])
    if code == 126:
        head += size.to_bytes(2, 'big')
    elif code == 127:
        head += size.to_bytes(8, 'big')
    return head + (mask_key + bytes(value[i] ^ mask_key[i % 4] for i in range(size)) if masked else value)


def ws_wire_fixture(role='mock-server', *, fragmented=False, binary=False, length_code=None, payload_size=None,
                    nonce=b'the sample nonce', mask_key=b'\x12\x34\x56\x78'):
    raw = {'runtime': {'mockPort': 28690, 'controlPort': 28610},
           'http': [], 'models': [], 'wire': [], 'controlFrames': []}
    server = role == 'mock-server'
    socket = {'id': 18, 'localHost': '127.0.0.1', 'localPort': 28690 if server else 41000,
              'remoteHost': '127.0.0.1', 'remotePort': 41000 if server else 28610}
    if not server:
        socket['url'] = 'ws://127.0.0.1:28610/ws?token=fixture'
    key = base64.b64encode(nonce)
    accept = base64.b64encode(hashlib.sha1(key + b'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest())
    request = (b'GET /ws?token=fixture HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\n'
               b'Connection: keep-alive, Upgrade\r\nSec-WebSocket-Version: 13\r\n'
               b'Sec-WebSocket-Key: ' + key + b'\r\n'
               b'Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits\r\n\r\n')
    response = (b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n'
                b'Sec-WebSocket-Accept: ' + accept + b'\r\n\r\n')
    records = []
    def add(kind, data, **fields):
        records.append({'sequence': len(records), 'kind': kind, 'socket': copy.deepcopy(socket),
                        'payload': blob(data), **fields})
    add('fixture-tcp-data' if server else 'fixture-tcp-write', request)
    add('fixture-tcp-write' if server else 'fixture-tcp-data', response)
    sent, received = b'{"send":1}', b'{"ok":true}'
    if payload_size is not None:
        sent, received = b'"' + b's' * (payload_size - 2) + b'"', b'"' + b'r' * (payload_size - 2) + b'"'
    sends = [sent[:5], sent[5:]] if fragmented else [sent]
    for index, part in enumerate(sends):
        add('fixture-ws-send', part)
        frame = ws_wire_frame(part, masked=not server, opcode=1 if index == 0 else 0,
                              fin=index == len(sends) - 1, length_code=length_code, mask_key=mask_key)
        # Split within the original header, not only at whole-frame boundaries.
        add('fixture-tcp-write', frame[:1])
        add('fixture-tcp-write', frame[1:])
    receives = [received[:7], received[7:]] if fragmented else [received]
    for index, part in enumerate(receives):
        frame = ws_wire_frame(part, masked=server, opcode=(2 if binary else 1) if index == 0 else 0,
                              fin=index == len(receives) - 1, length_code=length_code, mask_key=mask_key)
        add('fixture-tcp-data', frame[:1])
        add('fixture-tcp-data', frame[1:])
        if fragmented and index == 0:
            add('fixture-tcp-data', ws_wire_frame(b'ping', masked=server, opcode=9, mask_key=mask_key))
    add('fixture-ws-receive', received, binary=binary)
    raw['wire' if server else 'controlFrames'].append(
        {'binary': v2.base64.b64encode(received).decode()} if server and binary else json.loads(received))
    return raw, records


class WebSocketWireControls(unittest.TestCase):
    def test_real_upgrade_both_roles_split_headers_and_api_bindings(self):
        for role in ('mock-server', 'control-client'):
            with self.subTest(role=role):
                raw, records = ws_wire_fixture(role)
                observed = v2.ws_wire_observation(raw, records)
                connection = observed['connections'][0]
                self.assertEqual(connection['role'], role)
                self.assertEqual(connection['upgrade']['request']['target'], '/ws?token=fixture')
                self.assertEqual(connection['directions']['read']['messages'][0]['payload'], b'{"ok":true}')
                self.assertEqual([row['kind'] for row in connection['apiBindings']], ['send', 'receive'])
                self.assertFalse(observed['formalAcceptance'])

    def test_fragment_reassembly_control_interleaving_and_send_call_boundaries(self):
        for role in ('mock-server', 'control-client'):
            with self.subTest(role=role):
                raw, records = ws_wire_fixture(role, fragmented=True, binary=True)
                connection = v2.ws_wire_observation(raw, records)['connections'][0]
                incoming = connection['directions']['read']
                self.assertEqual([row['opcode'] for row in incoming['frames']], [2, 9, 0])
                self.assertEqual(incoming['messages'][0]['frames'], [0, 2])
                self.assertTrue(incoming['messages'][0]['binary'])
                self.assertEqual([row['kind'] for row in connection['apiBindings']], ['send', 'send', 'receive'])
                self.assertEqual(len(connection['interleaving']), 5)

    def test_length_encoding_and_actual_mask_bytes_are_preserved(self):
        headers = []
        for code, size in ((None, 10), (126, 126), (127, 65536)):
            raw, records = ws_wire_fixture(length_code=code, payload_size=size)
            frame = v2.ws_wire_observation(raw, records)['connections'][0]['directions']['read']['frames'][0]
            self.assertEqual(frame['lengthCode'], size if code is None else code)
            self.assertTrue(frame['wireHeaderHex'].endswith('12345678'))
            self.assertEqual(frame['wireHex'], frame['wireHeaderHex'] + frame['wirePayloadHex'])
            headers.append(frame['wireHeaderHex'])
        self.assertEqual(len(set(headers)), 3, 'original short, 16-bit and 64-bit headers remain distinct')

    def test_nonminimal_extended_lengths_are_malformed_not_normalized(self):
        for code in (126, 127):
            with self.subTest(code=code):
                raw, records = ws_wire_fixture(length_code=code)
                with self.assertRaisesRegex(ValueError, 'WS_WIRE_NONMINIMAL_LENGTH'):
                    v2.ws_wire_observation(raw, records)

    def test_control_api_url_must_match_actual_endpoint_and_upgrade_target(self):
        for url in (None, 'ws://127.0.0.1:28611/ws?token=fixture',
                    'ws://127.0.0.1:28610/other?token=fixture'):
            with self.subTest(url=url):
                raw, records = ws_wire_fixture('control-client')
                for row in records:
                    if row['kind'].startswith('fixture-ws-'):
                        row['socket']['url'] = url
                with self.assertRaisesRegex(ValueError, 'WS_WIRE_API_TARGET'):
                    v2.ws_wire_observation(raw, records)

    def test_untrusted_handshake_fields_fail_at_actual_tcp_boundary(self):
        changes = [('key', 'WS_WIRE_UPGRADE_KEY'), ('accept', 'WS_WIRE_UPGRADE_ACCEPT'),
                   ('connection', 'WS_WIRE_UPGRADE_CONNECTION'), ('extension', 'WS_WIRE_UNSUPPORTED_EXTENSION'),
                   ('protocol', 'WS_WIRE_UPGRADE_PROTOCOL')]
        for change, rule in changes:
            with self.subTest(change=change):
                raw, records = ws_wire_fixture()
                index = 0 if change == 'key' else 1
                data = v2.payload(records[index]['payload'])
                if change == 'key':
                    data = data.replace(b'dGhlIHNhbXBsZSBub25jZQ==', b'YQ==')
                elif change == 'accept':
                    data = data.replace(b's3pPLMBiTxaQ9kYGzzhZRbK+xOo=', b'AAAAAAAAAAAAAAAAAAAAAAAAAAA=')
                elif change == 'connection':
                    data = data.replace(b'Connection: Upgrade', b'Connection: close')
                else:
                    extra = (b'Sec-WebSocket-Extensions: permessage-deflate' if change == 'extension'
                             else b'Sec-WebSocket-Protocol: not-offered')
                    data = data[:-2] + extra + b'\r\n\r\n'
                records[index]['payload'] = blob(data)
                with self.assertRaisesRegex(ValueError, rule):
                    v2.ws_wire_observation(raw, records)

    def test_api_payload_binary_socket_and_missing_tcp_cannot_pass(self):
        for change, rule in [('payload', 'WS_WIRE_API_PAYLOAD'), ('binary', 'WS_WIRE_API_BINARY'),
                             ('socket', 'WS_WIRE_API_SOCKET'), ('missing-tcp', 'WS_WIRE_API_COUNT')]:
            with self.subTest(change=change):
                raw, records = ws_wire_fixture()
                receive = records[-1]
                if change == 'payload':
                    receive['payload'] = blob(b'{"ok":false}')
                    raw['wire'] = [{'ok': False}]
                elif change == 'binary':
                    receive['binary'] = True
                    raw['wire'] = [{'binary': v2.base64.b64encode(b'{"ok":true}').decode()}]
                elif change == 'socket':
                    for row in records:
                        if row['kind'].startswith('fixture-ws-'):
                            row['socket']['remoteHost'] = '127.0.0.2'
                else:
                    records = [row for row in records if not (row['kind'] == 'fixture-tcp-data' and row['sequence'] > 1)]
                with self.assertRaisesRegex(ValueError, rule):
                    v2.ws_wire_observation(raw, records)

    def test_frames_reject_wrong_mask_rsv_opcode_incomplete_payload_and_control(self):
        for change, rule in [('mask', 'WS_WIRE_MASK'), ('rsv', 'WS_WIRE_RSV'),
                             ('opcode', 'WS_WIRE_OPCODE'), ('payload', 'WS_WIRE_TRUNCATED_PAYLOAD'),
                             ('control', 'WS_WIRE_CONTROL'), ('length', 'WS_WIRE_TRUNCATED_LENGTH'),
                             ('mask-key', 'WS_WIRE_TRUNCATED_MASK'), ('continuation', 'WS_WIRE_CONTINUATION')]:
            with self.subTest(change=change):
                raw, records = ws_wire_fixture()
                incoming = [row for row in records if row['kind'] == 'fixture-tcp-data' and row['sequence'] > 1]
                data = bytearray(b''.join(v2.payload(row['payload']) for row in incoming))
                if change == 'mask': data[1] &= 127
                elif change == 'rsv': data[0] |= 64
                elif change == 'opcode': data[0] = 131
                elif change == 'payload': del data[-1:]
                elif change == 'control': data[0] = 9
                elif change == 'length': data = bytearray(b'\x81\xfe\x00')
                elif change == 'mask-key': data = bytearray(b'\x81\x80\x01')
                else: data[0] = 128
                incoming[0]['payload'] = blob(data)
                incoming[1]['payload'] = blob(b'')
                with self.assertRaisesRegex(ValueError, rule):
                    v2.ws_wire_observation(raw, records)

    def test_record_order_and_deferred_http_objects_are_authenticated(self):
        raw, records = ws_wire_fixture()
        send_index = next(index for index, row in enumerate(records) if row['kind'] == 'fixture-ws-send')
        records[send_index], records[send_index + 1] = records[send_index + 1], records[send_index]
        for sequence, row in enumerate(records): row['sequence'] = sequence
        with self.assertRaisesRegex(ValueError, 'WS_WIRE_API_ORDER'):
            v2.ws_wire_observation(raw, records)
        raw, records = ws_wire_fixture()
        forged = v2.http_wire_observation(raw, records)
        forged['upgrades'][0]['requestOffset'] += 1
        with self.assertRaisesRegex(ValueError, 'WS_WIRE_HTTP_BINDING'):
            v2.ws_wire_observation(raw, records, forged)


def full_wire_fixture(channel='qq', *, nonce=b'the sample nonce', mask_key=b'\x12\x34\x56\x78'):
    """Synthetic complete originals for integration controls, never a capture.

    The byte fixture encoder is independent of the production parsers. This
    helper is deliberately test-only; production never fills missing TCP data
    from application records.
    """
    raw, original, files = fixture(channel)
    records, sockets = [], {}
    for identity, role in ((2, 'mock-server'), (3, 'control-client')):
        _, handshake = ws_wire_fixture(role, nonce=nonce, mask_key=mask_key)
        socket = copy.deepcopy(handshake[0]['socket'])
        socket['id'] = identity
        sockets[identity] = socket
        for row in handshake[:2]:
            records.append(dict(row, socket=copy.deepcopy(socket)))
    http_socket = {'id': 1, 'localHost': '127.0.0.1', 'localPort': 28690,
                   'remoteHost': '127.0.0.1', 'remotePort': 41001}
    for original_row in original:
        row = copy.deepcopy(original_row)
        kind = row['kind']
        if kind == 'mock-http-request':
            body = v2.payload(row['body'])
            row['socket'] = copy.deepcopy(http_socket)
            row['headers']['content-length'] = str(len(body))
            row['rawHeaders'].extend(['Content-Length', str(len(body))])
            head = (row['method'] + ' ' + row['url'] + ' HTTP/1.1\r\n'
                    + ''.join(f'{key}: {value}\r\n' for key, value in
                              zip(row['rawHeaders'][::2], row['rawHeaders'][1::2])) + '\r\n')
            records.append({'kind': 'fixture-tcp-data', 'socket': copy.deepcopy(http_socket),
                            'payload': blob(head.encode('latin-1') + body)})
            records.append(row)
        elif kind == 'mock-http-response':
            records.append({'kind': 'fixture-tcp-write', 'socket': copy.deepcopy(http_socket),
                            'payload': blob(row['actualHeaderText'].encode('latin-1') + v2.payload(row['body']))})
            records.append(row)
        else:
            assert kind in ('fixture-ws-send', 'fixture-ws-receive')
            socket = copy.deepcopy(sockets[row['socket']['id']])
            row['socket'] = socket
            sending = kind == 'fixture-ws-send'
            masked = sending == (socket['id'] == 3)
            frame = ws_wire_frame(v2.payload(row['payload']), masked=masked,
                                  opcode=2 if row.get('binary') else 1, mask_key=mask_key)
            wire = {'kind': 'fixture-tcp-write' if sending else 'fixture-tcp-data',
                    'socket': copy.deepcopy(socket), 'payload': blob(frame)}
            records.extend([row, wire] if sending else [wire, row])
    for sequence, row in enumerate(records):
        row['sequence'] = sequence
    return raw, records, files


class WireProjectionControls(unittest.TestCase):
    def test_complete_pair_rejects_invalid_utf8_requests_and_text_frames(self):
        # A malformed HTTP body is observable: the producer keeps the bytes
        # and uses Buffer.toString() replacement characters in its raw row.
        # An invalid text WS frame must not invent a receive API event: the
        # real ws receiver validates UTF-8 before emitting a text message.
        def request(raw, records, body):
            raw, records = copy.deepcopy((raw, records))
            socket = {'id': 4, 'localHost': '127.0.0.1', 'localPort': 28690,
                      'remoteHost': '127.0.0.1', 'remotePort': 41002}
            value = {'raw': body.decode('utf-8', errors='replace')}
            raw['http'].append({'generation': 1, 'method': 'POST', 'path': '/v1/probe',
                                'body': value, 'authorization': 'Bearer fixture'})
            raw['models'].append(copy.deepcopy(value))
            identity = max(row['id'] for row in records if row['kind'] == 'mock-http-request') + 1
            headers = {'host': '127.0.0.1:28690', 'authorization': 'Bearer fixture',
                       'content-length': str(len(body))}
            head = ('POST /v1/probe HTTP/1.1\r\n'
                    + ''.join(f'{key}: {value}\r\n' for key, value in headers.items()) + '\r\n')
            reply = 'HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n'
            records.extend([
                {'kind': 'fixture-tcp-data', 'socket': socket, 'payload': blob(head.encode() + body)},
                {'kind': 'mock-http-request', 'id': identity, 'socket': socket, 'method': 'POST',
                 'url': '/v1/probe', 'headers': headers,
                 'rawHeaders': [item for pair in headers.items() for item in pair], 'body': blob(body)},
                {'kind': 'fixture-tcp-write', 'socket': socket, 'payload': blob(reply.encode() + b'{}')},
                {'kind': 'mock-http-response', 'id': identity, 'status': 200, 'finished': True,
                 'headers': {'content-type': 'application/json', 'content-length': '2'},
                 'actualHeaderText': reply, 'body': blob(b'{}')},
            ])
            for sequence, row in enumerate(records):
                row['sequence'] = sequence
            return raw, records

        def text_frame(raw, records, body, received):
            raw, records = copy.deepcopy((raw, records))
            sockets = [row['socket'] for row in records if row['kind'] == 'fixture-tcp-data'
                       and isinstance(row.get('socket'), dict)
                       and type(row['socket'].get('id')) is int and row['socket']['id'] == 2
                       and row['socket'].get('localPort') == raw['runtime']['mockPort']]
            self.assertTrue(sockets, 'the complete fixture must contain the real mock TCP connection')
            self.assertTrue(all(socket == sockets[0] for socket in sockets))
            socket = copy.deepcopy(sockets[0])
            records.append({'kind': 'fixture-tcp-data', 'socket': socket,
                            'payload': blob(ws_wire_frame(body, masked=True, opcode=1))})
            if received:
                raw['wire'].append(json.loads(body))
                records.append({'kind': 'fixture-ws-receive', 'socket': socket,
                                'payload': blob(body), 'binary': False})
            for sequence, row in enumerate(records):
                row['sequence'] = sequence
            return raw, records

        left_raw, left_records, files = full_wire_fixture()
        left_raw['engine'] = 'ts'
        right_raw, right_records, right_files = full_wire_fixture(
            nonce=b'another nonce 16', mask_key=b'abcd')
        self.assertEqual(files, right_files)
        originals = copy.deepcopy((left_raw, left_records, right_raw, right_records, files))
        with tempfile.TemporaryDirectory() as temp:
            roots = [Path(temp) / side for side in ('ts', 'native')]
            for root in roots:
                for original, data in files.items():
                    path = root / Path(original).relative_to('/test/cur')
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
            for domain in ('request', 'text-frame'):
                with self.subTest(domain=domain):
                    if domain == 'request':
                        left = request(left_raw, left_records, b'Xb')
                        right = request(right_raw, right_records, b'Xb')
                        invalid = request(right_raw, right_records, b'\xffb')
                    else:
                        left = text_frame(left_raw, left_records, b'{"probe":"valid"}', True)
                        right = text_frame(right_raw, right_records, b'{"probe":"valid"}', True)
                        invalid = text_frame(right_raw, right_records, b'\xffb', False)
                    inputs = copy.deepcopy((left, right, invalid))
                    def compare(pair):
                        return v2.compare_pair(left[0], pair[0], left[1], pair[1], files, files,
                                               ts_root=roots[0], native_root=roots[1])
                    positive = compare(right)
                    self.assertTrue(positive['equal'])
                    self.assertEqual(positive['differentFields'], [])
                    self.assertFalse(positive['formalAcceptance'])
                    # No successful pair/receipt may be returned. An unrelated
                    # framing, binding or business failure must not satisfy this.
                    if domain == 'request':
                        with self.assertRaises(UnicodeDecodeError):
                            compare(invalid)
                    else:
                        with self.assertRaisesRegex(ValueError, '^CHANNEL_V2_WS_WIRE_TEXT_UTF8$'):
                            compare(invalid)
                    self.assertEqual(inputs, (left, right, invalid))
        self.assertEqual(originals, (left_raw, left_records, right_raw, right_records, files))

    def test_http_text_sentinel_reaches_final_comparison(self):
        self._compare_http_payloads([(b'{"raw":"X"}', b'X', True, False)])

    def test_complete_pair_distinguishes_http_payload_types(self):
        self._compare_http_payloads([
            (b'{"rawBytesHex":"ff62"}', b'\xffb', True, False),
            (b'{"wireKind":"text","value":"X"}', b'X', True, False),
            (b'\xffb', b'\xffc', True, False),
            (b'plain', b'other', True, False),
            (b'NaN', b'Infinity', True, False),
            (b'NaN', b'NaN', True, True),
            (b'{"n":NaN}', b'{"n":null}', True, False),
            (b'', b'', True, True),
            (b'', b'""', True, False),
            (b'', b'null', True, False),
            (b'null', b'null', True, True),
            (b'\xffb', b'\xffb', True, True),
            # Existing JSON semantics remain, including duplicate-key last-wins.
            (b'{"a":1,"a":2}', b'{"a":2}      ', True, True),
            (b'{"a":2}', b' { "a": 2 } ', True, True),
            # Content-Length still distinguishes otherwise equal JSON bodies.
            (b'{"a":2}', b' { "a": 2 } ', False, False),
        ])

    def _compare_http_payloads(self, cases):
        # Synthetic complete originals exercise the final selector, not just
        # a helper. Live HTTP captures are separately retained as evidence.
        def changed_response(records, body, chunked):
            result = copy.deepcopy(records)
            index = next(i for i, row in enumerate(result)
                         if row['kind'] == 'mock-http-response'
                         and row['headers'].get('content-type') == 'application/json')
            response, wire = result[index], result[index - 1]
            self.assertEqual(wire['kind'], 'fixture-tcp-write')
            response['body'] = blob(body)
            headers = response['headers']
            headers.pop('content-length', None)
            headers.pop('transfer-encoding', None)
            if chunked:
                headers['transfer-encoding'] = 'chunked'
                chunk = f'{len(body):x}\r\n'.encode() + body + b'\r\n' if body else b''
                framed = chunk + b'0\r\n\r\n'
            else:
                headers['content-length'] = str(len(body))
                framed = body
            response['actualHeaderText'] = ('HTTP/1.1 200 OK\r\n'
                + ''.join(f'{key}: {value}\r\n' for key, value in headers.items()) + '\r\n')
            wire['payload'] = blob(response['actualHeaderText'].encode('latin-1') + framed)
            return result

        left_raw, left_records, files = full_wire_fixture()
        right_raw, right_records, right_files = full_wire_fixture(
            nonce=b'another nonce 16', mask_key=b'abcd')
        left_raw['engine'] = 'ts'
        self.assertEqual(files, right_files)
        originals = copy.deepcopy((left_raw, left_records, right_raw, right_records, files))
        with tempfile.TemporaryDirectory() as temp:
            roots = [Path(temp) / side for side in ('ts', 'native')]
            for root in roots:
                for original, data in files.items():
                    path = root / Path(original).relative_to('/test/cur')
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
            for left, right, chunked, equal in cases:
                with self.subTest(left=left, right=right, chunked=chunked):
                    result = v2.compare_pair(
                        left_raw, right_raw,
                        changed_response(left_records, left, chunked),
                        changed_response(right_records, right, chunked), files, files,
                        ts_root=roots[0], native_root=roots[1])
                    self.assertEqual(result['equal'], equal)
                    self.assertEqual(set(result['differentFields']),
                                     set() if equal else {'exchanges', 'wireHttpStreams'})
                    self.assertFalse(result['formalAcceptance'])
        self.assertEqual(originals, (left_raw, left_records, right_raw, right_records, files))

    def test_comparison_json_envelope_keeps_parser_and_normalizer_boundaries(self):
        compare = v2._load('compare', v2._apparatus / 'compare.py')
        identity = '12345678-1234-4234-8234-123456789abc'
        normalizer = compare.Normalizer()
        normalizer.bind(identity, ('probe', 'known'))
        known = {'entryId': identity, 'turnId': 'turn', 'sequence': 1,
                 'type': 'probe', 'createdAt': 123}
        projected = normalizer.value(v2._wire_value(json.dumps(known).encode()))
        self.assertEqual(projected['wireKind'], 'json')
        self.assertEqual(projected['value']['entryId'], normalizer.value(identity))
        self.assertEqual(projected['value']['createdAt'], '<wall-clock>')
        self.assertEqual(v2._wire_value('{"a":2}'.encode('utf-16')),
                         v2._wire_value(b'{"a":2}'))
        for token in (b'NaN', b'Infinity', b'-Infinity', b'{"n":NaN}'):
            with self.subTest(token=token):
                self.assertEqual(v2._wire_value(token),
                                 {'wireKind': 'text', 'value': token.decode()})
        # Valid JSON number syntax that overflows is still rejected at the
        # existing canonical comparison boundary, not silently made text.
        with self.assertRaises(ValueError):
            v2.canonical(v2._wire_value(b'1e999'))

    def test_model_and_channel_comparisons_keep_payload_types(self):
        # Component projections, not synthetic evidence of full business runs.
        raw, records, _ = fixture()
        seed = v2.transport_observation(raw, records)['exchanges'][0]
        def model(body):
            pair = copy.deepcopy(seed)
            pair['request'].update(url='/v1/probe', body=blob(body))
            trace = {'exchanges': [pair], 'channelSent': [], 'channelReceived': []}
            return v2.transport_projection(trace)
        left, right = model(b'{"raw":"X"}'), model(b'X')
        self.assertNotEqual(left['models'], right['models'])
        self.assertNotEqual(left['exchanges'], right['exchanges'])

        def channel(body, binary):
            trace = {'exchanges': [], 'channelReceived': [], 'channelSent': [
                {'sequence': 1, 'socket': {'id': 7, 'localPort': 28690, 'remotePort': 41000},
                 'payload': blob(body), 'binary': binary}]}
            return v2.transport_projection(trace)['channelStreams']
        for binary in (False, True):
            with self.subTest(binary=binary):
                self.assertNotEqual(channel(b'{"rawBytesHex":"ff62"}', binary),
                                    channel(b'\xffb', binary))
        self.assertEqual(channel(b'\xffb', True)[0][0]['payload'],
                         {'wireKind': 'bytes', 'value': 'ff62'})
        self.assertNotEqual(channel(b'{}', False), channel(b'{}', True))
        self.assertNotEqual(channel(b'', False), channel(b'""', False))
        self.assertNotEqual(channel(b'NaN', False), channel(b'Infinity', False))
        self.assertEqual(channel(b'NaN', False)[0][0]['payload'],
                         {'wireKind': 'text', 'value': 'NaN'})

    def test_http_then_upgrade_keeps_same_socket_relationship(self):
        raw, http_records = http_wire_fixture()
        ws_raw, ws_records = ws_wire_fixture()
        raw['wire'] = ws_raw['wire']
        separate = copy.deepcopy(http_records + ws_records)
        for sequence, row in enumerate(separate):
            row['sequence'] = sequence
        combined = copy.deepcopy(separate)
        for row in combined:
            if row.get('socket', {}).get('id') == 18:
                row['socket']['id'] = 1
        self.assertNotEqual(v2.wire_projection(raw, separate), v2.wire_projection(raw, combined))

    def test_trailers_and_informational_headers_reach_comparison(self):
        raw, records = http_wire_fixture(chunked=True)
        expected = v2.wire_projection(raw, records)
        changed = copy.deepcopy(records)
        row = next(row for row in changed if row['kind'] == 'fixture-tcp-write'
                   and b'x-trailer' in v2.payload(row['payload']).lower())
        row['payload'] = blob(v2.payload(row['payload']).replace(b'kept', b'changed'))
        self.assertNotEqual(expected, v2.wire_projection(raw, changed))
        raw, records = http_wire_fixture()
        expected = v2.wire_projection(raw, records)
        row = next(row for row in records if row['kind'] == 'fixture-tcp-write')
        row['payload'] = blob(b'HTTP/1.1 103 Early Hints\r\nLink: </kept>\r\nDate: first\r\n\r\n' + v2.payload(row['payload']))
        changed = v2.wire_projection(raw, records)
        self.assertNotEqual(expected, changed)
        self.assertEqual(changed['wireHttpStreams'][0][0]['informational'],
                         [{'line': 'HTTP/1.1 103 Early Hints', 'headers': {'link': ['</kept>']}}])
        row['payload'] = blob(v2.payload(row['payload']).replace(b'Date: first', b'Date: second'))
        self.assertEqual(changed, v2.wire_projection(raw, records))

    def test_approved_remask_is_equal_but_control_frames_reach_comparison(self):
        raw, records = ws_wire_fixture('control-client')
        expected = v2.wire_projection(raw, records)
        changed = copy.deepcopy(records)
        # Both mask keys encode exactly the same application message.
        for row in changed:
            if row['kind'] == 'fixture-tcp-write' and v2.payload(row['payload']) == b'\x81':
                continue
            if row['kind'] == 'fixture-tcp-write' and b'GET ' not in v2.payload(row['payload']):
                row['payload'] = blob(ws_wire_frame(b'{"send":1}', masked=True,
                                                   mask_key=b'abcd')[1:])
                break
        self.assertEqual(expected, v2.wire_projection(raw, changed))
        changed = copy.deepcopy(records)
        changed.append({'kind': 'fixture-tcp-data', 'socket': copy.deepcopy(changed[0]['socket']),
                        'payload': blob(ws_wire_frame(b'ping', opcode=9)), 'sequence': len(changed)})
        projected = v2.wire_projection(raw, changed)
        self.assertNotEqual(expected, projected)
        self.assertEqual(projected['wireWebsocketStreams'][0]['frames'][-1]['opcode'], 9)

    def test_compare_pair_requires_wire_and_accepts_independent_valid_nonce_masks(self):
        raw, records, files = full_wire_fixture()
        ts_raw = copy.deepcopy(raw)
        ts_raw['engine'] = 'ts'
        independent_raw, independent_records, independent_files = full_wire_fixture(
            nonce=b'another nonce 16', mask_key=b'abcd')
        self.assertEqual(raw, independent_raw)
        self.assertEqual(files, independent_files)
        self.assertNotEqual(records, independent_records, 'the inputs must not be a self-comparison')
        with tempfile.TemporaryDirectory() as temp:
            roots = [Path(temp) / name for name in ('ts', 'native')]
            for root in roots:
                for original, data in files.items():
                    path = root / Path(original).relative_to('/test/cur')
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
            def compare(native_records):
                return v2.compare_pair(ts_raw, raw, records, native_records, files, files,
                                       ts_root=roots[0], native_root=roots[1])
            equal = compare(independent_records)
            self.assertTrue(equal['equal'])
            self.assertEqual(equal['wireBindings'], [
                {'engine': engine, 'httpSameSocket': True, 'websocketFrames': True}
                for engine in ('ts', 'native')])
            self.assertFalse(equal['formalAcceptance'])
            missing = [row for row in records if not row['kind'].startswith('fixture-tcp')]
            with self.assertRaisesRegex(ValueError, 'HTTP_WIRE_SOCKET'):
                compare(missing)
            changed = copy.deepcopy(records)
            for index, row in enumerate(changed):
                if (row['kind'] == 'fixture-tcp-write' and row['socket']['id'] == 3
                        and changed[index - 1]['kind'] == 'fixture-ws-send'):
                    row['payload'] = blob(ws_wire_frame(v2.payload(changed[index - 1]['payload']),
                                                       masked=True, mask_key=b'abcd'))
                    break
            remasked = compare(changed)
            self.assertTrue(remasked['equal'])
            self.assertEqual(remasked['differentFields'], [])
            self.assertFalse(remasked['formalAcceptance'])

            # A validated non-exempt wire difference must reach the final verdict.
            changed = copy.deepcopy(independent_records)
            socket = next(row['socket'] for row in changed if row['socket']['id'] == 3)
            changed.append({'kind': 'fixture-tcp-data', 'socket': copy.deepcopy(socket),
                            'payload': blob(ws_wire_frame(b'ping', opcode=9)), 'sequence': len(changed)})
            different = compare(changed)
            self.assertFalse(different['equal'])
            self.assertEqual(different['differentFields'], ['wireWebsocketStreams'])
            self.assertFalse(different['formalAcceptance'])

            def unknown_uuid(source_raw, source_records, identity):
                changed_raw, changed_records = copy.deepcopy(source_raw), copy.deepcopy(source_records)
                request = changed_raw['persistedSessions'][0]['request']
                original_request = copy.deepcopy(request)
                request['probeUuid'] = identity  # An extra reference, never an entity definition.
                matches = [index for index, row in enumerate(changed_records)
                           if row['kind'] == 'fixture-ws-send'
                           and json.loads(v2.payload(row['payload'])) == original_request]
                self.assertEqual(len(matches), 1)
                index = matches[0]
                api, wire = changed_records[index:index + 2]
                self.assertEqual((api['socket']['id'], wire['socket']['id'], wire['kind']),
                                 (3, 3, 'fixture-tcp-write'))
                value = json.dumps(request).encode()
                api['payload'] = blob(value)
                wire['payload'] = blob(ws_wire_frame(value, masked=True, mask_key=b'abcd'))
                return changed_raw, changed_records

            identity = '12345678-1234-4234-8234-123456789abc'
            left_raw, left_records = unknown_uuid(ts_raw, records, identity)
            for right_identity, should_equal in ((identity, True),
                                                 ('87654321-4321-4321-8321-cba987654321', False)):
                with self.subTest(unknown_uuid=right_identity):
                    right_raw, right_records = unknown_uuid(raw, independent_records, right_identity)
                    result = v2.compare_pair(left_raw, right_raw, left_records, right_records, files, files,
                                             ts_root=roots[0], native_root=roots[1])
                    self.assertEqual(result['equal'], should_equal)
                    self.assertEqual(result['differentFields'], [] if should_equal else ['wireWebsocketStreams'])
                    self.assertFalse(result['formalAcceptance'])

    def test_approved_random_fields_keep_original_proof_and_nonmask_headers(self):
        for role in ('mock-server', 'control-client'):
            for size in (10, 126, 65536):
                with self.subTest(role=role, size=size):
                    left_raw, left = ws_wire_fixture(role, payload_size=size)
                    right_raw, right = ws_wire_fixture(role, payload_size=size,
                                                       nonce=b'another nonce 16', mask_key=b'abcd')
                    originals = copy.deepcopy((left_raw, left, right_raw, right))
                    proofs = [v2.ws_wire_observation(raw, rows)['connections'][0]
                              for raw, rows in ((left_raw, left), (right_raw, right))]
                    self.assertNotEqual(proofs[0]['upgrade'], proofs[1]['upgrade'])
                    direction = 'read' if role == 'mock-server' else 'write'
                    headers = [proof['directions'][direction]['frames'][0]['wireHeaderHex'] for proof in proofs]
                    self.assertNotEqual(headers[0], headers[1])
                    self.assertTrue(headers[0].endswith('12345678'))
                    self.assertTrue(headers[1].endswith('61626364'))
                    left_projection = v2.wire_projection(left_raw, left)
                    self.assertEqual(left_projection, v2.wire_projection(right_raw, right))
                    frame = next(frame for frame in left_projection['wireWebsocketStreams'][0]['frames']
                                 if frame['masked'])
                    self.assertEqual(frame['wireHeaderPrefixHex'], headers[0][:-8])
                    self.assertEqual(frame['maskKeyBytes'], 4)
                    self.assertEqual(frame['lengthCode'], size if size < 126 else (126 if size < 65536 else 127))
                    self.assertEqual(originals, (left_raw, left, right_raw, right))

    def test_mask_key_alone_cannot_change_nonempty_data_without_reencoding(self):
        for role in ('mock-server', 'control-client'):
            with self.subTest(role=role):
                raw, records = ws_wire_fixture(role)
                kind = 'fixture-tcp-data' if role == 'mock-server' else 'fixture-tcp-write'
                rows = [row for row in records if row['kind'] == kind and row['sequence'] > 1]
                rest = bytearray(v2.payload(rows[1]['payload']))
                rest[1] ^= 1  # Second chunk starts with length; byte 1 is the first mask byte.
                rows[1]['payload'] = blob(rest)
                with self.assertRaisesRegex(ValueError, 'WS_WIRE_API_PAYLOAD'):
                    v2.wire_projection(raw, records)

    def test_approved_nonce_still_requires_matching_accept_and_unique_headers(self):
        for change, rule in [('key-only', 'WS_WIRE_UPGRADE_ACCEPT'),
                             ('duplicate-key', 'WS_WIRE_UPGRADE_KEY'),
                             ('duplicate-accept', 'WS_WIRE_UPGRADE_ACCEPT')]:
            with self.subTest(change=change):
                raw, records = ws_wire_fixture('control-client')
                row = records[1 if change == 'duplicate-accept' else 0]
                value = v2.payload(row['payload'])
                if change == 'key-only':
                    value = value.replace(b'dGhlIHNhbXBsZSBub25jZQ==', base64.b64encode(b'another nonce 16'))
                else:
                    header = (b'Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=' if change == 'duplicate-accept'
                              else b'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==')
                    value = value[:-2] + header + b'\r\n\r\n'
                row['payload'] = blob(value)
                with self.assertRaisesRegex(ValueError, rule):
                    v2.wire_projection(raw, records)

        for key in (base64.b64encode(b'x' * 15), base64.b64encode(b'x' * 17), b'', b'!not-base64!'):
            with self.subTest(invalid_key=key):
                raw, records = ws_wire_fixture('control-client')
                accept = base64.b64encode(hashlib.sha1(key + b'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest())
                records[0]['payload'] = blob(v2.payload(records[0]['payload']).replace(
                    b'dGhlIHNhbXBsZSBub25jZQ==', key))
                records[1]['payload'] = blob(v2.payload(records[1]['payload']).replace(
                    b's3pPLMBiTxaQ9kYGzzhZRbK+xOo=', accept))
                with self.assertRaisesRegex(ValueError, 'WS_WIRE_UPGRADE_KEY'):
                    v2.wire_projection(raw, records)

    def test_business_headers_payload_and_unbound_uuid_remain_different(self):
        raw, records = ws_wire_fixture('control-client')
        expected = v2.wire_projection(raw, records)
        for value in (b'{"send":2}', b'{"send":"12345678-1234-4234-8234-123456789abc"}'):
            changed = copy.deepcopy(records)
            changed[2]['payload'] = blob(value)
            frame = ws_wire_frame(value, masked=True)
            changed[3]['payload'], changed[4]['payload'] = blob(frame[:1]), blob(frame[1:])
            self.assertNotEqual(expected, v2.wire_projection(raw, changed))
        changed = copy.deepcopy(records)
        changed[0]['payload'] = blob(v2.payload(changed[0]['payload'])[:-2] + b'X-Business: kept\r\n\r\n')
        self.assertNotEqual(expected, v2.wire_projection(raw, changed))
        responses = []
        for value in (b'one', b'two'):
            changed = copy.deepcopy(records)
            changed[1]['payload'] = blob(v2.payload(changed[1]['payload'])[:-2] + b'X-Server: ' + value + b'\r\n\r\n')
            responses.append(v2.wire_projection(raw, changed))
        self.assertNotEqual(responses[0], responses[1])
        compare = v2._load('compare', _apparatus / 'compare.py')
        uuid_projections = []
        for identity in ('12345678-1234-4234-8234-123456789abc', '87654321-4321-4321-8321-cba987654321'):
            changed = copy.deepcopy(records)
            value = json.dumps({'send': identity}).encode()
            changed[2]['payload'] = blob(value)
            frame = ws_wire_frame(value, masked=True)
            changed[3]['payload'], changed[4]['payload'] = blob(frame[:1]), blob(frame[1:])
            uuid_projections.append(compare.Normalizer().value(v2.wire_projection(raw, changed)))
        self.assertNotEqual(uuid_projections[0], uuid_projections[1])

    def test_control_frame_count_opcode_close_code_and_payload_are_not_exempt(self):
        raw, records = ws_wire_fixture('control-client')
        def projected(opcode, value):
            changed = copy.deepcopy(records)
            changed.append({'kind': 'fixture-tcp-data', 'socket': copy.deepcopy(changed[0]['socket']),
                            'payload': blob(ws_wire_frame(value, opcode=opcode)), 'sequence': len(changed)})
            return v2.wire_projection(raw, changed)
        expected = v2.wire_projection(raw, records)
        for opcode, first, second in ((9, b'ping', b'pong'), (10, b'ping', b'pong'),
                                      (8, b'\x03\xe8done', b'\x03\xe8donX'),
                                      (8, b'\x03\xe8done', b'\x03\xe9done')):
            with self.subTest(opcode=opcode, first=first, second=second):
                left, right = projected(opcode, first), projected(opcode, second)
                self.assertNotEqual(expected, left)
                self.assertNotEqual(left, right)
        self.assertNotEqual(projected(9, b'ping'), projected(10, b'ping'))

    def test_same_connection_order_is_not_exempt(self):
        raw, records = ws_wire_fixture('control-client')
        second = copy.deepcopy(records[2:5])
        value = b'{"send":2}'
        frame = ws_wire_frame(value, masked=True, mask_key=b'abcd')
        second[0]['payload'], second[1]['payload'], second[2]['payload'] = blob(value), blob(frame[:1]), blob(frame[1:])
        records[5:5] = second
        for sequence, row in enumerate(records):
            row['sequence'] = sequence
        changed = copy.deepcopy(records)
        changed[2:8] = changed[5:8] + changed[2:5]
        for sequence, row in enumerate(changed):
            row['sequence'] = sequence
        self.assertNotEqual(v2.wire_projection(raw, records), v2.wire_projection(raw, changed))

    def test_fin_fragment_boundaries_and_binary_opcode_remain_visible(self):
        raw, records = ws_wire_fixture('control-client')
        expected = v2.wire_projection(raw, records)
        for options in ({'fragmented': True}, {'binary': True}):
            changed_raw, changed = ws_wire_fixture('control-client', **options)
            self.assertNotEqual(expected, v2.wire_projection(changed_raw, changed))
        records[3]['payload'] = blob(b'\x01')  # An unfinished first data frame is not a valid remask.
        with self.assertRaisesRegex(ValueError, 'WS_WIRE_FRAGMENT_INCOMPLETE'):
            v2.wire_projection(raw, records)


if __name__ == '__main__':
    unittest.main()
