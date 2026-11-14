# anchor-selfcheck: unsupported shared process/source guard, no mutation definitions
"""Bounded process ownership shared by the gateway negative controls.

Every invocation owns a fresh session, so a timeout can kill the whole tree.
Collection after that kill is bounded too: a descendant that called setsid()
escapes the process group and can keep the inherited pipes open, which is why
a plain ``with Popen(...)`` plus ``communicate()`` can block forever even after
the leader is dead (#528).
"""
from contextlib import contextmanager, nullcontext
import builtins
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

# Total budget for a cargo slot wait in fixtures that also build afterwards
# (#1556). cargo-slots.sh may need one cold bootstrap of up to its 240 s
# budget on the first call of a machine, so a budget that is only one cold
# call long leaves no room to retry a full pool. Single source of truth: the
# number used to be copied into every fixture.
CARGO_SLOTS_WAIT_SECONDS = 600

# Default cap for ONE `cargo-slots.sh` sample inside wait_for_slot (#1692 §2).
# The total wait budget is only the deadline; a single sample must not hold the
# caller — and, in the mutating fixtures, the source-tree lock — for the whole
# budget when the gate hangs.
CARGO_SLOTS_SAMPLE_SECONDS = 10.0

# Escape hatch for the gate's own cold bootstrap. On the first call of a host
# `cargo-slots.sh` may spend up to its 240 s BOOTSTRAP_TIMEOUT inside one
# `--status` du (#1556 r1 P1-2). A 10 s sample would kill that bootstrap
# forever, so after one short sample times out the next sample may use this
# larger cap — still bounded, still below any caller's total budget.
CARGO_SLOTS_CALL_SECONDS = 300.0

# Depth of the source-restore critical section and the signal it swallowed.
# Read and written from a signal handler, which runs on the main thread between
# bytecodes, so plain globals are enough.
_RESTORE_DEPTH = 0
_DEFERRED_SIGNAL = None

# Signals the restore window shields itself.  SIGINT joins the two the fixture
# handlers already install, because a Ctrl-C lands on the same write-back loop;
# SIGQUIT joins them because ``^\`` is catchable and deferrable the same way
# (GLM sibling r20 P2), while SIGKILL stays the documented uncatchable
# exemption.
_SHIELDED_SIGNALS = ("SIGINT", "SIGTERM", "SIGHUP", "SIGQUIT")


def _record_signal(signum, _frame):
    """Handler installed for the duration of ``restore_critical``.

    Keeps the *first* signal: the window's contract is one deferred signum, so
    a second signal arriving inside the same window is dropped rather than
    queued -- and never overwrites the first (anthropic concurrency r20 P2-1 /
    boundary r20 P2-2, which the old unconditional store violated).
    """
    global _DEFERRED_SIGNAL
    if _DEFERRED_SIGNAL is None:
        _DEFERRED_SIGNAL = signum


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
# The registry is process state, not module state (#1166 r9): runner_control
# publishes an in-memory candidate copy of this module while the original
# already owns the tree lock, and a copy with its own empty registry would
# open a second fd and flock against its own process until the deadline.
# The one registry therefore lives on ``builtins``, outside every module
# ``__dict__`` that a published copy re-executes; ``dict.setdefault`` on that
# real dict is atomic, so racing first callers cannot end up with two.
_HELD_STATE = "_pdrs_negative_process_held"


def _held_state():
    state = globals().get(_HELD_STATE)
    if state is None:
        state = builtins.__dict__.setdefault(
            _HELD_STATE, {"held": {}, "guard": threading.Lock()})
        globals()[_HELD_STATE] = state
    return state


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
        state = _held_state()
        with state["guard"]:
            held = state["held"].get(key)
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
        with state["guard"]:
            state["held"][key] = {"fd": fd, "depth": 1}
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
        state = _held_state()
        if self._delegate:
            self._delegate = False
            with state["guard"]:
                held = state["held"].get(key)
                if held is not None:
                    held["depth"] -= 1
                    if held["depth"] <= 0:
                        del state["held"][key]
            return
        fd, self._fd = self._fd, None
        if fd is None:
            return
        with state["guard"]:
            state["held"].pop(key, None)
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)


