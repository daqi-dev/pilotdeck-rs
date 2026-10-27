"""Bounded process ownership shared by the gateway negative controls.

Every invocation owns a fresh session, so a timeout can kill the whole tree.
Collection after that kill is bounded too: a descendant that called setsid()
escapes the process group and can keep the inherited pipes open, which is why
a plain ``with Popen(...)`` plus ``communicate()`` can block forever even after
the leader is dead (#528).
"""
from contextlib import contextmanager
import fcntl
import os
from pathlib import Path
import signal
import subprocess
import threading
import time

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


def run_owned(command, *, env=None, timeout=900, cwd=None, kill_timeout=KILL_TIMEOUT):
    # cargo can leave test binaries (and inherited output pipes) behind when
    # killed alone. Do not use `with Popen(...)`: its __exit__ waits forever on
    # a tree that only the group signal can reach.
    process = subprocess.Popen(command, env=env, cwd=cwd,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except BaseException as error:
        _kill_group(process)
        stdout, stderr = _collect(process, kill_timeout)
        if isinstance(error, subprocess.TimeoutExpired):
            error.output = _prefer_more(_as_text(process, error.output), stdout)
            error.stderr = _prefer_more(_as_text(process, error.stderr), stderr)
        raise
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


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
        # A blocking flock cannot have a deadline by itself. In the main thread
        # use SIGALRM to interrupt the wait at the total timeout; other threads
        # poll the same kernel lock so this module never installs a handler from
        # a thread that cannot receive the signal.
        if threading.current_thread() is threading.main_thread():
            def expired(_signum, _frame):
                raise TimeoutError(f"source lock busy: {self.path}")

            previous = signal.signal(signal.SIGALRM, expired)
            if self.timeout > 0:
                signal.setitimer(signal.ITIMER_REAL, self.timeout)
            try:
                fcntl.flock(fd, fcntl.LOCK_EX)
            finally:
                signal.setitimer(signal.ITIMER_REAL, 0)
                signal.signal(signal.SIGALRM, previous)
            return
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


def _assert_product_pristine(root, baseline):
    # A runner only names the files it is about to mutate, but a killed
    # predecessor from another group can leave any product source mutated.
    # Checking the whole gateway product tree turns that residue into a
    # refusal for every next runner, not just the one that shares its file.
    area = "src/pilotdeck-gateway/src"
    changed = _git(root, "diff", "--name-only", baseline, "--", area).decode(
        "utf-8", "replace").splitlines()
    untracked = _git(root, "ls-files", "--others", "--exclude-standard",
                     "--", area).decode("utf-8", "replace").splitlines()
    unexpected = changed + untracked
    if unexpected:
        detail = "differs from" if changed else "untracked at"
        raise SourceBaselineError(root, unexpected[0],
                                  f"{detail} {baseline}")


@contextmanager
def mutating_sources(root, paths, *, baseline="HEAD", lock_timeout=900):
    """Lock the tree, prove ``paths`` are pristine, snapshot, mutate, restore.

    ``paths`` must be repo-relative or absolute paths inside ``root``. Every
    path is checked byte-for-byte against ``git show <baseline>:<path>`` while
    the tree lock is held; the whole gateway product tree is also checked so a
    killed runner from another group cannot leave a mutant behind. A mismatch
    aborts *before* anything is written and leaves the unexpected bytes for a
    human to inspect. On exit the snapshot is written back and compared both to
    the snapshot and to the same git object.

    Nested use by one thread is supported through :class:`TreeLock`, so a
    runner executed in-process (round6) can take the same lock as its caller.
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
