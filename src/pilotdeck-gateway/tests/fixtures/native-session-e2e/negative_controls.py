#!/usr/bin/env python3
"""#1140 fix-r1 e2e source mutations; HEAD guard is mandatory, all cases are summarized."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import json
import os
from pathlib import Path
import re

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

session = root / 'scripts/gateway-parity/session_domain.py'
common = root / 'scripts/gateway-parity/common.py'
collector = root / 'scripts/native-witness/collect.py'
fixture = Path(__file__).with_name('test_e2e.py')
# Each row mutates one #1140 fix-r1 addition and names the e2e test whose
# dedicated assertion message must fire (the named FAIL the classifier pins):
# the scoring-side shapes in session_domain.py and the recorder/contract-side
# shapes in collect.py, so neither the collector's output nor the frozen audit
# can silently drift away from the P1 recorder contract.
cases = [
    ('decoder-frame-for-send-rows', session,
     "        require(len(decoded) == 1\n"
     "                and decoded[0].get('raw' if row['direction'] == 'send' else 'frame') == row['frame'],\n"
     "                'OBSERVER_WIRE', 'wire observation disagrees with recorder')\n",
     "        require(len(decoded) == 1\n"
     "                and decoded[0].get('frame') == row['frame'],\n"
     "                'OBSERVER_WIRE', 'wire observation disagrees with recorder')\n",
     'test_e2e_scores_real_capture'),
    ('drop-empty-ledger-require', session,
     "    require(bool(requests), 'NETWORK_WITNESS',\n"
     "            'provider request ledger is empty: no model egress was observed')\n",
     "    pass\n",
     'test_e2e_data_control_empty_ledger'),
    ('record-state-batch-crash', session,
     "        projected = {'applicable': True, 'events': events, 'state': record.get('state'),\n",
     "        projected = {'applicable': True, 'events': events, 'state': record['state'],\n",
     'test_e2e_data_control_missing_record_state'),
    ('executed-includes-not-run', session,
     "            cell['executedCases'] = [row['case'] for row in native['verdicts']\n"
     "                                     if row['status'] != 'NOT_RUN']\n",
     "            cell['executedCases'] = [row['case'] for row in native['verdicts']]\n",
     'test_e2e_data_control_observation_null'),
    ('skip-structure-type-check', session,
     "    if not (isinstance(observation['frames'], list)\n"
     "            and all(isinstance(event, dict) and isinstance(event.get('type'), str)\n"
     "                    for event in observation['frames'])):\n"
     "        return 'frames'\n",
     "    if not isinstance(observation['frames'], list):\n"
     "        return 'frames'\n",
     'test_e2e_data_control_frames_without_type'),
    ('hellos-unfiltered', session,
     "    hellos = [r['frame'] for r in observer['rows']\n"
     "              if isinstance(r['frame'], dict) and r['frame'].get('type') == 'hello_ok']\n",
     "    hellos = [r['frame'] for r in observer['rows'] if r['frame'].get('type') == 'hello_ok']\n",
     'test_e2e_scores_real_capture'),
    ('drop-wire-shape-require', session,
     "        require(isinstance(row.get('wire'), dict), 'OBSERVER_WIRE',\n"
     "                'observer rows must carry wire metadata '\n"
     "                '(opcode/fin/rsv/masked/lengthCode/payloadLength)')\n"
     "        wire_frame(row)\n",
     "        wire_frame(row)\n",
     'test_e2e_data_control_row_without_wire'),
    ('send-frame-as-object', collector,
     "                          'frame': payload.decode(), 'connection': 1})\n",
     "                          'frame': json.loads(payload), 'connection': 1})\n",
     'test_e2e_capture_schema'),
    ('drop-row-wire-metadata', collector,
     "        self.rows.append({'direction': 'send', 'wireHex': frame.hex(),\n"
     "                          'wire': {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': True,\n"
     "                                   'lengthCode': 126 if 126 <= size <= 65535 else 127 if size > 65535 else size,\n"
     "                                   'payloadLength': size},\n"
     "                          'frame': payload.decode(), 'connection': 1})\n",
     "        self.rows.append({'direction': 'send', 'wireHex': frame.hex(),\n"
     "                          'frame': payload.decode(), 'connection': 1})\n",
     'test_e2e_capture_schema'),
    ('drop-contract-connections', collector,
     "                    'externalEndpoints': [f'127.0.0.1:{port + 1}'],\n"
     "                    'connections': 1,\n",
     "                    'externalEndpoints': [f'127.0.0.1:{port + 1}'],\n",
     'test_e2e_capture_schema'),
    ('hardcode-registry-applicable', collector,
     "        registry['applicable'] = bool(registry['events'] or registry['record'])\n",
     "        registry['applicable'] = True\n",
     'test_e2e_scores_real_capture'),
]
messages = {
    'decoder-frame-for-send-rows': '#1140 e2e real capture scores 72/72 per-case PASS',
    'drop-empty-ledger-require': '#1140 e2e an empty provider ledger is rejected',
    'record-state-batch-crash': '#1140 e2e a missing run-record state is a per-case FAIL',
    'executed-includes-not-run': '#1140 e2e a NOT_RUN case is not executed',
    'skip-structure-type-check': '#1140 e2e frames without a type are a structure rejection',
    'hellos-unfiltered': '#1140 e2e real capture scores 72/72 per-case PASS',
    'drop-wire-shape-require': '#1140 e2e a recorder row without wire is rejected',
    'send-frame-as-object': '#1140 e2e sent rows record the payload text',
    'drop-row-wire-metadata': '#1140 e2e recorder rows carry wire metadata',
    'drop-contract-connections': '#1140 e2e the collector contract declares connections',
    'hardcode-registry-applicable': '#1140 e2e real capture scores 72/72 per-case PASS',
}


def preflight():
    # Read-only: this never executes a mutant or bypasses the HEAD guard.
    errors = []
    for name, path, before, after, _ in cases:
        text = path.read_text()
        if text.count(before) != 1:
            errors.append(f'STALE ANCHOR {name}: {text.count(before)} matches')
        else:
            try:
                compile(text.replace(before, after, 1), str(path), 'exec')
            except SyntaxError as error:
                errors.append(f'INVALID compile {name}: {error}')
    if errors:
        print('\n'.join(errors), file=sys.stderr)
        raise SystemExit(2)
    print(f'#1140 fix-r1 {len(cases)} unique anchors; every Python mutant compiles')


def passed(result):
    log = result.stdout + result.stderr
    return result.returncode == 0 and re.search(r'^Ran 1 test in ', log, re.M) is not None and re.search(r'^OK$', log, re.M) is not None


def classify(result, test, marker):
    log = result.stdout + result.stderr
    if passed(result):
        return 'SURVIVED'
    if (result.returncode == 1 and re.search(r'^FAIL: ' + re.escape(test)
            + r' \(__main__\.E2ETests(?:\.' + re.escape(test) + r')?\)$', log, re.M)
            and re.search(r'^AssertionError:[^\n]*' + re.escape(marker) + r'[^\n]*$', log, re.M)
            and re.search(r'^Ran 1 test in ', log, re.M) and 'FAILED (failures=1)' in log):
        return 'KILLED'
    return 'INVALID'


def main():
    preflight()
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    output, baseline, only = None, 'HEAD', None
    arguments = sys.argv[1:]
    position = 0
    while position < len(arguments):
        argument = arguments[position]
        if output is None and not argument.startswith('--'):
            output = argument
        elif argument == '--case' and position + 1 < len(arguments):
            only = arguments[position + 1]
            position += 1
        elif argument == '--baseline' and position + 1 < len(arguments):
            baseline = arguments[position + 1]
            position += 1
        else:
            raise SystemExit('usage: negative_controls.py NEW_OUTPUT [--case NAME] '
                             '[--baseline TREE] | --check-anchors')
        position += 1
    if output is None:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT [--case NAME] '
                         '[--baseline TREE] | --check-anchors')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller (never create a second target)')
    selected = cases if only is None else [row for row in cases if row[0] == only]
    if not selected:
        raise SystemExit('unknown negative case')
    out = Path(output).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []
    def run(name, test):
        # This suite executes Python only. wait_for_slot is intentionally not
        # called: there is no cargo process or Rust product-source mutation.
        result = run_owned([sys.executable, '-I', str(fixture), 'E2ETests.' + test],
                            cwd=root, env=os.environ.copy(), timeout=300)
        (out / (name + '.log')).write_text(result.stdout + result.stderr)
        return result
    try:
        with mutating_sources(root, [session, common, collector, fixture, Path(__file__).resolve()],
                              baseline=baseline) as originals:
            for name, path, before, after, test in selected:
                status, reason, compiled = 'INVALID', '', False
                try:
                    text = originals[path].decode()
                    if text.count(before) != 1:
                        raise ValueError('STALE ANCHOR ' + name)
                    changed = text.replace(before, after, 1)
                    compile(changed, str(path), 'exec')
                    compiled = True
                    if not passed(run(name + '-positive', test)):
                        raise ValueError('positive bound test did not pass')
                    path.write_text(changed)
                    status = classify(run(name + '-negative', test), test, messages[name])
                    reason = {'KILLED': 'named assertion matched', 'SURVIVED': 'bound test passed',
                              'INVALID': 'exit/test identity/assertion marker did not match'}[status]
                except Exception as error:
                    status, reason = 'INVALID', str(error)
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not passed(run(name + '-restored', test)):
                        status, reason = 'INVALID', reason + '; restored test failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + '; restore: ' + str(error)
                results.append({'name': name, 'status': status, 'compiled': compiled, 'reason': reason})
    except Exception as error:
        for row in results:
            row.update(status='INVALID', compiled=False, reason='guard failure: ' + str(error))
        if not results:
            results = [{'name': name, 'status': 'INVALID', 'compiled': False, 'reason': 'guard refusal: ' + str(error)} for name, *_ in selected]
    # Publish only after mutating_sources has verified restoration.
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results}, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps({'controls': results}, indent=2))
    return 0 if len(results) == len(selected) and all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
