#!/usr/bin/env python3
"""Adversarial stdio peer for host unit tests; NOT the real TS cross-test."""
import json
import os
import sys
import time
import subprocess
import select
from pathlib import Path

mode = sys.argv[1]
if mode.startswith('reconnect-'):
    import runpy
    runpy.run_path(str(Path(__file__).with_name('resume_fixture.py')), run_name='__main__')
    sys.exit(0)
known_modes = {
    'positive', 'slow-event', 'no-ack', 'stderr-flood', 'rejected', 'cleanup-hang',
    'ack-failure', 'unknown-response', 'turn-completed', 'terminal-then-exit',
    'cancelled', 'failed-error', 'failed-max-turns', 'cancelled-success',
    'identity', 'duplicate-conflict', 'stale-generation', 'disconnect',
    'sequence', 'before-accepted', 'bad-session', 'wrong-call-conflict',
    'wrong-call-replay', 'event-generation', 'bad-terminal', 'unknown',
    'no-streaming', 'malformed', 'duplicate-same', 'callback-progress',
    'wrong-call', 'callback-disconnect', 'callback-timeout', 'handshake-hang',
    'oversized', 'close-hang', 'execute-request-mismatch',
    'hello-rejected', 'capabilities-rejected', 'module-routing', 'module-errors',
}
known_modes.update({
    'ack-disabled', 'resume-support-missing', 'resume-support-unary',
    'duplicate-acceptance', 'duplicate-acceptance-new-stream', 'empty-stream',
    'rejected-nonfinal', 'rejected-final-cancelled', 'rejected-error-message', 'rejected-unknown',
    'accepted-final', 'stream-mismatch', 'missing-messages', 'stderr-on-close',
    'terminal-pending-callback', 'module-call-limit', 'active-call-limit',
    'wrong-call-limit', 'duplicate-call-limit', 'event-queue-limit',
    'cache-byte-limit', 'response-byte-limit', 'event-byte-limit',
    'grandchild-close', 'grandchild-timeout', 'grandchild-drop', 'grandchild-exit',
    'cancel-pre-execute', 'cancel-before-accepted', 'cancel-after-accepted',
    'cancel-success', 'cancel-conflict', 'cancel-nack', 'cancel-disconnect',
    'cancel-timeout', 'cancel-unsupported', 'cancel-disabled',
    'cancel-execute-capability', 'cancel-empty-reason', 'cancel-compaction',
    'cancel-compaction-no-local', 'cancel-missing-result', 'cancel-compaction-completed',
    'cancel-compaction-conflict', 'cancel-compaction-no-messages', 'cancel-compaction-malformed',
    'frame-boundary', 'cancel-write-failure', 'cancel-duplicate-acceptance', 'event-then-disconnect',
    'cancel-before-timeout', 'cancel-before-disconnect',
    'batched-split-frames', 'accepted-then-rejected', 'close-term-cleanup',
    'ack-eof-recorder',
    'ack-eof-recorder-positive',
    'event-order', 'host-cancel-before-acceptance-hang',
    'host-cancel-after-acceptance-hang', 'host-cancel-no-terminal-hang',
    'host-cancel-late-terminal',
})
if mode not in known_modes:
    sys.exit('unknown fixture mode: ' + mode)
receipt = Path(sys.argv[2]) if len(sys.argv) > 2 else None


def write_receipt():
    if receipt is not None:
        receipt.write_text('PASS ' + mode + '\n')


def read():
    # Do not prefetch the next line: ordering probes must see all unread bytes
    # through select, including a cancel batched with the module response.
    line = sys.stdin.buffer.raw.readline()
    if not line:
        raise RuntimeError('unexpected host EOF')
    return json.loads(line)


def send(value):
    print(json.dumps(value, separators=(',', ':')), flush=True)


def response(request, **fields):
    return dict(kind='response', messageId='response-' + request['messageId'],
                inReplyTo=request['messageId'], ok=True, **fields)


hello = read()
if mode == 'batched-split-frames':
    # Two complete frames and half a third in one syscall; complete the third
    # only once the host has consumed the first two and asked for the rest.
    os.write(sys.stdout.fileno(), b'{"frame":1}\n{"frame":2}\n{"frame":')
    assert read() == {'continue': True}
    os.write(sys.stdout.fileno(), b'3}\n')
    assert sys.stdin.read() == ''
    write_receipt()
    sys.exit(0)
