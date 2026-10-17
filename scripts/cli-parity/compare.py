#!/usr/bin/env python3
"""Compare full D0 recordings. Missing evidence never compares equal."""
import argparse
import json
from pathlib import Path
import re
import stat
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import HEAD, SCENARIOS, environment, env_summary, hashes, initial_tree, snapshot

RECORDING_ORIGIN = 'live-os-sandbox'
PROBE_CHECKS = ('homeMatches', 'outsideReadDenied', 'outsideWriteDenied', 'listenDenied',
                'connectDenied', 'childDenied', 'runtimeWritable')


def plain(path, directory=False):
    """Evidence must not redirect reads/copies/mutations to another recording."""
    if path.absolute() != path.resolve():
        raise ValueError(f'evidence path contains symlink or traversal: {path}')
    info = path.lstat()
    if directory:
        if not stat.S_ISDIR(info.st_mode):
            raise ValueError(f'not a directory: {path}')
    elif not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise ValueError(f'not a plain, single-link evidence file: {path}')


def valid_sha(value):
    return isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value) is not None


def coalesce(events):
    out = []
    for row in events:
        channel = row['stream']
        if channel not in ('stdout', 'stderr'):
            raise ValueError('invalid stream')
        data = bytes.fromhex(row['hex'])
        if not data:
            raise ValueError('empty stream event')
        if out and out[-1][0] == channel:
            out[-1] = (channel, out[-1][1] + data)
        else:
            out.append((channel, data))
    return out


