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


# Executable script suffixes the shared probe table exercises.  The scan itself
# no longer keys on suffixes: registration is by exact path (see
# ``apparatus_whitelist``).  Keeping the tuple makes every one of these shapes
# an explicit negative control and documents what the apparatus may execute.
SCRIPT_SUFFIXES = ('.py', '.pyw', '.sh', '.bash', '.zsh', '.js', '.mjs', '.cjs',
                   '.jsx', '.ts', '.mts', '.cts', '.tsx')
# CPython probes these before falling back to a same-directory ``.py``, so an
# unregistered extension module shadows the registered source at import time.
# importlib.machinery is the authoritative list for this interpreter
# (``.cpython-<abi>-<platform>.so`` / ``.abi3.so`` / ``.so``; ``.pyd`` on
# Windows); the explicit tail keeps the probe table complete if a build omits
# one.
EXTENSION_SUFFIXES = tuple(dict.fromkeys(
    tuple(importlib.machinery.EXTENSION_SUFFIXES) +
    ('.so', '.pyd', '.dylib', '.node')))
# site only reads ``.pth`` from site-package directories (and none under ``-I``),
# so a ``.pth`` beside the apparatus is inert today; it is still an
# executable-input shape the whitelist must reject rather than assume.
SITE_SUFFIXES = ('.pth',)

# A5.3 registers E1 adapters here so A2 continues to reject unknown scripts.
# The new hashes require coordinator review/registration before merge (plan 005).
A2_SCRIPTS = (
    'build_proxy.py', 'check_frozen.py', 'check_ruler.py', 'common.py', 'compare.py',
    'controls/run_all.py', 'controls/prerequisites.py', 'driver.mts', 'mock.mjs',
    'observer.py', 'run_round.py', 'test_safety.py', 'watcher.mjs',
    'session_domain.py', 'session_observations.mjs', 'controls/session_domain.py',
)
A3_SCRIPTS = ('check_schema.py', 'export_schema.mjs', 'validate_fixtures.mjs')

# Non-executable apparatus inputs, listed by exact path so the whitelist cannot
# silently grow.  Everything else in the directory is unregistered, whatever its
# suffix: a ``.json``/``.txt``/extensionless file is as much an input as a
# ``.py`` one (fixtures drive the comparators; package files drive A3 tooling).
DATA_FILES = (
    'README.md',                                # apparatus documentation
    'fixtures/gateway-protocol.fixtures.json',  # A3/A4 protocol fixtures
    'fixtures/session-calibration.json',        # A5.3 E1 fixture; FROZEN hash row
    'package.json',                             # npm manifest for A3 tooling
    'package-lock.json',                        # pinned A3 dependency graph
)


def apparatus_whitelist():
    """Relative paths the apparatus accepts without a FROZEN hash row."""
    return tuple(sorted(set(A2_SCRIPTS) | set(A3_SCRIPTS) | set(DATA_FILES)))


# Case-insensitive lookup because APFS presents one inode under one stored
# spelling; _registered() still requires os.path.samefile, so a distinct
# upper/lower-case file on a case-sensitive volume is unregistered.  This is
# not a claim that CPython resolves a different-case module: measured on
# Python 3.14.3, ``import weird`` does not find ``WEIRD.PY`` (find_spec None),
# while Node CJS/ESM did load ``WEIRD.JS`` (fix-r4 log case-resolution.log).
_REGISTERED = {name.casefold(): name for name in apparatus_whitelist()}
NATIVE_SUFFIX = EXTENSION_SUFFIXES[0] if EXTENSION_SUFFIXES else '.so'

