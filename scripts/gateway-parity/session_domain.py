#!/usr/bin/env python3
"""E1 draft apparatus. Calibration is TS-only; A7 native execution is NOT_RUN.

No A2 proxy round is relabelled native. Native audit functions below consume
independent launcher/exec/network evidence; their mutation tests are apparatus
tests, never evidence that a Rust host has run or retired the TS host.
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from common import ROOT, SCRIPT, apparatus_hashes, digest, install_shutdown, parity_lock, save
from compare import wire_frame
from observer import Decoder

# 显式 --baseline > PDRS_BASELINE > 本 checkout 的 resources/PilotDeck（#187：不再写死主仓绝对路径）
BASELINE = Path(os.environ.get('PDRS_BASELINE') or ROOT / 'resources/PilotDeck')
BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
CELLS = ('no-registry-fast', 'no-registry-slow', 'no-registry-failure',
         'registry-fast', 'registry-slow', 'registry-failure')
MATRIX = tuple((host, sidecar) for host in ('ts', 'rust') for sidecar in ('ts', 'rust'))
REPEATS = 5
FIXTURE_NAMES = ('run.py', 'six_cells.mts', 'observation_rules.mjs',
                 'append-observer.mjs', 'cleanup-observer.mjs')


class EvidenceError(ValueError):
    def __init__(self, rule, detail):
        super().__init__(detail)
        self.rule = rule


def require(condition, rule, detail):
    if not condition:
        raise EvidenceError(rule, detail)


def read(path):
    return json.loads(Path(path).read_text())


def hashes():
    # E1 includes its transitive observation rules and the reused A2 primitives.
    paths = [SCRIPT / name for name in ('session_domain.py', 'session_observations.mjs',
             'controls/session_domain.py', 'fixtures/session-calibration.json',
             'common.py', 'compare.py', 'driver.mts', 'observer.py')]
    paths += [ROOT / 'scripts/a5-19' / name for name in FIXTURE_NAMES]
    return {**apparatus_hashes(), **{p.relative_to(ROOT).as_posix(): digest(p) for p in paths}}


def frozen_check():
    process = subprocess.run([sys.executable, str(SCRIPT / 'check_frozen.py')],
                             capture_output=True, text=True, timeout=15)
    return {'exitCode': process.returncode, 'stdout': process.stdout, 'stderr': process.stderr}


def process_tree(root_pid):
    """Read only our descendants. An inaccessible census is not an empty tree."""
    try:
        result = subprocess.run(['ps', '-axo', 'pid=,ppid=,command='],
                                capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError) as error:
        return {'status': 'BLOCKED', 'reason': str(error), 'processes': []}
    if result.returncode:
        return {'status': 'BLOCKED', 'reason': result.stderr.strip(), 'processes': []}
    rows = []
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) == 3:
            rows.append({'pid': int(fields[0]), 'ppid': int(fields[1]), 'argvText': fields[2]})
    owned = {root_pid}
    while True:
        children = {row['pid'] for row in rows if row['ppid'] in owned}
        if children <= owned:
            break
        owned |= children
    return {'status': 'SNAPSHOT_ONLY', 'processes': [r for r in rows if r['pid'] in owned],
            'limitation': 'sampling cannot prove absence of short-lived helpers; native requires exec witness'}


def run_fixture(output, baseline):
    command = [sys.executable, str(ROOT / 'scripts/a5-19/run.py'),
               '--baseline', str(baseline), '--output', str(output / 'six-cells')]
    audit = {'command': command, 'cwd': str(ROOT), 'launcherPid': os.getpid(),
             'processSnapshots': [], 'status': 'RUNNING', 'launcherAndFixtureReaped': False,
             'processTreeCleanup': 'NOT_VERIFIED'}
    save(output / 'launch.json', audit)
    child = None
    try:
        with (output / 'launcher.log').open('w') as log:
            child = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
                                     start_new_session=True)
            audit['pid'] = child.pid
            audit['pgid'] = child.pid
            deadline = time.monotonic() + 1250
            while child.poll() is None:
                if not audit['processSnapshots'] or audit['processSnapshots'][-1]['status'] != 'BLOCKED':
                    audit['processSnapshots'].append(process_tree(child.pid))
                if time.monotonic() >= deadline:
                    raise TimeoutError('A5.2a launcher exceeded 1250 seconds')
                time.sleep(.2)
            audit['exitCode'] = child.returncode
            audit['status'] = 'DONE' if child.returncode == 0 else 'FAIL'
    finally:
        if child is not None:
            if child.poll() is None:
                # Only the launcher-owned group; denial remains an audit failure.
                try:
                    os.killpg(child.pid, signal.SIGTERM)
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=5)
                except PermissionError as error:
                    audit['cleanupBlocked'] = str(error)
            launch = output / 'six-cells/launch.json'
            audit['launcherAndFixtureReaped'] = (child.poll() is not None and launch.is_file()
                                                and read(launch).get('childReaped') is True)
            if launch.is_file():
                fixture = read(launch)
                audit['fixtureProcess'] = {'pid': fixture.get('childPid'), 'ppid': child.pid,
                                          'argv': fixture['command'],
                                          'executableSha256': digest(Path(fixture['command'][0]))}
            if any(s['status'] == 'BLOCKED' for s in audit['processSnapshots']):
                audit['processTreeCleanup'] = 'BLOCKED'
        save(output / 'launch.json', audit)
    return audit


def observation_check(payload, output, name='observation-check'):
    """Node bridge imports the original observation oracle, including pair checks."""
    source = output / (name + '.input.json')
    save(source, payload)
    proc = subprocess.run(['node', str(SCRIPT / 'session_observations.mjs'), str(source)],
                          capture_output=True, text=True, timeout=60)
    (output / (name + '.stdout')).write_text(proc.stdout)
    (output / (name + '.stderr')).write_text(proc.stderr)
    try:
        result = json.loads(proc.stdout)
    except ValueError as error:
        raise EvidenceError('OBSERVER_EXECUTION', proc.stderr or str(error)) from error
    require(proc.returncode in (0, 1) and result.get('status') in ('PASS', 'FAIL'),
            'OBSERVER_EXECUTION', 'observation bridge did not finish')
    if proc.returncode or result['status'] != 'PASS':
        raise EvidenceError(result.get('rule', 'OBSERVER_EXECUTION'), result.get('detail', 'observer failed'))
    return result


def calibration_coverage(registration, manifest):
    """Check candidate bookkeeping against our independently versioned case list."""
    contract = read(SCRIPT / 'fixtures/session-calibration.json')
    require([c['name'] for c in registration['cells']] == list(CELLS),
            'COVERAGE', 'all six preregistered cells required')
    require(all(c.get('registry') is c['name'].startswith('registry-') for c in registration['cells']),
            'CAPABILITY', 'registry configuration must match the fixed six-cell topology')
    require(registration['phases'] == ['model', 'tool'] and registration['repeats'] == REPEATS,
            'COVERAGE', 'model/tool x five repetitions required')
    expected = [f"{cell['name']}/{variant['name']}/{phase}"
                for cell in registration['cells'] for variant in cell['variants'] for phase in registration['phases']]
    require(expected == contract['cases'] and all(c['variants'] for c in registration['cells']),
            'COVERAGE', 'preregistration must match independent E1 case manifest; no denominator shrink')
    require(len(expected) == len(set(expected)) and expected,
            'COVERAGE', 'nonempty unique cases required')
    require(manifest['expectedCases'] == expected and manifest['executedCases'] == expected,
            'COVERAGE', 'missing/duplicate/extra cases')
    return expected


def validate_calibration(folder, output, baseline):
    raw = folder / 'raw'
    registration, manifest = read(raw / 'preregistration.json'), read(raw / 'manifest.json')
    expected = calibration_coverage(registration, manifest)
    require(manifest['baselineCommit'] == BASELINE_COMMIT and manifest['repeats'] == REPEATS,
            'IDENTITY', 'wrong baseline or repetition count')
    fixture_hashes = {f'scripts/a5-19/{name}': digest(ROOT / 'scripts/a5-19' / name) for name in FIXTURE_NAMES}
    require({row['path']: row['sha256'] for row in manifest['fixtureFiles']} == fixture_hashes,
            'IDENTITY', 'fixture sources differ from the measured manifest')
    require(bool(manifest['importedSources']), 'IDENTITY', 'empty baseline import manifest')
    for row in manifest['importedSources']:
        path = (baseline / row['path']).resolve()
        require(path.is_relative_to(baseline) and digest(path) == row['sha256'],
                'IDENTITY', 'imported baseline changed: ' + row['path'])
    observations, groups = [], []
    for cell in registration['cells']:
        data = read(raw / (cell['name'] + '.json'))
        case_ids = [case for case in expected if case.startswith(cell['name'] + '/')]
        expected_ids = [f'{case}/{i}' for case in case_ids for i in range(1, REPEATS + 1)]
        rows = data['observations']
        require(bool(rows) and data['cell'] == cell['name'] and data['baselineCommit'] == BASELINE_COMMIT
                and [o['id'] for o in rows] == expected_ids, 'COVERAGE', cell['name'] + ': missing/duplicate measurements')
        for observation, expected_id in zip(rows, expected_ids):
            _, variant, phase, repetition = expected_id.split('/')
            require(type(observation.get('repetition')) is int and observation['repetition'] == int(repetition),
                    'COVERAGE', expected_id + ': repetition does not match preregistered ID')
            require(observation.get('phase') == phase,
                    'COVERAGE', expected_id + ': phase does not match preregistered ID')
            require(isinstance(observation.get('variant'), dict) and observation['variant'].get('name') == variant,
                    'COVERAGE', expected_id + ': variant does not match preregistered ID')
            require(bool(observation['frames']) and bool(observation['timeline']),
                    'EMPTY_OBSERVER', observation['id'])
            require(observation['registry']['applicable'] is cell['registry'],
                    'CAPABILITY', observation['id'] + ': forged registry unavailable')
            require(any(r['kind'] == 'loop.final' for r in observation['timeline']),
                    'INDEPENDENT_ORACLE', observation['id'] + ': no loop final witness')
        observations.extend(rows)
        groups.append({'id': cell['name'], 'status': 'OBSERVED', 'measurements': len(rows),
                       'path': str(raw / (cell['name'] + '.json'))})
    require(manifest['totalMeasurements'] == len(observations), 'COVERAGE', 'total measurement mismatch')
    rounds = [{'repetition': i, 'status': 'OBSERVED',
               'observations': [o['id'] for o in observations if o['repetition'] == i]}
              for i in range(1, REPEATS + 1)]
    require(not observations or all(round_['observations'] for round_ in rounds),
            'COVERAGE', 'nonempty observations require nonempty repetition ledgers')
    oracle = observation_check({'observations': observations, 'controls': True}, output)
    controls = manifest['checks']['observerControls']
    require(len(controls) == 9 and len({c['name'] for c in controls}) == 9
            and all(c['detected'] is True and c['hitRule'] == c['expectedRule'] for c in controls),
            'CONTROLS', 'A5.2a original observation controls missing/failed')
    require(oracle['controls'] == controls, 'CONTROLS', 'recorded controls differ from independently rerun original controls')
    launch = read(folder / 'launch.json')
    require(launch.get('exitCode') == 0 and launch.get('childReaped') is True,
            'CLEANUP', 'A5.2a did not finish/reap its child')
    return {'groups': groups, 'expectedCases': expected, 'executedCases': manifest['executedCases'],
            'repeats': REPEATS, 'measurements': len(observations), 'oracle': oracle,
            'rounds': rounds,
            'controls': controls, 'unstableGroups': manifest['unstableGroups'],
            'sidecar': {'status': 'NOT_APPLICABLE', 'reason': 'controlled in-process TS loop; no sidecar binary'},
            'connections': {'count': 0, 'reason': 'A5.2a prohibits listen; in-process queue observation'},
            'surfaces': {'output': 'frames', 'disk': ['durableStatus', 'registry'],
                         'http': {'status': 'NOT_APPLICABLE', 'reason': 'A5.2a in-process scope'}},
            'sourceManifest': str(raw / 'manifest.json'),
            'evidenceSha256': {p.name: digest(p) for p in sorted(raw.glob('*.json'))}}


def audit_native(evidence, contract):
    """Fail-closed contract for future A7 recorder integration, not an executor.

    contract is prepared by the launcher BEFORE execution, never copied from a
    candidate's report. execEvents must include exited children, and calls must
    come from the independent egress observer. A ps snapshot alone is insufficient.
    Current CLI deliberately cannot certify a native run; tests use labelled fixtures.
    """
    require(evidence['topology'] == 'native' and contract['host'] == 'rust',
            'NATIVE_TOPOLOGY', 'Rust host required for a native declaration')
    processes = evidence['processes']
    require(bool(processes), 'PROCESS_WITNESS', 'empty process tree')
    # Inspect the recorded argv as well as the launch identity. Labels alone cannot allow a process.
    for process in processes:
        argv = process['argv']
        require(isinstance(argv, list) and argv and all(isinstance(a, str) for a in argv),
                'PROCESS_WITNESS', 'missing argv')
        if process.get('kind') == 'ts-gateway' or any('gateway-cli' in a or 'host-helper' in a for a in argv):
            raise EvidenceError('TS_HOST', 'TS gateway/internal helper recorded in native topology')
    require(evidence['execWitness']['complete'] is True and evidence['execWitness']['source'] == 'exec-monitor',
            'PROCESS_WITNESS', 'complete exec/exit witness required, not sampled ps')
    actual_pids = [p['pid'] for p in processes]
    require(all(type(pid) is int and pid > 0 for pid in actual_pids)
            and len(set(actual_pids)) == len(actual_pids), 'PROCESS_WITNESS', 'invalid/duplicate PID')
    roles = Counter(p['role'] for p in processes)
    require(roles == Counter(contract['processRoles']), 'NATIVE_PROCESS', 'unexpected/missing process roles')
    for process in processes:
        allowed = contract['binaries'][process['role']]
        require(process['argv'] == allowed['argv'] and process['sha256'] == allowed['sha256']
                and process['executable'] == allowed['path']
                and digest(Path(allowed['path'])) == allowed['sha256'],
                'NATIVE_PROCESS', 'exec identity/argv/hash differs from pre-launch identity')
        expected_parent = contract['launcherPid'] if process['role'] == 'host' else next(p['pid'] for p in processes if p['role'] == 'host')
        require(process['ppid'] == expected_parent, 'PROCESS_WITNESS', 'wrong parent PID')
    live, started, exited = set(), set(), set()
    by_pid = {p['pid']: p for p in processes}
    for row in evidence['execWitness']['events']:
        pid = row['pid']
        require(pid in by_pid and row['event'] in ('exec', 'exit'),
                'PROCESS_WITNESS', 'unknown process/event, including short-lived children')
        if row['event'] == 'exec':
            require(pid not in started and all(row.get(k) == by_pid[pid][k]
                    for k in ('ppid', 'argv', 'executable', 'sha256')),
                    'PROCESS_WITNESS', 'exec identity disagrees with recorded launch identity')
            require(row['ppid'] == contract['launcherPid'] or row['ppid'] in live,
                    'PROCESS_WITNESS', 'child exec before parent or after parent exit')
            live.add(pid)
            started.add(pid)
        else:
            require(pid in live, 'PROCESS_WITNESS', 'exit before exec or duplicate exit')
            live.remove(pid)
            exited.add(pid)
    require(not live and started == exited == set(actual_pids), 'PROCESS_WITNESS', 'missing exec/exit records')
    require(evidence['networkWitness']['complete'] is True, 'NETWORK_WITNESS', 'egress observer incomplete')
    for call in evidence['networkWitness']['calls']:
        require(call['target'] in contract['externalEndpoints'] and call['ownerPid'] in actual_pids,
                'HIDDEN_HELPER', 'undeclared helper/endpoint invocation')
    require(evidence['expectedCases'] == contract['cases'] and evidence['executedCases'] == contract['cases']
            and len(set(contract['cases'])) == len(contract['cases']) and contract['cases'],
            'COVERAGE', 'missing/duplicate/extra native groups')
    require(evidence['capabilities'] == contract['capabilities'], 'CAPABILITY', 'configured capability forged unavailable')
    require(evidence['cleanup']['allStopped'] is True, 'CLEANUP', 'owned processes not stopped')
    require(set(evidence['surfaces']) == {'output', 'disk', 'http'}, 'SURFACES', 'output/disk/HTTP accounting required')
    for name, surface in evidence['surfaces'].items():
        require(surface['status'] == 'RECORDED' and surface['artifacts'], 'SURFACES', name + ' missing')
        for artifact in surface['artifacts']:
            require(digest(Path(artifact['path'])) == artifact['sha256'], 'SURFACES', name + ' artifact changed')
    observer = evidence['observer']
    require(bool(observer['rows']), 'EMPTY_OBSERVER', 'empty independent observer')
    require(observer['connections'] == len({r['connection'] for r in observer['rows']})
            and observer['connections'] == contract['connections'], 'CONNECTIONS', 'connection count mismatch')
    require(evidence['binding'] == contract['binding'], 'BINDING', 'wrong/old interaction binding')
    # Validate bytes using the existing independent TCP decoder and wire checker.
    for row in observer['rows']:
        wire_frame(row)
        decoded = []
        decoder = Decoder(decoded.append, row['direction'])
        decoder.feed(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n' + bytes.fromhex(row['wireHex']))
        require(len(decoded) == 1 and decoded[0].get('frame') == row['frame'],
                'OBSERVER_WIRE', 'wire observation disagrees with recorder')
    hellos = [r['frame'] for r in observer['rows'] if r['frame'].get('type') == 'hello_ok']
    require(bool(hellos), 'BINDING', 'actual server hello/binding missing')
    for frame in hellos:
        binding = frame.get('interactionBinding', {})
        require(frame.get('protocolVersion') == '1.2' and type(binding.get('generation')) is int
                and binding['generation'] >= 1 and isinstance(binding.get('connectionId'), str)
                and bool(binding['connectionId']) and binding == contract['binding'],
                'BINDING', 'actual server hello has invalid/wrong/old binding')
    require(observer['rows'] == contract['observerRows'], 'OBSERVER_DIFFERENCE', 'observer differs from independent driver')


def deadline_report():
    cells = []
    for host, sidecar in MATRIX:
        cells.append({'id': f'{host}-host/{sidecar}-sidecar', 'host': host, 'sidecar': sidecar,
                      'topology': 'native' if host == 'rust' else 'ts-reference',
                      'status': 'NOT_RUN', 'expectedGroups': list(CELLS),
                      'expectedCases': read(SCRIPT / 'fixtures/session-calibration.json')['cases'], 'executedCases': [],
                      'repeats': REPEATS, 'rounds': [], 'processes': [], 'connections': None,
                      'sidecarSha256': None, 'output': None, 'disk': None, 'http': None,
                      'oracle': 'NOT_RUN', 'cleanup': 'NOT_RUN'})
    result = {'status': 'NOT_RUN', 'structureStatus': 'PASS', 'cells': cells,
            'reason': 'A7 native host/driver/exec+egress witnesses and A5.4 frozen expectations are not ready',
            'executionStatus': 'NOT_RUN', 'acceptanceStatus': 'NOT_RUN'}
    require([(c['host'], c['sidecar']) for c in cells] == list(MATRIX)
            and all(c['status'] == 'NOT_RUN' and not c['executedCases'] and c['expectedCases'] for c in cells),
            'DEADLINE_STRUCTURE', 'four non-executed matrix cells required')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suite', choices=('calibrate', 'deadline'), required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, default=BASELINE)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    install_shutdown()
    report = {'suite': args.suite, 'scope': 'A5.3 draft apparatus', 'status': 'RUNNING',
              'acceptanceStatus': 'NOT_RUN', 'apparatusSha256': hashes(), 'frozen': frozen_check()}
    save(output / 'report.json', report)
    code = 0
    try:
        if args.suite == 'deadline':
            report.update(deadline_report())
        else:
            takeovers = []
            with parity_lock(timeout=30, takeovers=takeovers):
                report['lockTakeovers'] = takeovers
                audit = run_fixture(output, args.baseline.resolve())
                report['launch'] = audit
                require(audit['exitCode'] == 0, 'CALIBRATION_EXECUTION', 'see launcher.log and six-cells/run.log')
                require(audit['launcherAndFixtureReaped'], 'CLEANUP', 'launcher/fixture cleanup incomplete')
                report.update(validate_calibration(output / 'six-cells', output, args.baseline.resolve()))
                # Imported lazily to keep the independent negative-control CLI usable.
                import importlib.util
                spec = importlib.util.spec_from_file_location('session_controls', SCRIPT / 'controls/session_domain.py')
                controls = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(controls)
                report['e1Controls'] = controls.run_all(output / 'controls', output / 'six-cells')
                require(report['e1Controls']['status'] == 'PASS', 'CONTROLS', 'E1 mutation control failed')
                report['status'] = 'PASS'
                report['executionStatus'] = 'PASS'
                report['topology'] = 'ts-in-process-controlled'
                report['acceptanceStatus'] = 'NOT_RUN'
                report['calibrationStatus'] = 'OBSERVED'
                report['freezeStatus'] = 'DRAFT_AWAITING_A5_4'
        require(report['apparatusSha256'] == hashes(), 'APPARATUS_CHANGED', 'apparatus changed while collecting evidence')
        require(report['frozen'] == frozen_check(), 'FROZEN_CHANGED', 'frozen inputs changed during suite')
        if args.suite == 'calibrate' and report['frozen']['exitCode'] != 0:
            report['status'] = 'BLOCKED'
            report['freezeStatus'] = 'BLOCKED'
            report['failure'] = {'rule': 'FROZEN', 'detail': 'measurements completed; see frozen check output; no acceptance PASS'}
            code = 2
    except (EvidenceError, OSError, ValueError, KeyError, TypeError, StopIteration,
            subprocess.SubprocessError, TimeoutError, KeyboardInterrupt) as error:
        report['status'] = 'BLOCKED' if isinstance(error, (PermissionError, TimeoutError, KeyboardInterrupt)) else 'FAIL'
        report['failure'] = {'rule': getattr(error, 'rule', 'EVIDENCE'), 'detail': str(error)}
        code = 2 if report['status'] == 'BLOCKED' else 1
    save(output / 'report.json', report)
    print(json.dumps({'status': report['status'], 'suite': args.suite, 'report': str(output / 'report.json'),
                      'failure': report.get('failure'), 'acceptanceStatus': report['acceptanceStatus']}, ensure_ascii=False))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
