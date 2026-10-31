# anchor-selfcheck: unsupported shared process/source guard, no mutation definitions
"""Bounded process ownership shared by the gateway negative controls.

Every invocation owns a fresh session, so a timeout can kill the whole tree.
Collection after that kill is bounded too: a descendant that called setsid()
escapes the process group and can keep the inherited pipes open, which is why
a plain ``with Popen(...)`` plus ``communicate()`` can block forever even after
the leader is dead (#528).
"""
from contextlib import contextmanager, nullcontext
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import signal
import shutil
import stat
import subprocess
import sys
import threading
import time
from types import ModuleType

# Upper bound for collecting a killed tree. Kept short on purpose: the only
# writer that can still hold the pipes is a setsid() descendant that the group
# signal cannot reach.
KILL_TIMEOUT = 10


def _kill_group(process):
    for attempt in range(2):
        try:
            os.killpg(process.pid, signal.SIGKILL)
            return
        except ProcessLookupError:
            return
        except PermissionError:
            # macOS sandbox can report EPERM for an unreaped exited leader.
            # Reap it, then retry the group signal: descendants must still be
            # killed even when their leader has exited.
            if attempt or process.poll() is None:
                raise


def _as_text(process, value):
    # TimeoutExpired.output is built from os.read() chunks, so it stays bytes
    # even in text mode; normalize it like communicate() would (#528).
    if value is None:
        return ""
    if isinstance(value, bytes):
        encoding = getattr(process.stdout, "encoding", None) or "utf-8"
        errors = getattr(process.stdout, "errors", None) or "replace"
        return value.decode(encoding, errors)
    return value


def _collect(process, timeout):
    try:
        return process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        # A setsid() descendant escaped the group and holds the pipes, so EOF
        # never arrives. Stop reading and reap the already-killed leader;
        # keep whatever output the bounded read observed.
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                stream.close()
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            pass
        return _as_text(process, error.output), _as_text(process, error.stderr)


def _prefer_more(first, second):
    # communicate() keeps accumulating across retries, so the longer string is
    # the one that saw more bytes; never drop an earlier partial read.
    first = first or ""
    second = second or ""
    return second if len(second) > len(first) else first


def _communicate_progress(process, command, timeout, idle_timeout):
    """Bound silence separately from wall time; retain communicate's raw output.

    Cargo emits diagnostics/artifacts as compilation advances. A slow build
    can therefore exceed the test deadline without granting a silent/hung
    compiler unlimited time. Repeated communicate calls accumulate output.
    """
    started = progress = time.monotonic()
    size = 0
    while True:
        now = time.monotonic()
        remaining = idle_timeout - (now - progress)
        if timeout is not None:
            remaining = min(remaining, timeout - (now - started))
        try:
            return process.communicate(timeout=max(0, min(1, remaining)))
        except subprocess.TimeoutExpired as error:
            observed = len(error.output or b'') + len(error.stderr or b'')
            now = time.monotonic()
            if observed > size:
                size, progress = observed, now
            if ((timeout is not None and now - started >= timeout)
                    or now - progress >= idle_timeout):
                raise subprocess.TimeoutExpired(command, timeout or idle_timeout,
                                                error.output, error.stderr) from error


