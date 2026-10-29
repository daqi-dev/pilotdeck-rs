#!/usr/bin/env python3
"""Compare full D0 recordings. Missing evidence never compares equal."""
import sys
if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit(f'#701: os must be preloaded from CPython 3.11+ frozen modules; run with -I: python3 -I {__file__}')
import os

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before importing anything: a same-named stdlib
# file beside this entry (json.py, pathlib.py, tempfile.py) would otherwise
# execute before the guard. Discover can add the directory back even with
# -I, so reject that path too. ``sys`` is builtin; ``os`` is imported only
# after the preflight above proved it is a preloaded frozen module, so neither
# can be shadowed from the reinserted directory.
# The directory is identified by (device, inode), not by its spelling: a
# case-insensitive alias or a mixed ``//``/symlink form names this very
# directory while the raw string differs (fix-r2 GLM sibling P1).
_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_reinserted = any(
    os.path.isdir(entry or os.curdir)
    and os.path.samefile(entry or os.curdir, _boot_dir)
    for entry in sys.path)
if not sys.flags.isolated or _boot_reinserted:
    raise SystemExit(f'#701: unsafe import path; run with -I by file: python3 -I {__file__}')

import importlib.util
from pathlib import Path

# The bootstrap itself is also loaded from source by its exact sibling path.
_bootstrap_path = Path(__file__).resolve().with_name('_bootstrap.py')
_bootstrap_spec = importlib.util.spec_from_file_location('_cli_parity_bootstrap', _bootstrap_path)
_bootstrap = importlib.util.module_from_spec(_bootstrap_spec)
exec(compile(_bootstrap_path.read_bytes(), str(_bootstrap_path), 'exec'), _bootstrap.__dict__)
_load = _bootstrap.load

import argparse
import hashlib
import json
import re
import stat

_here = Path(__file__).resolve().parent
_common = _load('common', _here / 'common.py')
Blocked = _common.Blocked
HEAD = _common.HEAD
MOCK_PORT = _common.MOCK_PORT
OPENSSL_CONFIG = _common.OPENSSL_CONFIG
SCRIPT = _common.SCRIPT
SCENARIOS = _common.SCENARIOS
TOKEN = _common.TOKEN
digest_plain = _common.digest_plain
read_plain = _common.read_plain
environment = _common.environment
env_summary = _common.env_summary
hashes = _common.hashes
initial_tree = _common.initial_tree
mock_gateway_scenarios = _common.mock_gateway_scenarios
per_stream_scenarios = _common.per_stream_scenarios
snapshot = _common.snapshot
staged_skills_identity = _common.staged_skills_identity
sandbox_profile = _load('policy', _here / 'policy.py').sandbox_profile

RECORDING_ORIGIN = 'live-os-sandbox'
PENDING = 'PENDING-TS-CALIBRATION'
PROBE_CHECKS = ('homeMatches', 'outsideReadDenied', 'outsideWriteDenied', 'listenDenied',
                'connectDenied', 'childDenied', 'runtimeWritable', 'outsideMetadataDenied', 'privateReadDenied')
# #933: the relaxed mock profile must prove these five facts about itself.
GATEWAY_PROBE_CHECKS = ('connectAllowed', 'connectDefaultGatewayDenied', 'connectOtherDenied',
                        'connectUnixDenied', 'listenDenied')
# The probe reports the kernel error code for each denial; only the allowed
# port is a boolean.
GATEWAY_DENIAL_CODES = ('EPERM', 'EACCES')
SQLITE_WARNING = re.compile(
    rb'\A\(node:[0-9]+\) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n'
    rb'\(Use `node --trace-warnings \.\.\.` to show where the warning was created\)\n')


def comparison_view(side, stderr, streams):
    """Strip one exact TS stderr prefix; raw files/events stay untouched."""
    match = SQLITE_WARNING.match(stderr) if side == 'ts' else None
    if not match:
        return stderr, streams
    remaining, events = match.end(), []
    for channel, data in streams:
        if channel == 'stderr' and remaining:
            consumed = min(remaining, len(data))
            data, remaining = data[consumed:], remaining - consumed
        if data:
            events.append({'stream': channel, 'hex': data.hex()})
    return stderr[match.end():], coalesce(events)


def expected_stdout(scenario, runtime):
    """Instantiate recorded oracle inputs; never rewrite observed output bytes."""
    text = scenario['stdout']
    if 'stdoutRuntime' not in scenario:
        return text.encode()
    encoding = scenario['stdoutRuntime']
    if encoding not in ('json', 'text') or '{runtime}' not in text:
        raise ValueError('invalid stdout runtime binding')
    if 'stdoutSha256' in scenario or 'stdoutPrefix' in scenario:
        raise ValueError('runtime binding requires only a full stdout oracle')
    value = str(runtime)
    if encoding == 'json':
        value = json.dumps(value, ensure_ascii=False)[1:-1]
    return text.replace('{runtime}', value).encode()


