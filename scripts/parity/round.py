#!/usr/bin/env python3
"""Run one isolated Original parity round and record sidecar identity.

Source (copied before editing): the prior attempt's CORE01 harness formal-round.py,
outside this repository and never read at runtime.
Source sha256: 854d40a0c444bc9839e143c94b3137ccaf6db3652a4b17ceae0d55c8c22269b6
Identity-only bind.mjs observes spawns; production env selects the binary.
"""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it, exit before loading any apparatus module.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-parity-pyc-')
sys.dont_write_bytecode = True

import argparse
from contextlib import contextmanager
import fcntl
import importlib.util
import json
import math
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import signal
import time
import uuid

_HERE = Path(__file__).resolve().parent


def _load(name, path):
    """Load an apparatus module by explicit path; never resolve via sys.path."""
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


G = _load('g03_compare', _HERE / 'g03_compare.py')

LOCK = Path('/tmp/pdrs-parity.lock')
LOCK_MAX_AGE = 40 * 60
LOCK_MARGIN = 5 * 60
# 本次启动、尚未被 stop_group 证实全部停止的进程组（pgid）。锁能否释放只看它，
# 不看异常类型；#209 A1 与 cli-parity / gateway-parity A2 同一判据。
LIVE_GROUPS: set[int] = set()
DEFERRED = {signal.SIGINT, signal.SIGTERM}


@contextmanager
def deferred_signals():
    """挂起（非忽略）SIGINT/SIGTERM：期间到达的信号排队，解除后立即投递。

    清理与「进程组已建未登记」窗口里中断不会丢失，也不会落在登记完成之前。
    """
    old = signal.pthread_sigmask(signal.SIG_BLOCK, DEFERRED)
    try:
        yield
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, old)


def _unblock_in_child():
    # 子进程继承挂起掩码；exec 前解除，run.py 及其子进程按正常信号语义运行。
    signal.pthread_sigmask(signal.SIG_UNBLOCK, DEFERRED)


def publish(path, value):
    """原子发布进程间 owner/信号记录，读方不会读到半截或空串（#166 r8）。"""
    temporary = path.with_name(path.name + f'.tmp.{os.getpid()}')
    temporary.write_text(json.dumps(value) + '\n')
    temporary.replace(path)


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


def spawn(cmd, **kwargs):
    """启动独立会话并登记；Popen 与登记在同一挂起窗口内完成（#209 A1）。

    解除屏蔽时排队的信号会在返回后投递；异常路径先回收已登记的组再上抛，
    调用者不会接手一个「已登记却拿不到句柄」的孤儿组。回收失败则保持登记，
    锁保留（fail-closed）。
    """
    proc = None
    try:
        with deferred_signals():
            proc = subprocess.Popen(cmd, start_new_session=True,
                                    preexec_fn=_unblock_in_child, **kwargs)
            LIVE_GROUPS.add(proc.pid)  # start_new_session: pid 即 pgid
    except BaseException:
        if proc is not None:
            stop_group(proc)
        raise
    return proc


def stop_group(proc):
    """清理期间挂起信号；只有证实整组停止才从登记移除（#209 A1）。"""
    with deferred_signals():
        audit = _stop_group(proc)
        if audit['allStopped']:
            LIVE_GROUPS.discard(proc.pid)
        return audit