# One shared negative-control table: test_safety.py and controls/prerequisites.py
# both call materialize_probe()/remove_probe() below, so a newly rejected shape
# gets one machine control and one implementation instead of two hand-maintained
# lists/if-chains that can drift apart.  An unknown kind raises ValueError in
# both functions: adding a shape to this table forces the materializer to change.
# kinds: file | dir | dir-link | file-link | case-variant | nested-modules |
#        nested-modules-empty | unreadable-dir.
APPARATUS_PROBES = tuple(dict.fromkeys((
    *(('file', f'probe{suffix}') for suffix in SCRIPT_SUFFIXES),
    ('file', 'nested/extra.py'),
    ('file', 'nested/check_schema.py'),
    ('file', 'legacy.pyc'),
    ('file', 'legacy.pyo'),
    ('file', f'probe{NATIVE_SUFFIX}'),
    ('file', 'probe.abi3.so'),
    ('file', 'probe.so'),
    ('file', 'probe.pyd'),
    ('file', 'probe.dylib'),
    ('file', 'probe.node'),
    ('file', 'evil.pth'),
    # APFS is case-insensitive by default: ``import weird`` resolves WEIRD.PY,
    # so a case-sensitive suffix test would miss this file.
    ('file', 'WEIRD.PY'),
    ('file', 'PROBE.SO'),
    # The whitelist rejects by path, so these are unregistered too.
    ('file', 'notes.txt'),
    ('file', 'notes'),
    ('file', '.hidden'),
    # fix-r5: Python bytecode is never tolerated.  A cache whose stem names a
    # *registered* source, an orphan cache and a non-bytecode file inside
    # __pycache__ are all unregistered (an empty __pycache__ is reported as the
    # directory).  A case variant is a distinct inode only on a case-sensitive
    # volume; on APFS it is skipped as degenerate (see materialize_probe).
    ('file', '__pycache__/common.cpython-314.pyc'),
    ('file', '__pycache__/orphan.cpython-314.pyc'),
    ('file', '__pycache__/evil.py'),
    # Caches below a subdirectory are rejected by the same first-__pycache__-
    # segment rule; controls/run_all.py is loaded by path, so this is the shape
    # that could shadow a real control module.
    ('file', 'controls/__pycache__/run_all.cpython-314.pyc'),
    ('case-variant', 'COMMON.PY'),
    # macOS Finder metadata is tolerated only as a regular file (same rule as
    # ui-server-parity/apparatus.py); a link or directory of that name stays
    # rejected.
    ('file-link', '.DS_Store'),
    ('dir', '.DS_Store'),
    # Only SCRIPT/node_modules is covered by node_modules_entry()'s digest; a
    # nested node_modules at any depth (or in any case) is unregistered.
    ('nested-modules', 'nested/node_modules'),
    ('nested-modules', 'nested/deeper/node_modules'),
    ('nested-modules-empty', 'nested/NODE_MODULES'),
    # Symbolic links and module-named directories (fix-r3, kept).
    ('dir-link', 'linked-dir'),
    ('file-link', 'linked-file.txt'),
    ('file-link', f'compare{NATIVE_SUFFIX}'),
    ('dir-link', 'compare'),
    ('dir', 'observer'),
    # An unreadable subtree must fail the scan, never be skipped.
    ('unreadable-dir', 'unreadable'),
)))

# Host properties (case-folding filesystem, euid 0) can make a probe shape
# impossible to materialize.  A skip is never a pass: consumers record it as
# its own state, list it in the report and require the skipped set to be a
# subset of this declaration, so an unexpected skip still fails the gate.
# (fix-r6 P2: ``ProbeSkipped`` used to be recorded as ``passed: True``.)
DEGENERATE_PROBES = frozenset({
    ('case-variant', 'COMMON.PY'),      # APFS folds case: variant is common.py's inode
    ('unreadable-dir', 'unreadable'),   # euid 0 can read a mode-000 directory
})


def probe_expected(name):
    """Path the scanner reports for a probe: the first __pycache__ segment.

    The scan reports a ``__pycache__`` directory itself and never descends into
    it, so a file below one (root or nested) is expected as that directory.
    """
    parts = Path(name).parts
    for index, part in enumerate(parts):
        if part.casefold() == '__pycache__':
            return Path(*parts[:index + 1]).as_posix()
    return name


class ProbeSkipped(RuntimeError):
    """The current environment cannot materialize this probe shape."""


