"""Shared safety/IO helpers. Gateway ruler does not modify the frozen C ruler."""
from contextlib import contextmanager
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import signal
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = Path(__file__).resolve().parent
DEFAULT_OUT = ROOT / '_tmp/20261014-exp006'
SCENARIOS = ['auth', 'turn', 'readonly', 'reconnect', 'permission', 'writes', 'http', 'quirks']
READ_METHODS = ['native_archive_manifest', 'native_archive_entries', 'native_archive_artifact',
                'project_files_list', 'project_file_read', 'model_catalog_list',
                'session_model_get', 'supported_agents', 'list_projects', 'describe_project']
LOCK = Path('/tmp/pdrs-parity.lock')
MAX_AGE = 2400


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


# A5.3 registers E1 adapters here so A2 continues to reject unknown scripts.
# The new hashes require coordinator review/registration before merge (plan 005).
A2_SCRIPTS = (
    'check_frozen.py', 'check_ruler.py', 'common.py', 'compare.py',
    'controls/run_all.py', 'controls/prerequisites.py', 'driver.mts', 'mock.mjs',
    'observer.py', 'run_round.py', 'test_safety.py', 'watcher.mjs',
    'session_domain.py', 'session_observations.mjs', 'controls/session_domain.py',
)
A3_SCRIPTS = ('check_schema.py', 'export_schema.mjs', 'validate_fixtures.mjs')


def apparatus_hashes():
    """Reject unregistered scripts, then hash A2; missing members also fail."""
    scripts = set()
    for folder, dirs, files in os.walk(SCRIPT):
        dirs[:] = [name for name in dirs if name != 'node_modules']
        for name in files:
            path = Path(folder) / name
            if path.suffix in ('.py', '.mjs', '.mts'):
                scripts.add(path.relative_to(SCRIPT).as_posix())
    unknown = scripts - set(A2_SCRIPTS) - set(A3_SCRIPTS)
    if unknown:
        raise ValueError(f'unregistered gateway-parity scripts: {sorted(unknown)}')
    return {(SCRIPT / name).relative_to(ROOT).as_posix(): digest(SCRIPT / name)
            for name in sorted(A2_SCRIPTS)}


def owner(lock):
    try:
        data = json.loads((lock / 'owner.json').read_text())
        if type(data['pid']) is not int or data['pid'] <= 0 or not math.isfinite(data['timestamp']):
            return None
        return data
    except (OSError, ValueError, TypeError, KeyError):
        return None


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass
    return True


@contextmanager
def guard(lock):
    with lock.with_name(lock.name + '.guard').open('a') as file:
        fcntl.flock(file, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(file, fcntl.LOCK_UN)


@contextmanager
def parity_lock(timeout=600, takeovers=None, lock=LOCK):
    """Same owner/guard/stale takeover contract as scripts/parity/round.py.

    Entire k-round calibration holds the lock, including rename/snapshot.
    A run is bounded to 900s, comfortably below the 2400s stale lease.
    """
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValueError('lock timeout must be positive and finite')
    mine = {'pid': os.getpid(), 'timestamp': time.time(), 'token': uuid.uuid4().hex}
    deadline = time.monotonic() + timeout
    announced = False
    while True:
        with guard(lock):
            if lock.exists():
                previous = owner(lock)
                stamp = previous['timestamp'] if previous else lock.stat().st_mtime
                reason = ('dead_pid' if previous and not alive(previous['pid']) else
                          'expired' if time.time() - stamp > MAX_AGE else None)
                if reason:
                    event = {'reason': reason, 'previous_owner': previous, 'timestamp': time.time()}
                    shutil.rmtree(lock)
                    if takeovers is not None:
                        takeovers.append(event)
            if not lock.exists():
                lock.mkdir()
                # Lease age begins at acquisition, not before a long wait.
                mine['timestamp'] = time.time()
                try:
                    save(lock / 'owner.json', mine)
                except BaseException:
                    shutil.rmtree(lock)
                    raise
                break
        if time.monotonic() >= deadline:
            raise TimeoutError(f'parity lock wait exceeded {timeout}s')
        if not announced:
            print(f'waiting for {lock}', file=sys.stderr, flush=True)
            announced = True
        # Other suites may release and immediately launch their next round.
        # Keep the polling window shorter than that gap to avoid starvation;
        # ownership, lease and guard rules remain identical to the C lock.
        time.sleep(.1)
    try:
        yield
    finally:
        with guard(lock):
            if owner(lock) == mine:
                shutil.rmtree(lock)


def install_shutdown():
    def interrupt(signum, frame):
        raise KeyboardInterrupt(f'signal {signum}: cleaning owned groups and releasing lock')
    signal.signal(signal.SIGTERM, interrupt)
