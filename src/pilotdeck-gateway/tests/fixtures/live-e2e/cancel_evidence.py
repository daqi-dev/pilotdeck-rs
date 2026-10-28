# anchor-selfcheck: unsupported structured cancel negative eligibility, no source mutation definitions
"""Cancel negative-control eligibility; window-external errors are INVALID."""
import json
from selftest import strict_mismatch

EVENTS = ['model_request_started', 'model_event', 'model_event', 'model_event',
          'assistant_message', 'stop_requested', 'agent_status', 'turn_completed']
DETAILS = [{'type': 'model_request_started'},
           *[{'type': 'model_event', 'modelEventType': kind}
             for kind in ('request_started', 'message_start', 'text_delta')],
           *[{'type': kind} for kind in ('assistant_message', 'stop_requested', 'agent_status', 'turn_completed')]]


def abort_evidence_ok(case):
    try:
        result = json.loads((case / 'driver-result.json').read_text())
        mock = [json.loads(line) for line in (case / 'mock.log').read_text().splitlines()]
        wanted = {
            'scenario': 'cancel', 'injection': 'passthrough',
            'cancelWindowEnteredAtEvents': 2, 'abortAfterEvents': None, 'abortReason': None,
            'events': EVENTS, 'eventDetails': DETAILS, 'error': None,
            'terminal': {'type': 'success', 'stopReason': 'completed', 'turns': 1, 'messageCount': 2},
            'modelCalls': 1, 'modelEventCounts': [None, 3], 'modelInterrupted': False,
            'toolCalls': [], 'toolResults': [],
        }
        for key, value in wanted.items():
            if key not in result:
                return False, f'missing {key}'
            mismatch = strict_mismatch(result[key], value, key)
            if mismatch:
                return False, mismatch
        if 'timeout' in result:
            return False, 'watchdog evidence is not eligible'
        wanted_mock = [{'index': 1, 'scenario': 'hang', 'connection': 'completed', 'sent': 3,
                        'request': {'roles': ['user'], 'userText': 'LIVE_E2E_HANG run the cancel window'}}]
        mismatch = strict_mismatch(mock[1:], wanted_mock, 'completed mock request')
        if mismatch:
            return False, mismatch
        return True, 'entered window; no unrelated error; bounded complete mock stream and success terminal'
    except (OSError, ValueError, TypeError, KeyError) as error:
        return False, f'unreadable cancel evidence: {error}'