if mode == 'frame-boundary':
    send({'boundary': 'x'})
    sys.stdin.read()
    sys.exit(0)
assert hello['method'] == 'hello'
if mode == 'handshake-hang':
    time.sleep(60)
if mode == 'stderr-on-close':
    print('{invalid', flush=True)
    sys.stdin.read()
    sys.stderr.write('discarded-prefix' + 'x' * 8192 + 'EXIT-STDERR-TAIL\n')
    sys.stderr.flush()
    sys.exit(0)
if mode == 'malformed':
    print('{invalid', flush=True)
    time.sleep(60)
if mode == 'oversized':
    print('x' * 8192, flush=True)
    time.sleep(60)
binding = dict(protocolVersion='2.0', moduleId='fixture', moduleInstanceId='instance',
               connectionGeneration='current', capabilitiesVersion='v1')
if mode == 'hello-rejected':
    send(dict(response(hello, **binding), ok=False))
    sys.exit(0)
send(response(hello, **binding))
capabilities = read()
assert capabilities['method'] == 'capabilities'
if mode == 'capabilities-rejected':
    send(dict(response(capabilities, **binding), ok=False))
    sys.exit(0)
if mode == 'stale-generation':
    binding['connectionGeneration'] = 'old'
methods = [dict(name='execute', profiles=['streaming'], resumeSupport='streaming'),
           dict(name='resume'), dict(name='ack')]
if mode == 'no-ack':
    methods[1]['enabled'] = False
if mode == 'ack-disabled':
    methods[2]['enabled'] = False
if mode == 'resume-support-missing':
    del methods[0]['resumeSupport']
if mode == 'resume-support-unary':
    methods[0]['resumeSupport'] = 'unary'
if mode.startswith('cancel-'):
    if mode != 'cancel-unsupported':
        methods.append(dict(name='cancel', enabled=mode not in ('cancel-disabled', 'cancel-execute-capability')))
    if mode == 'cancel-execute-capability':
        methods[0]['cancel'] = True
if mode in ('host-cancel-after-acceptance-hang', 'host-cancel-late-terminal'):
    # Advertise the cancel wire path; the host must still bound its exit when
    # the peer accepts cancel but never emits (or delays) a terminal.
    methods.append(dict(name='cancel', enabled=True))
if mode.startswith('grandchild-'):
    # Inherits the sidecar process group and stderr/stdout descriptors.
    descendant = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
    assert receipt is not None
    receipt.with_suffix('.pid').write_text(str(descendant.pid))
if mode == 'no-streaming':
    methods[0]['profiles'] = ['unary']
if mode == 'close-hang':
    import signal
    assert receipt is not None
    signal.signal(signal.SIGTERM, lambda *_: receipt.with_suffix('.term').write_text('TERM\n'))
if mode == 'close-term-cleanup':
    import signal

    def cleanup(_signum, _frame):
        assert receipt.with_suffix('.eof').exists(), 'SIGTERM arrived before stdin EOF'
        write_receipt()
        sys.exit(0)

    signal.signal(signal.SIGTERM, cleanup)
send(response(capabilities, **binding, payload=dict(capabilitiesVersion='v1', methods=methods)))
if mode in ('close-term-cleanup', 'close-hang'):
    assert sys.stdin.read() == ''
    receipt.with_suffix('.eof').write_text('EOF\n')
    while True:
        signal.pause()
if mode == 'cancel-pre-execute':
    assert sys.stdin.read() == ''
    write_receipt()
    sys.exit(0)
if mode == 'grandchild-exit':
    sys.exit(0)
if mode == 'grandchild-close':
    # Dedicated kill-path test: ignore stdin EOF and never produce a receipt.
    time.sleep(60)
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


def module_call(number=1, padding=0):
    return dict(kind='request', messageId='call-' + str(number), method='module_call', module='model',
                runId=execute['runId'], operationId=execute['operationId'], requestId='callback-request',
                payload=dict(operation='stream', padding='x' * padding))