def run_owned(command, *, env=None, timeout=900, cwd=None, kill_timeout=KILL_TIMEOUT,
              idle_timeout=None):
    if idle_timeout is not None and (not math.isfinite(idle_timeout) or idle_timeout <= 0):
        raise ValueError('idle_timeout must be finite and positive')
    # cargo can leave test binaries (and inherited output pipes) behind when
    # killed alone. Do not use `with Popen(...)`: its __exit__ waits forever on
    # a tree that only the group signal can reach.
    process = subprocess.Popen(command, env=env, cwd=cwd,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True)
    try:
        if idle_timeout is None:
            stdout, stderr = process.communicate(timeout=timeout)
        else:
            stdout, stderr = _communicate_progress(process, command, timeout, idle_timeout)
    except BaseException as error:
        _kill_group(process)
        stdout, stderr = _collect(process, kill_timeout)
        if isinstance(error, subprocess.TimeoutExpired):
            error.output = _prefer_more(_as_text(process, error.output), stdout)
            error.stderr = _prefer_more(_as_text(process, error.stderr), stderr)
        raise
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def node_version(env=None, *, timeout=60):
    """Return ``(version, error)`` from the first stdout line of ``node --version``.

    Only stdout is evidence: a warning on stderr (for example from
    ``NODE_OPTIONS``) must not turn a matching host into a mismatch. A non-zero
    exit, an empty stdout or a missing node is an error, never a silently
    skipped control; callers record INVALID and exit non-zero (#818 fix-r3).
    """
    try:
        result = run_owned(['node', '--version'], env=env, timeout=timeout)
    except subprocess.TimeoutExpired:
        return None, f'node --version timed out after {timeout}s'
    except OSError as error:
        return None, f'node unavailable: {error}'
    if result.returncode != 0:
        detail = (result.stderr or result.stdout or '').strip().splitlines()
        suffix = f': {detail[0]}' if detail else ''
        return None, f'node --version exited {result.returncode}{suffix}'
    lines = result.stdout.splitlines()
    version = lines[0].strip() if lines else ''
    if not version:
        return None, 'node --version printed no version on stdout'
    return version, None



def new_output_directory(path):
    """Create this run's evidence directory; refuse to reuse earlier evidence.

    A negative control writes fixed log names. If a reused directory were
    accepted, the next attempt would overwrite the previous run's original
    bytes and destroy the evidence its failure report points at. A missing or
    empty directory is eligible, but only exclusive creation of .claim admits
    this invocation. Keep the claim even on failure/cancellation: an old runner
    may resume, so neither an empty claim nor a dead pid makes reuse safe.
    Refusal happens before any cargo or source mutation starts.
    """
    path = Path(path).resolve()
    try:
        path.mkdir(parents=True)
    except FileExistsError:
        if not path.is_dir():
            raise SystemExit(
                f"#953 output path exists and is not a directory: {path}; "
                "pass a new evidence directory")
        if any(path.iterdir()):
            raise SystemExit(
                f"#953 refusing to reuse a non-empty output directory: {path}; "
                "remove it or pass a new evidence directory")
    # mkdir/iterdir are not an ownership decision: another process can claim
    # the directory after either operation. O_EXCL is the admission point.
    try:
        with (path / ".claim").open("x", encoding="utf-8") as claim:
            json.dump({"pid": os.getpid(), "started_at": time.time(),
                       "argv": sys.argv}, claim)
            claim.write("\n")
    except FileExistsError as error:
        raise SystemExit(
            f"#1060 output directory already claimed: {path}; "
            "pass a new evidence directory") from error
    return path


def exclusive_write(path, text):
    """Create a new evidence file; never overwrite an earlier run's bytes."""
    with Path(path).open("x", encoding="utf-8") as stream:
        stream.write(text)




class SourceBaselineError(RuntimeError):
    """A path to be mutated does not match the committed baseline.

    Refusing here is the whole point: a runner that snapshots whatever bytes
    happen to be on disk after a killed predecessor would restore the mutant
    and report ``restoredPass: true``. The fix is to treat only the committed
    bytes as the original and to leave an unexpected working-tree change for
    its human owner instead of overwriting it.
    """

    def __init__(self, root, relative, detail):
        self.root = Path(root)
        self.relative = relative
        self.detail = detail
        super().__init__(
            f"source baseline mismatch: {relative} ({detail}); refusing to mutate. "
            f"This can be a killed negative control's residue or a human edit. "
            f"Inspect it, then restore with: git checkout -- {relative}"
        )


# Nested acquisition by the same thread is reentrant: round6 mutates a runner
# script and then executes that runner in-process, and the runner takes the
# same tree lock. A second ``flock`` on a separate fd would deadlock even in
# one process, so the registry only defers to the outermost holder. Separate
# threads and separate processes still exclude each other through the kernel.
_HELD = {}
_HELD_GUARD = threading.Lock()


def _held_key(path):
    return (os.getpid(), threading.get_ident(), os.path.abspath(path))


