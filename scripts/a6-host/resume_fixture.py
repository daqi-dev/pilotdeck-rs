#!/usr/bin/env python3
"""Resumable transport fixture; persists only its test stream across pipe peers.

This is NOT a production TS sidecar: its process-external state stands in for a
long-lived transport owner. Cursor/cleanup rules follow streamReplayStore.ts:
98-142. The resume cursor is the host's last ACCEPTED sequence (TS client:859,
906); replay restarts exactly there, and any duplicate/gap must fail closed
(TS client:889-892).
"""
import json
import os
from pathlib import Path
import sys
import time

mode = sys.argv[1]
receipt = Path(sys.argv[2])
state_path = receipt.with_suffix('.state')
state = json.loads(state_path.read_text()) if state_path.exists() else {'spawns': 0, 'acks': []}
state['spawns'] += 1
generation = state['spawns']


def save():
    # Atomic: the Rust test may read .state while this process is still running.
    temporary = state_path.with_suffix('.state.tmp')
    temporary.write_text(json.dumps(state))
    os.replace(temporary, state_path)


def read():
    line = sys.stdin.buffer.raw.readline()
    if not line:
        raise RuntimeError('unexpected host EOF')
    return json.loads(line)


def send(frame):
    print(json.dumps(frame, separators=(',', ':')), flush=True)


def reply(request, **fields):
    return dict(kind='response', messageId='reply-' + request['messageId'],
                inReplyTo=request['messageId'], ok=True, **fields)


def finish():
    assert sys.stdin.read() == '', 'unexpected trailing frames (old callback delivery?)'
    receipt.write_text('PASS ' + mode + '\n')
    save()
    sys.exit(0)


def wait_marker(name):
    until = time.monotonic() + 8
    while not receipt.with_suffix('.' + name).exists():
        assert time.monotonic() < until, 'handler barrier not reached: ' + name
        time.sleep(0.002)


save()
assert generation <= 2, 'more than one reconnect attempt'
if mode == 'reconnect-command':
    assert os.environ.get('PATH') == '/usr/bin:/bin'
    assert os.environ.get('HOST_REPLAY_ENV') == 'retained'
    assert 'HOST_REPLAY_REMOVE' not in os.environ
    assert 'HOME' not in os.environ  # inherited by the test process, cleared by Command
    # Apple's /usr/bin/python3 launcher adds SDK discovery variables itself.
    assert set(os.environ) <= {'PATH', 'HOST_REPLAY_ENV', 'LC_CTYPE',
                              '__CF_USER_TEXT_ENCODING', 'SDKROOT', 'CPATH',
                              'LIBRARY_PATH', 'MANPATH'}
    assert Path.cwd().samefile(receipt.parent)  # macOS /var and /private/var alias
hello = read()
assert hello['method'] == 'hello'
if generation == 2 and mode == 'reconnect-handshake-timeout':
    time.sleep(60)
if generation == 2 and mode == 'reconnect-handshake-eof':
    sys.exit(0)
binding = dict(protocolVersion='2.0', moduleId='fixture', moduleInstanceId='instance',
               connectionGeneration='generation-' + str(generation), capabilitiesVersion='v1')
if generation == 2 and mode == 'reconnect-instance-changed':
    binding['moduleInstanceId'] = 'new-instance'
if generation == 2 and mode == 'reconnect-module-changed':
    binding['moduleId'] = 'foreign'
send(reply(hello, **binding))
if generation == 2 and mode in ('reconnect-instance-changed', 'reconnect-module-changed'):
    finish()
caps = read()
assert caps['method'] == 'capabilities'
methods = [dict(name='execute', profiles=['streaming'], resumeSupport='streaming'),
           dict(name='resume'), dict(name='ack')]
if mode == 'reconnect-unsupported-missing':
    del methods[0]['resumeSupport']
if mode == 'reconnect-unsupported-unary':
    methods[0]['resumeSupport'] = 'unary'
if mode == 'reconnect-unsupported-disabled':
    methods[1]['enabled'] = False
if mode == 'reconnect-unsupported-ack':
    methods[2]['enabled'] = False
send(reply(caps, **binding, payload=dict(capabilitiesVersion='v1', methods=methods)))


