"""Full-path ID controls: a legal rename passes; entity collapse still fails.

Each row exercises the real recursive policy dispatcher and structural diff,
using cross-channel witnesses, not a mock normalization implementation.
"""
import json
import re

from compare import Normalizer, diff
from policy import HTTP_IDS, WS_IDS, DISK_IDS, SSE_IDS, field_key

A = '11111111-1111-4111-8111-111111111111'
B = '22222222-2222-4222-8222-222222222222'
C = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
D = 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb'


def path_parts(path):
    return [0 if p == '*' else json.loads(p) if p.startswith('"') else p
            for p in re.findall(r'"(?:[^"\\]|\\.)*"|[^.]+', path)]


def document(path, value):
    for part in reversed(path_parts(path)):
        value = [value] if part == 0 else {part: value}
    return value


def locations():
    for channel, contexts in (('http', HTTP_IDS), ('ws', WS_IDS), ('disk', DISK_IDS),
                              ('sse', {'upload.events:upload_completed': SSE_IDS})):
        for context, paths in contexts.items():
            for path, category in paths.items():
                if category not in ('upload-url', 'token'):
                    yield channel, context, path, category


def run_identity_controls():
    rows = []
    for channel, context, path, category in locations():
        expected_location = f'{channel}[{context}].body' + ''.join(
            f'[{part}]' if type(part) is int else '.' + field_key(part) for part in path_parts(path))
        def evaluate(mutation=None):
            errors = []
            left, right = Normalizer('left', errors), Normalizer('right', errors)
            # Two independent identities are established on other full paths.
            # Cross semantic categories/channels must share the same UUID map.
            for norm, one, two in ((left, A, B), (right, C, D)):
                norm.value([{'turnId': one}], 'disk', '@chat')
                norm.value({'runId': two}, 'ws', 'status')
            suffix = ':text:0' if category in ('block', 'timeline') else ''
            lv, rv = B + suffix, (C if mutation == 'collapse' else D) + suffix
            if mutation == 'suffix':
                rv = D + ':text:1'
            if mutation == 'kind':
                rv = D + ':thinking:0'
            location = f'{channel}[{context}].body'
            documents = [document(path, lv), document(path, rv)]
            if channel == 'http' and context == 'readonly.messages' and path.startswith('messages.'):
                for norm, doc in zip((left, right), documents):
                    stamp = '2023-11-14T22:13:20.000Z'
                    norm.wall_bounds = (1700000000000, 1700000010000)
                    message = doc['messages'][0]
                    message['timestamp'] = stamp
                    message.setdefault('sessionId', 'identity-fixture-session')
                    message.setdefault('sequence', 1)
                    norm.chat_clocks[message['sessionId'], message['sequence']] = (stamp, message.get('entryId'))
            a = left.value(documents[0], channel, context, location=location)
            b = right.value(documents[1], channel, context, location=location)
            rule = {'http': 'http.body', 'ws': 'ws.structure', 'disk': 'disk.snapshot', 'sse': 'sse.sequence'}[channel]
            diff(a, b, errors, rule, location)
            return errors, rule

        good, _ = evaluate()
        for mutation in (['collapse', 'suffix', 'kind'] if category in ('block', 'timeline') else ['collapse']):
            bad, rule = evaluate(mutation)
            hit = [d for d in bad if d['rule'] == rule and d['location'] == expected_location]
            rows.append({'name': f'id-{mutation}:{channel}:{context}:{path}',
                         'level': 'full-path-policy', 'expect': 'FAIL', 'expected_rule': rule,
                         'expected_location': expected_location, 'policy_path': path,
                         'equivalent_rename': not good, 'actual': 'FAIL' if bad else 'PASS',
                         'passed': not good and bool(hit), 'matched': hit[:1]})
    return rows
