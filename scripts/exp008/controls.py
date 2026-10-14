#!/usr/bin/env python3
"""Offline #41 positive/equivalence controls, also included in compare --controls.

No listeners. --before loads an old comparator and proves each regression was
formerly a false PASS. --rust includes the proxy's no-network positive controls.
"""
import argparse
import base64
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

import compare

ROOT = Path(__file__).resolve().parents[2]
NOW = 1_791_924_812_865
UUID1 = '11111111-1111-4111-8111-000000000001'
UUID2 = '22222222-2222-4222-8222-000000000002'


def etag(body, weak=True):
    raw = body.encode()
    digest = base64.b64encode(hashlib.sha1(raw).digest()).decode().rstrip('=')
    return ('W/' if weak else '') + f'"{len(raw):x}-{digest}"'


def http(body, path='/api/config', revision=None):
    body = json.dumps(body, separators=(',', ':'))
    headers = {'etag': etag(body), 'content-type': 'application/json'}
    if revision is not None:
        headers['x-projects-revision'] = str(revision)
    return {'case': 'control', 'method': 'GET', 'path': path, 'status': 200,
            'headers': headers, 'body': body, 'requestTimeMs': [NOW - 20, NOW + 20]}


def ws(body, direction='u2c', baseline=None):
    record = {'case': 'control', 'dir': direction, 'opcode': 'text', 'raw': json.dumps(body)}
    if baseline is not None:
        record['activityBaseline'] = baseline
    return record


def anchored(revision, base=0, session=UUID1):
    snapshot = {'type': 'session-activity-snapshot', 'activities': [] if base == 0 else [
        {'sessionId': 'previous-session', 'revision': base, 'processing': False}]}
    return [ws({'type': 'pilotdeck-command', 'options': {'sessionId': session}},
               'c2u', snapshot),
            ws({'type': 'session-activity', 'activity': {
                'sessionId': session, 'revision': revision, 'processing': True}})]


