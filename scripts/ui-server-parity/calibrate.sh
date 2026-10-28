#!/usr/bin/env bash
# Run OUTSIDE the no-listen sandbox. All captures use the one fixed slot.
set -euo pipefail
B2_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# argparse handles read-only help/preflight before taking the capture lock.
if [[ "${1:-}" == --help || "${1:-}" == -h || "${1:-}" == --preflight ]]; then
  exec python3 -I "$B2_SCRIPT_DIR/calibrate.py" "$@"
fi
# #209 B2: 锁只在本次启动的会话组被证实停止后释放。以下内联监管器先发布
# cleanupPending，再在独立 POSIX 会话里启动 calibrate.py；SIGINT/SIGTERM 先
# killpg 本次会话组并做有界 TERM→KILL→wait→killpg(0) 证实，未证实的残留组
# 原子写进 owner.json 并以 BLOCKED(2) 退出。租约仍是既有目录 + .guard flock。
exec python3 -I - "$B2_SCRIPT_DIR/calibrate.py" "$@" <<'PY'
"""B2 lock/signal supervisor for calibrate.py (inline in calibrate.sh)."""
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import uuid

LOCK = Path('/tmp/pdrs-parity.lock')
DEFERRED = {signal.SIGINT, signal.SIGTERM}
LIVE_GROUPS = set()
BLOCKED = 2
RECEIVED = 0


@contextmanager
def deferred_signals():
    """Block (not ignore) SIGINT/SIGTERM; queued signals are delivered on exit."""
    old = signal.pthread_sigmask(signal.SIG_BLOCK, DEFERRED)
    try:
        yield
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, old)


def _unblock_in_child():
    signal.pthread_sigmask(signal.SIG_UNBLOCK, DEFERRED)


def publish(path, value):
    """Atomically publish the interprocess owner record (never an empty read)."""
    temporary = path.with_name(path.name + f'.tmp.{os.getpid()}')
    temporary.write_text(json.dumps(value) + '\n')
    temporary.replace(path)


def read_owner(lock):
    try:
        owner = json.loads((lock / 'owner.json').read_text())
        if (type(owner['pid']) is not int or owner['pid'] <= 0
                or not isinstance(owner['token'], str)):
            raise ValueError('invalid lock owner')
        return owner
    except (OSError, ValueError, TypeError, KeyError):
        return None


@contextmanager
def lock_guard(lock):
    with lock.with_name(lock.name + '.guard').open('a') as guard:
        fcntl.flock(guard, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(guard, fcntl.LOCK_UN)


def acquire(lock=LOCK):
    mine = {'pid': os.getpid(), 'timestamp': time.time(), 'token': uuid.uuid4().hex,
            'cleanupPending': True}
    with deferred_signals(), lock_guard(lock):
        if lock.exists() or lock.is_symlink():
            raise FileExistsError(str(lock))
        lock.mkdir(mode=0o700)
        try:
            publish(lock / 'owner.json', mine)
        except BaseException:
            shutil.rmtree(lock)
            raise
    return mine


def release(lock, mine):
    """Delete our lease only when every group started here is proven stopped."""
    with deferred_signals(), lock_guard(lock):
        if read_owner(lock) != mine:
            return False
        release = not LIVE_GROUPS
        if release:
            (lock / 'owner.json').unlink()
            lock.rmdir()
        else:
            publish(lock / 'owner.json', dict(mine, cleanupPending=sorted(LIVE_GROUPS)))
        return release


def spawn(script, argv, **kwargs):
    proc = None
    try:
        with deferred_signals():
            proc = subprocess.Popen([sys.executable, '-I', str(script), *argv],
                                    start_new_session=True,
                                    preexec_fn=_unblock_in_child, **kwargs)
            LIVE_GROUPS.add(proc.pid)  # start_new_session: pid 即 pgid
    except BaseException:
        if proc is not None:
            stop_group(proc)
        raise
    return proc


def stop_group(proc):
    with deferred_signals():
        audit = _stop_group(proc)
        if audit['allStopped']:
            LIVE_GROUPS.discard(proc.pid)
        return audit


def _stop_group(proc):
    """Bounded TERM -> KILL -> wait -> final ESRCH; any error is unproven."""
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
    return {'allStopped': not errors and not remaining, 'remaining': remaining,
            'errors': errors}


def _audit(proc):
    if proc is None:
        return {'allStopped': True, 'remaining': [], 'errors': []}
    try:
        return stop_group(proc)
    except BaseException as error:
        return {'allStopped': False, 'remaining': sorted(LIVE_GROUPS),
                'errors': [f'{type(error).__name__}: {error}']}


def _interrupted(signum, frame):
    global RECEIVED
    RECEIVED = signum
    raise KeyboardInterrupt(f'signal {signum}')


def run(script, argv, lock=LOCK):
    mine = None
    try:
        # Hold the queued signal only across acquisition; once mine is
        # published, deliver it here and release before propagating.
        with deferred_signals():
            mine = acquire(lock)
    except BaseException:
        if mine is not None:
            release(lock, mine)
        raise
    proc = None
    code = BLOCKED
    try:
        try:
            proc = spawn(script, argv, stdin=subprocess.DEVNULL)
            code = proc.wait()
            audit = _audit(proc)
            if not audit['allStopped']:
                code = BLOCKED
            elif code is not None and code < 0:
                code = 128 - code
        except BaseException as error:
            audit = _audit(proc)
            remaining = sorted(set(audit['remaining']) | LIVE_GROUPS)
            if remaining:
                code = BLOCKED
                reason = f'owned process groups not proven stopped: {", ".join(map(str, remaining))}'
            elif isinstance(error, KeyboardInterrupt):
                code = 128 + (RECEIVED or signal.SIGINT)
                reason = str(error)
            else:
                code = BLOCKED
                reason = f'{type(error).__name__}: {error}'
            print(json.dumps({'status': 'BLOCKED' if code == BLOCKED else 'INTERRUPTED',
                              'reason': reason, 'ownedGroupsRemaining': remaining,
                              'cleanup': audit}, ensure_ascii=False), file=sys.stderr, flush=True)
        return code
    finally:
        # A failed release must stay loud and leave the owner record for a
        # human; never return calibrate's success code with a lease still held.
        release(lock, mine)


def main(argv):
    if not argv:
        print('usage: calibrate.sh <calibrate.py> [args...]', file=sys.stderr)
        return BLOCKED
    script = Path(argv[0]).resolve()
    if not script.is_file():
        print(json.dumps({'status': 'BLOCKED', 'reason': f'missing script: {script}'},
                         ensure_ascii=False), file=sys.stderr)
        return BLOCKED
    signal.signal(signal.SIGINT, _interrupted)
    signal.signal(signal.SIGTERM, _interrupted)
    try:
        return run(script, argv[1:])
    except FileExistsError:
        print(f'REFUSED: {LOCK} already held; do not remove another runner lock',
              file=sys.stderr)
        return BLOCKED
    except KeyboardInterrupt as error:
        # A queued signal delivered at the acquisition boundary arrives after
        # the inner release; report it without leaving the lease behind.
        print(json.dumps({'status': 'INTERRUPTED', 'reason': str(error),
                          'ownedGroupsRemaining': []}, ensure_ascii=False),
              file=sys.stderr, flush=True)
        return 128 + (RECEIVED or signal.SIGINT)


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
PY
