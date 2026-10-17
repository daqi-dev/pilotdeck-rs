#!/usr/bin/env python3
"""Run one isolated Original parity round and record sidecar identity.

Source (copied before editing): the prior attempt's CORE01 harness formal-round.py,
outside this repository and never read at runtime.
Source sha256: 854d40a0c444bc9839e143c94b3137ccaf6db3652a4b17ceae0d55c8c22269b6
Identity-only bind.mjs observes spawns; production env selects the binary.
"""
import argparse
from contextlib import contextmanager
import fcntl
import json
import math
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import signal
import time
import uuid

import g03_compare as G

LOCK = Path('/tmp/pdrs-parity.lock')
LOCK_MAX_AGE = 40 * 60
LOCK_MARGIN = 5 * 60


def read_owner(lock):
    try:
        owner = json.loads((lock / 'owner.json').read_text())
        if type(owner['pid']) is not int or owner['pid'] <= 0 or not math.isfinite(owner['timestamp']):
            raise ValueError('invalid lock owner')
        return owner
    except (OSError, ValueError, TypeError, KeyError):
        return None


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass  # A process owned by another user still holds the lock.
    return True


@contextmanager
def lock_guard(lock):
    # Serialize mkdir/owner publication, takeover and release. Keep the guard
    # inode stable so concurrent waiters cannot lock different guard files.
    with lock.with_name(lock.name + '.guard').open('a') as guard:
        fcntl.flock(guard, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(guard, fcntl.LOCK_UN)


@contextmanager
def parity_lock(lock=LOCK, takeovers=None):
    owner = {'pid': os.getpid(), 'timestamp': time.time(), 'token': uuid.uuid4().hex}
    announced = False
    while True:
        acquired = False
        with lock_guard(lock):
            if lock.exists():
                previous = read_owner(lock)
                timestamp = previous['timestamp'] if previous else lock.stat().st_mtime
                reason = ('dead_pid' if previous and not pid_alive(previous['pid']) else
                          'expired' if time.time() - timestamp > LOCK_MAX_AGE else None)
                if reason:
                    event = {'event': 'lock_takeover', 'lock': str(lock), 'reason': reason,
                             'previous_owner': previous, 'timestamp': time.time(), 'pid': os.getpid()}
                    shutil.rmtree(lock)
                    print(json.dumps(event), file=sys.stderr, flush=True)
                    if takeovers is not None:
                        takeovers.append(event)
            if not lock.exists():
                lock.mkdir()
                owner['timestamp'] = time.time()
                try:
                    (lock / 'owner.json').write_text(json.dumps(owner) + '\n')
                except BaseException:
                    shutil.rmtree(lock)
                    raise
                acquired = True
        if acquired:
            break
        if not announced:
            print(f'waiting for {lock}', file=sys.stderr, flush=True)
            announced = True
        time.sleep(2)
    try:
        yield
    finally:
        with lock_guard(lock):
            # An over-age owner may finish after another process took over.
            # It must not delete the replacement owner's lock.
            if read_owner(lock) == owner:
                shutil.rmtree(lock)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--side', choices=['ts', 'rust'], required=True)
    ap.add_argument('--label', required=True)
    ap.add_argument('--rust-binary', type=Path)
    ap.add_argument('--expected-sha256')
    ap.add_argument('--scenario', default='all')
    ap.add_argument('--node', default=os.environ.get('PDRS_NODE', 'node'))
    ap.add_argument('--adapter-timeout-seconds', default='30')
    ap.add_argument('--timeout-seconds', type=float, default=1500,
                    help='run.py wall-clock timeout (default: 1500 seconds)')
    args = ap.parse_args()
    if not math.isfinite(args.timeout_seconds) or args.timeout_seconds <= 0:
        ap.error('--timeout-seconds must be positive and finite')
    # 锁按年龄判陈旧、没有心跳：持锁时长必须明显短于 LOCK_MAX_AGE，否则长跑的活进程会被当成陈旧锁接管
    # （PR #13 第 2 轮评审 P2）。
    if args.timeout_seconds + LOCK_MARGIN >= LOCK_MAX_AGE:
        ap.error(f'--timeout-seconds must be < {LOCK_MAX_AGE - LOCK_MARGIN:g} (LOCK_MAX_AGE - margin)')
    if not args.label or Path(args.label).name != args.label or args.label in ('.', '..'):
        ap.error('--label must be a single directory name')
    catalog = G.scenario_catalog()
    if args.scenario != 'all' and args.scenario not in catalog:
        ap.error('unknown PilotDeck scenario')
    if args.side == 'rust' and (not args.rust_binary or not args.expected_sha256):
        ap.error('--rust-binary and --expected-sha256 required for rust')
    binary = args.rust_binary.resolve() if args.side == 'rust' else G.BASELINE / 'dist/src/cli/pilotdeck-agent-loop-sidecar.js'
    expected = args.expected_sha256 if args.side == 'rust' else G.sha256(binary)
    if G.sha256(binary) != expected:
        ap.error('candidate sha256 mismatch')
    out_root = Path(os.environ.get('PDRS_OUT', str(G.WORKTREE / '_tmp' / (time.strftime('%Y%m%d') + '-parity')))).resolve()
    # All mutable homes and artifacts are within THIS worktree's _tmp.
    if not out_root.is_relative_to((G.WORKTREE / '_tmp').resolve()):
        ap.error('PDRS_OUT must be below this worktree _tmp/')
    out = out_root / args.label
    node = shutil.which(args.node)
    if not node:
        ap.error('node executable not found')
    env = os.environ.copy()
    for key in ('PILOTDECK_CONFIG_PATH', 'PILOTDECK_STORAGE_ROOT', 'PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND',
                'PILOTDECK_AGENT_LOOP_SIDECAR_PATH', 'PILOTDECK_AGENT_LOOP_TCP_HOST', 'PILOTDECK_AGENT_LOOP_TCP_PORT'):
        env.pop(key, None)
    env.update(PDRS_BASELINE=str(G.BASELINE), PDRS_SIDE=args.side, PDRS_EXPECTED_SHA256=expected,
               PILOTDECK_AGENT_LOOP_TRANSPORT='stdio', PATH=str(Path(node).parent) + os.pathsep + env.get('PATH', ''),
               PILOT_HOME=str(out / 'pilot-home'), TMPDIR=str(out / 'tmp'))
    if args.side == 'rust':
        env['PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND'] = str(binary)
    adapter = shlex.join([node, str(Path(__file__).with_name('bind.mjs'))])
    cmd = [sys.executable, str(G.TOOLS / 'run.py'), '--pilotdeck-root', str(G.BASELINE),
           '--comparison', 'same-version', '--surface', 'gateway', '--scenario', args.scenario,
           '--output', str(out / 'artifacts'), '--adapter-timeout-seconds', args.adapter_timeout_seconds,
           '--pilotdeck-sidecar-cmd', adapter]
    takeovers = []
    with parity_lock(takeovers=takeovers):
        out.mkdir(parents=True, exist_ok=False)
        for directory in ('pilot-home', 'tmp'):
            (out / directory).mkdir()
        run = {'side': args.side, 'label': args.label, 'scenario': args.scenario, 'argv': cmd,
               'python_version': sys.version, 'node': node, 'node_sha256': G.sha256(Path(node)), 'node_version': subprocess.check_output([node, '--version'], text=True).strip(),
               'baseline': str(G.BASELINE), 'candidate': str(binary), 'expected_sha256': expected,
               'inputs_sha256': {f: G.sha256(G.TOOLS / f) for f in ('run.py', 'trace.py', 'scenarios.json', 'adapters/pilotdeck_gateway_impl.mjs')},
               'driver_sha256': {p.name: G.sha256(p) for p in (Path(__file__), Path(__file__).with_name('bind.mjs'))},
               'PILOT_HOME': env['PILOT_HOME'], 'TMPDIR': env['TMPDIR'], 'started_wall': time.time(),
               'timeout_seconds': args.timeout_seconds, 'lock_takeovers': takeovers}
        (out / 'run.json').write_text(json.dumps(run, indent=2) + '\n')
        started = time.monotonic()
        try:
            # 独立进程组：超时时连同 run.py 拉起的 node / mock 子进程一起杀掉，不留孤儿继续写 artifacts
            # （PR #13 第 2 轮评审 P3）。
            child = subprocess.Popen(cmd, cwd=G.TOOLS, env=env, text=True, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, start_new_session=True)
            try:
                stdout, stderr = child.communicate(timeout=args.timeout_seconds)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                stdout, stderr = child.communicate()
                raise subprocess.TimeoutExpired(cmd, args.timeout_seconds, output=stdout, stderr=stderr)
            proc = subprocess.CompletedProcess(cmd, child.returncode, stdout, stderr)
        except subprocess.TimeoutExpired as error:
            # TimeoutExpired may carry bytes even with text=True.
            for filename, output in (('run.py.stdout', error.stdout), ('run.py.stderr', error.stderr)):
                if isinstance(output, bytes):
                    output = output.decode('utf-8', errors='replace')
                (out / filename).write_text(output or '')
            run.update(run_py_exit=None, elapsed_s=round(time.monotonic() - started, 3),
                       identity_ok=False, valid_round=False, status='BLOCKED',
                       blocked=[f'run.py timed out after {args.timeout_seconds:g} seconds'])
            (out / 'run.json').write_text(json.dumps(run, indent=2, ensure_ascii=False) + '\n')
            print(json.dumps({k: run[k] for k in ('side', 'label', 'status', 'blocked', 'elapsed_s', 'valid_round')}, ensure_ascii=False))
            return 1  # parity_lock's finally releases the owned lock.
        (out / 'run.py.stdout').write_text(proc.stdout)
        (out / 'run.py.stderr').write_text(proc.stderr)
        run.update(run_py_exit=proc.returncode, elapsed_s=round(time.monotonic() - started, 3))
        identity = {}
        for sid in catalog if args.scenario == 'all' else [args.scenario]:
            trace = out / 'artifacts' / f'{sid}.pilotdeck-current-sidecar.jsonl'
            try:
                records = G.load_aligned(trace)
                proofs = [r for r in records if r.get('kind') == 'harness.proof' and r.get('state') == 'sidecar_identity']
                identity[sid] = len(proofs) == 1 and proofs[0].get('side') == args.side and proofs[0].get('sha256') == expected and proofs[0].get('spawnObserved') is True and not G.trace_health(sid, records, catalog[sid])
            except (OSError, ValueError, TypeError):
                identity[sid] = False
        summary_path = out / 'artifacts' / 'summary.json'
        upstream = json.loads(summary_path.read_text()) if summary_path.exists() else None
        run.update(identity=identity, identity_ok=all(identity.values()) and G.sha256(binary) == expected,
                   upstream_summary=upstream,
                   artifacts={str(p.relative_to(out)): G.sha256(p) for p in (out / 'artifacts').rglob('*') if p.is_file() and '.runtime' not in p.parts})
        # run.py FAIL for native/sidecar semantics is not a sidecar identity failure.
        run['valid_round'] = run['identity_ok'] and proc.returncode in (0, 1) and upstream is not None and not upstream.get('blocked') and not upstream.get('oracleFailures')
        (out / 'run.json').write_text(json.dumps(run, indent=2, ensure_ascii=False) + '\n')
        print(json.dumps({k: run[k] for k in ('side', 'label', 'run_py_exit', 'elapsed_s', 'identity_ok', 'valid_round')}, ensure_ascii=False))
        return 0 if run['valid_round'] else 1


if __name__ == '__main__':
    sys.exit(main())
