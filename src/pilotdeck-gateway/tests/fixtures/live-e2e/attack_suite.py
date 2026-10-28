#!/usr/bin/env python3
# anchor-selfcheck: unsupported forged-result comparator attacks, no source mutation definitions
"""#969 forged *raw* evidence against committed, independently captured TS goldens.

No source writes. Every row starts with a distinct deep copy of a real cell;
expected-file attacks write only an owned temporary fixture directory.
"""
import copy
import json
from pathlib import Path
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import evidence
import selftest


def change_result(key, value):
    return lambda raw: raw['result'].__setitem__(key, value)


def terminal(key, value):
    return lambda raw: raw['result']['terminal'].__setitem__(key, value)


def event_sequence(value):
    def edit(raw):
        # The targeted frame is found in the real capture, never synthesized.
        frame = next(row['frame'] for row in raw['frames']['s2h']
                     if row['dir'] == 's2h' and row['frame'].get('sequence') == 0)
        frame['sequence'] = value
    return edit


def resize_events(raw, append=False):
    for key in ('events', 'eventDetails'):
        if append:
            raw['result'][key].append(copy.deepcopy(raw['result'][key][-1]))
        else:
            raw['result'][key].pop()


# name, cell, diagnostic, edit actual. Each is bound by its exact ESCAPED name.
ATTACKS = [
    ('bool-for-int-count', 'text-passthrough', 'boolean', change_result('modelCalls', True)),
    ('float-for-int-count', 'text-passthrough', 'number', change_result('modelCalls', 1.0)),
    ('bool-in-terminal', 'text-passthrough', 'boolean', terminal('turns', True)),
    ('float-in-terminal', 'text-passthrough', 'number', terminal('turns', 1.0)),
    ('string-for-int-in-terminal', 'text-passthrough', 'string', terminal('messageCount', '2')),
    ('extra-terminal-key', 'text-passthrough', 'unexpected key', terminal('errors', [])),
    ('missing-terminal-key', 'text-passthrough', 'missing key', lambda r: r['result']['terminal'].pop('stopReason')),
    ('missing-error-field', 'text-passthrough', 'missing key', lambda r: r['result'].pop('error')),
    ('missing-terminal-field', 'text-passthrough', 'missing key', lambda r: r['result'].pop('terminal')),
    ('int-for-null-error', 'text-passthrough', 'integer', change_result('error', 0)),
    ('bool-in-frame-sequence', 'text-passthrough', 'boolean', event_sequence(False)),
    ('float-in-frame-sequence', 'text-passthrough', 'number', event_sequence(0.0)),
    ('events-reordered', 'text-passthrough', 'result.eventDetails', lambda r: (r['result']['events'].reverse(), r['result']['eventDetails'].reverse())),
    ('eventdetails-not-a-list', 'text-passthrough', 'string', change_result('eventDetails', 'bogus')),
    ('events-not-a-list', 'text-passthrough', 'integer', change_result('events', 5)),
    ('failing-case-carries-verified-false', 'text-passthrough', 'result.modelCalls', change_result('modelCalls', 2)),
    ('events-truncated-together', 'text-passthrough', 'items', resize_events),
    ('events-appended-together', 'text-passthrough', 'items', lambda r: resize_events(r, True)),
    ('toolcalls-string', 'tool-passthrough', 'string', change_result('toolCalls', 'x')),
    ('duplicate-sequence-false', 'text-duplicate', 'boolean', event_sequence(False)),
    ('empty-frames', 'text-passthrough', 'items', lambda r: (r['frames']['h2s'].clear(), r['frames']['s2h'].clear())),
    ('truncated-frames', 'text-passthrough', 'items', lambda r: r['frames']['s2h'].pop()),
    ('literal-context-token', 'tool-passthrough', 'toolCalls', lambda r: r['result']['toolCalls'][0]['input'].__setitem__('path', '<CASE>/sandbox/tool-target.txt')),
    ('wrong-mock-port', 'text-passthrough', 'mock[0].port', lambda r: r['mock'][0].__setitem__('port', 18789)),
    ('empty-actual', 'text-passthrough', 'missing key', lambda r: r['result'].clear()),
    ('expected-file-tampered', 'text-passthrough', 'result.terminal.messageCount', lambda r: None),
]


def run_attack(name, cell, edit):
    original = evidence.load_expected(cell)
    raw = evidence.instantiate(copy.deepcopy(original), Path('/owned-case'), 12345)
    edit(raw)
    with tempfile.TemporaryDirectory(prefix='pdrs-969-attack-') as temp:
        root = Path(temp)
        mock = copy.deepcopy(raw['mock'])
        log = root / 'mock.log'
        log.write_text(''.join(json.dumps(row) + '\n' for row in mock))
        old_run, old_expected = selftest.run_case, evidence.EXPECTED
        try:
            if name == 'expected-file-tampered':
                original['result']['terminal']['messageCount'] = 999
                (root / f'{cell}.json').write_text(json.dumps(original))
                evidence.EXPECTED = root
            selftest.run_case = lambda *args: (raw['result'], raw['frames'], raw['summary'], Path('/owned-case'))
            return selftest.verify_caught(cell, *cell.split('-', 1), 12345, root, Path('/owned-case'), log)
        finally:
            selftest.run_case, evidence.EXPECTED = old_run, old_expected


def main(report=None):
    for cell in sorted({row[1] for row in ATTACKS}):
        baseline = run_attack('baseline', cell, lambda raw: None)
        if baseline.get('verified') is not True:
            print(f'BASELINE BROKEN: {cell}: {baseline}')
            return 2
    escaped = []
    statuses = []
    for name, cell, marker, edit in ATTACKS:
        result = run_attack(name, cell, edit)
        message = result.get('failed', '')
        if result.get('verified') is False and '#969' in message and marker in message:
            print(f'caught {name}: {message}')
        else:
            print(f'ESCAPED {name}: {result}')
            escaped.append(name)
        statuses.append({'name': name, 'caught': name not in escaped})
    print(f'{len(ATTACKS) - len(escaped)}/{len(ATTACKS)} attacks caught')
    if report is not None:
        Path(report).write_text(json.dumps(statuses, indent=2) + '\n')
    return 1 if escaped else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[2] if len(sys.argv) == 3 and sys.argv[1] == "--report" else None))