class TreeLock:
    """Exclusive same-tree lock for in-place source mutations.

    The lock is a kernel ``flock`` on one fixed file inside the tree. The
    kernel releases it when the owning process exits, including ``SIGKILL``,
    so no pid probing, stale-directory reclaim or directory removal exists:
    those three steps were not atomic and could hand the same tree to two
    runners (the loser then captured mutated bytes as its ``original``).
    Whoever owns the lock reads the pristine bytes *after* acquisition and
    keeps them until every mutation is restored.
    """

    def __init__(self, path, *, timeout=900, interval=0.05):
        self.path = Path(path)
        self.timeout = timeout
        self.interval = interval
        self._fd = None
        self._key = None
        self._delegate = False

    def __enter__(self):
        key = _held_key(self.path)
        with _HELD_GUARD:
            held = _HELD.get(key)
            if held is not None:
                held["depth"] += 1
                self._key = key
                self._delegate = True
                return self
        # O_CLOEXEC keeps the lock out of every cargo/test child: a SIGKILLed
        # runner's children must not hold the flock after the runner is gone.
        flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_CLOEXEC", 0)
        fd = os.open(self.path, flags, 0o644)
        try:
            self._acquire(fd)
        except BaseException:
            os.close(fd)
            raise
        with _HELD_GUARD:
            _HELD[key] = {"fd": fd, "depth": 1}
        self._fd = fd
        self._key = key
        return self

    def _acquire(self, fd):
        # Poll in every thread: alarm() and setitimer(ITIMER_REAL) share one
        # process-wide timer. Borrowing it here would replace/cancel scratch's
        # total budget, and could delay a shorter caller deadline while waiting.
        # Leave both its timer and handler untouched, including on cancellation.
        deadline = time.monotonic() + self.timeout
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return
            except BlockingIOError:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"source lock busy: {self.path}")
                time.sleep(min(self.interval, remaining))

    def __exit__(self, *exc):
        key, self._key = self._key, None
        if self._delegate:
            self._delegate = False
            with _HELD_GUARD:
                held = _HELD.get(key)
                if held is not None:
                    held["depth"] -= 1
                    if held["depth"] <= 0:
                        del _HELD[key]
            return
        fd, self._fd = self._fd, None
        if fd is None:
            return
        with _HELD_GUARD:
            _HELD.pop(key, None)
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)


def tree_lock(root, **kwargs):
    # The lock is a persistent file, never removed: unlinking a flock-ed file
    # would let the next opener create a different inode and acquire a second
    # "exclusive" lock while the first holder still owns the old one.
    return TreeLock(Path(root) / ".negative-source.lock", **kwargs)


def _git(root, *args):
    # Popen, not subprocess.run: runner meta-controls patch subprocess.run to
    # catch cargo launched outside run_owned, and git is not such a launch.
    process = subprocess.Popen(["git", "-C", str(root), *args],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stdout, stderr = process.communicate()
    if process.returncode != 0:
        detail = stderr.decode("utf-8", "replace").strip() or "git failed"
        raise RuntimeError(f"git {' '.join(args)}: {detail}")
    return stdout


def _head_bytes(root, relative, baseline):
    try:
        return _git(root, "show", f"{baseline}:{relative}")
    except RuntimeError as error:
        raise SourceBaselineError(root, relative,
                                  f"cannot read {baseline} bytes: {error}")


def _scanner_environment(env=None):
    """One target interpretation, independent of the Python/Cargo caller cwd.

    The target may be outside this tree. Each coordinator must dedicate it to
    one tree; location is not provenance. Relative paths are rooted here and
    the absolute value is passed to both Cargo and the scanner.
    """
    env = dict(os.environ if env is None else env)
    scanner_root = Path(__file__).resolve().parents[4]
    if 'CARGO_TARGET_DIR' not in env:
        env['CARGO_TARGET_DIR'] = str(scanner_root / '_build/cargo-target')
    return _target_environment(scanner_root, env)


def _ensure_workspace_scanner(env=None):
    """Bootstrap only a missing helper under its pristine source-tree lease.

    Never silently replace an existing helper: its embedded source digest is
    checked on every scan, including deliberate scanner mutations. Rebuilding a
    stale helper is the owner's job. Cargo failure/timeout aborts admission.
    """
    env = _scanner_environment(env)
    executable = Path(env['CARGO_TARGET_DIR']) / 'debug/examples/workspace_inputs'
    if executable.is_file():
        return
    root = Path(__file__).resolve().parents[4]
    with tree_lock(root):
        _assert_workspace_sources(root, 'HEAD')
        # Another cold caller may have completed while this one waited.
        if executable.is_file():
            return
        wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15).check_returncode()
        result = run_owned(['cargo', 'build', '--offline', '--manifest-path', 'src/Cargo.toml',
                            '-p', 'pilotdeck-gateway', '--example', 'workspace_inputs'],
                           cwd=root, env=env, timeout=1800, idle_timeout=600)
        result.check_returncode()
        if not executable.is_file():
            raise RuntimeError('#1166 scanner build produced no executable')


