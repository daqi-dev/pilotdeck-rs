"""Shared safety/IO helpers. Gateway ruler does not modify the frozen C ruler."""
from contextlib import contextmanager
import fcntl
import hashlib
import importlib.machinery
import json
import math
import os
from pathlib import Path
import shutil
import signal
import subprocess
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


# Executable script suffixes: anything the apparatus could import or execute.
# Python launches .py/.pyw; POSIX shells may launch .sh/.bash/.zsh; Node/tsx
# loads .js/.mjs/.cjs/.jsx/.ts/.mts/.cts/.tsx directly or via its loader. A new
# file with one of these suffixes must be registered before it can affect a run.
SCRIPT_SUFFIXES = ('.py', '.pyw', '.sh', '.bash', '.zsh', '.js', '.mjs', '.cjs',
                   '.jsx', '.ts', '.mts', '.cts', '.tsx')
BYTECODE_SUFFIXES = ('.pyc', '.pyo')
# CPython probes these before falling back to a same-directory ``.py``, so an
# unregistered extension module shadows the registered source at import time.
# importlib.machinery is the authoritative list for this interpreter
# (``.cpython-<abi>-<platform>.so`` / ``.abi3.so`` / ``.so``; ``.pyd`` on
# Windows); the explicit tail keeps the scan complete if a build omits one.
EXTENSION_SUFFIXES = tuple(dict.fromkeys(
    tuple(importlib.machinery.EXTENSION_SUFFIXES) +
    ('.so', '.pyd', '.dylib', '.node')))
# site only reads ``.pth`` from site-package directories (and none under ``-I``),
# so a ``.pth`` beside the apparatus is inert today; reject it anyway because it
# is an executable-input shape that must not appear unregistered.
SITE_SUFFIXES = ('.pth',)


def tree_digest(folder):
    """Hash actual on-disk bytes of a dependency tree, including link targets.

    The root is resolved (pnpm/npm package directories may be symlinks); nested
    directory symlinks are recorded but not followed, matching Node's own
    package resolution boundaries.
    """
    folder = Path(folder).resolve()
    entries = {}
    for current, dirs, files in os.walk(folder, followlinks=False):
        current = Path(current)
        dirs.sort()
        for name in sorted(dirs):
            path = current / name
            if path.is_symlink():
                entries[path.relative_to(folder).as_posix()] = 'link:' + os.readlink(path)
        for name in sorted(files):
            path = current / name
            rel = path.relative_to(folder).as_posix()
            if path.is_symlink():
                entries[rel] = 'link:' + os.readlink(path)
            elif path.is_file():
                entries[rel] = digest(path)
    return hashlib.sha256(json.dumps(entries, sort_keys=True,
                                     separators=(',', ':')).encode()).hexdigest()


def node_modules_entry(script=None):
    """Digest the installed in-tree dependencies, or omit them when absent."""
    folder = (Path(script) if script is not None else SCRIPT) / 'node_modules'
    if not folder.is_dir():
        return None
    return tree_digest(folder)


def unknown_apparatus_files(script=None):
    """Executable/runtime files that are neither A2 apparatus nor A3 tools.

    __pycache__ bytecode is tolerated because CPython only consumes it next to a
    matching source file; a legacy ``.pyc``/``.pyo`` beside the sources could be
    imported on its own, so it is rejected like an unregistered script.
    Extension modules are rejected wherever they sit: CPython consults
    ``EXTENSION_SUFFIXES`` in the same directory *before* ``.py``, so a
    ``compare.cpython-<abi>-<platform>.so`` would run while ``compare.py`` keeps
    its registered hash.  Node native addons (``.node``) and ``.pth`` files are
    rejected for the same executable-input rule.

    Symbolic links are rejected outright outside ``node_modules``, whatever
    their own suffix: ``os.walk`` does not descend into a linked directory, so
    files in its target would escape this scan, and a link can stand in for a
    registered module.  Measured on CPython 3.14: with ``compare.py`` present,
    a valid ``compare -> <dir containing __init__.py>`` link makes ``import
    compare`` load the linked package while ``compare.py`` keeps its registered
    hash, so ``check_frozen``/``apparatus_hashes`` would otherwise PASS.  A
    plain directory named like a registered module is rejected as well: a
    directory with ``__init__.py`` already has that file caught below (measured:
    an empty directory does not shadow ``.py``, since the file suffix loop still
    runs), but reporting the shadow container names the real hazard instead of a
    nested file.  Links inside ``node_modules`` stay under the
    installed-dependency rule: ``tree_digest`` records each as
    ``link:<target>`` and never follows it, so repointing a link changes the
    digest, while the bytes behind it are not bound (documented boundary).
    """
    script = Path(script) if script is not None else SCRIPT
    # Importable names a directory could shadow: module stems, e.g. ``compare``.
    module_names = {Path(name).stem for name in A2_SCRIPTS + A3_SCRIPTS}
    found = set()
    for folder, dirs, files in os.walk(script, followlinks=False):
        current = Path(folder)
        descend = []
        for name in dirs:
            path = current / name
            if name == 'node_modules':
                continue
            if path.is_symlink() or name in module_names:
                found.add(path.relative_to(script).as_posix())
            else:
                descend.append(name)
        dirs[:] = sorted(descend)
        for name in files:
            path = current / name
            rel = path.relative_to(script)
            if path.is_symlink():
                # Covers links to files, broken links, and links whose target
                # is a directory but which os.walk lists as a file.
                found.add(rel.as_posix())
            elif path.suffix in SCRIPT_SUFFIXES:
                if rel.as_posix() not in A2_SCRIPTS and rel.as_posix() not in A3_SCRIPTS:
                    found.add(rel.as_posix())
            elif path.suffix in BYTECODE_SUFFIXES and '__pycache__' not in rel.parts:
                found.add(rel.as_posix())
            elif name.endswith(EXTENSION_SUFFIXES) or name.endswith(SITE_SUFFIXES):
                found.add(rel.as_posix())
    return sorted(found)


