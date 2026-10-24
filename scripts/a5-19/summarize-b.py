#!/usr/bin/env python3
"""Bind the three completed, separately collected reports to retained raw evidence."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def reference(path):
    return {'path': str(path.relative_to(ROOT)), 'sha256': digest(path)}


def report(main_dir, side_dir, old_dir):
    main_path, side_path, old_path = [p / 'raw/manifest.json' for p in (main_dir, side_dir, old_dir)]
    main, side, old = [json.loads(p.read_text()) for p in (main_path, side_path, old_path)]
    if main['expectedCases'] != main['executedCases'] or main['measurements'] != 170 or side['measurements'] != 20 or old['totalMeasurements'] != 360:
        raise ValueError('COVERAGE: incomplete collected manifest')
    # Refuse to attach stale measurements to a newer apparatus. This also
    # makes coordinator recollection after registration commit reviewable.
    for manifest in (main, side, old):
        for item in manifest['fixtureFiles']:
            if digest(ROOT / item['path']) != item['sha256']:
                raise ValueError(f"apparatus changed since collection: {item['path']}")
    for directory in (main_dir, side_dir, old_dir):
        if json.loads((directory / 'launch.json').read_text())['exitCode'] != 0:
            raise ValueError(f'collection did not exit successfully: {directory}')
    evidence = []
    for path in sorted((main_dir / 'raw/measurements').rglob('observation.json')):
        observation = json.loads(path.read_text())
        evidence.append({'caseId': observation['caseId'], 'repetition': observation['repetition'],
                         'observation': reference(path), 'journal': reference(path.with_name('timeline.ndjson')),
                         'storage': [reference(Path(d['path'])) for d in observation['durable'] if Path(d['path']).exists()],
                         'registry': reference(Path(observation['registry']['path'])) if observation['registry']['applicable'] else observation['registry']['reason']})
    side_evidence = []
    for path in sorted((side_dir / 'raw').glob('*/observation.json')):
        observation = json.loads(path.read_text())
        terminal = observation['sidecarProof']['unknownEntries'][0]
        side_evidence.append({'repetition': observation['repetition'], 'observation': reference(path),
                              'journal': reference(path.with_name('timeline.ndjson')),
                              'childLoadedFiles': len(observation['childLoaded']), 'unknownOutcome': terminal['outcome'],
                              'unknownCode': terminal['code'], 'lastAppliedSequence': terminal['lastAppliedSequence'],
                              'statuses': [e['event'] for e in observation['durable'][0]['entries'] if e['type'] == 'agent_status_message'],
                              'realFrames': observation['realtimeFrames'],
                              'spawnAndExit': [r for r in observation['timeline'] if r['kind'] in ('sidecar.spawn', 'sidecar.exit')]})
    if len(evidence) != 170 or len(side_evidence) != 20:
        raise ValueError('COVERAGE: missing observation files')
    validation = ROOT / '_tmp/a52b/validation'
    tests = (validation / 'test.log').read_text()
    totals = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored', tests)
    test_counts = dict(zip(('passed', 'failed', 'ignored'), [sum(int(t[i]) for t in totals) for i in range(3)]))
    ledger = json.loads((validation / 'ledger.log').read_text())
    return {'baselineCommit': main['baselineCommit'], 'worktreeCommit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
            'node': main['node'], 'scope': 'TS-only controlled observations; NOT full E1; no approved differences',
            'preregistration': {'files': [reference(ROOT / 'scripts/a5-19/preregistration-b.mjs'), reference(ROOT / 'scripts/a5-19/preregistration-b-extra.mjs'), reference(ROOT / 'scripts/a5-19/rules-b.mjs')],
                                'commitStatus': main['preregistrationCommit']},
            'main': {'manifest': reference(main_path), 'expectedCases': main['expectedCases'], 'executedCases': main['executedCases'],
                     'repeats': main['repeats'], 'measurements': main['measurements'], 'unstableGroups': main['unstableGroups'],
                     'summary': [{k: v for k, v in row.items() if k != 'alternatives' or not row['stable']} for row in main['summaries']],
                     'controls': main['controls'], 'evidence': evidence},
            'controlEvidence': [reference(p) for p in sorted((main_dir / 'raw/controls').rglob('*')) if p.is_file()],
            'sidecar': {'manifest': reference(side_path), 'caseId': side['caseId'], 'measurements': side['measurements'],
                        'stable': side['stable'], 'distinct': side['distinct'], 'evidence': side_evidence},
            'a52aRegression': {'manifest': reference(old_path), 'measurements': old['totalMeasurements'], 'repeats': old['repeats'],
                              'unstableGroups': old['unstableGroups'], 'controls': old['checks']['observerControls']},
            'apparatus': [reference(p) for p in sorted((ROOT / 'scripts/a5-19').iterdir()) if p.is_file() and
                          (p.stem.endswith('-b') or p.name in ('no-listen-child.mjs', 'preregistration-b-extra.mjs'))],
            'external': {'cold': 'NOT_RUN: run-external-b.py --coordinator --suite cold, 2 cases x5',
                         'ws': 'NOT_RUN: run-external-b.py --coordinator --suite ws, 15 cases x5, high loopback port only',
                         'bridge': 'NOT_RUN: separate real UI/D5 evidence required'},
            'verification': {'checks': json.loads((validation / 'checks.json').read_text()), 'clippyLog': reference(validation / 'clippy-local-cache.log'),
                             'workspaceTests': test_counts, 'testLog': reference(validation / 'test.log'),
                             'ledger': {'status': ledger['status'], 'selfTests': len(ledger['negative_self_tests']), 'log': reference(validation / 'ledger.log')},
                             'schemaLog': reference(validation / 'schema.log')},
            'totalMeasurements': main['measurements'] + side['measurements'] + old['totalMeasurements'],
            'totalControls': len(main['controls']) + len(old['checks']['observerControls'])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--main', type=Path, required=True)
    parser.add_argument('--sidecar', type=Path, required=True)
    parser.add_argument('--a52a', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    data = report(args.main.resolve(), args.sidecar.resolve(), args.a52a.resolve())
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'measurements': data['totalMeasurements'], 'controls': data['totalControls'], 'report': str(args.report)}))


if __name__ == '__main__':
    main()