def load_round(path):
    errors, cases = [], {}
    def require(condition, rule):
        if not condition:
            errors.append(rule)
    try:
        plain(path, directory=True)
        plain(path / 'round.json')
        plain(path / 'cases', directory=True)
        require({p.name for p in path.iterdir()} == {'round.json', 'cases'}, 'round.artifact-members')
        run = json.loads((path / 'round.json').read_text())
        require(run['schema'] == 1 and run['status'] == 'DONE', 'round.complete')
        require(run['kind'] == 'cli-d0-recording', 'round.kind')
        require(run['origin'] == RECORDING_ORIGIN, 'round.origin')
        require(run['side'] in ('ts', 'rust'), 'round.side')
        require(run['apparatus'] == hashes(), 'round.apparatus')
        ids = [s['id'] for s in SCENARIOS]
        require(run['scenarios'] == ids, 'round.coverage')
        require(sorted(p.name for p in (path / 'cases').iterdir()) == sorted(ids), 'round.case-files')
        isolation = run['isolation']
        require(isolation['status'] == 'PASS' and isolation['errors'] == [] and isolation['exitCode'] == 0,
                'round.isolation')
        require(isolation['audit'] == {'allStopped': True, 'remaining': [], 'errors': []}, 'round.probe-cleanup')
        require(isolation['probe']['status'] == 'PASS' and all(isolation['probe'][key] is True for key in PROBE_CHECKS)
                and isolation['probe']['stdinIsTTY'] is False, 'round.probe')
        require(type(isolation['driverProcess']['pid']) is int and isolation['driverProcess']['pid'] > 0,
                'round.driver-process')
        require(valid_sha(isolation['profileSha256']), 'round.profile')
        provenance = run['provenance']
        require(valid_sha(provenance['executable']['sha256']) and Path(provenance['executable']['path']).is_absolute()
                and re.fullmatch('[0-9a-f]{40}', provenance['sourceHead']) is not None, 'round.provenance')
        baseline = provenance['baseline']
        require(baseline['sourceHead'] == HEAD and valid_sha(baseline['entrySha256'])
                and Path(baseline['path']).is_absolute() and bool(baseline['files'])
                and all(valid_sha(value) for value in baseline['files'].values()), 'round.baseline-provenance')
        if run['side'] == 'ts':
            require(provenance['sourceHead'] == HEAD, 'round.ts-source')
        runtime = Path(run['runtime'])
        require(runtime.is_absolute() and isolation['probe']['osHomedir'] == str(runtime / 'home'), 'round.os-home')
        for scenario in SCENARIOS:
            name = scenario['id']
            folder = path / 'cases' / name
            plain(folder, directory=True)
            require({p.name for p in folder.iterdir()} == {'record.json', 'stdout.bin', 'stderr.bin',
                    'events.json', 'initial.json', 'final.json', 'tree'}, f'{name}.artifact-members')
            for filename in ('record.json', 'stdout.bin', 'stderr.bin', 'events.json', 'initial.json', 'final.json'):
                plain(folder / filename)
            record = json.loads((folder / 'record.json').read_text())
            stdout = (folder / 'stdout.bin').read_bytes()
            stderr = (folder / 'stderr.bin').read_bytes()
            events = json.loads((folder / 'events.json').read_text())
            streams = coalesce(events)
            require(b''.join(data for channel, data in streams if channel == 'stdout') == stdout, f'{name}.stdout.events')
            require(b''.join(data for channel, data in streams if channel == 'stderr') == stderr, f'{name}.stderr.events')
            require(record['status'] == 'DONE' and record['timedOut'] is False, f'{name}.complete')
            require(record['argv'] == scenario['argv'], f'{name}.argv')
            command = [provenance['executable']['path']]
            if run['side'] == 'ts':
                command.append('--disable-warning=ExperimentalWarning')  # normalization.md 第 6 条
                command.append(str(Path(baseline['path']) / 'dist/src/cli/pilotdeck.js'))
            require(record['executedArgv'] == command + scenario['argv'], f'{name}.executed-argv')
            require(record['stdinHex'] == scenario['stdinHex'], f'{name}.stdin')
            require(record['cwd'] == str(runtime / 'project'), f'{name}.cwd')
            require(record['env'] == env_summary(environment(runtime, scenario)), f'{name}.env')
            audit = record['audit']
            require(audit['allStopped'] is True and audit['remaining'] == [] and audit['errors'] == [], f'{name}.cleanup')
            require(type(record['pid']) is int and record['pid'] > 0 and bool(record['processTree'])
                    and all(row['pid'] == row['pgid'] == record['pid'] for row in record['processTree']), f'{name}.process-tree')
            require(type(record['exitCode']) is int, f'{name}.exit-type')
            before = json.loads((folder / 'initial.json').read_text())
            after = snapshot(folder / 'tree')
            require(before == initial_tree(), f'{name}.initial')
            require(after == json.loads((folder / 'final.json').read_text()), f'{name}.disk.manifest')
            if run['side'] == 'ts':
                require(record['exitCode'] == scenario['exit'], f'{name}.oracle.exit')
                for channel, data in (('stdout', stdout), ('stderr', stderr)):
                    if channel in scenario:
                        require(data == scenario[channel].encode(), f'{name}.oracle.{channel}')
                    else:
                        require(data.startswith(scenario[channel + 'Prefix'].encode()) and data.endswith(b'\n'), f'{name}.oracle.{channel}')
                additions = scenario.get('diskAdditions', {})
                require(not (before.keys() & additions.keys()) and after == before | additions,
                        f'{name}.oracle.disk')
            cases[name] = {'stdout': stdout, 'stderr': stderr, 'order': streams,
                           'exitCode': record['exitCode'], 'initial': before, 'disk': after}
        return run, cases, errors
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RuntimeError) as error:
        return {}, cases, errors + [f'evidence.invalid: {error}']


def compare(left, right):
    a, ac, ae = load_round(Path(left))
    b, bc, be = load_round(Path(right))
    failures = [{'side': side, 'rule': rule} for side, errors in (('left', ae), ('right', be)) for rule in errors]
    if not failures:
        if a['side'] != 'ts':
            failures.append({'rule': 'round.reference-must-be-ts'})
        for field in ('runtime', 'apparatus', 'scenarios'):
            if a[field] != b[field]:
                failures.append({'rule': f'round.{field}'})
        if a['provenance']['baseline'] != b['provenance']['baseline']:
            failures.append({'rule': 'round.baseline'})
        for name in ac:
            for field in ac[name]:
                if ac[name][field] != bc[name][field]:
                    failures.append({'rule': f'{name}.{field}'})
    return {'status': 'FAIL' if failures else 'PASS', 'failures': failures,
            'scenarios': len(ac), 'scope': 'D0-pr1 calibration only'}


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('left', type=Path)
    ap.add_argument('right', type=Path)
    args = ap.parse_args()
    result = compare(args.left, args.right)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    raise SystemExit(0 if result['status'] == 'PASS' else 1)
