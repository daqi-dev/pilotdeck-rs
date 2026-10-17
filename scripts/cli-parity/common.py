"""D0 recording contract; deliberately independent of the frozen rulers."""
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = Path(__file__).resolve().parent
HEAD = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
LOCK = Path('/tmp/pdrs-parity.lock')
TOKEN = 'd0-fixture-not-a-secret'
DIRS = ('home', 'pilot', 'project', 'tmp', 'bin', 'xdg-config', 'xdg-cache', 'xdg-data', 'xdg-state')
FILES = {'pilot/pilotdeck.yaml': b'schemaVersion: 1\n', 'project/keep.txt': b'D0 fixture\x00\xff\n'}
SCENARIOS = json.loads((SCRIPT / 'scenarios.json').read_text())


class Blocked(RuntimeError):
    pass


class CleanupBlocked(Blocked):
    """An owned process may remain: do not hand the shared lock to another run."""


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def hashes():
    paths = list(SCRIPT.glob('*.py')) + list(SCRIPT.glob('*.mjs')) + [SCRIPT / 'scenarios.json']
    paths += list((ROOT / 'specs/cli-parity').glob('*.md'))
    return {p.relative_to(ROOT).as_posix(): digest(p) for p in sorted(paths)}


def baseline_path(explicit=None, environ=None):
    env = os.environ if environ is None else environ
    value = explicit if explicit is not None else (env.get('PDRS_BASELINE') or ROOT / 'resources/PilotDeck')
    path = Path(value).expanduser().resolve()
    if not (path / 'dist/src/cli/pilotdeck.js').is_file():
        raise Blocked(f'baseline missing: {path}; use --baseline or PDRS_BASELINE')
    return path


def output_path(path):
    path = Path(path).absolute()
    # Reject links, including existing parents; never write through a redirect.
    if path != path.resolve() or not path.is_relative_to(ROOT / '_tmp') or path == ROOT / '_tmp':
        raise Blocked('output must be a non-symlink child of this checkout _tmp')
    if path.exists():
        raise Blocked(f'refusing to overwrite evidence: {path}')
    return path


def fixture(path):
    path.mkdir(mode=0o700)
    for name in DIRS:
        (path / name).mkdir(mode=0o700)
    for name, data in FILES.items():
        (path / name).write_bytes(data)
        (path / name).chmod(0o600)


def snapshot(root):
    """lstat only; neither follow symlinks nor open special files."""
    if root.is_symlink() or not root.is_dir():
        raise ValueError('runtime root missing or symlink')
    result = {'.': {'type': 'directory', 'mode': stat.S_IMODE(root.stat().st_mode)}}
    def visit(folder):
        for path in sorted(folder.iterdir()):
            info = path.lstat()
            row = {'mode': stat.S_IMODE(info.st_mode)}
            if stat.S_ISLNK(info.st_mode):
                target = os.readlink(path)
                if Path(target).is_absolute() or not path.resolve().is_relative_to(root.resolve()):
                    raise ValueError(f'outside symlink: {path.relative_to(root)}')
                row.update(type='symlink', target=target)
            elif stat.S_ISDIR(info.st_mode):
                row['type'] = 'directory'
                visit(path)
            elif stat.S_ISREG(info.st_mode):
                if info.st_nlink != 1:
                    raise ValueError(f'hardlink: {path.relative_to(root)}')
                row.update(type='file', size=info.st_size, sha256=digest(path))
            else:
                raise ValueError(f'special node: {path.relative_to(root)}')
            result[path.relative_to(root).as_posix()] = row
    visit(root)
    return dict(sorted(result.items()))


def initial_tree():
    result = {name: {'mode': 0o700, 'type': 'directory'} for name in DIRS}
    result['.'] = {'mode': 0o700, 'type': 'directory'}
    result.update({name: {'mode': 0o600, 'type': 'file', 'size': len(data),
                         'sha256': hashlib.sha256(data).hexdigest()} for name, data in FILES.items()})
    return dict(sorted(result.items()))


def environment(runtime, scenario):
    paths = {'HOME': 'home', 'USERPROFILE': 'home', 'CFFIXED_USER_HOME': 'home',
             'PILOT_HOME': 'pilot', 'PILOTDECK_CONFIG_PATH': 'pilot/pilotdeck.yaml',
             'XDG_CONFIG_HOME': 'xdg-config', 'XDG_CACHE_HOME': 'xdg-cache',
             'XDG_DATA_HOME': 'xdg-data', 'XDG_STATE_HOME': 'xdg-state',
             'TMPDIR': 'tmp', 'TMP': 'tmp', 'TEMP': 'tmp', 'PWD': 'project', 'PATH': 'bin'}
    env = {key: str(runtime / value) for key, value in paths.items()}
    env.update(LANG='C', LC_ALL='C', TZ='UTC', TERM='dumb', NO_COLOR='1',
               ANALYTICS_ENABLED='0', DOCKER_CONTAINER='1', COMMIT_HASH=HEAD,
               PILOTDECK_GATEWAY_PORT='28999', PILOTDECK_GATEWAY_HOST='127.0.0.1',
               PILOTDECK_GATEWAY_URL='ws://127.0.0.1:28999/ws',
               PILOTDECK_SDK_GATEWAY_URL='ws://127.0.0.1:28999/ws')
    if scenario['token']:
        env['PILOTDECK_SDK_AUTH_TOKEN'] = TOKEN
    return env


def env_summary(env):
    return {key: ({'redacted': True, 'length': len(value),
                   'sha256': hashlib.sha256(value.encode()).hexdigest()} if 'TOKEN' in key else value)
            for key, value in sorted(env.items())}


@contextmanager
def parity_lock(lock=LOCK):
    """Cooperate with existing .guard + mkdir contract; never steal a lock."""
    mine = {'pid': os.getpid(), 'timestamp': time.time(), 'token': uuid.uuid4().hex}
    with lock.with_name(lock.name + '.guard').open('a') as guard:
        fcntl.flock(guard, fcntl.LOCK_EX)
        try:
            lock.mkdir()
        except FileExistsError as error:
            raise Blocked(f'parity lock held: {lock}') from error
        try:
            save(lock / 'owner.json', mine)
        except BaseException:
            lock.rmdir()
            raise
        finally:
            fcntl.flock(guard, fcntl.LOCK_UN)
    release = True
    try:
        yield mine
    except CleanupBlocked:
        release = False
        raise
    finally:
        with lock.with_name(lock.name + '.guard').open('a') as guard:
            fcntl.flock(guard, fcntl.LOCK_EX)
            if json.loads((lock / 'owner.json').read_text()) != mine:
                raise Blocked('lock ownership changed; retaining lock')
            if release:
                (lock / 'owner.json').unlink()
                lock.rmdir()