if mode in ('host-cancel-before-acceptance-hang', 'host-cancel-after-acceptance-hang',
            'host-cancel-no-terminal-hang'):
    if os.environ.get('PDRS_HOST_CANCEL_PID_LOG'):
        with open(os.environ['PDRS_HOST_CANCEL_PID_LOG'], 'a') as log:
            log.write(str(os.getpid()) + '\n')
    # Post-cancel exit-bound probe: hold the connection open and never send a
    # terminal. The host must close us after its bounded deadline.
    assert receipt is not None
    if mode != 'host-cancel-before-acceptance-hang':
        send(response(execute, requestId=execute['requestId'], streamId='stream'))
        send(event(sequence=0))
    receipt.with_suffix('.ready').write_text(mode + ' ready\n')
    frames = [json.loads(line) for line in sys.stdin.read().splitlines() if line.strip()]
    received = [frame.get('method') for frame in frames]
    if mode == 'host-cancel-before-acceptance-hang':
        assert 'cancel' not in received, received
    elif mode == 'host-cancel-no-terminal-hang':
        assert 'cancel' not in received, received
    else:
        assert 'cancel' in received, received
    receipt.with_suffix('.closing').write_text('stdin EOF; cleanup started\n')
    until = time.monotonic() + 30
    while not receipt.with_suffix('.close-release').exists():
        assert time.monotonic() < until, 'close release not signaled'
        time.sleep(0.002)
    write_receipt()
    sys.exit(0)


if mode == 'host-cancel-late-terminal':
    # Post-cancel deadline oracle: accept, emit one applicable event, then hold
    # the authoritative terminal until the host test releases it at +3 s of
    # virtual time. A 5 s bound must let that late-but-valid terminal win; a
    # shorter bound (the former 1 s close_timeout) would drop it as Timeout.
    # The bounded wait keeps an aborted test from leaving an orphan behind.
    assert receipt is not None
    send(response(execute, requestId=execute['requestId'], streamId='stream'))
    send(event(sequence=0))
    receipt.with_suffix('.ready').write_text(mode + ' ready\n')
    while True:
        frame = read()
        if frame.get('method') == 'cancel':
            break
        assert frame.get('method') == 'ack', frame
    release = receipt.with_suffix('.release')
    deadline = time.monotonic() + 30
    while not release.exists():
        assert time.monotonic() < deadline, 'late-terminal release not signaled'
        time.sleep(0.01)
    send(event(sequence=1, final=True))
    assert sys.stdin.read() == ''
    write_receipt()
    sys.exit(0)


if mode in ('rejected-nonfinal', 'rejected-final-cancelled', 'rejected-error-message', 'rejected-unknown'):
    failure = dict(response(execute, requestId=execute['requestId']), ok=False)
    if mode == 'rejected-nonfinal':
        failure.update(code='CONTROLLED_REJECTION')
    elif mode == 'rejected-final-cancelled':
        failure.update(final=True, outcome='cancelled')
    elif mode == 'rejected-error-message':
        failure.update(error=dict(message='controlled failure message'))
    send(failure)
    sys.stdin.read()
    sys.exit(0)
if mode == 'accepted-final':
    send(response(execute, requestId=execute['requestId'], final=True, outcome='completed'))
    sys.stdin.read()
    sys.exit(0)
if mode.startswith('cancel-before-'):
    # Handler triggers abort here. No cancel is legal before this acceptance.
    send(module_call())
    reply = read()
    assert reply['inReplyTo'] == 'call-1' and reply['ok']
    if mode in ('cancel-before-accepted', 'cancel-before-disconnect'):
        readable, _, _ = select.select([sys.stdin], [], [], 0.05)
        assert not readable, 'host wrote bytes before execute acceptance'
    if mode == 'cancel-before-timeout':
        receipt.with_suffix('.ready').write_text('module response received\n')
        assert sys.stdin.read() == ''  # no acceptance, hence never send cancel
        write_receipt()
        sys.exit(0)
    if mode == 'cancel-before-disconnect':
        write_receipt()
        sys.exit(0)
if mode == 'rejected':
    send(dict(response(execute, requestId=execute['requestId']), ok=False, final=True, outcome='failed', code='DEADLINE_EXCEEDED'))
    write_receipt()
    sys.exit(0)
