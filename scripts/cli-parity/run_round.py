#!/usr/bin/env python3
"""Offline CLI calibration: two TS rounds, optionally one --rust-bin round."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import (Blocked, CleanupBlocked, HEAD, OPENSSL_CONFIG, ROOT, SCRIPT, SCENARIOS, baseline_path, digest, environment,
                    env_summary, fixture, hashes, output_path, parity_lock, save, snapshot)
from compare import compare
from policy import sandbox_profile

MAX_BYTES = 4 * 1024 * 1024


def git_head(path):
    # Explicit executable and fresh environment: no GIT_* or user/global config.
    result = subprocess.run(['/usr/bin/git', '-c', 'safe.directory=*', '-C', str(path), 'rev-parse', 'HEAD'],
                            env={'PATH': '/usr/bin:/bin', 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null'},
                            capture_output=True, text=True, timeout=10)
    if result.returncode:
        raise Blocked('cannot identify source HEAD: ' + result.stderr.strip())
    return result.stdout.strip()


def executable(path):
    path = Path(path).resolve()
    if not path.is_file() or not os.access(path, os.X_OK):
        raise Blocked(f'not executable: {path}')
    return {'path': str(path), 'sha256': digest(path)}


def directory_identity(folder, baseline):
    """Full installed content, including hidden pnpm entries and link targets.

    pnpm hardlinks are content-hashed, not rejected. External symlinks fail
    closed: a source outside this baseline has no registered read contract.
    """
    if not folder.is_dir():
        raise Blocked(f'baseline dependency missing: {folder}')
    manifest = {}
    def visit(path, active):
        resolved = path.resolve()
        if not resolved.is_relative_to(baseline):
            raise Blocked(f'baseline external link: {path}')
        if resolved in active:
            raise Blocked(f'baseline link cycle: {path}')
        info = path.lstat()
        row = {'mode': stat.S_IMODE(info.st_mode)}
        if path.is_symlink():
            # Target content is covered by its canonical subtree. Following
            # dependency links recursively would recurse around pnpm cycles.
            relative = resolved.relative_to(baseline)
            if not resolved.exists():
                raise Blocked(f'baseline dangling link target: {path}')
            # Existing pnpm links to ui/apps stay inventoried, but their target
            # data is not readable under policy.py (canonical vnode matching).
            row.update(type='symlink', target=os.readlink(path), resolved=relative.as_posix())
        elif path.is_dir():
            row['type'] = 'directory'
            for child in sorted(path.iterdir()):
                visit(child, active | {resolved})
        elif path.is_file():
            row.update(type='file', size=path.stat().st_size, sha256=digest(path))
        else:
            raise Blocked(f'baseline special or dangling file: {path}')
        manifest[path.relative_to(folder).as_posix()] = row
    visit(folder, set())
    return hashlib.sha256(json.dumps(manifest, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def baseline_identity(baseline):
    head = git_head(baseline)
    if head != HEAD:
        raise Blocked(f'baseline HEAD {head} does not match registered {HEAD}')
    # Match every baseline subtree the TS policy permits, including dependencies
    # and bundled skills. resources/ is not readable and is deliberately absent.
    trees = {name: directory_identity(baseline / name, baseline)
             for name in ('src', 'dist', 'packages', 'node_modules', 'skills')}
    manifest = {name: digest(baseline / name)
                for name in ('package.json', 'package-lock.json', 'pnpm-lock.yaml')
                if (baseline / name).is_file()}
    return {'path': str(baseline), 'sourceHead': head, 'entrySha256': digest(baseline / 'dist/src/cli/pilotdeck.js'),
            'files': manifest, 'trees': trees, 'trackedClean': 'NOT_ASSERTED (HEAD plus actual file hashes)'}


def rust_identity(binary, source):
    return {'side': 'rust', 'executable': executable(binary), 'sourceHead': git_head(source),
            'sourcePath': str(Path(source).resolve())}


def check_budget(timeout, with_rust):
    rounds = 3 if with_rust else 2
    # Reserve five minutes for hashing/probes/controls; a separate hard deadline
    # covers slower-than-estimated work while the shared lock is held.
    if len(SCENARIOS) > 30 or len(SCENARIOS) * timeout * rounds + 300 >= 2400:
        raise Blocked('batch exceeds 30 scenarios or shared-lock 2400s lease budget')


def lease_expired(signum, frame):
    raise KeyboardInterrupt('batch lease deadline')


def process_table():
    result = subprocess.run(['/bin/ps', '-axo', 'pid=,ppid=,pgid=,stat=,comm='],
                            capture_output=True, text=True, timeout=5,
                            env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'})
    if result.returncode:
        raise Blocked('process tree unavailable: ' + result.stderr.strip())
    rows = []
    for line in result.stdout.splitlines():
        pid, ppid, pgid, state, command = line.strip().split(None, 4)
        rows.append({'pid': int(pid), 'ppid': int(ppid), 'pgid': int(pgid), 'state': state, 'command': command})
    if not any(row['pid'] == os.getpid() for row in rows):
        raise Blocked('process tree missing driver PID')
    return rows


def stop_group(proc):
    errors = []
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(proc.pid, sig)
        except ProcessLookupError:
            break
        except OSError as error:
            errors.append(str(error))
        try:
            proc.wait(timeout=1)
        except subprocess.TimeoutExpired:
            pass
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        errors.append('leader did not exit')
    remaining = []
    try:
        os.killpg(proc.pid, 0)
        remaining.append(proc.pid)
    except ProcessLookupError:
        pass
    except OSError as error:
        errors.append(str(error))
    return {'allStopped': not errors and not remaining, 'remaining': remaining, 'errors': errors}


def capture(argv, env, cwd, stdin, timeout):
    """Bounded pipe recording, one POSIX session, kill whole group even after EOF."""
    record = {'status': 'ERROR', 'timedOut': False, 'processTree': []}
    events, data = [], {'stdout': bytearray(), 'stderr': bytearray()}
    started = time.monotonic()
    proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, start_new_session=True, umask=0o077)
    record['pid'] = proc.pid
    selector = selectors.DefaultSelector()
    try:
        # This batch fixes stdin to empty; future input must use nonblocking writes.
        if stdin:
            raise ValueError('D0-pr1 only registers empty stdin')
        proc.stdin.close()
        for name, stream in (('stdout', proc.stdout), ('stderr', proc.stderr)):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, name)
        sampled = 0.0
        while selector.get_map() or proc.poll() is None:
            now = time.monotonic()
            if now - started >= timeout:
                record['timedOut'] = True
                raise TimeoutError('scenario timeout')
            if now >= sampled:
                rows = [row for row in process_table() if row['pgid'] == proc.pid or row['ppid'] == proc.pid]
                record['processTree'].extend(row for row in rows if row not in record['processTree'])
                if any(row['pid'] != proc.pid for row in rows):
                    raise RuntimeError('unexpected child: offline profile forbids fork')
                sampled = now + 0.05
            for key, _ in selector.select(.02):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                data[key.data].extend(chunk)
                events.append({'stream': key.data, 'hex': chunk.hex()})
                if sum(map(len, data.values())) > MAX_BYTES:
                    raise RuntimeError('output limit exceeded; recording rejected')
        record['exitCode'] = proc.wait(timeout=1)
        record['status'] = 'DONE'
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        record['error'] = str(error)
    finally:
        selector.close()
        record['audit'] = stop_group(proc)
        record['exitCode'] = proc.returncode
        record['elapsedSeconds'] = time.monotonic() - started
        for stream in (proc.stdout, proc.stderr):
            stream.close()
    if not record['audit']['allStopped'] or not record['processTree']:
        record['status'] = 'ERROR'
    return record, {key: bytes(value) for key, value in data.items()}, events


def preflight(output, runtime, profile, node):
    evidence = {'status': 'BLOCKED', 'errors': [], 'profileSha256': digest(profile),
                'profilePath': str(profile), 'profileText': profile.read_text(), 'probeProfileSha256': digest(profile)}
    sentinel = output / 'outside-sentinel.txt'
    sentinel.write_text('D0 must stay outside child read/write scope\n')
    original = digest(sentinel)
    try:
        evidence['driverProcess'] = next(row for row in process_table() if row['pid'] == os.getpid())
    except (Blocked, OSError) as error:
        evidence['errors'].append(str(error))
    proc = subprocess.Popen(['/usr/bin/sandbox-exec', '-f', str(profile), str(node),
                             str(SCRIPT / 'isolation_probe.mjs'), str(sentinel)],
                            cwd=runtime / 'project', env=environment(runtime, SCENARIOS[0]),
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            start_new_session=True, umask=0o077)
    stdout, stderr = b'', b''
    try:
        stdout, stderr = proc.communicate(b'', timeout=10)
    except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
        evidence['errors'].append(f'isolation probe interrupted: {error}')
    finally:
        evidence['audit'] = stop_group(proc)
        for stream in (proc.stdin, proc.stdout, proc.stderr):
            stream.close()
    (output / 'isolation.stdout.bin').write_bytes(stdout)
    (output / 'isolation.stderr.bin').write_bytes(stderr)
    evidence['exitCode'] = proc.returncode
    if not evidence['audit']['allStopped']:
        evidence['errors'].append('isolation probe cleanup failed')
    if proc.returncode:
        evidence['errors'].append('sandbox probe failed: ' + stderr.decode(errors='replace')[:1000])
    else:
        evidence['probe'] = json.loads(stdout)
        if evidence['probe']['status'] != 'PASS':
            evidence['errors'].append('isolation probe assertions failed')
    if digest(sentinel) != original:
        evidence['errors'].append('outside sentinel changed')
    if not evidence['errors']:
        evidence['status'] = 'PASS'
    save(output / 'isolation.json', evidence)
    if not evidence['audit']['allStopped']:
        raise CleanupBlocked('isolation probe cleanup failed; retaining shared lock')
    if evidence['status'] != 'PASS':
        raise Blocked('; '.join(evidence['errors']))
    return evidence


def rust_isolation(output, runtime, profile, node, ts_isolation):
    """The common probe uses the TS policy; Rust is its restricted policy.

    Its actual policy is reconstructed by compare. Additionally test exec of
    Node under that exact Rust policy, without adding a probe exec exception.
    """
    record, streams, _ = capture(['/usr/bin/sandbox-exec', '-f', str(profile), node, '--version'],
                                 environment(runtime, SCENARIOS[0]), runtime / 'project', b'', 10)
    stderr = streams['stderr']
    denied = record['status'] == 'DONE' and record['timedOut'] is False and record['exitCode'] not in (None, 0) and not streams['stdout'] and b'execvp' in stderr and any(
        s in stderr for s in (b'Operation not permitted', b'Permission denied'))
    proof = {'denied': denied, 'exitCode': record['exitCode'], 'stdoutHex': streams['stdout'].hex(),
             'stderrHex': stderr.hex(), 'audit': record['audit'], 'status': record['status'], 'timedOut': record['timedOut']}
    evidence = {**ts_isolation, 'profilePath': str(profile), 'profileText': profile.read_text(),
                'profileSha256': digest(profile), 'nodeExec': proof, 'status': 'PASS' if denied else 'BLOCKED'}
    save(output / 'rust-isolation.json', evidence)
    if not record['audit']['allStopped']:
        raise CleanupBlocked('Rust Node-denial probe cleanup failed')
    if not denied:
        raise Blocked('Rust sandbox did not prove Node exec denied')
    return evidence


def run_round(output, runtime, profile, isolation, side, provenance, timeout):
    if baseline_identity(Path(provenance['baseline']['path'])) != provenance['baseline']:
        raise Blocked('baseline changed before round')
    folder = output / side
    folder.mkdir()
    (folder / 'cases').mkdir()
    actual_side = 'ts' if side.startswith('ts-') else 'rust'
    run = {'schema': 1, 'kind': 'cli-d0-recording', 'origin': 'live-os-sandbox', 'side': actual_side, 'status': 'RUNNING',
           'runtime': str(runtime), 'scenarios': [s['id'] for s in SCENARIOS],
           'apparatus': hashes(), 'isolation': isolation, 'provenance': provenance}
    save(folder / 'round.json', run)
    for scenario in SCENARIOS:
        case = folder / 'cases' / scenario['id']
        case.mkdir()
        fixture(runtime)
        save(case / 'initial.json', snapshot(runtime))
        command = [provenance['executable']['path']]
        if actual_side == 'ts':
            command.append(str(Path(provenance['baseline']['path']) / 'dist/src/cli/pilotdeck.js'))
        command.extend(scenario['argv'])
        env = environment(runtime, scenario)
        if digest(profile) != isolation['profileSha256']:
            raise Blocked('sandbox profile changed before scenario')
        sandbox_argv = ['/usr/bin/sandbox-exec', '-f', str(profile), *command]
        record, streams, events = capture(sandbox_argv,
                                           env, runtime / 'project', bytes.fromhex(scenario['stdinHex']), timeout)
        record.update(argv=scenario['argv'], executedArgv=command, stdinHex=scenario['stdinHex'],
                      cwd=str(runtime / 'project'), env=env_summary(env), sandboxArgv=sandbox_argv,
                      profileSha256=digest(profile))
        for name, value in streams.items():
            (case / f'{name}.bin').write_bytes(value)
        save(case / 'events.json', events)
        save(case / 'record.json', record)
        save(case / 'final.json', snapshot(runtime))
        runtime.rename(case / 'tree')
        print(f'{side} {scenario["id"]}: {record["status"]} exit={record["exitCode"]}', flush=True)
        if not record['audit']['allStopped']:
            raise CleanupBlocked(f'{side}/{scenario["id"]}: cleanup failed; retaining shared lock')
        if record['profileSha256'] != isolation['profileSha256']:
            raise Blocked('sandbox profile changed during scenario')
        if record['status'] != 'DONE':
            raise Blocked(f'{side}/{scenario["id"]}: incomplete recording')
    if baseline_identity(Path(provenance['baseline']['path'])) != provenance['baseline']:
        raise Blocked('baseline changed during round')
    run['status'] = 'DONE'
    save(folder / 'round.json', run)
    return folder


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--baseline', type=Path, help='explicit > PDRS_BASELINE > this checkout resources/PilotDeck')
    ap.add_argument('--rust-bin', type=Path, help='optional Rust CLI; no candidate is run by default')
    ap.add_argument('--rust-source', type=Path, help='required with --rust-bin: checkout used to build the binary')
    ap.add_argument('--node', default=shutil.which('node'), help='installed Node executable; no download')
    ap.add_argument('--output', type=Path, default=ROOT / '_tmp/20261017-d0/calibration')
    ap.add_argument('--timeout', type=float, default=15, help='per scenario seconds, (0, 60]')
    args = ap.parse_args()
    if not math.isfinite(args.timeout) or not 0 < args.timeout <= 60:
        ap.error('--timeout must be finite and in (0, 60]')
    if bool(args.rust_bin) != bool(args.rust_source):
        ap.error('--rust-bin and --rust-source must be supplied together')
    check_budget(args.timeout, bool(args.rust_bin))
    output = output_path(args.output)
    output.mkdir(parents=True, mode=0o700)
    summary = {'status': 'BLOCKED', 'scope': 'D0-pr1 calibration; no command migration claim'}
    try:
        if sys.platform != 'darwin' or not Path('/usr/bin/sandbox-exec').is_file():
            raise Blocked('D0-pr1 requires macOS sandbox-exec; no unconfined fallback')
        baseline = baseline_path(args.baseline)
        if not args.node:
            raise Blocked('Node executable not found')
        node = executable(args.node)
        identity = baseline_identity(baseline)
        provenance = {'side': 'ts', 'executable': node, 'sourceHead': HEAD, 'sourcePath': str(baseline),
                      'baseline': identity, 'node': node, 'opensslConfig': OPENSSL_CONFIG}
        runtime = output / 'runtime'
        expected_rust = rust_identity(args.rust_bin, args.rust_source) if args.rust_bin else None
        if expected_rust and (expected_rust['executable']['path'] == node['path']
                              or expected_rust['executable']['sha256'] == node['sha256']
                              or expected_rust['sourcePath'] == str(baseline)):
            raise Blocked('Rust candidate must not be the TS runtime or source')
        profile = output / 'ts.sb'
        profile.write_text(sandbox_profile(runtime, baseline, node['path'], 'ts'))
        with parity_lock() as owner:
            # Stop before other runners may consider the lease expired. The
            # handler raises through capture's finally so owned groups are reaped.
            previous_alarm = signal.signal(signal.SIGALRM, lease_expired)
            signal.alarm(2280)
            save(output / 'lock.json', owner)
            fixture(runtime)
            isolation = preflight(output, runtime, profile, node['path'])
            runtime.rename(output / 'preflight-tree')
            first = run_round(output, runtime, profile, isolation, 'ts-1', provenance, args.timeout)
            second = run_round(output, runtime, profile, isolation, 'ts-2', provenance, args.timeout)
            summary['selfComparison'] = compare(first, second)
            save(output / 'self-comparison.json', summary['selfComparison'])
            if summary['selfComparison']['status'] != 'PASS':
                summary['status'] = 'FAIL'
            else:
                from controls import run_controls
                summary['controls'] = run_controls(first, output / 'controls')
                summary['status'] = summary['controls']['status']
                if args.rust_bin:
                    rust_profile = output / 'rust.sb'
                    rust_profile.write_text(sandbox_profile(runtime, baseline, expected_rust['executable']['path'], 'rust'))
                    fixture(runtime)
                    rust_probe = rust_isolation(output, runtime, rust_profile, node['path'], isolation)
                    runtime.rename(output / 'rust-preflight-tree')
                    rust_provenance = {**expected_rust, 'baseline': identity, 'node': node, 'opensslConfig': OPENSSL_CONFIG}
                    candidate = run_round(output, runtime, rust_profile, rust_probe, 'rust', rust_provenance, args.timeout)
                    summary['rustComparison'] = compare(first, candidate, expected_rust)
                    if summary['rustComparison']['status'] != 'PASS':
                        summary['status'] = 'FAIL'
            # Ensure baseline executable and observed source/build were unchanged during recording.
            if executable(args.node) != node or baseline_identity(baseline) != identity:
                raise Blocked('baseline/runtime changed during recording')
            if args.rust_bin and rust_identity(args.rust_bin, args.rust_source) != expected_rust:
                raise Blocked('Rust executable/source HEAD changed during recording')
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        summary['status'] = 'BLOCKED'
        summary['error'] = str(error)
    finally:
        if 'previous_alarm' in locals():
            signal.alarm(0)
            signal.signal(signal.SIGALRM, previous_alarm)
    save(output / 'summary.json', summary)
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    return {'PASS': 0, 'FAIL': 1, 'BLOCKED': 2}[summary['status']]


if __name__ == '__main__':
    def interrupted(signum, frame):
        raise KeyboardInterrupt(f'signal {signum}')
    signal.signal(signal.SIGTERM, interrupted)
    try:
        code = main()
    except (Blocked, OSError) as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}))
        code = 2
    raise SystemExit(code)