def _workspace_literal_inputs(root, baseline, *, env=None):
    """Run the prebuilt #449/#1166 proc-macro2 scanner; never launch Cargo here.

    The shared entry points bootstrap a missing helper inside a pristine lease.
    No inventory cache: included external Rust
    sources are re-read every time. Embedded source identity rejects stale builds.
    """
    root = Path(root).resolve()
    try:
        target = _scanner_environment(env)['CARGO_TARGET_DIR']
        scanner_root = Path(__file__).resolve().parents[4]
        sources = (scanner_root / 'src/pilotdeck-gateway/examples/workspace_inputs.rs',
                   scanner_root / 'src/pilotdeck-gateway/tests/common/source_inputs.rs',
                   scanner_root / 'src/pilotdeck-gateway/Cargo.toml',
                   scanner_root / 'src/Cargo.toml', scanner_root / 'src/Cargo.lock')
        digest = hashlib.sha256(b''.join(path.read_bytes() for path in sources)).hexdigest()
        executable = Path(target).resolve() / 'debug/examples/workspace_inputs'
        expected_head = _git(root, 'rev-parse', '--verify', baseline + '^{commit}').decode().strip()
        result = run_owned([str(executable), str(root), expected_head], timeout=60, cwd=root)
        if result.returncode != 0:
            raise ValueError('Rust scanner failed: ' + result.stderr[-4000:])
        document = json.loads(result.stdout)
        if (not isinstance(document, dict) or document.get('head') != expected_head
                or document.get('root') != str(root) or document.get('scanner_sha256') != digest):
            raise ValueError('Rust scanner identity mismatch; prebuild the current example offline')
        inputs = document.get('inputs')
        if not isinstance(inputs, list):
            raise ValueError('Rust scanner input list is malformed')
        for name in inputs:
            if (not isinstance(name, str) or not name or Path(name).is_absolute()
                    or '..' in Path(name).parts or Path(name).as_posix() != name):
                raise ValueError('Rust scanner input path is malformed')
        if len(set(inputs)) != len(inputs):
            raise ValueError('Rust scanner input list has duplicates')
        return tuple(':(top,literal)' + name for name in inputs)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        raise SourceBaselineError(root, 'src', f'#1166 cannot resolve Rust build input: {error}') from error


def _assert_product_pristine(root, baseline, *, env=None):
    # A runner only names the files it is about to mutate, but a killed
    # predecessor from another group can leave any product source mutated.
    # Checking the whole workspace (including fixtures and path dependencies)
    # turns that residue into a
    # refusal for every next runner, not just the one that shares its file.
    # include_str!/include_bytes! can read non-Rust files; src/vendor
    # and src/data are path dependencies too. Do not maintain a crate list.
    _assert_workspace_sources(root, baseline)
    literal_inputs = _workspace_literal_inputs(root, baseline, env=env)
    # Explicitly included files are inputs even when named like a cache. Bound
    # argv by encoded bytes (not characters/count); preserve every literal.
    for batch in _pathspec_batches(literal_inputs):
        _assert_input_areas(root, baseline, batch, exclude_caches=False)


def _assert_workspace_sources(root, baseline):
    # This cheap preflight does not need the scanner it protects at cold start.
    areas = ('src', '.cargo', ':(top,glob)rust-toolchain*')
    _assert_input_areas(root, baseline, areas)


def _pathspec_batches(paths):
    batch, size = [], 0
    for path in paths:
        length = len(os.fsencode(path)) + 1
        if batch and size + length > 32768:
            yield tuple(batch)
            batch, size = [], 0
        batch.append(path)
        size += length
    if batch:
        yield tuple(batch)