def pair_failures(ac, bc, per_stream):
    return [{'rule': f'{name}.{field}'} for name in ac.keys() & bc.keys()
            for field in ac[name] if not (field == 'order' and name in per_stream)
            and ac[name][field] != bc[name][field]]


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
        per_stream_scenarios(SCENARIOS)
        mock_ids = mock_gateway_scenarios(SCENARIOS)
        plain(path, directory=True)
        plain(path / 'cases', directory=True)
        require({p.name for p in path.iterdir()} == {'round.json', 'cases'}, 'round.artifact-members')
        run = json.loads(read_plain(path / 'round.json'))
        require(run['schema'] == 1 and run['status'] == 'DONE', 'round.complete')
        require(run['kind'] == 'cli-d0-recording', 'round.kind')
        require(run['origin'] == RECORDING_ORIGIN, 'round.origin')
        require(run['side'] in ('ts', 'rust'), 'round.side')
        require(run['apparatus'] == hashes(), 'round.apparatus')
        ids = [s['id'] for s in SCENARIOS]
        require(ids, 'round.empty-registry')  # 空场景表不能产生 PASS（gate-final boundary P1）
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
        require(set(baseline['trees']) == {'src', 'dist', 'packages', 'node_modules', 'skills'}
                and all(valid_sha(value) for value in baseline['trees'].values()), 'round.baseline-trees')
        require(provenance['side'] == run['side'], 'round.provenance-side')
        require(provenance['opensslConfig'] == OPENSSL_CONFIG, 'round.openssl-config')
        node = provenance['node']
        require(valid_sha(node['sha256']) and Path(node['path']).is_absolute(), 'round.node')
        if run['side'] == 'ts':
            require(provenance['sourceHead'] == HEAD and provenance['executable'] == node
                    and provenance['sourcePath'] == baseline['path'], 'round.ts-source')
        else:
            require(Path(provenance['sourcePath']).is_absolute()
                    and provenance['sourcePath'] != baseline['path']
                    and provenance['executable']['path'] != node['path']
                    and provenance['executable']['sha256'] != node['sha256'], 'round.rust-source')
        runtime = Path(run['runtime'])
        require(runtime.is_absolute() and isolation['probe']['osHomedir'] == str(runtime / 'home'), 'round.os-home')
        executed = provenance['executable']
        if run['side'] == 'rust':
            # The run uses the staged copy (run_round.stage_rust); identity stays the original.
            executed = provenance['stagedExecutable']
            require(executed == {'path': str(runtime.parent / 'rust-stage/bin/pilotdeck'),
                                 'sha256': provenance['executable']['sha256']}, 'round.rust-stage')
            # Offline re-validation reads the copy's actual bytes, not only the recorded hash.
            try:
                staged = Path(executed['path'])
                on_disk = digest_plain(staged)
            except (OSError, ValueError):
                on_disk = None
            require(on_disk == provenance['executable']['sha256'], 'round.rust-stage-bytes')
            skills = provenance.get('stagedSkills', {})
            skills_path = runtime.parent / 'rust-stage/skills'
            require(isinstance(skills, dict) and set(skills) == {'path', 'sha256'}
                    and skills['path'] == str(skills_path) and valid_sha(skills['sha256']),
                    'round.rust-stage-skills')
            try:
                on_disk_skills = staged_skills_identity(skills_path)
            except (OSError, ValueError):
                on_disk_skills = None
            require(on_disk_skills is not None and on_disk_skills == skills, 'round.rust-stage-skills-bytes')
        policy = sandbox_profile(runtime, Path(baseline['path']), executed['path'], run['side'])
        require(isolation['profileText'] == policy and isolation['profileSha256'] == hashlib.sha256(policy.encode()).hexdigest(),
                'round.profile-binding')
        probe_policy = sandbox_profile(runtime, Path(baseline['path']), node['path'], 'ts')
        require(isolation['probeProfileSha256'] == hashlib.sha256(probe_policy.encode()).hexdigest(), 'round.probe-profile')
        # #933 mock-gateway evidence: present exactly when the registry marks scenarios.
        mock = run.get('mockIsolation')
        require((mock is not None) == bool(mock_ids), 'round.mock-gateway-presence')
        if mock is not None:
            require(isinstance(mock, dict) and mock['status'] == 'PASS' and mock['errors'] == []
                    and mock['exitCode'] == 0, 'round.mock-isolation')
            mock_policy = sandbox_profile(runtime, Path(baseline['path']), executed['path'], run['side'], mock_gateway=True)
            require(mock['profileText'] == mock_policy
                    and mock['profileSha256'] == hashlib.sha256(mock_policy.encode()).hexdigest(), 'round.mock-profile-binding')
            mock_probe_policy = sandbox_profile(runtime, Path(baseline['path']), node['path'], 'ts', mock_gateway=True)
            require(mock['probeProfileSha256'] == hashlib.sha256(mock_probe_policy.encode()).hexdigest(), 'round.mock-probe-profile')
            probe = mock['gatewayProbe']
            require(probe.get('connectAllowed') is True
                    and all(probe.get(key) in GATEWAY_DENIAL_CODES
                            for key in GATEWAY_PROBE_CHECKS[1:]), 'round.gateway-probe')
            require(mock['audit'] == {'allStopped': True, 'remaining': [], 'errors': []}
                    and mock.get('probeCleanup') == {'unixSocketRemoved': True}, 'round.mock-probe-cleanup')
            process = mock['mockProcess']
            require(type(process['pid']) is int and process['pid'] > 0
                    and process['argv'] == [node['path'], str(SCRIPT / 'mock_gateway.mjs'), str(MOCK_PORT)]
                    and process['token'] == {'redacted': True, 'length': len(TOKEN),
                                             'sha256': hashlib.sha256(TOKEN.encode()).hexdigest()}, 'round.mock-process')
            if run['side'] == 'rust':
                denial = mock['nodeExec']
                require(denial['denied'] is True and denial['status'] == 'DONE' and denial['timedOut'] is False
                        and type(denial['exitCode']) is int and denial['exitCode'] != 0
                        and denial['stdoutHex'] == ''
                        and b'execvp' in bytes.fromhex(denial['stderrHex'])
                        and any(s in bytes.fromhex(denial['stderrHex']) for s in (b'Operation not permitted', b'Permission denied'))
                        and denial['audit'] == {'allStopped': True, 'remaining': [], 'errors': []}, 'round.mock-node-exec')
        if run['side'] == 'rust':
            denial = isolation['nodeExec']
            require(denial['denied'] is True and denial['status'] == 'DONE' and denial['timedOut'] is False
                    and type(denial['exitCode']) is int and denial['exitCode'] != 0
                    and denial['stdoutHex'] == ''
                    and b'execvp' in bytes.fromhex(denial['stderrHex'])
                    and any(s in bytes.fromhex(denial['stderrHex']) for s in (b'Operation not permitted', b'Permission denied'))
                    and denial['audit'] == {'allStopped': True, 'remaining': [], 'errors': []}, 'round.rust-node-exec')
        for scenario in SCENARIOS:
            name = scenario['id']
            folder = path / 'cases' / name
            plain(folder, directory=True)
            require({p.name for p in folder.iterdir()} == {'record.json', 'stdout.bin', 'stderr.bin',
                    'events.json', 'initial.json', 'final.json', 'tree'}, f'{name}.artifact-members')
            record = json.loads(read_plain(folder / 'record.json'))
            stdout = read_plain(folder / 'stdout.bin')
            stderr = read_plain(folder / 'stderr.bin')
            events = json.loads(read_plain(folder / 'events.json'))
            streams = coalesce(events)
            require(b''.join(data for channel, data in streams if channel == 'stdout') == stdout, f'{name}.stdout.events')
            require(b''.join(data for channel, data in streams if channel == 'stderr') == stderr, f'{name}.stderr.events')
            stderr, streams = comparison_view(run['side'], stderr, streams)
            require(record['status'] == 'DONE' and record['timedOut'] is False, f'{name}.complete')
            require(record['argv'] == scenario['argv'], f'{name}.argv')
            command = [executed['path']]
            if run['side'] == 'ts':
                command.append(str(Path(baseline['path']) / 'dist/src/cli/pilotdeck.js'))
            require(record['executedArgv'] == command + scenario['argv'], f'{name}.executed-argv')
            binding = mock if (mock is not None and scenario['id'] in mock_ids) else isolation
            require(record['sandboxArgv'] == ['/usr/bin/sandbox-exec', '-f', binding['profilePath'], *command, *scenario['argv']]
                    and record['profileSha256'] == binding['profileSha256'], f'{name}.sandbox-binding')
            require(record['stdinHex'] == scenario['stdinHex'], f'{name}.stdin')
            require(record['cwd'] == str(runtime / 'project'), f'{name}.cwd')
            require(record['env'] == env_summary(environment(runtime, scenario)), f'{name}.env')
            audit = record['audit']
            require(audit['allStopped'] is True and audit['remaining'] == [] and audit['errors'] == [], f'{name}.cleanup')
            require(type(record['pid']) is int and record['pid'] > 0 and bool(record['processTree'])
                    and all(row['pid'] == row['pgid'] == record['pid'] for row in record['processTree']), f'{name}.process-tree')
            require(type(record['exitCode']) is int, f'{name}.exit-type')
            before = json.loads(read_plain(folder / 'initial.json'))
            after = snapshot(folder / 'tree')
            require(before == initial_tree(scenario), f'{name}.initial')
            require(after == json.loads(read_plain(folder / 'final.json')), f'{name}.disk.manifest')
            if run['side'] == 'ts' and scenario.get('oracle') == PENDING:
                # Registered but not yet calibrated from a real TS round: never a PASS.
                require(False, f'{name}.oracle.pending')
            elif run['side'] == 'ts':
                require(record['exitCode'] == scenario['exit'], f'{name}.oracle.exit')
                for channel, data in (('stdout', stdout), ('stderr', stderr)):
                    if channel in scenario or (channel == 'stdout' and 'stdoutRuntime' in scenario):
                        expected = expected_stdout(scenario, runtime) if channel == 'stdout' else scenario[channel].encode()
                        require(data == expected, f'{name}.oracle.{channel}')
                    else:
                        require(data.startswith(scenario[channel + 'Prefix'].encode()) and data.endswith(b'\n'), f'{name}.oracle.{channel}')
                    if channel + 'Sha256' in scenario:
                        require(hashlib.sha256(data).hexdigest() == scenario[channel + 'Sha256'], f'{name}.oracle.{channel}')
                additions = scenario.get('diskAdditions', {})
                removals = scenario.get('diskRemovals', [])
                retained = {key: value for key, value in before.items() if key not in removals}
                require(isinstance(removals, list) and len(set(removals)) == len(removals)
                        and set(removals) <= before.keys() and '.' not in removals
                        and not (retained.keys() & additions.keys()) and after == retained | additions,
                        f'{name}.oracle.disk')
            cases[name] = {'stdout': stdout, 'stderr': stderr, 'order': streams,
                           'exitCode': record['exitCode'], 'initial': before, 'disk': after}
        return run, cases, errors
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RuntimeError) as error:
        return {}, cases, errors + [f'evidence.invalid: {error}']