if mode == 'before-accepted':
    send(event())
else:
    request_id = 'foreign' if mode == 'execute-request-mismatch' else execute['requestId']
    send(response(execute, requestId=request_id, streamId='' if mode == 'empty-stream' else 'stream'))
if mode == 'cancel-write-failure':
    os.close(sys.stdin.fileno())
    send(event())
    send(event(sequence=1, final=True))
    write_receipt()
    sys.exit(0)
if mode == 'ack-eof-recorder-positive':
    # Positive control for the recorder below: the event is applied, so the
    # host ACKs it; record frames until that ACK arrives, then finish with a
    # final and keep recording until stdin EOF. A recorder that never records
    # (wrong key, stdin not read) would leave this record empty.
    assert receipt is not None
    send(event())
    acks = []
    sent_final = False
    for line in sys.stdin:
        if line.strip():
            frame = json.loads(line)
            acks.append({key: frame.get(key)
                         for key in ('method', 'lastAppliedSequence')})
            if not sent_final and frame.get('method') == 'ack':
                send(event(sequence=1, final=True))
                sent_final = True
    receipt.with_suffix('.acks').write_text(json.dumps(acks) + '\n')
    sys.exit(0)
if mode == 'ack-eof-recorder':
    # Wire ACK receipt for the lifecycle-rejection path: emit one applicable
    # event, then record every host frame until stdin EOF instead of blocking
    # on the expected ACK read. A host that ACKs a validation-rejected event
    # (or writes anything else before closing) is exposed in the .acks record;
    # the Rust test asserts that record is empty after the failed execute.
    assert receipt is not None
    send(event())
    acks = []
    for line in sys.stdin:
        if line.strip():
            frame = json.loads(line)
            acks.append({key: frame.get(key)
                         for key in ('method', 'lastAppliedSequence')})
    receipt.with_suffix('.acks').write_text(json.dumps(acks) + '\n')
    sys.exit(0)
if mode.startswith('cancel-'):
    local_abort = mode != 'cancel-compaction-no-local'
    supported = mode not in ('cancel-unsupported', 'cancel-disabled')
    before = mode == 'cancel-before-accepted'
    if not before:
        send(event())
    got_ack, got_cancel = before, not (local_abort and supported)
    while not got_ack or not got_cancel:
        frame = read()
        if frame.get('method') == 'ack':
            assert not got_ack and frame['lastAppliedSequence'] == 0
            got_ack = True
        else:
            assert not got_cancel and frame['method'] == 'cancel'
            assert set(frame) == {'kind', 'messageId', 'method', 'runId', 'operationId', 'requestId', 'reason'}
            assert all(frame[k] == ids[k] for k in ids)
            expected_reason = 'compaction_persistence_failed' if mode.startswith('cancel-compaction') else 'host_abort' if mode == 'cancel-empty-reason' else 'test_abort'
            assert frame['reason'] == expected_reason, frame
            got_cancel = True
            if mode == 'cancel-nack':
                send(dict(response(frame), ok=False, code='CANCEL_REJECTED'))
    if mode == 'cancel-disconnect':
        sys.stderr.write('CANCEL-DISCONNECT-TAIL\n')
        sys.exit(0)
    if mode == 'cancel-timeout':
        receipt.with_suffix('.ready').write_text('ACK and cancel received\n')
        time.sleep(60)
    if mode == 'cancel-duplicate-acceptance':
        send(response(execute, requestId=execute['requestId'], streamId='stream'))
    terminal = event(sequence=0 if before else 1, final=True)
    if mode not in ('cancel-success', 'cancel-unsupported', 'cancel-disabled', 'cancel-compaction-completed'):
        terminal['outcome'] = 'cancelled'
        terminal['payload']['result']['type'] = 'aborted'
    if mode in ('cancel-conflict', 'cancel-compaction-conflict'):
        terminal['payload']['result']['type'] = 'success'
    if mode in ('cancel-compaction', 'cancel-compaction-no-local', 'cancel-missing-result', 'cancel-compaction-completed'):
        del terminal['payload']['result']
    if mode == 'cancel-compaction-no-messages':
        del terminal['payload']['messages']
    if mode == 'cancel-compaction-malformed':
        terminal['payload']['result']['type'] = 'invalid'
    send(terminal)
    assert sys.stdin.read() == ''  # no duplicate cancel, no final ACK
    write_receipt()
    sys.exit(0)
