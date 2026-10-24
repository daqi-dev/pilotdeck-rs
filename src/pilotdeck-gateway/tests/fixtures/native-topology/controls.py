#!/usr/bin/env python3
"""Replay real A5.3 captures through run.py and reject targeted mutations.

This checks the apparatus, not native entry acceptance. Source captures are
read-only; every negative starts with a fresh copy of one passing capture.
The sibling run.py remains the sole definition of observation rules.
"""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import sys

import run as matrix

MAX_FILE_BYTES = 16 * 1024 * 1024
MAX_ROWS = 10_000


def read_bytes(path):
    with path.open('rb') as stream:
        data = stream.read(MAX_FILE_BYTES + 1)
    if len(data) > MAX_FILE_BYTES:
        raise ValueError(f'INPUT_BOUND: file exceeds {MAX_FILE_BYTES} bytes: {path}')
    return data


def sha(data):
    return hashlib.sha256(data).hexdigest()


def event_rows(rows):
    return [row for row in rows if row['kind'] == 'event']


def terminals(rows):
    # The terminal is recorded twice: once as an event and once as execute's
    # return. Mutate both so equality is retained and the target rule is reached.
    return [row['value'] for row in rows if row['kind'] == 'terminal'
            or row['kind'] == 'event' and row['value'].get('final') is True]


def terminal_edit(rows, edit):
    for terminal in terminals(rows):
        edit(terminal)


def request_edit(http, index, edit):
    edit(http[index]['request'])
    # Keep the peer's raw-body witness consistent with its parsed projection.
    http[index]['requestBytes'] = json.dumps(http[index]['request'], ensure_ascii=False)


