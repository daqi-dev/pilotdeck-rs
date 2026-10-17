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
import subprocess
import sys
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import (Blocked, CleanupBlocked, HEAD, ROOT, SCRIPT, SCENARIOS, baseline_path, digest, environment,
                    env_summary, fixture, hashes, output_path, parity_lock, save, snapshot)
from compare import compare

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


def baseline_identity(baseline):
    head = git_head(baseline)
    if head != HEAD:
        raise Blocked(f'baseline HEAD {head} does not match registered {HEAD}')
    # Hash actual build output and SDK source/build, not just the entry shim.
    files = set()
    for prefix in ('src', 'dist', 'packages/sdk'):
        folder = baseline / prefix
        if not folder.is_dir():
            raise Blocked(f'baseline dependency missing: {folder}')
        files.update(p for p in folder.rglob('*') if p.is_file() and 'node_modules' not in p.relative_to(folder).parts)
    for name in ('package.json', 'package-lock.json', 'pnpm-lock.yaml'):
        if (baseline / name).is_file():
            files.add(baseline / name)
    manifest = {p.relative_to(baseline).as_posix(): digest(p) for p in sorted(files)}
    if not (baseline / 'node_modules').is_dir():
        raise Blocked('baseline node_modules missing; no automatic install')
    return {'path': str(baseline), 'sourceHead': head, 'entrySha256': digest(baseline / 'dist/src/cli/pilotdeck.js'),
            'files': manifest, 'trackedClean': 'NOT_ASSERTED (HEAD plus actual file hashes)'}


def sandbox_profile(runtime, baseline, binary, node):
    if sys.platform != 'darwin' or not Path('/usr/bin/sandbox-exec').is_file():
        raise Blocked('D0-pr1 requires macOS sandbox-exec; no unconfined fallback')
    # Product cannot write evidence, source, installed tools or any user directory.
    # baseline/skills：内置 skills 根（LocalGatewayBootConfig.ts:81），缺读权会让 TS 多报一行迁移失败（真实定标实测）
    read_dirs = [runtime, baseline / 'dist', baseline / 'node_modules', baseline / 'packages', baseline / 'skills',
                 Path('/System'), Path('/usr/lib'), Path('/Library/Apple'), Path('/opt/homebrew/Cellar'),
                 # Homebrew node 经 /opt/homebrew/opt/* 链接加载 dylib，启动时读 OpenSSL 配置（本机实测缺任一即 abort/exit 1）
                 Path('/opt/homebrew/opt'), Path('/opt/homebrew/etc/openssl@3')]
    read_files = [Path('/'), Path(node), Path(binary), baseline / 'package.json', SCRIPT / 'isolation_probe.mjs',
                  Path('/dev/null'), Path('/dev/urandom'), Path('/dev/random')]
    def item(kind, path):
        return f'({kind} {json.dumps(str(path.resolve()))})'
    rules = ['(version 1)', '(deny default)', '(allow file-read-metadata)', '(allow sysctl-read)',
             '(allow mach-lookup (global-name "com.apple.system.opendirectoryd.libinfo"))',
             '(allow file-read-data file-map-executable ' + ' '.join(item('subpath', p) for p in read_dirs) + ' ' +
             ' '.join(item('literal', p) for p in read_files) + ')',
             '(allow file-write* ' + item('subpath', runtime) + ')',
             '(allow file-read* file-write* (literal "/dev/null") (literal "/dev/stdin") (literal "/dev/stdout") (literal "/dev/stderr"))',
             '(allow process-exec ' + ' '.join(item('literal', Path(p)) for p in {binary, node}) + ')',
             # deny-default covers fork, network including loopback, and other files.
             '(deny network*)', '(deny process-fork)']
    return '\n'.join(rules) + '\n'


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
    evidence = {'status': 'BLOCKED', 'errors': [], 'profileSha256': digest(profile)}
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


def run_round(output, runtime, profile, isolation, side, provenance, timeout):
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
            # Node 运行时实验特性警告（如 SQLite ExperimentalWarning，含每次不同的 PID）不是产品输出；
            # 只关这一类运行时警告，产品自身 stderr 不过滤（specs/cli-parity/normalization.md 第 6 条）
            command.append('--disable-warning=ExperimentalWarning')
            command.append(str(Path(provenance['baseline']['path']) / 'dist/src/cli/pilotdeck.js'))
        command.extend(scenario['argv'])
        env = environment(runtime, scenario)
        record, streams, events = capture(['/usr/bin/sandbox-exec', '-f', str(profile), *command],
                                           env, runtime / 'project', bytes.fromhex(scenario['stdinHex']), timeout)
        record.update(argv=scenario['argv'], executedArgv=command, stdinHex=scenario['stdinHex'],
                      cwd=str(runtime / 'project'), env=env_summary(env))
        for name, value in streams.items():
            (case / f'{name}.bin').write_bytes(value)
        save(case / 'events.json', events)
        save(case / 'record.json', record)
        save(case / 'final.json', snapshot(runtime))
        runtime.rename(case / 'tree')
        print(f'{side} {scenario["id"]}: {record["status"]} exit={record["exitCode"]}', flush=True)
        if not record['audit']['allStopped']:
            raise CleanupBlocked(f'{side}/{scenario["id"]}: cleanup failed; retaining shared lock')
        if record['status'] != 'DONE':
            raise Blocked(f'{side}/{scenario["id"]}: incomplete recording')
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
    output = output_path(args.output)
    output.mkdir(parents=True)
    summary = {'status': 'BLOCKED', 'scope': 'D0-pr1 calibration; no command migration claim'}
    try:
        baseline = baseline_path(args.baseline)
        if not args.node:
            raise Blocked('Node executable not found')
        node = executable(args.node)
        identity = baseline_identity(baseline)
        provenance = {'executable': node, 'sourceHead': HEAD, 'baseline': identity}
        runtime = output / 'runtime'
        binary = executable(args.rust_bin) if args.rust_bin else node
        profile = output / 'sandbox.sb'
        profile.write_text(sandbox_profile(runtime, baseline, binary['path'], node['path']))
        with parity_lock() as owner:
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
                    rust_provenance = {'executable': binary, 'sourceHead': git_head(args.rust_source),
                                       'sourcePath': str(args.rust_source.resolve()), 'baseline': identity}
                    candidate = run_round(output, runtime, profile, isolation, 'rust', rust_provenance, args.timeout)
                    summary['rustComparison'] = compare(first, candidate)
                    if summary['rustComparison']['status'] != 'PASS':
                        summary['status'] = 'FAIL'
            # Ensure baseline executable and observed source/build were unchanged during recording.
            if executable(args.node) != node or baseline_identity(baseline) != identity:
                raise Blocked('baseline/runtime changed during recording')
            if args.rust_bin and executable(args.rust_bin) != binary:
                raise Blocked('Rust executable changed during recording')
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        summary['status'] = 'BLOCKED'
        summary['error'] = str(error)
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