if mode in ('module-routing', 'module-errors'):
    oracle = json.loads((Path(__file__).resolve().parents[2] /
                         'src/pilotdeck-gateway/tests/fixtures/host-modules/oracle.json').read_text())
    key = 'routing' if mode == 'module-routing' else 'errors'
    rejected_modules = {'unknown', '\ud800'}
    source_cases = oracle[key]
    rejected = [case for case in source_cases if case['call']['module'] in rejected_modules]
    cases = [case for case in source_cases if case['call']['module'] not in rejected_modules]
    assert {case['call']['module'] for case in rejected} <= rejected_modules
    assert len(cases) + len(rejected) == len(source_cases)
    assert len(cases) == oracle['stdioCounts'][key] > 0, (key, len(cases))
    expected = {case['call']['messageId']: case['response'] for case in cases}
    assert len(expected) == len(cases), 'duplicate messageId'
    # Keep multiple requests in flight, with a bounded window independent of
    # oracle size so neither OS pipe nor the host callback queue can fill up.
    for start in range(0, len(cases), 16):
        batch = cases[start:start + 16]
        for case in batch:
            send(case['call'])
        for _ in batch:
            reply = read()
            assert reply == expected.pop(reply['inReplyTo']), reply
    assert not expected
    send(event(final=True))
    assert sys.stdin.read() == ''
    write_receipt()
    sys.exit(0)

if mode == 'event-order':
    # 16 event.emit calls in flight at once: the host sink must see them in
    # this send order (TS emits before the handler's first await).
    for number in range(16):
        send(dict(kind='request', messageId='emit-' + str(number), method='module_call', module='event',
                  runId=execute['runId'], operationId=execute['operationId'], requestId='callback-request',
                  payload=dict(operation='emit', event=dict(type='probe', sessionId=execute['sessionId'],
                                                            turnId=execute['turnId'], index=number))))
    replies = {read()['inReplyTo'] for _ in range(16)}
    assert replies == {'emit-' + str(number) for number in range(16)}, replies
    send(event(final=True))
    assert sys.stdin.read() == ''
    write_receipt()
    sys.exit(0)

if mode.startswith('grandchild-'):
    send(module_call())
    time.sleep(60)
if mode == 'accepted-then-rejected':
    send(event())
    ack = read()
    assert ack['method'] == 'ack' and ack['lastAppliedSequence'] == 0
    send(dict(response(execute, requestId=execute['requestId']), ok=False,
              final=True, outcome='failed', code='CONTROLLED_REJECTION'))
    assert sys.stdin.read() == ''
    write_receipt()
    sys.exit(0)
if mode in ('duplicate-acceptance', 'duplicate-acceptance-new-stream'):
    new_stream = 'replacement' if mode.endswith('new-stream') else 'stream'
    if new_stream == 'replacement':
        send(event())
        ack = read()
        assert ack['method'] == 'ack' and ack['lastAppliedSequence'] == 0
    send(response(execute, requestId=execute['requestId'], streamId=new_stream))
    send(event(sequence=1 if new_stream == 'replacement' else 0, final=True, streamId=new_stream))
    assert sys.stdin.read() == ''
    write_receipt()
    sys.exit(0)
if mode == 'stream-mismatch':
    send(event(streamId='foreign'))
elif mode == 'missing-messages':
    terminal = event(final=True)
    del terminal['payload']['messages']
    send(terminal)
elif mode == 'terminal-pending-callback':
    send(module_call())
    send(event(final=True))
elif mode in ('module-call-limit', 'active-call-limit', 'wrong-call-limit'):
    for number in range(1, 5):
        call = module_call(number)
        if mode == 'wrong-call-limit':
            call['runId'] = 'foreign'
        send(call)
    sys.stdin.read()
    sys.exit(0)
elif mode == 'duplicate-call-limit':
    for _ in range(5):
        send(module_call())
    sys.stdin.read()
    sys.exit(0)
