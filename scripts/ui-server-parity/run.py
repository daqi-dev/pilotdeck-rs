#!/usr/bin/env -S python3 -I
"""exp010 S1/S2: fresh direct and proxied runs, six driver scenarios each.

Recorder 13101 -> [B1 DUT 13103 ->] TS UI 13102 -> gateway 28990.
Enumerator 13104; reserved browser frontend 13105. Loopback only.
"""
# Isolated bootstrap: verify the source loader before executing local code.
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:
    print('{"verdict":"FAIL","errors":["os must be preloaded from CPython 3.11+ frozen modules"]}')
    raise SystemExit(1)
import os as _boot_os
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (fix-r3 P1, same defect as #701 fix-r2). Nonexistent sys.path entries skip.
if any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.isdir(root)
                and _boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))
        for entry in _boot_sys.path):
    print('{"verdict":"FAIL","errors":["apparatus directory on sys.path; run the entry file directly with -I"]}')
    raise SystemExit(1)
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
import hashlib as _boot_hashlib
import subprocess as _boot_subprocess
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_os.path.isfile(_boot_manifest):
        with open(_boot_manifest) as _boot_file:
            _boot_pins = [line.split()[0] for line in _boot_file
                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]
        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:
            raise ValueError('apparatus bootstrap hash mismatch')
    else:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import argparse
import base64
import binascii
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import uuid

from apparatus import load_source, prepare_cargo, cargo_process, cargo_identity, node_environment
from snapshot import capture
from common import apparatus_hashes, source_hashes, node_ancestor_admission, ClockMonitor, ClockRefused, validate_clock_check

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
# Reuse B1 isolated fixture/env/process-group ownership + lsof assertions.
# Its comparator/scenarios are never invoked here (S3 belongs to the next PR).
# B1 imports its own `compare` at module load. Isolate that import so loading
# S1 after S3 (e.g. unittest discovery) cannot bind either comparator incorrectly.
b1_comparator = load_source('b1_comparator', ROOT / 'scripts/exp008/compare.py')
previous_compare = sys.modules.get('compare')
previous_path = sys.path[:]
try:
    sys.modules['compare'] = b1_comparator
    b1 = load_source('b1_device', ROOT / 'scripts/exp008/run.py')
finally:
    sys.path[:] = previous_path
    if previous_compare is None:
        sys.modules.pop('compare', None)
    else:
        sys.modules['compare'] = previous_compare
b1.PORTS = {'rust': 13103, 'ui': 13102, 'enum': 13104, 'gateway': 28990, 'vite': 13105}
b1.ALL_PORTS = set(b1.PORTS.values()) | {13101}
BINARY = None


# Only fixture values that B1 explicitly sets are admitted. Inherited settings
# (including Node loaders, proxy variables and dynamic loaders) never reach Node.
FIXTURE_ENV = ('HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME',
               'XDG_STATE_HOME', 'XDG_RUNTIME_DIR', 'EXP008_OUT', 'PILOT_HOME', 'PILOTDECK_CONFIG_PATH', 'TMPDIR',
               'WORKSPACES_ROOT', 'PILOTDECK_GATEWAY_HOST', 'PILOTDECK_GATEWAY_PORT',
               'PILOTDECK_GATEWAY_TOKEN', 'PILOTDECK_GATEWAY_TOKEN_PATH',
               'PILOTDECK_GATEWAY_URL', 'HOST', 'SERVER_PORT', 'VITE_PORT',
               'PILOTDECK_DISABLE_LOCAL_AUTH', 'DISABLE_LOCAL_AUTH',
               'PILOTDECK_SKIP_BROWSER_OPEN', 'EXP008_POLL_WATCH', 'EXP008_MOCK',
               'EXP008_MODEL_LOG')


def build_env(out, label, extra=None):
    node_ancestor_admission(b1.BASELINE)
    generated = b1.build_env(out, label)
    values = {key: generated[key] for key in FIXTURE_ENV}
    values['HOME'] = str(out / 'home')
    values.update(extra or {})
    return node_environment(values)


def build(target):
    """Standalone captures also use a newly created target, never _build artifacts."""
    global BINARY
    cwd, home = target.parent, target.parent / 'cargo-home'
    prepare_cargo(cwd, home, target, ROOT / '_build/cargo-home', root=ROOT)
    identity = cargo_identity(cwd, home, target, root=ROOT)
    b1.save(cwd, 'toolchain.json', identity)
    cargo_process(['cargo', 'build', '--locked', '--offline', '--manifest-path',
                   str(ROOT / 'scripts/exp008/proxy/Cargo.toml')], cwd, home, target, root=ROOT)
    BINARY = target / 'debug/pdrs-exp008-proxy'