def _assert_input_areas(root, baseline, areas, *, exclude_caches=True):
    for options in ((), ('--cached',)):
        changed = _git(root, 'diff', *options, '--name-only', '-z', baseline,
                       '--', *areas).split(b'\0')
        if changed[0]:
            raise SourceBaselineError(root, os.fsdecode(changed[0]),
                                      f'differs from {baseline}')
    flags = _git(root, 'ls-files', '-v', '-z', '--', *areas).split(b'\0')
    for entry in flags:
        if entry and (entry[:1].islower() or entry[:1] == b'S'):
            raise SourceBaselineError(root, os.fsdecode(entry[2:]),
                                      'assume-unchanged / skip-worktree input')
    # Include ignored inputs: .gitignore is not a compilation boundary.
    # Only known disposable Cargo/Python/OS caches are excluded, and only here (never
    # from the tracked/index checks). Keep artifacts outside source folders.
    caches = ('__pycache__/**', '*.pyc', '.DS_Store', '.pytest_cache/**')
    exclusions = (tuple(':(exclude,glob)src/**/' + name for name in caches)
                  + (':(exclude,glob)src/target/**', ':(exclude,glob)src/*/target/**')
                  if exclude_caches else ())
    untracked = _git(root, 'ls-files', '--others', '-z', '--', *areas, *exclusions).split(b'\0')
    if untracked[0]:
        raise SourceBaselineError(root, os.fsdecode(untracked[0]),
                                  f'untracked build input at {baseline}')


def copy_binary(source, destination, *, expected_sha256=None):
    """Create a new inode, optionally checking an explicitly pinned snapshot.

    A pinned snapshot needs no source-tree lease: it reads no source or shared
    Cargo artifact. Hash the copied bytes, so changing the source during copy
    fails rather than attributing new bytes to the caller's expected build.
    """
    source, destination = Path(source), Path(destination)
    with source.open('rb') as reader, destination.open('xb') as writer:
        shutil.copyfileobj(reader, writer)
        os.fchmod(writer.fileno(), stat.S_IMODE(os.fstat(reader.fileno()).st_mode))
    digest = hashlib.sha256(destination.read_bytes()).hexdigest()
    if expected_sha256 is not None and digest != expected_sha256:
        raise RuntimeError('#1166 binary snapshot SHA-256 mismatch')
    return str(destination)


def _target_environment(root, env):
    raw = env.get('CARGO_TARGET_DIR')
    if raw is None or not raw.strip():
        raise RuntimeError('#1166 CARGO_TARGET_DIR must be supplied by the caller')
    if raw.strip() == '.':
        raise RuntimeError('#1166 target cannot be dot')
    env = dict(env)
    scanner_root = Path(__file__).resolve().parents[4]
    target = raw
    env['CARGO_TARGET_DIR'] = str((scanner_root / target).resolve())
    target = Path(env['CARGO_TARGET_DIR'])
    if target == Path(target.anchor):
        raise RuntimeError('#1166 target cannot be filesystem root')
    if target in (root, Path(__file__).resolve().parents[4]):
        raise RuntimeError('#1166 target cannot be repository root')
    if any(target.is_relative_to(base / 'src')
           for base in (root, Path(__file__).resolve().parents[4])):
        raise RuntimeError('#1166 target cannot be inside src')
    return env


def _publish_build_owner(root, env):
    owner = dict(root=str(root), head=_git(root, 'rev-parse', 'HEAD').decode().strip(),
                 nonce=os.urandom(16).hex())
    target = Path(env['CARGO_TARGET_DIR'])
    target.mkdir(parents=True, exist_ok=True)
    marker = target / '.negative-build-owner.json'
    payload = json.dumps(owner, sort_keys=True) + '\n'
    pending = marker.with_name(marker.name + '.' + owner['nonce'] + '.tmp')
    try:
        with pending.open('x') as stream:
            stream.write(payload)
        pending.replace(marker)
    finally:
        pending.unlink(missing_ok=True)
    return marker, payload


def _artifact_identity(path):
    info = Path(path).stat()
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def _verify_snapshot_owner(marker, payload, artifact, identity, started_ns):
    try:
        if marker.read_text() != payload:
            raise RuntimeError('#1166 build owner changed during snapshot')
        if _artifact_identity(artifact) != identity:
            raise RuntimeError('#1166 build artifact changed during snapshot')
        if identity[3] < started_ns:
            raise RuntimeError('#1166 build artifact predates this build')
    except OSError as error:
        raise RuntimeError('#1166 build provenance unavailable after copy') from error