# A5.3 registers E1 adapters here so A2 continues to reject unknown scripts.
# The new hashes require coordinator review/registration before merge (plan 005).
A2_SCRIPTS = (
    'build_proxy.py', 'check_frozen.py', 'check_ruler.py', 'common.py', 'compare.py',
    'controls/run_all.py', 'controls/prerequisites.py', 'driver.mts', 'mock.mjs',
    'observer.py', 'run_round.py', 'test_safety.py', 'watcher.mjs',
    'session_domain.py', 'session_observations.mjs', 'controls/session_domain.py',
)
A3_SCRIPTS = ('check_schema.py', 'export_schema.mjs', 'validate_fixtures.mjs')


def apparatus_hashes():
    """Reject unregistered executable files, then hash A2 and installed deps."""
    unknown = unknown_apparatus_files()
    if unknown:
        raise ValueError(f'unregistered gateway-parity scripts: {unknown}')
    hashes = {(SCRIPT / name).relative_to(ROOT).as_posix(): digest(SCRIPT / name)
              for name in sorted(A2_SCRIPTS)}
    installed = node_modules_entry()
    if installed is not None:
        # A3 dependencies (ajv / typescript / ts-json-schema-generator) are not
        # loaded by a TS round, but they are part of the frozen apparatus's
        # executable surface; bind the bytes actually installed.
        hashes[(SCRIPT / 'node_modules').relative_to(ROOT).as_posix()] = installed
    return hashes


def baseline_dependency_hashes(baseline, roots=('tsx', 'ws')):
    """Hash the baseline packages the A2 round actually loads.

    Actual file bytes are used rather than package-lock/pnpm integrity: the
    threat here is on-disk drift or a repository-local override, and integrity
    metadata does not constrain the installed files Node resolves. Dependency
    names are followed from package.json so pnpm symlink layouts are covered.
    """
    baseline = Path(baseline)
    hashes = {}
    queue = [(name, True, baseline / 'node_modules') for name in roots]
    while queue:
        name, required, base = queue.pop(0)
        key = 'node_modules/' + name
        if key in hashes:
            continue
        link = base / name
        if not (link.exists() or link.is_symlink()):
            if required:
                raise ValueError(f'baseline {baseline} lacks {key}; A2 cannot bind its runtime')
            continue
        real = link.resolve()
        if not real.is_dir():
            if required:
                raise ValueError(f'baseline {baseline} {key} is not a directory')
            continue
        hashes[key] = tree_digest(real)
        try:
            package = json.loads((real / 'package.json').read_text(encoding='utf-8'))
        except (OSError, ValueError):
            continue
        dependencies = dict(package.get('dependencies') or {})
        dependencies.update(package.get('optionalDependencies') or {})
        # pnpm stores a package's dependencies as siblings in the same virtual
        # node_modules directory; fall back to the baseline's top level.
        queue.extend((dep, False, real.parent) for dep in sorted(dependencies))
    return hashes


def cargo_config_problems(root=None, environ=None):
    """Return cargo build inputs outside the frozen, reviewable repository.

    Cargo reads config from $CARGO_HOME and from .cargo/config[.toml] in the
    worktree and every ancestor directory. An untracked worktree config is a
    repository-local change that review cannot see; a persistent CARGO_HOME
    config (or an ancestor outside the worktree) is likewise not pinned here.
    """
    root = (Path(root) if root is not None else ROOT).resolve()
    environ = os.environ if environ is None else environ
    problems = {}
    configs = ('.cargo/config.toml', '.cargo/config')
    for name in configs:
        path = root / name
        if path.exists() or path.is_symlink():
            tracked = subprocess.run(
                ['git', '-C', str(root), 'ls-files', '--error-unmatch', '--', name],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
            if tracked.returncode:
                problems[path] = 'untracked worktree config (cargo would read it)'
    anchors = ('.cargo/config.toml', '.cargo/config')
    for parent in root.parents:
        for name in anchors:
            path = parent / name
            if path.exists() or path.is_symlink():
                problems[path] = 'cargo config outside the worktree (ancestor lookup)'
    cargo_home = Path(environ.get('CARGO_HOME') or Path.home() / '.cargo')
    for name in ('config.toml', 'config'):
        path = cargo_home / name
        if path.exists() or path.is_symlink():
            problems[path] = 'persistent CARGO_HOME config'
    return [f'{path}: {reason}' for path, reason in sorted(problems.items())]



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