def batch_binary(path):
    from calibrate import Batch
    path = path.resolve()
    state = json.loads((path / 'batch.json').read_text())
    if state.get('root') != str(ROOT) or state.get('schema') != 1:
        raise ValueError('build batch belongs to another root/schema')
    batch = Batch(path, state)
    batch.guard()
    return batch.binary


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
    env = build_env(out, 'enumerate', {
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
        subprocess.run([sys.executable, '-I', str(ROOT / 'scripts/exp008/reconcile_routes.py'),
                        str(out / 'router-enumeration.json'),
                        str(ROOT / 'docs/research/006_phase-b-ui-server/route-inventory.md'),
                        '--out', str(out / 'route-reconciliation.json')], check=True)
    finally:
        cleanup(out, first)


def record_side(out, side, slot=None):
    directory = out / (slot or side)
    directory.mkdir()
    meta = {'schema': 1, 'mode': side, 'fixture_path': str(directory.resolve()),
            'run_id': str(uuid.uuid4()), 'observer_port': 13102, 'synthetic': False,
            'baseline_path': str(b1.BASELINE), 'apparatus_sha256': apparatus_hashes(),
            'source_sha256': source_hashes(b1.BASELINE, BINARY)}
    monitor = ClockMonitor()
    result = None
    failure = None
    try:
        meta['wall_start_ms'] = monitor.start()
        result = record_monitored_side(directory, side, meta)
        # Driver reports can carry a FAIL even without raising an exception.
        if len(result) != 6 or any(case.get('status') != 'PASS' for case in result.values()):
            raise RuntimeError('DUT/driver scenario FAIL or incomplete; inspect scenarios.json')
        meta['dut_result'] = {'status': 'PASS'}
    except ClockRefused as error:
        failure = error
        meta['dut_result'] = {'status': 'NOT_RUN', 'reason': str(error)}
    except Exception as error:
        failure = error
        meta['dut_result'] = {'status': 'FAIL', 'error': f'{type(error).__name__}: {error}'}
    finally:
        monitor.finish(meta)
        b1.save(directory, 'run-meta.json', meta)
    # Preserve a DUT failure while recording independent clock evidence.
    if failure is not None and not isinstance(failure, ClockRefused):
        raise failure
    validate_clock_check(meta)
    if failure is not None:
        raise failure
    return result


def record_monitored_side(directory, side, meta):
    home, project = b1.make_home(directory)
    env = build_env(directory, side, {'VITE_IS_PLATFORM': 'false', 'EXP008_POLL_WATCH': '0'})
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
        meta['source_sha256_end'] = source_hashes(b1.BASELINE, BINARY)
        b1.save(directory, 'run-meta.json', meta)
    if meta['source_sha256_end'] != meta['source_sha256']:
        raise RuntimeError('binary/baseline changed during capture')
    validate_recording(directory / 'recording.jsonl')
    b1.save(directory, 'snapshot.json', capture(directory))
    return results


def validate_recording(path):
    # Recorder completion/format gate only; no behavioral diff or normalization.
    records = [json.loads(line) for line in b1_comparator.jsonl_lines(path.read_text())]
    if not records or records[0].get('kind') != 'run_start' or not any(r.get('kind') == 'run_end' for r in records):
        raise RuntimeError('recording is missing start/end markers')
    if [r.get('seq') for r in records] != list(range(1, len(records) + 1)):
        raise RuntimeError('recording has missing/repeated/out-of-order seq')
    if any(r.get('kind') in ('recorder_error', 'http_body_error') for r in records):
        raise RuntimeError('recorder/HTTP body error present')
    definitions = json.loads((HERE / 'scenarios.json').read_text())['http_cases']
    case_requests = {r['id']: r for r in records if r.get('kind') == 'http_request' and r.get('case')}
    responses = {r['id']: r for r in records if r.get('kind') == 'http_response'}
    case_connections = {r['connection'] for r in responses.values()
                        if r['id'] in case_requests and 'connection' in r}
    for record in records:
        if record.get('kind') == 'http_aborted' and record.get('id') in case_requests:
            raise RuntimeError(f"http_aborted in case {case_requests[record['id']]['case']}")
        if record.get('kind') == 'ws_partial' and record.get('connection') in case_connections:
            raise RuntimeError(f"ws_partial in case connection {record['connection']}")
    bodies = {}
    for record in records:
        if record.get('kind') in ('http_chunk', 'http_end', 'http_aborted', 'http_body_error'):
            bodies.setdefault((record['id'], record['direction']), []).append(record)
    for case in definitions:
        requests = [r for r in records if r.get('kind') == 'http_request' and r.get('case') == case['id']]
        if not requests or not any(responses.get(r['id'], {}).get('status') in case['expected_statuses'] for r in requests):
            raise RuntimeError(f"recording lacks HTTP request/response evidence for {case['id']}")
        # Every attempt needs complete bodies, even if another attempt succeeded.
        for request in requests:
            response = responses.get(request['id'])
            if response is None:
                raise RuntimeError(f"recording lacks HTTP request/response evidence for {case['id']}")
            for direction, metadata in (('request', request), ('response', response)):
                validate_http_body(case['id'], direction, metadata,
                                   bodies.get((request['id'], direction), []))


def validate_http_body(case_id, direction, metadata, events):
    label = f'{case_id} {direction}'
    if not events or events[-1].get('kind') != 'http_end' or sum(e.get('kind') == 'http_end' for e in events) != 1:
        raise RuntimeError(f'{label}: body must finish with one http_end')
    offset = 0
    for event in events:
        if type(event.get('offset')) is not int or event['offset'] != offset:
            raise RuntimeError(f'{label}: body offset disagrees with recorded bytes')
        if event['kind'] == 'http_chunk' and 'body_b64' in event:
            try:
                data = base64.b64decode(event['body_b64'], validate=True)
            except (ValueError, TypeError, binascii.Error) as error:
                raise RuntimeError(f'{label}: invalid body base64') from error
            if type(event.get('bytes')) is not int or event['bytes'] != len(data):
                raise RuntimeError(f'{label}: chunk bytes disagree with body base64')
            offset += len(data)
    lengths = []
    for header in metadata.get('headers', []):
        if header['name'].lower() == 'content-length':
            try:
                raw = base64.b64decode(header['value_b64'], validate=True).decode('ascii')
            except (ValueError, TypeError, binascii.Error) as error:
                raise RuntimeError(f'{label}: invalid Content-Length') from error
            if not re.fullmatch(r'[0-9]+', raw):
                raise RuntimeError(f'{label}: invalid Content-Length')
            lengths.append(int(raw))
    if lengths and any(length != offset for length in lengths):
        raise RuntimeError(f'{label}: http_end offset {offset} disagrees with Content-Length {lengths}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True, help='new directory; existing evidence is never overwritten')
    parser.add_argument('--baseline', type=Path, default=b1.BASELINE)
    parser.add_argument('--build-batch', type=Path, help='use only this batch’s verified fresh build')
    parser.add_argument('--side', choices=('direct', 'proxy'), help='one fresh side for k=5 calibration/DUT comparison')
    parser.add_argument('--slot', default='direct', choices=('direct', 'proxy'), help='single-side fixture directory; use the SAME slot and --out on every round')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    b1.BASELINE = args.baseline.resolve()
    report = {'scope': 'S1/S2 recording plus S3 snapshot inputs; single-side PASS is not calibration', 'status': 'FAIL', 'sides': {}}
    failures, clock_refusals = [], []
    try:
        global BINARY
        if args.build_batch:
            BINARY = batch_binary(args.build_batch)
        else:
            build(out / 'cargo-target')
        enum = out / 'enumeration'
        enum.mkdir()
        b1.make_home(enum)
        enumerate_routes(enum)
        subprocess.run([sys.executable, '-I', str(HERE / 'check_coverage.py'), '--enumeration',
                        str(enum / 'router-enumeration.json'), '--out', str(out / 'coverage-declarations.json')], check=True)
        sides = (args.side,) if args.side else ('direct', 'proxy')
        for side in sides:
            # Independent fresh services and homes; one failed side cannot suppress the other.
            try:
                report['sides'][side] = record_side(out, side, args.slot if args.side else None)
                result = report['sides'][side]
                if len(result) != 6 or any(case.get('status') != 'PASS' for case in result.values()):
                    failures.append(side + ' incomplete/failing driver scenarios')
            except ClockRefused as error:
                clock_refusals.append(str(error))
                report['sides'][side] = {'device': {'status': 'REFUSED', 'error': str(error)}}
            except Exception as error:
                failures.append(str(error))
                report['sides'][side] = {'device': {'status': 'FAIL', 'error': str(error)}}
            metadata = out / (args.slot if args.side else side) / 'run-meta.json'
            if metadata.is_file():
                meta = json.loads(metadata.read_text())
                report.setdefault('capture_checks', {})[side] = {
                    'dut_result': meta.get('dut_result'),
                    'clock_regression_check': meta.get('clock_regression_check')}
            b1.save(out, 'result.json', report)
        evidence = out / 'cases.jsonl'
        if args.side:
            source = out / args.slot / 'cases.jsonl'
            evidence.write_text(source.read_text() if source.exists() else '')
            # S2's combined evidence gate requires both modes. Single-side runs
            # are certified only after S3 consumes two independently archived sides.
            coverage_code = 0
        else:
            evidence.write_text(''.join((out / side / 'cases.jsonl').read_text() if (out / side / 'cases.jsonl').exists() else '' for side in sides))
            coverage_code = 0
            if not failures and not clock_refusals:
                coverage = subprocess.run([sys.executable, '-I', str(HERE / 'check_coverage.py'), '--enumeration',
                            str(enum / 'router-enumeration.json'), '--evidence', str(evidence),
                            '--out', str(out / 'coverage-evidence.json')])
                coverage_code = coverage.returncode
        if coverage_code != 0:
            failures.append('coverage evidence FAIL')
        scenarios_pass = all(len(result) == 6 and all(s['status'] == 'PASS' for s in result.values())
                             for result in report['sides'].values())
        if not failures and not clock_refusals and not scenarios_pass:
            failures.append('incomplete/failing driver scenarios')
        if failures:
            report['status'] = 'FAIL'
        elif clock_refusals:
            report['status'] = 'REFUSED'
        elif scenarios_pass:
            report['status'] = 'PASS'
    except Exception as error:
        report['status'] = 'FAIL'
        report['error'] = f'{type(error).__name__}: {error}'
    finally:
        b1.save(out, 'result.json', report)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 2 if report['status'] == 'REFUSED' else 0 if report['status'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