def compare(left, right, expected_rust=None):
    try:
        per_stream = per_stream_scenarios(SCENARIOS)
    except Blocked as error:
        return {'status': 'BLOCKED', 'failures': [{'rule': 'apparatus.stream-order', 'error': str(error)}],
                'scenarios': 0, 'scope': 'D0-pr1 calibration only'}
    try:
        mock_gateway_scenarios(SCENARIOS)
    except Blocked as error:
        return {'status': 'BLOCKED', 'failures': [{'rule': 'apparatus.mock-gateway', 'error': str(error)}],
                'scenarios': 0, 'scope': 'D0-pr1 calibration only'}
    a, ac, ae = load_round(Path(left))
    b, bc, be = load_round(Path(right))
    failures = [{'side': side, 'rule': rule} for side, errors in (('left', ae), ('right', be)) for rule in errors]
    if a and b:
        if a['side'] != 'ts':
            failures.append({'rule': 'round.reference-must-be-ts'})
        for field in ('runtime', 'apparatus', 'scenarios'):
            if a[field] != b[field]:
                failures.append({'rule': f'round.{field}'})
        if a['provenance']['baseline'] != b['provenance']['baseline']:
            failures.append({'rule': 'round.baseline'})
        if a['provenance']['node'] != b['provenance']['node']:
            failures.append({'rule': 'round.node'})
        if b['side'] == 'rust':
            observed = {key: b['provenance'].get(key) for key in ('side', 'executable', 'sourcePath', 'sourceHead')}
            if expected_rust is None or observed != expected_rust:
                failures.append({'rule': 'round.rust-binding'})
        elif expected_rust is not None:
            failures.append({'rule': 'round.expected-rust-side'})
        # Keep testing pair semantics even when the independent TS oracle fails.
        # Real negative controls must exercise both guards, not only the oracle.
        failures.extend(pair_failures(ac, bc, per_stream))
    return {'status': 'FAIL' if failures else 'PASS', 'failures': failures,
            'scenarios': len(ac), 'scope': 'D0-pr1 calibration only'}


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('left', type=Path)
    ap.add_argument('right', type=Path)
    ap.add_argument('--rust-bin', type=Path)
    ap.add_argument('--rust-source', type=Path)
    args = ap.parse_args()
    if bool(args.rust_bin) != bool(args.rust_source):
        ap.error('--rust-bin and --rust-source must be supplied together')
    if args.rust_bin:
        expected = _load('run_round', _here / 'run_round.py').rust_identity(
            args.rust_bin, args.rust_source)
    else:
        expected = None
    result = compare(args.left, args.right, expected)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    raise SystemExit({'PASS': 0, 'FAIL': 1, 'BLOCKED': 2}[result['status']])
