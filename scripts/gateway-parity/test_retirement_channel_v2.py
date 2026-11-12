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
        self.assertEqual(result['models'][0][0]['temperature'], 0.2)

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
        self.assertEqual([row['request']['msg_type'] for row in calls], ['text', 'text', 'post', 'text', 'text', 'post'])

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
        raw, records, files = fixture()
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


if __name__ == '__main__':
    unittest.main()