def drop_event(rows, middle=False):
    positions = [i for i, row in enumerate(rows) if row['kind'] == 'event']
    del rows[positions[len(positions) // 2 if middle else 0]]


def duplicate_event(rows):
    index = next(i for i, row in enumerate(rows) if row['kind'] == 'event')
    rows.insert(index + 1, copy.deepcopy(rows[index]))


def reorder_events(rows):
    first, second = [i for i, row in enumerate(rows) if row['kind'] == 'event'][:2]
    rows[first], rows[second] = rows[second], rows[first]


def remove_operation(rows, operation):
    rows[:] = [row for row in rows if not (row['kind'] == 'module_call'
               and row['value']['payload']['operation'] == operation)]


def identity(rows, key):
    for row in rows:
        if row['kind'] in ('event', 'terminal'):
            row['value'][key] = 'wrong-execution-identity'


def mutations():
    # Mutation callbacks only edit input evidence. All judgments belong to
    # matrix.check_observations, including the exact rule named in this table.
    return (
        ('both-sides-wrong-content', 'model-stream', 'CONTENT',
         lambda rows, http, reference: terminal_edit(rows, lambda terminal:
             terminal['payload']['result']['finalMessage']['content'][0].update(text='same-wrong-output'))),
        ('drop-first-event', 'model-stream', 'EVENT_ORDER',
         lambda rows, http, reference: drop_event(rows)),
        ('drop-middle-event', 'model-stream', 'EVENT_ORDER',
         lambda rows, http, reference: drop_event(rows, middle=True)),
        ('duplicate-event', 'model-stream', 'EVENT_ORDER',
         lambda rows, http, reference: duplicate_event(rows)),
        ('unordered-events', 'model-stream', 'EVENT_ORDER',
         lambda rows, http, reference: reorder_events(rows)),
        ('premature-final', 'model-stream', 'TERMINAL',
         lambda rows, http, reference: event_rows(rows)[0]['value'].update(final=True)),
        ('wrong-run-identity', 'model-stream', 'IDENTITY',
         lambda rows, http, reference: identity(rows, 'runId')),
        ('wrong-operation-identity', 'model-stream', 'IDENTITY',
         lambda rows, http, reference: identity(rows, 'operationId')),
        ('empty-module-observer', 'model-stream', 'EMPTY_OBSERVER',
         lambda rows, http, reference: rows.__setitem__(slice(None),
             [row for row in rows if row['kind'] != 'module_call'])),
        ('missing-prepare', 'model-stream', 'MODULES',
         lambda rows, http, reference: remove_operation(rows, 'prepare')),
        ('missing-stream-next', 'model-stream', 'MODULES',
         lambda rows, http, reference: remove_operation(rows, 'stream_next')),
        ('provider-code-lost', 'provider-error', 'PROVIDER_ERROR',
         lambda rows, http, reference: terminal_edit(rows, lambda terminal:
             terminal['error']['details'].pop('code'))),
        ('provider-message-lost', 'provider-error', 'PROVIDER_ERROR',
         lambda rows, http, reference: terminal_edit(rows, lambda terminal:
             terminal['error']['details'].pop('message'))),
        ('unconfigured-code-lost', 'provider-unconfigured', 'UNCONFIGURED_CODE',
         lambda rows, http, reference: terminal_edit(rows, lambda terminal:
             terminal['error']['details'].pop('code'))),
        ('unconfigured-message-lost', 'provider-unconfigured', 'UNCONFIGURED_MESSAGE',
         lambda rows, http, reference: terminal_edit(rows, lambda terminal:
             terminal['error'].pop('message'))),
        ('unconfigured-issued-http', 'provider-unconfigured', 'UNCONFIGURED',
         lambda rows, http, reference: http.append(copy.deepcopy(reference[0]))),
        ('missing-judge-request', 'token-saver-judge', 'HTTP_COUNT',
         lambda rows, http, reference: http.pop(0)),
        ('judge-request-streams', 'token-saver-judge', 'JUDGE',
         lambda rows, http, reference: request_edit(http, 0, lambda request: request.update(stream=True))),
        ('judge-model-lost', 'token-saver-judge', 'JUDGE',
         lambda rows, http, reference: request_edit(http, 0, lambda request: request.update(model='controlled'))),
        ('economical-route-lost', 'token-saver-judge', 'HTTP_MODEL',
         lambda rows, http, reference: request_edit(http, -1, lambda request: request.update(model='controlled'))),
        ('missing-model-stream', 'token-saver-judge', 'HTTP_STREAM',
         lambda rows, http, reference: request_edit(http, -1, lambda request: request.update(stream=False))),
    )


def classify_error(error):
    # A KeyError/TypeError or arbitrary crash never counts as a rejected control.
    return str(error).split(':', 1)[0] if type(error) is ValueError else 'UNEXPECTED_EXCEPTION'


def rejection_check(check, expected):
    try:
        check()
    except Exception as error:
        observed = classify_error(error)
        return {'status': 'PASS' if observed == expected else 'FAIL',
                'observedRule': observed, 'detail': str(error)}
    return {'status': 'FAIL', 'observedRule': 'ACCEPTED',
            'detail': 'mutated capture was not rejected'}


def matrix_controls(samples, report):
    # This fixture exercises only the registered denominator. Its PASS labels
    # come from check_observations above, not from the historical report; the
    # comparison keys are structural inputs, not claims of semantic equality.
    shape = {'cells': [
        {'id': mode, 'status': 'PASS', 'rounds': [
            {'case': case, 'round': round_id, 'status': 'PASS'}
            for sample_mode, round_id, case in samples if sample_mode == mode]}
        for mode in matrix.MODES]}
    keys = {(round_id, case) for _, round_id, case in samples}
    report['matrixStructureControl'] = {
        'scope': 'denominator-only fixture; does not assert dual-configuration equality',
        'roundCount': sum(len(cell['rounds']) for cell in shape['cells']),
        'comparisonKeyCount': len(keys), 'status': 'FAIL'}
    matrix.check_matrix(shape, keys)
    report['matrixStructureControl']['status'] = 'PASS'
    variants = (
        ('empty-matrix', 'MATRIX_CELL', lambda value, comparisons: value['cells'].clear()),
        ('empty-rounds', 'MATRIX_COVERAGE',
         lambda value, comparisons: [cell['rounds'].clear() for cell in value['cells']]),
        ('missing-round', 'MATRIX_COVERAGE', lambda value, comparisons: value['cells'][0]['rounds'].pop()),
        ('duplicate-round', 'MATRIX_COVERAGE', lambda value, comparisons:
            value['cells'][0]['rounds'].__setitem__(-1, copy.deepcopy(value['cells'][0]['rounds'][0]))),
        ('missing-comparison', 'MATRIX_COMPARE',
         lambda value, comparisons: comparisons.remove((0, matrix.CASES[0]))),
        ('duplicate-cell', 'MATRIX_CELL',
         lambda value, comparisons: value['cells'].append(copy.deepcopy(value['cells'][0]))),
        ('failed-round', 'MATRIX_CASE',
         lambda value, comparisons: value['cells'][0]['rounds'][0].update(status='FAIL')),
    )
    for name, expected, mutate in variants:
        value, comparisons = copy.deepcopy(shape), set(keys)
        mutate(value, comparisons)
        checked = rejection_check(lambda: matrix.check_matrix(value, comparisons), expected)
        report['negativeControls'].append({'name': name, 'case': 'matrix-coverage',
                                          'expectedRule': expected, 'status': checked['status'],
                                          'checks': [checked]})


def load_capture(capture, report):
    samples = {}
    report['inputHashes'] = {}
    for mode in matrix.MODES:
        for round_id in range(matrix.REPEATS):
            for case in matrix.CASES:
                directory = capture / mode / str(round_id) / case
                rows_path, http_path = directory / 'stdout.jsonl', directory / 'provider-http.json'
                raw, raw_http = read_bytes(rows_path), read_bytes(http_path)
                lines = raw.splitlines()
                if not 0 < len(lines) <= MAX_ROWS:
                    raise ValueError(f'INPUT_BOUND: invalid row count: {rows_path}')
                rows = [json.loads(line) for line in lines]
                http = json.loads(raw_http)
                if not isinstance(http, list) or len(http) > MAX_ROWS:
                    raise ValueError(f'INPUT_BOUND: invalid HTTP rows: {http_path}')
                for path, data in ((rows_path, raw), (http_path, raw_http)):
                    report['inputHashes'][str(path)] = sha(data)
                # The independent challenge is the registered driver input,
                # never a value recovered from the observed assistant output.
                nonce = f'a53-round-{round_id}-独立证据'
                samples[(mode, round_id, case)] = (rows, http, nonce)
                positive = {'mode': mode, 'round': round_id, 'case': case, 'status': 'FAIL'}
                report['positiveControls'].append(positive)
                try:
                    matrix.check_observations(case, rows, http, nonce)
                except Exception as error:
                    positive.update(rule=classify_error(error), detail=str(error))
                    raise ValueError(f'POSITIVE_CONTROL: {mode}/{round_id}/{case}: {error}') from error
                positive['status'] = 'PASS'
    return samples


def execute(args, report):
    capture = args.capture.resolve()
    if not capture.is_dir():
        raise ValueError('INPUT: --capture must be the raw matrix output directory')
    source_report = capture / 'report.json'
    if source_report.exists():
        data = read_bytes(source_report)
        original = json.loads(data)
        report['originalMatrixStatus'] = original.get('moduleMatrixStatus', original.get('status'))
        report['originalReportSha256'] = sha(data)
    samples = load_capture(capture, report)
    for name, case, expected, mutate in mutations():
        control = {'name': name, 'case': case, 'expectedRule': expected, 'status': 'PASS', 'checks': []}
        report['negativeControls'].append(control)
        paired = {}
        # Run every mutation independently on both implementations and all
        # captured rounds. In particular the same wrong output must fail on
        # BOTH sides instead of merely disappearing in an equality comparison.
        for mode in matrix.MODES:
            for round_id in range(matrix.REPEATS):
                original_rows, original_http, nonce = samples[(mode, round_id, case)]
                rows, http = copy.deepcopy(original_rows), copy.deepcopy(original_http)
                reference = samples[(mode, round_id, 'model-stream')][1]
                checked = {'mode': mode, 'round': round_id, 'status': 'FAIL'}
                control['checks'].append(checked)
                try:
                    mutate(rows, http, reference)
                except Exception as error:
                    checked.update(observedRule='MUTATION_ERROR', detail=f'{type(error).__name__}: {error}')
                    control['status'] = 'FAIL'
                    continue
                if name == 'both-sides-wrong-content':
                    normalized = matrix.semantic_observations(rows)
                    if mode == matrix.MODES[0]:
                        paired[round_id] = normalized
                    else:
                        equal = normalized == paired[round_id]
                        control.setdefault('pairEqualityChecks', []).append({
                            'round': round_id, 'status': 'PASS' if equal else 'FAIL',
                            'meaning': 'both wrong outputs still compare equal',
                            'normalizedSha256': sha(json.dumps(normalized, sort_keys=True).encode())})
                        if not equal:
                            control['status'] = 'FAIL'
                checked.update(rejection_check(
                    lambda: matrix.check_observations(case, rows, http, nonce), expected))
                if checked['status'] != 'PASS':
                    control['status'] = 'FAIL'
    matrix_controls(samples, report)
    for mode in matrix.MODES:
        rows = samples[(mode, 0, 'model-stream')][0]
        config_path = capture / mode / '0' / 'model-stream' / 'config.json'
        config_data = read_bytes(config_path)
        report['inputHashes'][str(config_path)] = sha(config_data)
        config = json.loads(config_data)
        matrix.check_launch(rows, config)
        for name, expected, mutate in (
            ('wrong-module-binding', 'BINDING', lambda row: row['binding'].update(moduleId='ts-gateway')),
            ('empty-instance-binding', 'BINDING', lambda row: row['binding'].update(moduleInstanceId='')),
            ('empty-generation-binding', 'BINDING', lambda row: row['binding'].update(connectionGeneration='')),
            ('wrong-sidecar-program', 'LAUNCH', lambda row: row.update(program='/unregistered/helper')),
            ('wrong-sidecar-argv', 'LAUNCH', lambda row: row.update(argv=['gateway'])),
        ):
            changed = copy.deepcopy(rows)
            mutate(next(row for row in changed if row['kind'] == 'launch'))
            checked = rejection_check(lambda: matrix.check_launch(changed, config), expected)
            report['negativeControls'].append({'name': name, 'mode': mode, 'expectedRule': expected,
                                               'status': checked['status'], 'checks': [checked]})
    for path, expected in report['inputHashes'].items():
        if sha(read_bytes(Path(path))) != expected:
            raise ValueError(f'INPUT_CHANGED: source evidence changed: {path}')
    report['negativeCheckCount'] = sum(len(control['checks']) for control in report['negativeControls'])
    report['positiveCheckCount'] = len(report['positiveControls'])
    report['status'] = 'PASS' if report['negativeControls'] and all(
        control['status'] == 'PASS' for control in report['negativeControls']) else 'FAIL'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--capture', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True, help='new evidence directory; writes report.json')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    runner = Path(matrix.__file__).resolve()
    runner_hash = sha(read_bytes(runner))
    report = {'status': 'FAIL', 'scope': 'A5.3 real-capture observation-rule controls',
              'acceptanceStatus': 'NOT_RUN', 'capture': str(args.capture.resolve()),
              'runner': str(runner), 'runnerSha256': runner_hash,
              'controlsSha256': sha(read_bytes(Path(__file__).resolve())),
              'positiveControls': [], 'negativeControls': []}
    try:
        execute(args, report)
        if sha(read_bytes(runner)) != runner_hash:
            raise ValueError('RUNNER_CHANGED: check_observations changed during controls')
    except Exception as error:
        report.update(status='FAIL', failure={'rule': classify_error(error),
                                            'type': type(error).__name__, 'detail': str(error)})
    output = args.output / 'report.json'
    matrix.save(output, report)
    print(json.dumps({'status': report['status'], 'report': str(output.resolve()),
                      'positiveChecks': report.get('positiveCheckCount', len(report['positiveControls'])),
                      'negativeChecks': report.get('negativeCheckCount', 0),
                      'acceptanceStatus': 'NOT_RUN'}, ensure_ascii=False))
    return 0 if report['status'] == 'PASS' else 1


if __name__ == '__main__':
    sys.exit(main())
