#!/usr/bin/env python3
"""Bounded adversarial peer, not a TS oracle; all gates have a 30 s parent alarm."""
import json
import os
import signal
import sys
import time
from pathlib import Path

signal.alarm(30)
mode = sys.argv[1]
receipt = Path(sys.argv[2])
state = receipt.with_suffix('.generation')
generation = int(state.read_text()) + 1 if state.exists() else 1
state.write_text(str(generation))
receipt.with_suffix('.pid-' + str(generation)).write_text(str(os.getpid()))
if os.environ.get('PDRS_HOST_CANCEL_PID_LOG'):
    with open(os.environ['PDRS_HOST_CANCEL_PID_LOG'], 'a') as log:
        log.write(str(os.getpid()) + '\n')
assert generation <= 2


def mark(name):
    receipt.with_suffix('.' + name).write_text(name + '\n')


def wait(name):
    while not receipt.with_suffix('.' + name).exists():
        time.sleep(0.002)


def read():
    line = sys.stdin.buffer.raw.readline()
    assert line, 'unexpected EOF before protocol phase completed'
    return json.loads(line)


def send(value):
    print(json.dumps(value, separators=(',', ':')), flush=True)


def reply(request, **fields):
    send(dict(kind='response', messageId='reply-' + request['messageId'],
              inReplyTo=request['messageId'], ok=True, **fields))


def close():
    frames = [json.loads(line) for line in sys.stdin.read().splitlines() if line.strip()]
    assert all(f.get('method') in ('ack', 'cancel') for f in frames), frames
    mark('closing')
    if mode == 'terminal-ack-stall':
        # The second non-final application was collected before the deadline
        # exit, so the host acknowledged it. A deadline-first select would abort
        # the application without collecting it and never send this ACK.
        acked = [f.get('lastAppliedSequence') for f in frames
                 if f.get('method') == 'ack']
        assert 1 in acked, ('missing ack for the collected application', frames)
    wait('close-release')
    receipt.write_text('PASS ' + mode + '\n')
    sys.exit(0)


hello = read()
assert hello['method'] == 'hello'
if generation == 2:
    mark('replacement-ready')
    if mode == 'pending-hello':
        close()
    wait('handshake-release')
binding = dict(protocolVersion='2.0', moduleId='fixture', moduleInstanceId='instance',
               connectionGeneration='generation-' + str(generation), capabilitiesVersion='v1')
reply(hello, **binding)
caps = read()
assert caps['method'] == 'capabilities'
reply(caps, **binding, payload=dict(capabilitiesVersion='v1', methods=[
    dict(name='execute', profiles=['streaming'], resumeSupport='streaming'),
    dict(name='resume'), dict(name='ack'), dict(name='cancel')]))
request = read()
if generation == 2:
    assert request['method'] == 'resume'
    assert request['lastAppliedSequence'] == 0
    mark('resume-ready')
    if mode == 'pending-resume':
        close()
    wait('resume-release')
    reply(request, streamId='stream')
    mark('resumed')
    close()

assert request['method'] == 'execute'
reply(request, requestId=request['requestId'], streamId='stream')


def event(sequence, final=False):
    frame = dict(kind='event', eventType='agent_event', streamId='stream',
                 sequence=sequence, final=final,
                 **{k: request[k] for k in ('runId', 'operationId', 'requestId')})
    if final:
        frame.update(outcome='completed', payload=dict(messages=[], result=dict(
            type='success', sessionId=request['sessionId'], turnId=request['turnId'],
            stopReason='stop', turns=1)))
    else:
        frame['payload'] = dict(type='text_delta', sessionId=request['sessionId'],
                                turnId=request['turnId'], text='hello')
    send(frame)


event(0)
if mode.startswith('terminal-'):
    if mode == 'terminal-module-stall':
        send(dict(kind='request', messageId='call-1', method='module_call', module='model',
                  runId=request['runId'], operationId=request['operationId'],
                  requestId='callback-request', payload=dict(operation='stream')))
    elif mode == 'terminal-ack-stall':
        # A second non-final event. The test blocks its callback and releases it
        # once the cancel deadline has already passed, so its completed result
        # is waiting in the host's callback set, uncollected, at deadline time.
        event(1)
    mark('ready')
    wait('terminal-release')
    event(2 if mode == 'terminal-ack-stall' else 1, True)
    close()
else:
    assert mode in ('pending-hello', 'pending-resume', 'silent-resume')
    assert read()['method'] == 'ack'
    mark('ready')
    wait('disconnect-release')
    # Deliberately expose EOF before OS exit. The host must not conflate them:
    # its old-connection close polls OS exit on Tokio's (paused) clock.
    sys.stdout.flush()
    os.close(sys.stdout.fileno())
    sys.stdout = None
    wait('original-exit-release')
    # The first transport exits normally; only replacement cleanup emits closing.
