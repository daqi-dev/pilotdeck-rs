#!/usr/bin/env python3
"""Adversarial stdio peer for host unit tests; NOT the real TS cross-test."""
import json
import sys
import time
from pathlib import Path

mode = sys.argv[1]
receipt = Path(sys.argv[2])


def read():
    line = sys.stdin.readline()
    if not line:
        raise RuntimeError('unexpected host EOF')
    return json.loads(line)


def send(value):
    print(json.dumps(value, separators=(',', ':')), flush=True)


def response(request, **fields):
    return dict(kind='response', messageId='response-' + request['messageId'],
                inReplyTo=request['messageId'], ok=True, **fields)


hello = read()
assert hello['method'] == 'hello'
if mode == 'handshake-hang':
    time.sleep(60)
if mode == 'malformed':
    print('{invalid', flush=True)
    time.sleep(60)
if mode == 'oversized':
    print('x' * 8192, flush=True)
    time.sleep(60)
binding = dict(protocolVersion='2.0', moduleId='fixture', moduleInstanceId='instance',
               connectionGeneration='current', capabilitiesVersion='v1')
send(response(hello, **binding))
capabilities = read()
assert capabilities['method'] == 'capabilities'
if mode == 'stale-generation':
    binding['connectionGeneration'] = 'old'
methods = [dict(name='execute', profiles=['streaming'], resumeSupport='streaming'),
           dict(name='resume'), dict(name='ack')]
if mode == 'no-ack':
    methods[1]['enabled'] = False
if mode == 'no-streaming':
    methods[0]['profiles'] = ['unary']
send(response(capabilities, **binding, payload=dict(capabilitiesVersion='v1', methods=methods)))
execute = read()
assert execute['method'] == 'execute'
ids = {key: execute[key] for key in ('runId', 'operationId', 'requestId')}


def event(sequence=0, final=False, **changes):
    payload = dict(type='text_delta', sessionId=execute['sessionId'], turnId=execute['turnId'], text='hello')
    value = dict(kind='event', eventType='agent_event', streamId='stream', sequence=sequence,
                 final=final, payload=payload, **ids)
    if final:
        value.update(outcome='completed', payload=dict(messages=[], result=dict(type='success',
                     sessionId=execute['sessionId'], turnId=execute['turnId'], stopReason='stop', turns=1)))
    value.update(changes)
    return value


if mode == 'rejected':
    send(dict(response(execute, requestId=execute['requestId']), ok=False, final=True, outcome='failed', code='DEADLINE_EXCEEDED'))
    receipt.write_text('PASS rejected\n')
    sys.exit(0)
if mode == 'before-accepted':
    send(event())
else:
    send(response(execute, requestId=execute['requestId'], streamId='stream'))
if mode == 'disconnect':
    sys.stderr.write('controlled disconnect\n')
    sys.exit(0)
if mode == 'identity':
    send(event(runId='foreign'))
elif mode == 'bad-session':
    send(event(payload=dict(type='text_delta', sessionId='foreign')))
elif mode == 'event-generation':
    send(event(connectionGeneration='old'))
elif mode == 'sequence':
    send(event())
    send(event())
elif mode == 'unknown':
    send(event(final=True, outcome='result_unknown'))
elif mode == 'bad-terminal':
    send(event(final=True, payload=dict(messages=[], result=dict(type='error', sessionId=execute['sessionId'], turnId=execute['turnId'], stopReason='error', turns=1))))
elif mode in ('duplicate-conflict', 'duplicate-same', 'callback-progress', 'callback-disconnect', 'callback-timeout', 'wrong-call', 'wrong-call-conflict'):
    call = dict(kind='request', messageId='call-1', method='module_call', module='model',
                runId=execute['runId'], operationId=execute['operationId'], requestId='callback-request',
                payload=dict(operation='stream'))
    if mode in ('wrong-call', 'wrong-call-conflict'):
        call['runId'] = 'foreign'
    send(call)
    if mode == 'duplicate-conflict':
        send(dict(call, payload=dict(operation='different')))
    elif mode == 'callback-disconnect':
        sys.exit(0)
    elif mode == 'callback-timeout':
        time.sleep(60)
    else:
        if mode == 'duplicate-same':
            send(call)
        if mode == 'callback-progress':
            # Host callback waits for this event application. Serial callback I/O deadlocks.
            send(event())
        replies = 2 if mode == 'duplicate-same' else 1
        acked = mode != 'callback-progress'
        while replies or not acked:
            reply = read()
            if reply.get('method') == 'ack':
                assert reply['lastAppliedSequence'] == 0
                acked = True
            else:
                assert reply['inReplyTo'] == 'call-1' and reply['requestId'] == 'callback-request'
                assert reply['ok'] == (mode not in ('wrong-call', 'wrong-call-conflict'))
                replies -= 1
        if mode == 'wrong-call-conflict':
            send(dict(call, runId=execute['runId']))
        else:
            send(event(sequence=1 if mode == 'callback-progress' else 0, final=True))
elif mode in ('positive', 'no-ack', 'cleanup-hang', 'stderr-flood', 'slow-event'):
    if mode == 'stderr-flood':
        sys.stderr.write('x' * (512 * 1024))
        sys.stderr.flush()
    send(event())
    if mode != 'no-ack':
        ack = read()
        assert ack['method'] == 'ack' and ack['streamId'] == 'stream' and ack['lastAppliedSequence'] == 0
    send(event(sequence=1, final=True))
# Test parent requires this receipt; a terminal followed by exit=1 is not PASS.
for line in sys.stdin:
    raise RuntimeError('unexpected trailing host frame: ' + line)
receipt.write_text('PASS ' + mode + '\n')
if mode == 'cleanup-hang':
    time.sleep(60)
