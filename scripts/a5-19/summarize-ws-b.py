#!/usr/bin/env python3
"""Summarize retained WS observations against matching in-process cases, never recollect."""
import argparse
import hashlib
import json
from pathlib import Path

INPUT_INDEX = Path(__file__).resolve().parents[2] / 'docs/experiments/exp015-a52b-session-fixtures/ws-inputs.json'


def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def project(o):
    assert sorted(c['name'] for c in o['connections']) == ['A', 'B'], 'actual A/B connections required'
    assert len({c['binding']['connectionId'] for c in o['connections']}) == 2, 'binding IDs must be distinct'
    names = {c['binding']['connectionId']: c['name'] for c in o['connections']}
    first = min(c['binding']['generation'] for c in o['connections'])
    def clean(v, path=()):
        if isinstance(v, list):
            return [clean(x, path + (str(i),)) for i, x in enumerate(v)]
        if isinstance(v, dict):
            out = {}
            for k, x in v.items():
                # Only FileRunRegistry wall clock/PID metadata is excluded.
                if ((path[:2] == ('registry', 'records') and len(path) == 3) or
                    (path[:1] == ('connections',) and path[-1:] == ('result',) and
                     {'sessionKey', 'runId', 'state', 'revision', 'lastSeq'} <= v.keys())) and k in ('acceptedAt', 'updatedAt', 'ownerPid'):
                    continue
                if k == 'connectionId' and x in names:
                    x = names[x]
                elif k == 'generation' and 'connectionId' in v and v['connectionId'] in names:
                    x = x - first
                out[k] = clean(x, path + (k,))
            return out
        return v
    return clean(dict(connections=[dict(name=c['name'], frames=c['frames']) for c in o['connections']],
                      durable=[dict(session=d['session'], entries=d['entries'], invalid=d['invalid'], diagnostics=d['fresh']['diagnostics']) for d in o['durable']],
                      registry={k: o['registry'][k] for k in ('records', 'events')},
                      answers=[dict(call=r['call'], delivered=r['delivered']) for r in o['timeline'] if r['kind'] == 'answer.result'],
                      aborts=[dict(input=r['input'], activeRun=r.get('activeRun')) for r in o['timeline'] if r['kind'] == 'abort.input']))


def differences(a, b, path=''):
    if type(a) != type(b):
        return [path]
    if isinstance(a, dict):
        return [p for k in sorted(a.keys() | b.keys()) for p in (differences(a[k], b[k], path + '/' + k) if k in a and k in b else [path + '/' + k])]
    if isinstance(a, list):
        return [path + '/length'] if len(a) != len(b) else [p for i, (x, y) in enumerate(zip(a, b)) for p in differences(x, y, path + '/' + str(i))]
    return [] if a == b else [path]


def read_verified(path, expected):
    raw = path.read_bytes()
    assert hashlib.sha256(raw).hexdigest() == expected, f'historical input changed: {path}'
    return json.loads(raw)


def require_match(result):
    assert result['measurements'] == 75 and len(result['cases']) == 15
    for row in result['cases']:
        assert row['measurements'] == 5 and row['distinct'] == 1, f"unstable: {row['caseId']}"
        assert len(row['mainComparisons']) == len(row['crossRepetitionDifferences']) == 5
        for comparison in row['mainComparisons'] + row['crossRepetitionDifferences']:
            assert not comparison['differences'], f"{row['caseId']}: {comparison}"