def build_snapshot(root, output, *, package, target, kind='test', release=True,
                   env=None):
    """Build verified workspace inputs and copy the executable under one lease.

    tree_lock alone only serializes readers/writers; it does not validate
    source provenance. For pristine single test/bin measurements only; lib,
    feature-mutant and rlib probes retain their own build/run inside the
    mutating_sources lease. This entry point and mutating_sources validate after
    acquiring it. Only this run's exclusive snapshot escapes the lease.
    """
    root = Path(root).resolve()
    env = dict(os.environ if env is None else env)
    env = _target_environment(root, env)
    if kind not in ('test', 'bin'):
        raise ValueError('#1166 unsupported measurement target kind')
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test' if kind == 'test' else 'build', '--offline',
               '--manifest-path', 'src/Cargo.toml', '-p', package,
               '--' + kind, target, '--message-format=json']
    if kind == 'test':
        command.append('--no-run')
    if release:
        command.append('--release')
    with tree_lock(root, timeout=900):
        _assert_workspace_sources(root, 'HEAD')
        _ensure_workspace_scanner(env)
        _assert_product_pristine(root, 'HEAD', env=env)
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)
        (output / 'slots.log').write_text(slot.stdout + slot.stderr)
        marker, owner = _publish_build_owner(root, env)
        started_ns = time.time_ns()
        result = run_owned(command, cwd=root, env=env, timeout=1800, idle_timeout=600)
        (output / 'build.log').write_text(result.stdout + result.stderr)
        result.check_returncode()
        executables = set()
        for line in result.stdout.splitlines():
            row = json.loads(line)
            if (row.get('reason') == 'compiler-artifact'
                    and row.get('target', {}).get('name') == target
                    and row['target'].get('kind') == [kind] and row.get('executable')):
                executables.add(row['executable'])
        if len(executables) != 1:
            raise RuntimeError('#1166 expected exactly one measurement executable')
        artifact = executables.pop()
        identity = _artifact_identity(artifact)
        binary = copy_binary(artifact, output / 'executable')
        _verify_snapshot_owner(marker, owner, artifact, identity, started_ns)
        (output / 'build-owner.json').write_text(owner)
        (output / 'sha256').write_text(hashlib.sha256(Path(binary).read_bytes()).hexdigest() + '\n')
        return binary


@contextmanager
def _parity_mutation_lock(root, snapshot, timeout):
    """Use the ruler's own lease/guard protocol, loaded from verified bytes.

    This checkout uses a lease directory plus a persistent .guard flock file.
    Bound the guard's flock too: parity_lock's polling deadline alone cannot
    interrupt its blocking guard acquisition. Never import mutated disk code.
    """
    path = root / "scripts/gateway-parity/common.py"
    if path not in snapshot:
        raise RuntimeError("#166 parity lock requires common.py in guarded sources")
    common = ModuleType("negative_pristine_parity")
    common.__file__ = str(path)
    exec(compile(snapshot[path], str(path), "exec"), common.__dict__)
    common.guard = lambda lock: TreeLock(
        lock.with_name(lock.name + ".guard"), timeout=timeout)
    restored = [False]
    with common.parity_lock(timeout=timeout):
        try:
            yield restored
        finally:
            if not restored[0]:
                # A restoration error must not publish a reusable lease.
                # This private module uses an explicit non-PID cleanup reason;
                # parity_lock persists it in cleanupPending instead of deleting
                # the lease. Even a failed save leaves the original True marker.
                common.LIVE_GROUPS.add("source-restore-unverified")


