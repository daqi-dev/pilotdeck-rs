#!/usr/bin/env python3
"""Read preserved Playwright ZIPs/reports only; no browser, spawn or listen.

Monotonic network timestamps are request starts. Completion estimates are
start + HAR duration; they are not independently captured response events.
Historical trace paths/hashes are evidence, not a dependency of live preflight.
"""
import argparse
import hashlib
import json
from pathlib import Path
import zipfile

ROOT = Path(__file__).resolve().parents[2]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def specs(report):
    def visit(suite):
        yield from suite.get('specs', [])
        for child in suite.get('suites', []):
            yield from visit(child)
    for suite in report['suites']:
        yield from visit(suite)


def inspect_trace(report_path, spec, test):
    attempt = test['results'][0]
    trace = Path(next(a['path'] for a in attempt['attachments'] if a['name'] == 'trace'))
    with zipfile.ZipFile(trace) as archive:
        traces = {name: [json.loads(line) for line in archive.read(name).splitlines()]
                  for name in archive.namelist() if name.endswith('-trace.trace')}
        member, events = next((name, rows) for name, rows in traces.items()
                              if any(e['type'] == 'before' and e.get('method') == 'fill'
                                     and e['params'].get('value') == 'replacement proof' for e in rows))
        network_member = member.replace('.trace', '.network')
        network = [json.loads(line)['snapshot'] for line in archive.read(network_member).splitlines()]
        fill = next(e for e in events if e['type'] == 'before' and e.get('method') == 'fill'
                    and e['params'].get('value') == 'replacement proof')
        fill_end = next(e for e in events if e['type'] == 'after' and e.get('callId') == fill['callId'])
        click = next(e for e in events if e['type'] == 'before' and e.get('method') == 'click'
                     and e['params'].get('selector') == 'internal:role=button[name="Search"s]')
        logs = [e for e in events if e['type'] == 'log' and e.get('callId') == click['callId']]
        runtimes = [{
            'requestStartMs': e['_monotonicTime'],
            'completionEstimateMs': round(e['_monotonicTime'] + e['time'], 3),
            'wallRequestStart': e['startedDateTime'], 'status': e['response']['status'],
        } for e in network if e['request']['url'].endswith('/api/modules/runtime')]
        queries = []
        for e in network:
            if not e['request']['url'].endswith('/api/modules/knowledge/query'):
                continue
            resource = e['response']['content']['_sha1']
            body = json.loads(archive.read('resources/' + resource))
            queries.append({'requestStartMs': e['_monotonicTime'], 'status': e['response']['status'],
                            'errorCode': body['error']['code']})
        loading = [{'name': e['snapshot']['snapshotName'], 'timestampMs': e['snapshot']['timestamp']}
                   for e in events if e['type'] == 'frame-snapshot'
                   and 'Verifying runtime modules before chat is available.' in str(e['snapshot']['html'])
                   and e['snapshot']['timestamp'] >= fill['startTime']]
        error_location = attempt.get('error', {}).get('location')
        if error_location:
            error_location = {**error_location, 'file': Path(error_location['file']).name}
        return {
            'identity': {'file': Path(spec['file']).name, 'title': spec['title']},
            'project': test['projectName'], 'rawStatus': attempt['status'],
            'report': str(report_path.relative_to(ROOT)), 'reportSha256': sha(report_path),
            'trace': str(trace.relative_to(ROOT)), 'traceSha256': sha(trace),
            'traceMember': member, 'networkMember': network_member,
            'fill': {'callId': fill['callId'], 'startMs': fill['startTime'], 'endMs': fill_end['endTime']},
            'searchClick': {'callId': click['callId'], 'startMs': click['startTime'],
                            'disabled': any('locator resolved to <button disabled' in e['message'] for e in logs),
                            'firstNotEnabledMs': next((e['time'] for e in logs if 'element is not enabled' in e['message']), None)},
            'runtimeRequests': runtimes, 'loadingSnapshotsAfterFillStarted': loading,
            'queries': queries, 'errorLocation': error_location,
            'errorContext': str((trace.parent / 'error-context.md').relative_to(ROOT)),
        }


def prove():
    observations, auth = [], []
    for run in ['old2', 'old3']:
        for kind in ['formal', 'auth']:
            path = ROOT / f'_tmp/fe-parity/coordinator-b-{run}/old-replacement-{kind}.json'
            report = json.loads(path.read_text())
            for spec in specs(report):
                if 'replacement' not in spec['title']:
                    continue
                for test in spec['tests']:
                    row = inspect_trace(path, spec, test)
                    (observations if kind == 'formal' else auth).append(row)
    assert len(observations) == 4 and len(auth) == 2
    desktop = next(o for o in observations if 'old3/' in o['report'] and o['project'] == 'desktop')
    assert desktop['rawStatus'] == 'timedOut' and desktop['errorLocation'] is None
    assert desktop['searchClick']['disabled'] and desktop['queries'] == []
    assert desktop['fill']['endMs'] < desktop['runtimeRequests'][2]['requestStartMs']
    assert desktop['loadingSnapshotsAfterFillStarted'][0]['name'] == 'after@' + desktop['fill']['callId']
    for row in [o for o in observations if o is not desktop] + auth:
        assert row['rawStatus'] == 'failed' and not row['searchClick']['disabled']
        assert len(row['queries']) == 1 and row['queries'][0]['status'] == 501
        assert row['queries'][0]['errorCode'] == 'MODULE_IDENTITY_UNAVAILABLE'
        assert row['errorLocation']['line'] == (64 if row in observations else 164)
        assert row['errorLocation']['column'] == 131
    for row in observations:
        if row is not desktop:
            assert max(r['completionEstimateMs'] for r in row['runtimeRequests']) < row['fill']['startMs']
    return {'status': 'PASS', 'scope': 'READ_ONLY_PRESERVED_TRACE_PROOF: not a rerun',
            'timeBasis': 'trace monotonic milliseconds; network start + duration is only a completion estimate',
            'observations': observations, 'authControls': auth}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--verify', action='store_true', help='Require exact reviewed evidence, including file hashes')
    args = parser.parse_args()
    result = prove()
    if args.verify:
        catalog = json.loads((ROOT / 'scripts/fe-parity/baseline-stale.json').read_text())
        reviewed = next(row for row in catalog['tests'] if row.get('oldSideNondeterministic'))
        assert result['observations'] == reviewed['oldSideNondeterminism']['observations'], 'Historical trace evidence drift'
        result['reviewedEvidenceVerified'] = True
    print(json.dumps(result, ensure_ascii=False, indent=2))