def event(sequence, final=False):
    execute = state['execute']
    frame = dict(kind='event', eventType='agent_event', streamId='stream', sequence=sequence,
                 final=final, **{k: execute[k] for k in ('runId', 'operationId', 'requestId')})
    if final:
        frame.update(outcome='completed', payload=dict(messages=[], result=dict(type='success',
                     sessionId=execute['sessionId'], turnId=execute['turnId'], stopReason='stop', turns=1)))
    else:
        frame['payload'] = dict(type='text_delta', sessionId=execute['sessionId'],
                                turnId=execute['turnId'], text=str(sequence))
    return frame


def call():
    execute = state['execute']
    return dict(kind='request', messageId='call-1', method='module_call', module='model',
                runId=execute['runId'], operationId=execute['operationId'], requestId='callback-request',
                payload=dict(operation='stream'))


def ack(expected):
    frame = read()
    assert frame['method'] == 'ack' and frame['streamId'] == 'stream', frame
    assert frame['lastAppliedSequence'] == expected, frame
    state['acks'].append(expected)
    # Cumulative deletion, never just drop the single explicitly ACKed event.
    state['retained'] = [n for n in state['retained'] if n > expected]
    state['truncatedThrough'] = expected
    save()


if generation == 1:
    execute = read()
    assert execute['method'] == 'execute'
    state['execute'] = execute
    state['retained'] = list(range(68 if mode == 'reconnect-ack-coalesced' else 5))
    state['truncatedThrough'] = -1
    save()
    if mode == 'reconnect-before-accepted':
        sys.exit(0)
    send(reply(execute, requestId=execute['requestId'], streamId='stream'))
    if mode.startswith('reconnect-unsupported-') or mode == 'reconnect-no-provider':
        sys.exit(0)
    if mode == 'reconnect-accepted':
        sys.exit(0)
    send(event(0))
    ack(0)
    if mode in ('reconnect-events', 'reconnect-event-failure'):
        send(event(1))
        send(event(2))
        wait_marker('event-started')
    elif mode in ('reconnect-budget', 'reconnect-budget-control', 'reconnect-module-pending', 'reconnect-module-cached',
                  'reconnect-module-conflict', 'reconnect-module-write-failure'):
        send(call())
        if mode == 'reconnect-module-pending':
            wait_marker('call-started')
        elif mode == 'reconnect-module-write-failure':
            # A small write can finish even after our stdin closes under spawn
            # pressure (#230). Cut a response larger than the pipe only after
            # its first byte arrives, so a required write is still in flight.
            assert os.read(sys.stdin.fileno(), 1) == b'{'
            os.close(sys.stdin.fileno())
            # Leave stdout open. Only the required response write EPIPE triggers recovery.
            time.sleep(60)
        else:
            response = read()
            assert response['inReplyTo'] == 'call-1' and response['ok']
            state['response'] = response
            save()
    elif mode in ('reconnect-terminal', 'reconnect-backward'):
        send(event(1))
        ack(1)
    elif mode == 'reconnect-ack-coalesced':
        send(call())
        # The >1 MiB required response is now in flight and cannot fit in a
        # pipe. Stop reading its body while 64 events are applied and ACKed.
        first = os.read(sys.stdin.fileno(), 1)
        assert first == b'{'
        for n in range(1, 65):
            send(event(n))
        wait_marker('applied-64')
        data = bytearray(first)
        while not data.endswith(b'\n'):
            # Do not consume the next NDJSON frame if it shares a pipe read.
            # Large response text has no newlines; its known size is checked below.
            part = sys.stdin.buffer.raw.readline(4096)
            assert part
            data.extend(part)
            time.sleep(0.001)
        response = json.loads(data)
        assert len(response['payload']['padding']) == 2 * 1024 * 1024
        ack(64)  # Exactly the latest cursor: intermediate 1..63 were coalesced.
        assert state['retained'] == [65, 66, 67]
        # One more applied event loses its ACK, so resume must use the local
        # cursor, which is ahead of the server's cumulative cleanup boundary.
        os.close(sys.stdin.fileno())
        send(event(65))
        wait_marker('applied-65')
    sys.exit(0)

