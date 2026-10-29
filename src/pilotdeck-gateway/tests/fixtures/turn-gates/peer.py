# anchor-selfcheck: unsupported protocol peer fixture, no mutation definitions
"""Deterministic execute/model/terminal boundaries; no timeout advances a gate."""
import json
import sys
from pathlib import Path
mode, marker = sys.argv[1:]
def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    value = json.loads(line)
    if value.get('method') == 'execute':
        with Path(marker).open('a') as output:
            output.write('execute\n')
    return value
def send(value):
    print(json.dumps(value), flush=True)
def reply(request, **fields):
    return dict(kind='response', messageId='reply-' + request['messageId'], inReplyTo=request['messageId'], ok=True, **fields)
binding = dict(protocolVersion='2.0', moduleId='fixture', moduleInstanceId='instance', connectionGeneration='current', capabilitiesVersion='v1')
send(reply(read(), **binding))
send(reply(read(), **binding, payload=dict(capabilitiesVersion='v1', methods=[dict(name='execute', profiles=['streaming']), dict(name='cancel')])))
request = read()
assert request['method'] == 'execute'
send(reply(request, requestId=request['requestId'], streamId='stream'))
identity = {k: request[k] for k in ('runId', 'operationId', 'requestId')}
def event(seq, payload, final=False, outcome=None):
    send(dict(kind='event', eventType='agent_event', streamId='stream', sequence=seq, final=final, payload=payload, **identity, **({'outcome':outcome} if final else {})))
if mode.startswith('error'):
    sys.exit(0)
if mode.startswith('protocol'):
    event(99, dict(type='model_event'))
else:
    send(dict(kind='request', messageId='model', method='module_call', module='model', **identity, payload={}))
    while read().get('inReplyTo') != 'model':
        pass
    event(0, dict(type='model_event', sessionId=request['sessionId'], turnId=request['turnId'], event=dict(type='request_started')))
    cancelled = mode.startswith('cancel')
    if cancelled:
        while read().get('method') != 'cancel':
            pass
    result = dict(type='aborted' if cancelled else 'success', sessionId=request['sessionId'], turnId=request['turnId'], stopReason='aborted_streaming' if cancelled else 'completed', turns=1)
    event(1, dict(type='turn_completed', sessionId=request['sessionId'], turnId=request['turnId'], result=result))
    event(2, dict(result=result, messages=request['payload']['messages']), True, 'cancelled' if cancelled else 'completed')
while True:
    read()