def tree_lock(root, **kwargs):
    # The lock is a persistent file, never removed: unlinking a flock-ed file
    # would let the next opener create a different inode and acquire a second
    # "exclusive" lock while the first holder still owns the old one.
    return TreeLock(Path(root) / ".negative-source.lock", **kwargs)


GIT_TIMEOUT = 120


def _git(root, *args):
    # Popen, not subprocess.run: runner meta-controls patch subprocess.run to
    # catch cargo launched outside run_owned, and git is not such a launch.
    # The call runs inside the restore window, where SIGINT/SIGTERM are
    # swallowed until the window closes; a git that blocked forever would hold
    # that window open with no way to interrupt it, so it gets its own session
    # and a hard timeout (MiniMax concurrency r19 P1).
    process = subprocess.Popen(["git", "-C", str(root), *args],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=GIT_TIMEOUT)
    except subprocess.TimeoutExpired:
        # Group kill, then a bounded collect: git runs in its own session, so
        # a plain kill() only reaches the leader while a setsid descendant
        # (external diff / textconv under a non-default repo config) still
        # holds the pipes and would block an unbounded communicate() forever
        # -- inside the shielded window, with no signal left to interrupt it
        # (GLM concurrency r20 P2-1).  ``run_owned`` uses the same pair.
        _kill_group(process)
        _collect(process, KILL_TIMEOUT)
        raise RuntimeError(f"git {' '.join(args)}: timed out after "
                           f"{GIT_TIMEOUT}s")
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


def target_environment(root, env=None):
    """Admit the caller's CARGO_TARGET_DIR through the one shared rule.

    #1166 redesign spec: sibling runners must not keep their own allow-lists of
    target directories. This is the single sibling entry point: an unset
    variable defaults to this tree's shared target, and every rejection is
    defined only in ``_target_environment`` (blank, dot, filesystem root,
    repository root, inside ``src``); an in-tree ``_build`` directory or the
    host chain's dedicated out-of-tree target is equally admissible.
    """
    root = Path(root).resolve()
    env = dict(os.environ if env is None else env)
    if 'CARGO_TARGET_DIR' not in env:
        env['CARGO_TARGET_DIR'] = str(root / '_build/cargo-target')
    return _target_environment(root, env)