def materialize_probe(root, kind, name, outside):
    """Create one rejected apparatus shape; unknown kinds raise ValueError.

    Shared by test_safety.py and controls/prerequisites.py so both machine
    controls always exercise the same shape.  ``outside`` is a directory
    outside the apparatus holding link targets (``evil.py``/``__init__.py``).
    A mode-000 directory cannot be built as root; callers must treat
    ``ProbeSkipped`` as a documented degenerate case, never as a pass.
    """
    path = Path(root) / name
    path.parent.mkdir(parents=True, exist_ok=True)
    if kind == 'file':
        path.write_text('unregistered apparatus input')
    elif kind == 'dir':
        path.mkdir()
    elif kind == 'dir-link':
        path.symlink_to(outside, target_is_directory=True)
    elif kind == 'file-link':
        path.symlink_to(Path(outside) / 'evil.py')
    elif kind == 'case-variant':
        # A distinct upper-case spelling exists only on a case-sensitive
        # volume.  Where the filesystem folds case, creating COMMON.PY would
        # alias (or overwrite/delete) a registered common.py, so skip instead
        # of silently testing the wrong inode.
        registered = path.parent / Path(name).name.casefold()
        if registered.exists() and _same_inode(registered, path):
            raise ProbeSkipped('case-insensitive volume: case variant is the same inode')
        path.write_text('unregistered apparatus input')
        if registered.exists() and _same_inode(registered, path):
            path.unlink()
            raise ProbeSkipped('case-insensitive volume: case variant is the same inode')
    elif kind == 'nested-modules':
        path.mkdir()
        (path / 'evil.py').write_text('unregistered nested dependency')
    elif kind == 'nested-modules-empty':
        path.mkdir()
    elif kind == 'unreadable-dir':
        if os.geteuid() == 0:
            raise ProbeSkipped('root can read mode-000 directories')
        path.mkdir()
        (path / 'evil.py').write_text('unregistered file in an unreadable tree')
        os.chmod(path, 0)
    else:
        raise ValueError(f'unknown apparatus probe kind: {kind!r}')
    return path


def remove_probe(kind, path):
    """Remove a shape created by materialize_probe(); unknown kinds raise."""
    path = Path(path)
    if kind in ('file', 'case-variant', 'dir-link', 'file-link'):
        path.unlink()
        if path.parent.name == '__pycache__':
            # Keep repeated probe runs on a clean tree: the probe's own
            # __pycache__ directory is empty after the file is gone.
            try:
                path.parent.rmdir()
            except OSError:
                pass
    elif kind in ('dir', 'nested-modules-empty'):
        path.rmdir()
    elif kind == 'nested-modules':
        shutil.rmtree(path)
    elif kind == 'unreadable-dir':
        os.chmod(path, 0o700)
        shutil.rmtree(path)
    else:
        raise ValueError(f'unknown apparatus probe kind: {kind!r}')


def _raise(error):
    """os.walk error hook: an unreadable directory must fail, never be skipped."""
    raise error


def _same_inode(left, right):
    try:
        return os.path.samefile(left, right)
    except OSError:
        return False


def _registered(script, rel):
    """True only for a whitelisted path, case-insensitively the same inode."""
    name = _REGISTERED.get(rel.as_posix().casefold())
    if name is None:
        return False
    if name == rel.as_posix():
        return True
    # APFS presents a registered path under a different case; accept it only
    # when it is the same inode.  On a case-sensitive filesystem the variant is
    # a distinct, unregistered file.
    return _same_inode(script / name, script / rel)