def _stop_group(proc):
    """有界 TERM→KILL→wait→最终 ESRCH 探测；任一步失败都算未证实。

    与 cli-parity 的 `_stop_group` 同一顺序和上限；OSError 不跳过后续步骤，
    但会进入 errors，使 allStopped=False，从而保留锁。
    """
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
    # 最终探测留 2s 让被 reparent 的僵尸被 init/launchd 收走；只有 ESRCH
    # 才算证实，超时或探测错误都按残留处理。
    remaining = []
    deadline = time.monotonic() + 2
    while True:
        try:
            os.killpg(proc.pid, 0)
        except ProcessLookupError:
            break
        except OSError as error:
            errors.append(str(error))
            remaining.append(proc.pid)
            break
        if time.monotonic() >= deadline:
            remaining.append(proc.pid)
            break
        time.sleep(.02)
    return {'allStopped': not errors and not remaining, 'remaining': remaining, 'errors': errors}


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
    # 获取时先发布 cleanupPending：SIGKILL/OOM 或后续残留记录写入失败都不会留下
    # 可被 dead_pid/expired 自动接管的空标记（#209 A1，同 gateway-parity #990）。
    owner = {'pid': os.getpid(), 'timestamp': time.time(), 'token': uuid.uuid4().hex,
             'cleanupPending': True}
    announced = False
    try:
        while True:
            acquired = False
            with deferred_signals(), lock_guard(lock):
                if lock.exists():
                    previous = read_owner(lock)
                    timestamp = previous['timestamp'] if previous else lock.stat().st_mtime
                    reason = ('dead_pid' if previous and not pid_alive(previous['pid']) else
                              'expired' if time.time() - timestamp > LOCK_MAX_AGE else None)
                    # 损坏的已发布 owner 可能是中断的 cleanupPending 写入；只有从未发布
                    # 的陈旧锁可按 mtime 回收，已发布的损坏 owner 必须人工核实。
                    damaged = previous is None and (lock / 'owner.json').exists()
                    # 带 cleanupPending 的锁表示上一次运行可能还有存活进程组，任何装置
                    # 都不得自动接管；核实原 owner 与残留组后再人工回收。
                    if reason and not damaged and not (previous and previous.get('cleanupPending')):
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
                        publish(lock / 'owner.json', owner)
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
        yield
    finally:
        # 释放也挂起信号：清理/残留记录必须完整落盘后才投递中断。只有本次启动
        # 的进程组全部被 stop_group 证实停止，才允许删除本 owner 的共享锁。
        with deferred_signals(), lock_guard(lock):
            # An over-age owner may finish after another process took over.
            # It must not delete the replacement owner's lock.
            if read_owner(lock) == owner:
                release = not LIVE_GROUPS
                if release:
                    shutil.rmtree(lock)
                else:
                    publish(lock / 'owner.json', dict(owner, cleanupPending=sorted(LIVE_GROUPS)))


def _decode_stream(value):
    if isinstance(value, bytes):
        return value.decode('utf-8', errors='replace')
    return value or ''


def _communicate_bounded(child, timeout=5):
    """Reap pipes after the group was killed without waiting forever.

    A setsid() descendant can hold the inherited pipes after the group signal;
    bound the collection and keep whatever bytes were already read (#528).
    """
    try:
        return child.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        for stream in (child.stdout, child.stderr):
            if stream is not None:
                stream.close()
        try:
            child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            pass
        return _decode_stream(error.output), _decode_stream(error.stderr)