def _scanner_environment(env=None):
    """One target interpretation, independent of the Python/Cargo caller cwd.

    The target may be outside this tree. Each coordinator must dedicate it to
    one tree; location is not provenance. Relative paths are rooted here and
    the absolute value is passed to both Cargo and the scanner.
    """
    return target_environment(Path(__file__).resolve().parents[4], env)


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
        # --locked, not --offline: a cold registry (fresh CI runner) must be
        # allowed to download exactly the committed Cargo.lock versions, while
        # the lockfile itself stays pinned (#1166 fix-r6 metadata-observer red).
        result = run_owned(['cargo', 'build', '--locked', '--manifest-path', 'src/Cargo.toml',
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
            raise ValueError('Rust scanner identity mismatch; prebuild the current example')
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


def _verify_snapshot_owner(marker, payload, artifact, identity):
    """共享构建目录的产物守卫只对协作写者负责，对抗写者归已知边界。

    A reused cargo artifact legitimately predates this build's start (cargo
    skips recompilation), so no mtime lower bound exists (#1166 redesign:
    the r4 bound rejected every warm target as a false FAIL).
    """
    try:
        if marker.read_text() != payload:
            raise RuntimeError('#1166 build owner changed during snapshot')
        if _artifact_identity(artifact) != identity:
            raise RuntimeError('#1166 build artifact changed during snapshot')
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
        _verify_snapshot_owner(marker, owner, artifact, identity)
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
def published_module(name, path, source):
    """Compile and publish ``source`` as module ``name`` for the with-body.

    A valid __pycache__ entry bypasses every import loader patch, so an
    in-memory negative control must compile its candidate and publish the
    module object directly — the same publication this module uses for the
    pristine ruler module above. The disk copy stays untouched; the previous
    ``sys.modules`` entry is restored on exit.
    """
    module = ModuleType(name)
    module.__file__ = str(path)
    exec(compile(source, str(path), "exec"), module.__dict__)
    replaced = sys.modules.get(name)
    sys.modules[name] = module
    try:
        yield module
    finally:
        if replaced is not None:
            sys.modules[name] = replaced
        else:
            sys.modules.pop(name, None)


def _shield_signals():
    """Swap in the recording handler; return what to put back."""
    previous = {}
    for name in _SHIELDED_SIGNALS:
        signum = getattr(signal, name, None)
        if signum is None:
            continue
        try:
            current = signal.getsignal(signum)
            if current is signal.SIG_IGN:
                continue
            signal.signal(signum, _record_signal)
        except (ValueError, OSError):
            # Not the main thread, or a platform without the signal: the
            # consumer-side ``defer_signal_exit`` fallback still applies.
            previous.pop(signum, None)
            continue
        previous[signum] = current
    return previous


def _unshield_signals(previous):
    """Put the handlers back unless the window installed its own.

    A handler registered *inside* the window is a deliberate later choice and
    outranks the one the window swapped out; only the shield's own recorder is
    reverted.
    """
    for signum, handler in previous.items():
        try:
            if signal.getsignal(signum) is not _record_signal:
                continue
            signal.signal(signum, handler)
        except (ValueError, OSError):
            continue


@contextmanager
def restore_critical():
    """Shield the source-restore window from the fixture's own SIGTERM exit.

    ``terminate_cleanly`` raises ``SystemExit`` from the signal handler so the
    ``finally`` blocks unwind and the mutated sources are written back.  That
    raise lands on an arbitrary bytecode boundary -- including *inside* the
    write-back loop -- and an exit there leaves a mutant in the tree, which the
    next run then reads as a real kill (MiniMax concurrency r17 P1-1).  Inside
    this section the handler only records the signal; ``take_deferred_signal``
    hands it back once the restore and its byte verification are complete, so
    the process still exits with 128+signum, just not mid-restore.

    Recording is *not* delegated to the consumer: a fixture that installs its
    own ``signal.signal`` handler for SIGTERM never calls
    ``defer_signal_exit``, so the window has to swap the handlers itself for
    every signal it can (SIGINT, SIGTERM, SIGHUP, SIGQUIT); otherwise one more
    fixture -- or a Ctrl-C -- keeps killing the write-back mid-loop
    (MiniMax sibling r18 P1-2 / GLM sibling r18 P1-3).  A handler that is
    ``SIG_IGN`` stays ignored, and a non-main thread, where ``signal.signal``
    is unavailable, keeps the old behaviour through ``defer_signal_exit``.
    """
    global _DEFERRED_SIGNAL, _RESTORE_DEPTH
    previous = _shield_signals()
    _RESTORE_DEPTH += 1
    try:
        yield
    except BaseException:
        # The body failed.  Its exception is what the caller must see, but the
        # recorded signum may not outlive this window: the caller's
        # ``take_deferred_signal`` line is never reached on that path, so a
        # later window in the same process would take the stale signal and exit
        # 128+signum for a signal that arrived minutes earlier (MiniMax
        # boundary r19 P2).
        _DEFERRED_SIGNAL = None
        raise
    finally:
        _RESTORE_DEPTH -= 1
        _unshield_signals(previous)


def defer_signal_exit(signum):
    """Record *signum* instead of exiting when the restore is in progress.

    Same keep-first rule as the window's own recorder: a second signal never
    overwrites the first deferred one.
    """
    global _DEFERRED_SIGNAL
    if not _RESTORE_DEPTH:
        return False
    if _DEFERRED_SIGNAL is None:
        _DEFERRED_SIGNAL = signum
    return True


def take_deferred_signal():
    """The signum deferred during the restore, consumed once."""
    global _DEFERRED_SIGNAL
    signum = _DEFERRED_SIGNAL
    _DEFERRED_SIGNAL = None
    return signum


# Scratch directory for atomic source publication (#1903). Relative to the tree
# root on purpose: `_assert_input_areas` scans `src` / `.cargo` /
# `rust-toolchain*` with `git ls-files --others`, which counts ignored files, so
# a SIGKILL residue beside the target would lock every later run out. The root
# is outside every scanned area -- that is the protection; the `.gitignore`d
# `.negctl-*` name only keeps `git status` clean.
PUBLISH_DIR = '.negctl-publish'


DEFAULT_SOURCE_MODE = 0o644


def write_source_atomic(root, path, data, *, mode=None):
    """Publish source bytes in one rename, never a truncating in-place write.

    #1903 (#1692 §9 residue): `path.write_text` / `write_bytes` truncate first,
    so a SIGKILL in the middle of the write-back leaves an empty or
    half-written source -- not even recoverable by `git checkout`. The temp
    file goes to `PUBLISH_DIR` at the tree root, which is outside the areas
    `_assert_input_areas` scans, so a residue left by a SIGKILL (which never
    runs `finally`) is not read back as an "untracked build input". Same
    filesystem, so `os.replace` is ONE atomic rename: a reader sees the old
    bytes or the new ones, never a half-written file.

    The temp is a NEW inode, so the rename would publish the temp's own mode
    (0666 & ~umask) instead of the source's. `core.filemode=true` would then
    show a mode-only `git diff` on `src/...` and the next preflight would
    refuse the whole tree; the mode is taken from the source and applied to
    the temp before the rename.

    Fail-closed: any failing step raises. Nothing here swallows an error,
    because a silent skip is exactly the defect class this closes.
    """
    path = Path(path)
    publish = Path(root) / PUBLISH_DIR
    publish.mkdir(parents=True, exist_ok=True)
    temporary = publish / f'.negctl-{path.name}.tmp'
    # 目标可能在变异窗口里被删掉（变异体自己删文件是合法形态）。此时没有 mode
    # 可继承 —— 无条件的 `path.stat()` 会在写第一份之前抛 FileNotFoundError，
    # 而还原是**顺序循环**，一份失败会连带丢掉后面所有受守卫文件的还原。
    # 兜底取 git 记录的常规文件模式 100644：兜底成 0o600 会与 HEAD 只差 mode，
    # `core.filemode=true` 的 `git diff --name-only` 照样列出，下一次 preflight
    # 整树拒跑 —— 正是这个函数存在的意义（见上文的 mode 说明）。调用方知道原
    # mode 时必须显式传 `mode=`：`mutating_sources` 在快照时就记下了它。
    if mode is None:
        mode = path.stat().st_mode & 0o7777 if path.exists() else DEFAULT_SOURCE_MODE
    try:
        with temporary.open('wb') as stream:
            stream.write(data)
            stream.flush()
            # SIGKILL is the threat here, not power loss: the page cache
            # outlives the process, so the flush above is enough; the rename
            # itself needs no directory fsync for that.
            os.fsync(stream.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


@contextmanager
def mutating_sources(root, paths, *, baseline="HEAD", lock_timeout=900):
    """Lock the tree, prove ``paths`` are pristine, snapshot, mutate, restore.

    ``paths`` must be repo-relative or absolute paths inside ``root``. Every
    path is checked byte-for-byte against ``git show <baseline>:<path>`` while
    the tree lock is held; the whole workspace input tree is also checked so a
    killed runner from another group cannot leave a mutant behind. A mismatch
    aborts *before* anything is written and leaves the unexpected bytes for a
    human to inspect. On exit the snapshot is published back through
    ``write_source_atomic`` (one rename, never a truncating in-place write) and
    compared both to the snapshot and to the same git object.

    Nested product-only use by one thread is supported through TreeLock;
    parity mutation windows must not nest their non-reentrant ruler lease.
    Any scripts/gateway-parity path requires common.py in the guarded set:
    acquire the shared parity lease before yielding, then retain it through
    restoration and byte verification. Lease polling and each guard flock
    acquisition share the caller's lock_timeout budget (#1528: the old 60s
    cap starved runners queuing behind another tree's case window; the
    budget is now the parity family's largest live hold, not merely this
    caller's own window, so a queued runner survives it); a busy lock
    still fails before source writes, and a residual lease (published
    owner dead while cleanupPending is set) fails fast with a
    diagnostic instead of waiting the budget out (#2929 fix-r3).
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
        # 快照时就记下 mode：变异体可能把目标删掉，还原时再 stat 就没有 mode 可
        # 继承了。传回去才不会有 mode-only `git diff` 把下一次 preflight 卡死。
        snapshot_modes = {path: path.stat().st_mode & 0o7777
                          for _alias, path, _relative in normalized}
        parity = any(relative.startswith("scripts/gateway-parity/")
                     for _alias, _path, relative in normalized)
        lock = (_parity_mutation_lock(root, snapshot, lock_timeout)
                if parity else nullcontext())
        with lock as restored:
            try:
                yield {alias: snapshot[path] for alias, path, _relative in normalized}
            finally:
                with restore_critical():
                    for _alias, path, _relative in normalized:
                        write_source_atomic(root, path, snapshot[path],
                                            mode=snapshot_modes[path])
                    for _alias, path, relative in normalized:
                        if path.read_bytes() != snapshot[path]:
                            raise RuntimeError(f"restore failed: {relative}")
                        if _head_bytes(root, relative, baseline) != snapshot[path]:
                            raise RuntimeError(
                                f"restored {relative} does not match {baseline}")
                if restored is not None:
                    restored[0] = True
    # A SIGTERM that landed while the sources were being written back was
    # recorded instead of raised; exit only now that the tree, the parity lease
    # and its byte verification are all settled (MiniMax concurrency r17 P1-1).
    pending = take_deferred_signal()
    if pending is not None:
        raise SystemExit(128 + pending)


def wait_for_slot(script, *, timeout=900, interval=30, gate_required=False,
                  sample_timeout=CARGO_SLOTS_SAMPLE_SECONDS):
    if not math.isfinite(sample_timeout) or sample_timeout <= 0:
        # A non-positive/infinite cap would recreate the unbounded sample this
        # parameter exists to remove (#1692 §2). Fail closed like the caller's
        # own slot guard does.
        raise ValueError(f'sample_timeout must be finite and positive: {sample_timeout!r}')
    deadline = time.monotonic() + timeout
    stdout, stderr = [], []
    command = ["bash", str(script)]
    cold_bootstrap = False
    # The long (cold-bootstrap) sample is granted at most ONCE per wait: if it
    # times out as well the gate is hung rather than bootstrapping, and spending
    # the rest of the budget in CARGO_SLOTS_CALL_SECONDS chunks would hide the
    # hang from the caller (review P2).
    long_sample_available = True
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(command, timeout,
                                            "".join(stdout), "".join(stderr))
        # One sample is bounded by sample_timeout; the total budget only decides
        # whether another sample may start. After a short sample was killed we
        # allow one long sample so the gate's documented cold bootstrap can
        # finish (#1556) without giving every hung call the whole budget.
        use_long = cold_bootstrap and long_sample_available
        cap = min(remaining, CARGO_SLOTS_CALL_SECONDS if use_long else sample_timeout)
        long_sample_available = long_sample_available and not cold_bootstrap
        try:
            result = run_owned(command, timeout=cap)
        except subprocess.TimeoutExpired:
            # Retry while the total budget lasts; only then let the timeout out —
            # state/decisions-20261106.md §5 classifies `TimeoutExpired ...
            # cargo-slots.sh` as "environment did not produce a verdict", not as
            # a negative-control result.
            if time.monotonic() >= deadline:
                raise
            cold_bootstrap = True
            continue
        cold_bootstrap = False
        stdout.append(result.stdout)
        stderr.append(result.stderr)
        if result.returncode in (0, 69):
            # 0 = slot free. 69 (EX_UNAVAILABLE) = cargo-slots.sh found no
            # unified gate (a CI runner has no ~/ws/pdrs-ops nor a
            # ~/ws/pdrs-runner-tools runner copy): no shared quota exists to
            # wait for, so proceed. Only 1 (pool full) means retry;
            # 2 / 3 / 4 / 64 mean the gate itself is broken -> hard error.
            if result.returncode == 69 and gate_required:
                # The caller already proved an executable gate exists (see
                # run_host.slot_gate), so 69 can only mean the pinned gate lost
                # its executable bit or vanished during the wait. Proceeding
                # would run cargo with the per-host quota silently skipped.
                raise subprocess.CalledProcessError(
                    69, command, "".join(stdout), "".join(stderr))
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