elif mode in ('event-queue-limit', 'event-byte-limit'):
    for number in range(8):
        frame = event(sequence=number)
        if mode == 'event-byte-limit':
            frame['payload']['text'] = 'x' * 1024
        send(frame)
    sys.stdin.read()
    sys.exit(0)
elif mode == 'cache-byte-limit':
    for number in range(1, 15):
        send(module_call(number, padding=512))
        try:
            read()
        except RuntimeError:
            sys.exit(0)
    raise RuntimeError('cache budget did not stop execution')
elif mode == 'response-byte-limit':
    send(module_call())
    sys.stdin.read()
    sys.exit(0)
if mode == 'terminal-then-exit':
    # Close first so an ACK cannot win a race against process exit and mask EPIPE.
    os.close(sys.stdin.fileno())
    send(event())
    send(event(sequence=1, final=True))
    write_receipt()
    sys.exit(0)
if mode == 'event-then-disconnect':
    send(event())
    sys.exit(0)
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
elif mode in ('cancelled', 'failed-error', 'failed-max-turns', 'cancelled-success'):
    outcome, result_type = {
        'cancelled': ('cancelled', 'aborted'),
        'failed-error': ('failed', 'error'),
        'failed-max-turns': ('failed', 'max_turns'),
        'cancelled-success': ('cancelled', 'success'),
    }[mode]
    terminal = event(final=True, outcome=outcome)
    terminal['payload']['result']['type'] = result_type
    send(terminal)
elif mode in ('duplicate-conflict', 'duplicate-same', 'callback-progress', 'callback-disconnect', 'callback-timeout', 'wrong-call', 'wrong-call-replay', 'wrong-call-conflict'):
    call = dict(kind='request', messageId='call-1', method='module_call', module='model',
                runId=execute['runId'], operationId=execute['operationId'], requestId='callback-request',
                payload=dict(operation='stream'))
    if mode in ('wrong-call', 'wrong-call-replay', 'wrong-call-conflict'):
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
                assert reply['ok'] == (mode not in ('wrong-call', 'wrong-call-replay', 'wrong-call-conflict'))
                replies -= 1
        if mode == 'wrong-call-replay':
            # TS client:1120-1121 revalidates identity before the cache: an
            # identical wrong-identity replay fails closed, not replayed.
            assert reply['code'] == 'HOST_MODULE_FAILED'
            assert 'identity does not match' in reply['error']['message']
            send(call)
            sys.stdin.read()
            sys.exit(0)
        if mode == 'wrong-call-conflict':
            send(dict(call, runId=execute['runId']))
        else:
            send(event(sequence=1 if mode == 'callback-progress' else 0, final=True))
elif mode in ('positive', 'no-ack', 'ack-disabled', 'resume-support-missing', 'resume-support-unary', 'cleanup-hang', 'stderr-flood', 'slow-event', 'ack-failure', 'unknown-response', 'turn-completed'):
    if mode == 'stderr-flood':
        sys.stderr.write('x' * (512 * 1024))
        sys.stderr.flush()
    send(event())
    if mode not in ('no-ack', 'ack-disabled', 'resume-support-missing', 'resume-support-unary'):
        ack = read()
        assert ack['method'] == 'ack' and ack['streamId'] == 'stream' and ack['lastAppliedSequence'] == 0
        if mode in ('ack-failure', 'unknown-response'):
            reply = dict(response(ack), ok=False, code='ACK_REJECTED')
            if mode == 'unknown-response':
                reply['inReplyTo'] = 'unrecognized-request'
            send(reply)
    final_sequence = 1
    if mode == 'turn-completed':
        send(event(sequence=1, payload=dict(type='turn_completed', sessionId=execute['sessionId'], turnId=execute['turnId'])))
        send(event(sequence=2))
        ack = read()
        # An ACK for suppressed sequence 1 must fail before the final is sent.
        assert ack['method'] == 'ack' and ack['streamId'] == 'stream' and ack['lastAppliedSequence'] == 2
        final_sequence = 3
    send(event(sequence=final_sequence, final=True))
# Test parent requires this receipt; a terminal followed by exit=1 is not PASS.
for line in sys.stdin:
    raise RuntimeError('unexpected trailing host frame: ' + line)
write_receipt()
if mode == 'cleanup-hang':
    time.sleep(60)