def _save_run(out, run):
    temporary = out / f'run.json.tmp.{os.getpid()}'
    temporary.write_text(json.dumps(run, indent=2, ensure_ascii=False) + '\n')
    temporary.replace(out / 'run.json')


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
        _save_run(out, run)
        started = time.monotonic()
        child = None
        try:
            try:
                # 独立进程组：超时/中断时连同 run.py 拉起的 node / mock 子进程一起
                # 回收，不留孤儿继续写 artifacts（PR #13 第 2 轮评审 P3，#209 A1）。
                child = spawn(cmd, cwd=G.TOOLS, env=env, text=True, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE)
                try:
                    stdout, stderr = child.communicate(timeout=args.timeout_seconds)
                except subprocess.TimeoutExpired:
                    stop_group(child)
                    stdout, stderr = _communicate_bounded(child)
                    raise subprocess.TimeoutExpired(cmd, args.timeout_seconds,
                                                    output=stdout, stderr=stderr)
            except subprocess.TimeoutExpired as error:
                audit = (stop_group(child) if child is not None else
                         {'allStopped': True, 'remaining': [], 'errors': []})
                for filename, output in (('run.py.stdout', error.stdout), ('run.py.stderr', error.stderr)):
                    (out / filename).write_text(_decode_stream(output))
                remaining = sorted(set(audit['remaining']) | LIVE_GROUPS)
                blocked = [f'run.py timed out after {args.timeout_seconds:g} seconds']
                if remaining:
                    blocked.append('owned process groups not proven stopped: ' +
                                   ', '.join(map(str, remaining)))
                run.update(run_py_exit=None, elapsed_s=round(time.monotonic() - started, 3),
                           identity_ok=False, valid_round=False, status='BLOCKED',
                           blocked=blocked, cleanup=audit, ownedGroupsRemaining=remaining)
                _save_run(out, run)
                print(json.dumps({k: run[k] for k in ('side', 'label', 'status', 'blocked', 'elapsed_s', 'valid_round')}, ensure_ascii=False))
                return 1  # parity_lock releases only when the group audit proved absence.
            # 正常退出也证实本次会话组已消失：仍有组员则先有界回收，回收未证实
            # 就保留共享锁，不能把可能还活着的组交给下一次运行（#209 A1）。
            audit = stop_group(child)
            remaining = sorted(set(audit['remaining']) | LIVE_GROUPS)
            if not audit['allStopped'] or remaining:
                run.update(run_py_exit=child.returncode, elapsed_s=round(time.monotonic() - started, 3),
                           identity_ok=False, valid_round=False, status='BLOCKED',
                           blocked=['process-group cleanup not proven after run.py: ' +
                                    (', '.join(map(str, remaining)) or 'unknown')],
                           cleanup=audit, ownedGroupsRemaining=remaining)
                _save_run(out, run)
                print(json.dumps({k: run[k] for k in ('side', 'label', 'status', 'blocked', 'elapsed_s', 'valid_round')}, ensure_ascii=False))
                return 1
            proc = subprocess.CompletedProcess(cmd, child.returncode, stdout, stderr)
        except BaseException as error:
            # KeyboardInterrupt / SIGTERM 转成的中断，或 spawn 失败：先回收本次
            # 登记的会话组并证实停止，再让 parity_lock 决定是否释放锁。
            try:
                audit = (stop_group(child) if child is not None else
                         {'allStopped': True, 'remaining': [], 'errors': []})
            except BaseException as cleanup_error:
                audit = {'allStopped': False, 'remaining': sorted(LIVE_GROUPS),
                         'errors': [f'{type(cleanup_error).__name__}: {cleanup_error}']}
            remaining = sorted(set(audit['remaining']) | LIVE_GROUPS)
            blocked = [f'interrupted by {type(error).__name__}: {error}']
            if remaining:
                blocked.append('owned process groups not proven stopped: ' +
                               ', '.join(map(str, remaining)))
            run.update(run_py_exit=None, elapsed_s=round(time.monotonic() - started, 3),
                       identity_ok=False, valid_round=False, status='BLOCKED',
                       blocked=blocked, cleanup=audit, ownedGroupsRemaining=remaining)
            _save_run(out, run)
            print(json.dumps({k: run[k] for k in ('side', 'label', 'status', 'blocked', 'elapsed_s', 'valid_round')}, ensure_ascii=False))
            return 1
        (out / 'run.py.stdout').write_text(proc.stdout)
        (out / 'run.py.stderr').write_text(proc.stderr)
        run.update(run_py_exit=proc.returncode, elapsed_s=round(time.monotonic() - started, 3),
                   cleanup=audit)
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
        _save_run(out, run)
        print(json.dumps({k: run[k] for k in ('side', 'label', 'run_py_exit', 'elapsed_s', 'identity_ok', 'valid_round')}, ensure_ascii=False))
        return 0 if run['valid_round'] else 1


if __name__ == '__main__':
    # SIGTERM 与 Ctrl-C 走同一条「先回收本次会话组、证实停止后才释放锁」路径。
    def interrupted(signum, frame):
        raise KeyboardInterrupt(f'signal {signum}')

    signal.signal(signal.SIGTERM, interrupted)
    sys.exit(main())