def summarize(ws, main, input_index=INPUT_INDEX):
    index = json.loads(input_index.read_bytes())
    assert index['scope'] == 'historical-ws-75' and index['repeats'] == 5
    manifests = {r['role']: r for r in index['manifests']}
    assert len(index['manifests']) == len(manifests) == 2 and set(manifests) == {'ws', 'main'}
    assert all(r['path'] == 'raw/manifest.json' for r in manifests.values())
    manifest = read_verified(ws / 'raw/manifest.json', manifests['ws']['sha256'])
    baseline = read_verified(main / 'raw/manifest.json', manifests['main']['sha256'])
    cases = manifest['expectedCases']
    assert cases == index['cases'] and len(cases) == len(set(cases)) == 15 and manifest['executedCases'] == cases
    assert manifest['measurements'] == 75 and manifest['repeats'] == 5
    assert manifest['preregistrationSha256'] == baseline['preregistrationSha256']
    def sources(m):
        rows = m['importedSources']
        result = {v['path']: v['sha256'] for v in rows}
        assert result and len(result) == len(rows), 'source corpus must be nonempty and unique'
        return result
    wsrc, msrc = sources(manifest), sources(baseline)
    assert msrc.keys() <= wsrc.keys() and all(wsrc[k] == msrc[k] for k in msrc)
    assert index['launch']['path'] == 'launch.json'
    launch = read_verified(ws / 'launch.json', index['launch']['sha256'])
    assert launch['exitCode'] == 0 and launch['reaped'] and launch['ownsOnlySpawnedChild']
    inputs = {(r['role'], r['caseId'], r['repetition']): r for r in index['observations']}
    expected = {(role, c, r) for role in ('ws', 'main') for c in cases for r in range(1, 6)}
    assert len(index['observations']) == len(inputs) == 150 and inputs.keys() == expected
    rows = []
    for c in cases:
        signatures = []
        matches = []
        for r in range(1, 6):
            wp = ws / 'raw' / c / str(r) / 'observation.json'
            mp = main / 'raw/measurements' / c / str(r) / 'observation.json'
            observations = []
            for role, path, root in [('ws', wp, ws), ('main', mp, main)]:
                proof = inputs[(role, c, r)]
                assert proof['path'] == str(path.relative_to(root)), 'index path must match the selected case'
                observations.append(read_verified(path, proof['sha256']))
                assert sha(path.with_name('timeline.ndjson')) == proof['journalSha256'], f'journal changed: {path}'
            wo, mo = observations
            assert wo['caseId'] == mo['caseId'] == c and wo['repetition'] == mo['repetition'] == r
            assert wo['expectedConnections'] == mo['expectedConnections'] == ['A', 'B']
            signatures.append(project(wo))
            matches.append(dict(repetition=r, differences=differences(project(mo), signatures[-1])))
        rows.append(dict(caseId=c, measurements=5, distinct=len({json.dumps(s, sort_keys=True) for s in signatures}),
                         crossRepetitionDifferences=[dict(repetition=i+1, differences=differences(signatures[0], s)) for i, s in enumerate(signatures)],
                         mainComparisons=matches))
    assert len(list((ws / 'raw').rglob('observation.json'))) == 75
    result = dict(measurements=75, cases=rows,
                inputIndex=dict(path=str(input_index.relative_to(Path(__file__).resolve().parents[2]))
                                if input_index.is_relative_to(Path(__file__).resolve().parents[2]) else str(input_index),
                                sha256=sha(input_index)),
                process=dict(pid=launch['pid'], exitCode=launch['exitCode'], reaped=True, ownsOnlySpawnedChild=True,
                             note='75 observations in one launcher-owned Node process; not 75 distinct processes'),
                historicalApparatus='Historical WS manifest has no fixtureFiles: no claim of current HEAD identity',
                normalization='Only registry record acceptedAt/updatedAt/ownerPid and actual binding connectionId/generation mapping; other fields retained')
    require_match(result)
    result['status'] = 'PASS'
    return result


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--ws', type=Path, required=True)
    p.add_argument('--main', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--input-index', type=Path, default=INPUT_INDEX)
    a = p.parse_args()
    result = summarize(a.ws, a.main, a.input_index)
    a.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(dict(measurements=75, stableCases=sum(r['distinct'] == 1 for r in result['cases']),
                          matchingCases=sum(all(not v['differences'] for v in r['mainComparisons']) for r in result['cases']))))