@contextmanager
def mutating_sources(root, paths, *, baseline="HEAD", lock_timeout=900):
    """Lock the tree, prove ``paths`` are pristine, snapshot, mutate, restore.

    ``paths`` must be repo-relative or absolute paths inside ``root``. Every
    path is checked byte-for-byte against ``git show <baseline>:<path>`` while
    the tree lock is held; the whole workspace input tree is also checked so a
    killed runner from another group cannot leave a mutant behind. A mismatch
    aborts *before* anything is written and leaves the unexpected bytes for a
    human to inspect. On exit the snapshot is written back and compared both to
    the snapshot and to the same git object.

    Nested product-only use by one thread is supported through TreeLock;
    parity mutation windows must not nest their non-reentrant ruler lease.
    Any scripts/gateway-parity path requires common.py in the guarded set:
    acquire the shared parity lease before yielding, then retain it through
    restoration and byte verification. Lease polling and each guard flock
    acquisition have separate 60s bounds; a busy lock fails before source writes.
    """
    root = Path(root).resolve()
    normalized = []
    for path in paths:
        alias = Path(path)
        candidate = alias
        if not candidate.is_absolute():
            candidate = root / candidate
        candidate = candidate.resolve()
        try:
            relative = candidate.relative_to(root).as_posix()
        except ValueError:
            raise ValueError(f"source to mutate is outside {root}: {candidate}")
        normalized.append((alias, candidate, relative))

    with tree_lock(root, timeout=lock_timeout):
        _assert_workspace_sources(root, baseline)
        _ensure_workspace_scanner()
        _assert_product_pristine(root, baseline)
        baseline_bytes = {path: _head_bytes(root, relative, baseline)
                          for _alias, path, relative in normalized}
        for _alias, path, relative in normalized:
            if not path.is_file():
                raise SourceBaselineError(root, relative, "missing file")
            if path.read_bytes() != baseline_bytes[path]:
                raise SourceBaselineError(root, relative,
                                          f"differs from {baseline}")
        snapshot = dict(baseline_bytes)
        parity = any(relative.startswith("scripts/gateway-parity/")
                     for _alias, _path, relative in normalized)
        lock = (_parity_mutation_lock(root, snapshot, min(lock_timeout, 60))
                if parity else nullcontext())
        with lock as restored:
            try:
                yield {alias: snapshot[path] for alias, path, _relative in normalized}
            finally:
                for _alias, path, _relative in normalized:
                    path.write_bytes(snapshot[path])
                for _alias, path, relative in normalized:
                    if path.read_bytes() != snapshot[path]:
                        raise RuntimeError(f"restore failed: {relative}")
                    if _head_bytes(root, relative, baseline) != snapshot[path]:
                        raise RuntimeError(
                            f"restored {relative} does not match {baseline}")
                if restored is not None:
                    restored[0] = True


def wait_for_slot(script, *, timeout=900, interval=30):
    deadline = time.monotonic() + timeout
    stdout, stderr = [], []
    command = ["bash", str(script)]
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(command, timeout,
                                            "".join(stdout), "".join(stderr))
        result = run_owned(command, timeout=remaining)
        stdout.append(result.stdout)
        stderr.append(result.stderr)
        if result.returncode == 0:
            return subprocess.CompletedProcess(command, 0, "".join(stdout), "".join(stderr))
        if result.returncode != 1:
            result.check_returncode()
        time.sleep(min(interval, max(0, deadline - time.monotonic())))


def committed_status_contract(root):
    """Recover the shared pure status contract even when common.py is dirty.

    Only the three literal tables and aggregate_status from committed HEAD are
    evaluated. No working-tree module, bootstrap or import is executed. This
    does not authorize mutation: mutating_sources still enforces every byte.
    """
    import ast
    from types import SimpleNamespace
    source = _git(root, 'show', 'HEAD:scripts/gateway-parity/common.py').decode()
    tree = ast.parse(source)
    names = {'BATCH_STATUS_ORDER', 'BATCH_EXIT_CODES', 'MUTATION_STATUS'}
    namespace = {}
    for node in tree.body:
        if (isinstance(node, ast.Assign) and len(node.targets) == 1
                and isinstance(node.targets[0], ast.Name) and node.targets[0].id in names):
            namespace[node.targets[0].id] = ast.literal_eval(node.value)
    functions = [node for node in tree.body if isinstance(node, ast.FunctionDef)
                 and node.name == 'aggregate_status']
    if set(namespace) != names or len(functions) != 1:
        raise RuntimeError('#166 committed shared status contract is incomplete')
    exec(compile(ast.Module(body=functions, type_ignores=[]), '<HEAD status contract>', 'exec'), namespace)
    return SimpleNamespace(aggregate_status=namespace['aggregate_status'])
