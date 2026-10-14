#!/usr/bin/env python3
"""exp010 S1/S2: fresh direct and proxied runs, six driver scenarios each.

Recorder 13101 -> [B1 DUT 13103 ->] TS UI 13102 -> gateway 28990.
Enumerator 13104; reserved browser frontend 13105. Loopback only.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
# Reuse B1 isolated fixture/env/process-group ownership + lsof assertions.
# Its comparator/scenarios are never invoked here (S3 belongs to the next PR).
sys.path.insert(0, str(ROOT / 'scripts/exp008'))
spec = importlib.util.spec_from_file_location('b1_device', ROOT / 'scripts/exp008/run.py')
b1 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b1)
b1.PORTS = {'rust': 13103, 'ui': 13102, 'enum': 13104, 'gateway': 28990, 'vite': 13105}
b1.ALL_PORTS = set(b1.PORTS.values()) | {13101}
BINARY = ROOT / '_build/exp010-target/debug/pdrs-exp008-proxy'


def build():
    env = dict(os.environ, CARGO_HOME=str(ROOT / '_build/cargo-home'),
               CARGO_TARGET_DIR=str(ROOT / '_build/exp010-target'))
    subprocess.run(['cargo', 'build', '--locked', '--offline', '--manifest-path',
                    str(ROOT / 'scripts/exp008/proxy/Cargo.toml')], env=env, check=True)


def cleanup(out, first):
    owned = b1.PROCESSES[first:]
    for proc, log, entry in reversed(owned):
        b1.stop(proc)
        entry['exitCode'] = proc.returncode
        log.close()
    remaining = []
    for proc, _, _ in owned:
        try:
            os.killpg(proc.pid, 0)
            remaining.append(proc.pid)
        except ProcessLookupError:
            pass
    audit = {'processes': [entry for _, _, entry in owned], 'owned_groups_remaining': remaining,
             'cleanup_errors': b1.CLEANUP_ERRORS, 'listeners': b1.LISTENERS}
    b1.save(out, 'audit.json', audit)
    if remaining or b1.CLEANUP_ERRORS:
        raise RuntimeError('owned process group cleanup failed; see audit.json')
    for _, _, entry in owned:
        if entry['name'] == 'recorder' and entry['exitCode'] != 0:
            raise RuntimeError(f"recorder exited {entry['exitCode']}; recording cannot be accepted")


def enumerate_routes(out):
    env = b1.build_env(out, 'enumerate', {
        'SERVER_PORT': '13104', 'VITE_IS_PLATFORM': 'false', 'PARITY_ENUM_OUT': str(out / 'router-enumeration.json'),
        'PARITY_BASELINE': str(b1.BASELINE),
    })
    first = len(b1.PROCESSES)
    try:
        proc = b1.start('enumerate', ['node', '--import', str(HERE / 'enumerate_routes.mjs'),
                      '--import', str(b1.BASELINE / 'node_modules/tsx/dist/loader.mjs'),
                      str(b1.BASELINE / 'ui/server/index.js')], env, 13104, cwd=out / 'project')
        b1.assert_loopback_when_listening(out, 13104, proc)
        if proc.wait(timeout=90):
            raise RuntimeError('enumerator exited nonzero')
        enumeration = json.loads((out / 'router-enumeration.json').read_text())
        if not enumeration['appCaptured'] or enumeration['uniqueRouteCount'] < 100:
            raise RuntimeError('runtime enumeration implausible')
        subprocess.run([sys.executable, str(ROOT / 'scripts/exp008/reconcile_routes.py'),
                        str(out / 'router-enumeration.json'),
                        str(ROOT / 'docs/research/006_phase-b-ui-server/route-inventory.md'),
                        '--out', str(out / 'route-reconciliation.json')], check=True)
    finally:
        cleanup(out, first)


def record_side(out, side):
    directory = out / side
    directory.mkdir()
    home, project = b1.make_home(directory)
    env = b1.build_env(directory, side, {'VITE_IS_PLATFORM': 'false', 'EXP008_POLL_WATCH': '0'})
    # Inherit no external record/DUT configuration.
    for key in list(env):
        if key.startswith(('EXP010_', 'PARITY_')):
            env.pop(key)
    first = len(b1.PROCESSES)
    try:
        gateway = b1.start('gateway', ['node', '--import', str(ROOT / 'scripts/exp008/preload.mjs'),
                           str(b1.BASELINE / 'dist/src/cli/pilotdeck.js'), 'server'], env, 28990, cwd=project)
        b1.wait_http(directory, 28990, gateway)
        ui = b1.start('ui', ['node', '--import', str(ROOT / 'scripts/exp008/preload.mjs'),
                      '--import', str(b1.BASELINE / 'node_modules/tsx/dist/loader.mjs'),
                      str(b1.BASELINE / 'ui/server/index.js')], env, 13102, cwd=project)
        b1.wait_http(directory, 13102, ui)
        if (home / 'server-token').stat().st_mode & 0o777 != 0o600:
            raise RuntimeError('shared gateway token must have mode 0600')
        upstream = 13102
        if side == 'proxy':
            dut_env = dict(env, EXP008_LISTEN='127.0.0.1:13103', EXP008_UPSTREAM='http://127.0.0.1:13102', EXP008_HANDLER='ts')
            dut = b1.start('dut', [str(BINARY)], dut_env, 13103)
            b1.wait_http(directory, 13103, dut)
            upstream = 13103
        recorder_env = dict(env, EXP008_LISTEN='127.0.0.1:13101',
                            EXP008_UPSTREAM=f'http://127.0.0.1:{upstream}',
                            EXP010_RECORD_LOG=str(directory / 'recording.jsonl'))
        recorder = b1.start('recorder', [str(BINARY)], recorder_env, 13101)
        b1.wait_http(directory, 13101, recorder)
        driver = b1.start('driver', ['node', str(HERE / 'driver.mjs'), str(directory),
                          str(b1.BASELINE), str(project), side, '13101', '13102', str(home)], env)
        code = driver.wait(timeout=480)
        if code or recorder.poll() is not None:
            raise RuntimeError(f'{side} driver/recorder failed; inspect scenarios.json and logs')
        results = json.loads((directory / 'scenarios.json').read_text())
    finally:
        cleanup(directory, first)
    validate_recording(directory / 'recording.jsonl')
    return results


def validate_recording(path):
    # Recorder completion/format gate only; no behavioral diff or normalization.
    records = [json.loads(line) for line in path.read_text().splitlines()]
    if not records or records[0].get('kind') != 'run_start' or not any(r.get('kind') == 'run_end' for r in records):
        raise RuntimeError('recording is missing start/end markers')
    if [r.get('seq') for r in records] != list(range(1, len(records) + 1)):
        raise RuntimeError('recording has missing/repeated/out-of-order seq')
    if any(r.get('kind') in ('recorder_error', 'http_body_error') for r in records):
        raise RuntimeError('recorder/HTTP body error present')
    definitions = json.loads((HERE / 'scenarios.json').read_text())['http_cases']
    responses = {r['id']: r for r in records if r.get('kind') == 'http_response'}
    for case in definitions:
        requests = [r for r in records if r.get('kind') == 'http_request' and r.get('case') == case['id']]
        if not requests or not any(responses.get(r['id'], {}).get('status') in case['expected_statuses'] for r in requests):
            raise RuntimeError(f"recording lacks HTTP request/response evidence for {case['id']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True, help='new directory; existing evidence is never overwritten')
    parser.add_argument('--baseline', type=Path, default=b1.BASELINE)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    b1.BASELINE = args.baseline.resolve()
    report = {'scope': 'S1/S2 only', 'status': 'FAIL', 'sides': {}}
    try:
        build()
        enum = out / 'enumeration'
        enum.mkdir()
        b1.make_home(enum)
        enumerate_routes(enum)
        subprocess.run([sys.executable, str(HERE / 'check_coverage.py'), '--enumeration',
                        str(enum / 'router-enumeration.json'), '--out', str(out / 'coverage-declarations.json')], check=True)
        for side in ('direct', 'proxy'):
            # Independent fresh services and homes; one failed side cannot suppress the other.
            try:
                report['sides'][side] = record_side(out, side)
            except Exception as error:
                report['sides'][side] = {'device': {'status': 'FAIL', 'error': str(error)}}
            b1.save(out, 'result.json', report)
        evidence = out / 'cases.jsonl'
        evidence.write_text(''.join((out / side / 'cases.jsonl').read_text() if (out / side / 'cases.jsonl').exists() else '' for side in ('direct', 'proxy')))
        coverage = subprocess.run([sys.executable, str(HERE / 'check_coverage.py'), '--enumeration',
                        str(enum / 'router-enumeration.json'), '--evidence', str(evidence),
                        '--out', str(out / 'coverage-evidence.json')])
        if coverage.returncode == 0 and all(len(result) == 6 and all(s['status'] == 'PASS' for s in result.values()) for result in report['sides'].values()):
            report['status'] = 'PASS'
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
    finally:
        b1.save(out, 'result.json', report)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if report['status'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