def tree_digest(folder):
    """Hash actual on-disk bytes of a dependency tree, including link targets.

    The root is resolved (pnpm/npm package directories may be symlinks); nested
    directory symlinks are recorded but not followed, matching Node's own
    package resolution boundaries.  An unreadable directory raises instead of
    being skipped, so the digest can never cover a silently partial tree.
    """
    folder = Path(folder).resolve()
    entries = {}
    for current, dirs, files in os.walk(folder, onerror=_raise, followlinks=False):
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
    """Apparatus files that no whitelist row covers.

    Registration is by path, not by suffix: every file under ``SCRIPT`` must be
    an A2/A3 member (both already hash-bound) or one of the explicit
    ``DATA_FILES``, so a new ``.json``, ``.txt``, extensionless or
    upper-case-suffixed input fails exactly like a new ``.py``.  ``FROZEN.md``
    keeps the other direction honest: ``check_frozen`` requires a hash row for
    every A2 member that exists.

    Python bytecode is never tolerated.  ``.pyc``/``.pyo`` bytes are executable
    and are not covered by any FROZEN row; a PEP 552 unchecked-hash cache is
    loaded without re-reading the source (measured: fix-r5 negative control
    imports a crafted ``__pycache__/common.cpython-314.pyc`` and runs its
    code while ``common.py`` keeps its registered hash), so a source-adjacent
    cache is not covered by the source hash.  Every apparatus entry
    sets ``sys.dont_write_bytecode = True`` and points ``sys.pycache_prefix`` at
    a fresh temp directory before importing apparatus modules, so a normal run
    cannot create a cache and an existing repository cache is never read.
    ``-B``/dont_write_bytecode only stops writes: without the prefix redirect, a
    crafted unchecked-hash cache is imported before the scan and can override
    this function, making ``check_frozen`` PASS (measured in the fix-r5
    negative control; with the redirect it FAILs and the payload does not run).

    Bytecode is not the only pre-scan import hazard: CPython's FileFinder tries
    a same-named package directory and extension module before the ``.py`` file,
    so ``common/__init__.py`` or ``common.cpython-<abi>-<platform>.so`` would
    replace this module before it could scan (fix-r6 P1).  Every entry therefore
    loads apparatus modules with ``spec_from_file_location(name, path)`` and
    asserts the loaded module's ``__file__`` is exactly that path; it never
    relies on ``sys.path`` resolution.  Both shadow shapes remain unregistered
    files, so the scan reports them once the real ``common.py`` runs.
    A ``__pycache__`` directory anywhere in
    the apparatus is reported unregistered (an empty one included), as is any
    ``.pyc``/``.pyo`` file, an orphan cache, or a non-bytecode file inside
    ``__pycache__``.

    macOS ``.DS_Store`` is the one explicit metadata exemption, matching
    ``scripts/ui-server-parity/apparatus.py``: only a regular file with that
    exact basename is accepted; a link, directory, FIFO or socket of that name
    is still unregistered.

    Paths compare case-insensitively because APFS presents one inode under one
    stored spelling.  A registered name presented in another case is accepted
    only when ``os.path.samefile`` shows it is the very same inode, so on a
    case-sensitive filesystem a distinct case variant remains unregistered.
    (Measured on Python 3.14.3, ``import weird`` does not resolve ``WEIRD.PY``;
    a case-sensitive suffix test is not what protects this file — the path
    whitelist is.)

    Only the top-level ``node_modules`` is exempt: ``node_modules_entry`` binds
    that whole tree by digest.  A ``node_modules`` at any deeper level is
    rejected as a directory, since that digest cannot cover it; a top-level case
    variant is exempt only when it is the same inode as the digested directory.

    Symbolic links are rejected outright outside ``node_modules``, whatever
    their own suffix: ``os.walk`` does not descend into a linked directory, so
    files in its target would escape this scan, and a link can stand in for a
    registered module.  Measured on CPython 3.14: with ``compare.py`` present,
    a valid ``compare -> <dir containing __init__.py>`` link makes ``import
    compare`` load the linked package while ``compare.py`` keeps its registered
    hash, so ``check_frozen``/``apparatus_hashes`` would otherwise PASS.  A
    plain directory named like a registered module is rejected as well: a
    directory with ``__init__.py`` already has that file caught as a file
    (measured: an empty directory does not shadow ``.py``, since the file suffix
    loop still runs), but reporting the shadow container names the real hazard.
    Links inside ``node_modules`` stay under the installed-dependency rule:
    ``tree_digest`` records each as ``link:<target>`` and never follows it, so
    repointing a link changes the digest, while the bytes behind it are not
    bound (documented boundary).

    ``os.walk`` gets an ``onerror`` hook that re-raises: an unreadable directory
    fails the scan instead of silently dropping its subtree (``check_frozen``
    turns the ``OSError`` into ``cannot scan apparatus``).
    """
    script = Path(script if script is not None else SCRIPT).resolve()
    # Importable names a directory could shadow: module stems, e.g. ``compare``.
    module_names = {Path(name).stem.casefold() for name in A2_SCRIPTS + A3_SCRIPTS}
    found = set()
    for folder, dirs, files in os.walk(script, onerror=_raise, followlinks=False):
        current = Path(folder)
        descend = []
        for name in dirs:
            path = current / name
            rel = path.relative_to(script)
            folded = name.casefold()
            if folded == 'node_modules':
                # Only the directory node_modules_entry() digests is covered.
                if current == script and _same_inode(script / 'node_modules', path):
                    continue
                found.add(rel.as_posix())
                continue
            if folded == '__pycache__' or name == '.DS_Store':
                # Bytecode never gets a tolerance class (fix-r5).  A directory
                # named .DS_Store is not the regular-file metadata exemption.
                found.add(rel.as_posix())
                continue
            if path.is_symlink() or folded in module_names:
                found.add(rel.as_posix())
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
            elif not path.is_file():
                # FIFO/socket/device: never a registered data or script file.
                found.add(rel.as_posix())
            elif name == '.DS_Store':
                # Exact basename + regular file only, same as ui-server.
                continue
            elif not _registered(script, rel):
                found.add(rel.as_posix())
    return sorted(found)


def apparatus_hashes():
    """Reject unregistered files, then hash A2 and installed deps."""
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