def cases():
    out = []

    def add(name, channel, left, right, location, expected='FAIL', before='PASS'):
        out.append((name, channel, left, right, location, expected, before))

    for key in ('revision', 'generation', 'connectionGeneration', 'timestamp', 'pid'):
        add(f'inject:unknown-http-{key}', 'http', [http({key: 7})], [http({key: 999})],
            f'body.{key}')
    for event in ('unknown-event', 'session-activity'):
        add(f'inject:wrong-full-path-{event}', 'ws',
            [ws({'type': event, 'metadata': {'revision': 7, 'timestamp': 7}})],
            [ws({'type': event, 'metadata': {'revision': 999, 'timestamp': 999}})],
            'body.metadata.revision')
    add('inject:unknown-identity', 'ws', [ws({'type': 'unknown', 'id': UUID1})],
        [ws({'type': 'unknown', 'id': UUID2})], 'body.id')
    add('inject:known-event-nested-identity', 'ws',
        [ws({'type': 'session-input-accepted', 'metadata': {'sessionId': UUID1}})],
        [ws({'type': 'session-input-accepted', 'metadata': {'sessionId': UUID2}})],
        'body.metadata.sessionId')
    add('inject:wrong-channel-sse-clock', 'sse',
        [{'case': 'control', 'index': 0, 'data': json.dumps({'timestamp': 7})}],
        [{'case': 'control', 'index': 0, 'data': json.dumps({'timestamp': 999})}],
        'data.timestamp')
    add('inject:wrong-direction-identity', 'ws',
        [ws({'type': 'session-input-accepted', 'sessionId': UUID1}, 'c2u')],
        [ws({'type': 'session-input-accepted', 'sessionId': UUID2}, 'c2u')],
        'body.sessionId')
    add('inject:array-clock', 'http', [http({'items': [{'timestamp': 7}]})],
        [http({'items': [{'timestamp': 999}]})], 'body.items[0].timestamp')
    add('inject:single-unanchored-revision', 'ws',
        [ws({'type': 'session-activity', 'activity': {'sessionId': UUID1, 'revision': 7}})],
        [ws({'type': 'session-activity', 'activity': {'sessionId': UUID1, 'revision': 999}})],
        'body.activity.revision')
    add('inject:anchored-first-revision', 'ws', anchored(7), anchored(999),
        'body.activity.revision')
    add('inject:literal-timeline-revision', 'ws',
        [ws({'kind': 'stream_delta', 'timeline': {'revision': 7}})],
        [ws({'kind': 'stream_delta', 'timeline': {'revision': 999}})], 'body.timeline.revision')
    add('inject:anchored-nonadvancing-revision', 'ws', anchored(1, 2), anchored(1, 2),
        'body.activity.revision')
    malformed = anchored(3, 2)
    malformed[0]['activityBaseline']['activities'][0]['processing'] = True
    add('inject:unsettled-activity-anchor', 'ws', malformed, malformed, 'activityBaseline')
    add('equivalence:independent-activity-anchors', 'ws', anchored(1), anchored(3, 2, UUID2),
        '', 'PASS')
    add('equivalence:unknown-payload-literal', 'ws', [ws({'type': 'unknown', 'revision': 7})],
        [ws({'type': 'unknown', 'revision': 7})], '', 'PASS')
    health_left = http({'timestamp': '2026-10-14T00:00:00Z', 'startedAt': '2026-10-13T23:00:00Z', 'pid': 1}, '/health')
    health_right = http({'timestamp': '2026-10-14T00:00:01Z', 'startedAt': '2026-10-13T23:00:01Z', 'pid': 2}, '/health')
    add('inject:invalid-health-clock', 'http', [http({'timestamp': 7}, '/health')],
        [http({'timestamp': 999}, '/health')], 'body.timestamp')
    add('equivalence:health-clock-valid-etags', 'http', [health_left], [health_right], '', 'PASS')
    weak = http({'ok': True})
    strong = copy.deepcopy(weak)
    strong['headers']['etag'] = strong['headers']['etag'][2:]
    add('inject:etag-strength', 'http', [weak], [strong], 'header.etag')
    add('inject:etag-strength-volatile-body', 'http', [health_left],
        [{**health_right, 'headers': {**health_right['headers'], 'etag': etag(health_right['body'], False)}}],
        'header.etag')
    invalid_left, invalid_right = copy.deepcopy(health_left), copy.deepcopy(health_right)
    invalid_left['headers']['etag'] = invalid_right['headers']['etag'] = 'invalid-but-identical'
    add('inject:identical-invalid-etags-volatile-body', 'http', [invalid_left], [invalid_right],
        'header.etag')
    good = http({'ok': True}, '/api/projects', NOW)
    for label, revision in (('one', '1'), ('unicode', str(NOW).translate(str.maketrans('0123456789', '٠١٢٣٤٥٦٧٨٩'))),
                            ('future', str(NOW + 5000))):
        bad = copy.deepcopy(good)
        bad['headers']['x-projects-revision'] = revision
        add(f'inject:clock-header-{label}', 'http', [good], [bad], 'header.x-projects-revision')
    bad = copy.deepcopy(good)
    bad['headers']['x-projects-revision'] = '1'
    add('inject:both-invalid-clock', 'http', [bad], [bad], 'header.x-projects-revision')
    no_time = copy.deepcopy(good)
    del no_time['requestTimeMs']
    add('inject:clock-without-request-time', 'http', [no_time], [no_time], 'header.x-projects-revision')
    add('inject:clock-wrong-route', 'http', [http({}, '/api/config', 7)],
        [http({}, '/api/config', 999)], 'header.x-projects-revision')
    stale = copy.deepcopy(good)
    stale['headers']['x-projects-revision'] = str(NOW - 1)
    add('inject:cached-old-snapshot', 'http', [good], [stale], 'header.x-projects-revision')
    fresh = copy.deepcopy(good)
    fresh['headers']['x-projects-revision'] = str(NOW + 1)
    add('equivalence:fresh-clock-revision', 'http', [good], [fresh], '', 'PASS')
    return out


def evaluate(comparator, channel, left, right):
    with tempfile.TemporaryDirectory(prefix='exp008-regression-') as directory:
        directories = [Path(directory) / name for name in ('direct', 'proxy')]
        for target, records in zip(directories, (left, right)):
            target.mkdir()
            for kind in compare.CHANNELS:
                compare.write_jsonl(target / f'{kind}.jsonl', records if kind == channel else [])
        return comparator.compare_sides(*directories)