resume = read()
assert resume['method'] == 'resume', 'must not repeat execute'
assert set(resume) == {'kind', 'messageId', 'method', 'streamId', 'previousBinding', 'lastAppliedSequence'}
assert resume['streamId'] == 'stream'
assert resume['previousBinding'] == dict(moduleInstanceId='instance', connectionGeneration='generation-1')
cursor = resume['lastAppliedSequence']
state['resumeCursor'] = cursor
assert state['truncatedThrough'] <= cursor <= state['retained'][-1]
state['replayed'] = [n for n in state['retained'] if n > cursor]
save()
if mode == 'reconnect-rejected':
    send(dict(reply(resume), ok=False, code='CURSOR_EXPIRED'))
    finish()
if mode == 'reconnect-wrong-reply':
    send(dict(reply(resume), inReplyTo='foreign'))
    finish()
if mode == 'reconnect-wrong-stream':
    send(reply(resume, streamId='foreign'))
    finish()
if mode == 'reconnect-event-before-resume':
    send(event(1))
    finish()
if mode == 'reconnect-call-before-resume':
    send(call())
    finish()
if mode == 'reconnect-malformed':
    print('{invalid', flush=True)
    finish()
send(reply(resume, streamId='stream'))
if mode in ('reconnect-budget', 'reconnect-budget-control'):
    large = event(1)
    large['payload']['text'] = 'x' * (7500 if mode == 'reconnect-budget' else 3000)
    send(large)
    if mode == 'reconnect-budget':
        finish()
    ack(1)
    send(event(2, final=True))
    finish()
if mode == 'reconnect-twice':
    send(event(1))
    ack(1)
    sys.exit(0)
if mode in ('reconnect-gap', 'reconnect-duplicate'):
    assert cursor == 0
    # First replayed frame must be exactly cursor + 1: next + 1 or next - 1 fails.
    send(event(2 if mode == 'reconnect-gap' else 0))
    finish()
if mode == 'reconnect-backward':
    assert cursor == 1
    send(event(2))
    send(event(1))
    sys.stdin.read()  # ACK 2 may or may not be written before the host fails.
    sys.exit(0)
if mode.startswith('reconnect-module-'):
    if mode == 'reconnect-module-pending':
        send(event(1))  # Releases the original pending callback, never starts another.
    frame = call()
    if mode == 'reconnect-module-conflict':
        frame['payload']['operation'] = 'changed'
        send(frame)
        finish()
    send(frame)
    if mode == 'reconnect-module-pending':
        send(frame)
    responses = []
    need_ack = mode == 'reconnect-module-pending'
    while len(responses) < (2 if need_ack else 1) or (need_ack and 1 not in state['acks']):
        frame = read()
        if frame.get('method') == 'ack':
            assert frame['lastAppliedSequence'] == 1
            state['acks'].append(1)
        else:
            assert frame['inReplyTo'] == 'call-1' and frame['ok']
            if mode == 'reconnect-module-write-failure':
                assert frame['payload']['padding'] == 'x' * (2 * 1024 * 1024)
            responses.append(frame)
    if 'response' in state:
        assert responses == [state['response']], 'cached response was changed'
    send(event(2 if need_ack else 1, final=True))
    finish()
if mode == 'reconnect-ack-coalesced':
    assert cursor == 65 and state['acks'] == [0, 64]
    assert state['replayed'] == [66, 67]
    send(event(66))
    ack(66)
    send(event(67, final=True))
    finish()
if mode in ('reconnect-events', 'reconnect-event-failure'):
    # 1 is applying and 2 is queued: both were accepted, so the host keeps and
    # applies them itself; replay starts at 3 with no overlap (TS client:906).
    assert cursor == 2, 'resume cursor must be the last accepted sequence'
    send(event(3))
    if mode == 'reconnect-event-failure':
        # An application error must remain an error even after resume succeeds.
        sys.stdin.read()
        sys.exit(0)
    last_ack = 0
    while last_ack < 3:
        frame = read()
        assert frame['method'] == 'ack'
        assert frame['lastAppliedSequence'] > last_ack
        last_ack = frame['lastAppliedSequence']
    send(event(4, final=True))
    finish()
if mode == 'reconnect-terminal':
    assert cursor == 1
    send(event(2, final=True))
    finish()
if mode == 'reconnect-accepted':
    assert cursor == -1
    send(event(0))
    ack(0)
    send(event(1, final=True))
    finish()
if mode in ('reconnect-observer-panic', 'reconnect-command'):
    assert cursor == 0
    send(event(1))
    ack(1)
    send(event(2, final=True))
    finish()
raise RuntimeError('unknown resume path: ' + mode)
