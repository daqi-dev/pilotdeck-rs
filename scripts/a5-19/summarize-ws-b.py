#!/usr/bin/env python3
"""Summarize retained WS observations against matching in-process cases, never recollect."""
import argparse
import hashlib
import json
from pathlib import Path


def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def project(o):
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


def summarize(ws, main):
    manifest = json.loads((ws / 'raw/manifest.json').read_text())
    baseline = json.loads((main / 'raw/manifest.json').read_text())
    cases = manifest['expectedCases']
    assert len(cases) == len(set(cases)) == 15 and manifest['executedCases'] == cases
    assert manifest['measurements'] == 75 and manifest['repeats'] == 5
    assert manifest['preregistrationSha256'] == baseline['preregistrationSha256']
    sources = lambda m: {v['path']: v['sha256'] for v in m['importedSources']}
    wsrc, msrc = sources(manifest), sources(baseline)
    assert all(wsrc[k] == msrc[k] for k in wsrc.keys() & msrc.keys())
    launch = json.loads((ws / 'launch.json').read_text())
    assert launch['exitCode'] == 0 and launch['reaped'] and launch['ownsOnlySpawnedChild']
    rows, provenance = [], []
    for c in cases:
        signatures = []
        matches = []
        for r in range(1, 6):
            wp = ws / 'raw' / c / str(r) / 'observation.json'
            mp = main / 'raw/measurements' / c / str(r) / 'observation.json'
            wo, mo = [json.loads(p.read_text()) for p in (wp, mp)]
            assert wo['caseId'] == mo['caseId'] == c and wo['repetition'] == mo['repetition'] == r
            assert wo['expectedConnections'] == ['A', 'B']
            signatures.append(project(wo))
            matches.append(dict(repetition=r, differences=differences(project(mo), signatures[-1])))
            for p in (wp, mp):
                provenance.append(dict(path=str(p), sha256=sha(p), journalSha256=sha(p.with_name('timeline.ndjson'))))
        rows.append(dict(caseId=c, measurements=5, distinct=len({json.dumps(s, sort_keys=True) for s in signatures}),
                         crossRepetitionDifferences=[dict(repetition=i+1, differences=differences(signatures[0], s)) for i, s in enumerate(signatures)],
                         mainComparisons=matches))
    assert len(list((ws / 'raw').rglob('observation.json'))) == 75
    return dict(measurements=75, cases=rows, provenance=provenance,
                manifests=[dict(path=str(p), sha256=sha(p)) for p in (ws / 'raw/manifest.json', main / 'raw/manifest.json')],
                process=dict(pid=launch['pid'], exitCode=launch['exitCode'], reaped=True, ownsOnlySpawnedChild=True,
                             note='75 observations in one launcher-owned Node process; not 75 distinct processes'),
                historicalApparatus='Historical WS manifest has no fixtureFiles: no claim of current HEAD identity',
                normalization='Only registry record acceptedAt/updatedAt/ownerPid and actual binding connectionId/generation mapping; other fields retained')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--ws', type=Path, required=True)
    p.add_argument('--main', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    result = summarize(a.ws, a.main)
    a.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(dict(measurements=75, stableCases=sum(r['distinct'] == 1 for r in result['cases']),
                          matchingCases=sum(all(not v['differences'] for v in r['mainComparisons']) for r in result['cases']))))