def rust_controls():
    env = dict(os.environ, CARGO_HOME=str(ROOT / '_build/cargo-home'),
               CARGO_TARGET_DIR=str(ROOT / '_build/exp008-target'))
    command = ['cargo', 'test', '--locked', '--offline', '--manifest-path',
               str(ROOT / 'scripts/exp008/proxy/Cargo.toml'), 'positive_controls', '--', '--nocapture']
    result = subprocess.run(command, env=env, cwd=ROOT, capture_output=True, text=True, timeout=180)
    return [{'name': name, 'expect': 'FAIL', 'actual': 'FAIL' if result.returncode == 0 else 'ERROR',
             'before': 'PASS',
             'passed': result.returncode == 0 and f'CONTROL {name} before=PASS after=FAIL safe=PASS' in result.stdout,
             'command': command,
             'evidence': result.stdout + result.stderr}
            for name in ('inject:ws-hop-headers', 'inject:log-queue-overflow')]


def readonly_control(before=None, before_runner=None):
    """Exercise the real readonly assertion with recorded-style fake responses."""
    spec = importlib.util.spec_from_file_location('exp008_runner', ROOT / 'scripts/exp008/run.py')
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)

    def replay(port, method, path, headers=None):
        record = http([], path, NOW if port == runner.PORTS['ui'] else NOW - 1)
        if path != '/api/projects':
            del record['headers']['x-projects-revision']
        if port == runner.PORTS['rust']:
            record['headers']['x-pilotdeck-handler'] = 'ts'
        record['body'] = record['body'].encode()
        return record

    runner.raw_http = replay
    with tempfile.TemporaryDirectory(prefix='exp008-readonly-control-') as directory:
        try:
            runner.scenario_readonly(Path(directory))
        except AssertionError as error:
            caught = 'cached projects revision' in str(error)
            message = str(error)
        else:
            caught, message = False, 'stale replay was accepted'
    result = {'name': 'inject:readonly-cached-snapshot', 'expect': 'FAIL',
              'actual': 'FAIL' if caught else 'PASS', 'passed': caught, 'evidence': message}
    if before is not None and before_runner is not None:
        spec = importlib.util.spec_from_file_location('exp008_old_runner', before_runner)
        old_runner = importlib.util.module_from_spec(spec)
        original = sys.modules['compare']
        try:
            sys.modules['compare'] = before
            spec.loader.exec_module(old_runner)
        finally:
            sys.modules['compare'] = original
        old_runner.raw_http = replay
        with tempfile.TemporaryDirectory(prefix='exp008-before-readonly-') as directory:
            try:
                old = old_runner.scenario_readonly(Path(directory))
                result['before'] = old['status']
            except AssertionError:
                result['before'] = 'FAIL'
        result['passed'] &= result['before'] == 'PASS'
    return result



def run_regressions(before=None, include_rust=False, before_runner=None):
    controls = []
    for name, channel, left, right, location, expected, old_expected in cases():
        report = evaluate(compare, channel, left, right)
        named = any(location in item['location'] for item in report['differences'])
        result = {'name': name, 'expect': expected, 'actual': report['verdict'],
                  'expectedLocation': location, 'namedLocation': named,
                  'differences': report['differences'][:3],
                  'passed': report['verdict'] == expected and (expected == 'PASS' or named)}
        if before is not None:
            old = evaluate(before, channel, left, right)
            result.update(before=old['verdict'], beforeExpected=old_expected)
            result['passed'] &= old['verdict'] == old_expected
        controls.append(result)
    controls.append(readonly_control(before, before_runner))
    if include_rust:
        controls.extend(rust_controls())
    return controls


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before', type=Path)
    parser.add_argument('--before-runner', type=Path, help='old run.py for readonly false-PASS proof')
    parser.add_argument('--rust', action='store_true')
    parser.add_argument('--out', type=Path)
    args = parser.parse_args()
    before = None
    if args.before:
        spec = importlib.util.spec_from_file_location('exp008_before', args.before)
        before = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(before)
    controls = run_regressions(before, args.rust, args.before_runner)
    report = {'verdict': 'PASS' if all(c['passed'] for c in controls) else 'FAIL', 'controls': controls}
    output = json.dumps(report, ensure_ascii=False, indent=2) + '\n'
    if args.out:
        args.out.write_text(output)
    print(output)
    return 0 if report['verdict'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
