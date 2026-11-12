# anchor-selfcheck: unsupported shared mutation runner, no mutation definitions
"""Bounded negative controls: mutate only disposable /tmp copies."""
import contextlib
import difflib
import errno
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import tempfile
import time
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import (CARGO_SLOTS_CALL_SECONDS, CARGO_SLOTS_WAIT_SECONDS,
                             mutating_sources, run_owned, wait_for_slot)
from negative_selection import (missing_target, refuse,
                                refuse_unusable_target)
import pending_reanchor as pending

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'


@contextlib.contextmanager
def scratch(label, *, deadline=580):
    """SIGTERM/INT/deadline unwind child waits before deleting the directory."""
    def stop(signum, frame):
        raise SystemExit(f'interrupted by signal {signum}')

    signals = (signal.SIGTERM, signal.SIGINT, signal.SIGALRM)
    previous = {sig: signal.signal(sig, stop) for sig in signals}
    signal.alarm(deadline)
    root = None
    status = subprocess.check_output(['git', 'status', '--short'], cwd=REPO)
    try:
        with tempfile.TemporaryDirectory(prefix=f'pdrs222-{label}-', dir='/tmp') as name:
            root = Path(name)
            print(f'temporary directory: {root}', flush=True)
            yield root
    finally:
        signal.alarm(0)
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        if root is not None:
            # #1666: explicit raise, not `assert` -- `-O` strips asserts and
            # this is the only check that a control did not leak its scratch
            # directory or dirty the tree.
            if root.exists():
                raise RuntimeError(f'temporary directory leaked: {root}')
            print(f'temporary directory removed: {root}', flush=True)
        after = subprocess.check_output(['git', 'status', '--short'], cwd=REPO)
        if after != status:
            raise RuntimeError('negative control changed git status')
        print('git status --short unchanged (including pending repair edits)', flush=True)


def environment(root):
    if missing_target():
        # #1666/#1734: a missing caller target is "never started" (2), not
        # "ran and did not pass" (1). `SystemExit('...')` carried the message
        # and therefore exited 1; `refuse()` writes it to stderr and passes
        # the code as an int. An empty or all-space value is the same caller
        # error, and a presence-only check let it through to the shared
        # normalisation (#1666 fix-r8, found by two reviewers on both sides).
        refuse('CARGO_TARGET_DIR must be supplied by the caller')
    env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG=os.environ.get('CARGO_PROFILE_DEV_DEBUG', 'line-tables-only'),
               CARGO_PROFILE_TEST_DEBUG=os.environ.get('CARGO_PROFILE_TEST_DEBUG', 'line-tables-only'),
               CARGO_INCREMENTAL='0', RUSTC_WRAPPER='', TMPDIR=str(root))
    for key in ('PDRS_RECORDER_ORACLE', 'PDRS_SESSION_DOMAIN_ORACLE',
                'PDRS_TRANSCRIPT_ADAPTER_ORACLE'):
        env.pop(key, None)
    return env


def run(name, command, env, cwd=SRC, diagnostic=None, test=None):
    if command[0] == 'cargo':
        # 等待预算 / 单次调用上限都取共享常量（#1843 E，见 negative_process 的同名常量）：
        # 预算若只够一次冷调用，冷缓存 + 名额已满时第一次调用就把预算吃光，
        # 后面直接 assert 失败（#1556 r2）；两者相等（300/300）同样没有重试余地，
        # 且 assert 硬崩会被记成假 FAIL 而不是「环境未取得判定」（#1556 r6 sibling P1）。
        # 与 negative_process.wait_for_slot 同口径：单次超时只算一次机会，预算内重试。
        deadline = time.monotonic() + CARGO_SLOTS_WAIT_SECONDS
        while True:
            # 单次上限 ≥ 脚本自身的 bootstrap 预算（#1556 r5 sibling P1）：
            # 首次碰统一闸要等冷 du（实测 31.8–42.2 s，结构上限是闸自己的 180 s du 超时），
            # 之后走名额缓存是毫秒级。见 scripts/cargo-slots.sh 头注释。
            try:
                slot = subprocess.run([str(REPO / 'scripts/cargo-slots.sh')],
                                      capture_output=True, text=True,
                                      timeout=CARGO_SLOTS_CALL_SECONDS)
            except subprocess.TimeoutExpired:
                if time.monotonic() >= deadline:
                    raise
                continue
            print(slot.stdout + slot.stderr, end='', flush=True)
            if slot.returncode in (0, 69):
                # 69 (EX_UNAVAILABLE) = 本机没有统一闸（CI 就没有）：没有共享名额可等，
                # 直接继续。2/3/4/64 = 闸坏了，必须硬失败，不得当成有名额。
                break
            if slot.returncode != 1 or time.monotonic() >= deadline:
                raise RuntimeError(f'cargo slots full: gate returned {slot.returncode}'
                                   ' and the wait budget is spent')
            time.sleep(10)
    print(f'--- {name} ---', flush=True)
    # A file avoids pipe backpressure; the complete raw output is printed before
    # cleanup, so the caller can retain evidence outside the disposable directory.
    log_path = Path(env['TMPDIR']) / 'child.log'
    with log_path.open('w') as log:
        child = subprocess.Popen(command, cwd=cwd, env=env, stdout=log,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = child.wait(timeout=240)
        except BaseException:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            raise
    output = log_path.read_text()
    print(output, end='', flush=True)
    print(f'{name}: exit={code}; pid={child.pid}; reaped={child.poll() is not None}', flush=True)
    if command[0] == 'cargo':
        # #1666: these four decide KILLED vs PASS. As `assert` they vanished
        # under `-O`, so a mutant that never compiled or never ran the bound
        # test would have been recorded KILLED -- a ruler passing without
        # measuring anything.
        if 'running 1 test' not in output:
            raise RuntimeError('compilation or zero-test failure is not evidence')
        if diagnostic is None and 'test result: ok. 1 passed' not in output:
            expected = 'test result: ok. 1 passed'
            raise RuntimeError(f'{name}: expected {expected!r} in the output')
    if diagnostic:
        # #1087 fix-r2: the kill criterion is the shared strict #813 judge --
        # the named test's own FAILED status line plus the diagnostic inside
        # that test's own panic payload (negative_panic judges both). The old
        # whole-log `expected` / `diagnostic in output` checks also matched a
        # sibling test's panic, captured stdout and build output.
        if test is None:
            raise RuntimeError(f'{name}: a diagnostic run must name the bound test')
        if code == 0 or not named_test_killed(output, test, diagnostic):
            raise RuntimeError(f'{name}: the bound test {test} did not fail with the '
                               f'expected diagnostic {diagnostic!r} (exit={code})')
        print(f'{name}: KILLED', flush=True)
    else:
        if code != 0:
            raise RuntimeError(f'{name}: exited {code}, expected 0')
        print(f'{name}: PASS', flush=True)


def _tracked(*paths):
    """Repository-relative tracked files under the given paths."""
    listing = subprocess.check_output(['git', 'ls-files', '-z', '--', *paths], cwd=REPO)
    return [Path(os.fsdecode(entry)) for entry in listing.split(b'\0') if entry]


def _untracked(*paths):
    """Repository-relative files under `paths` that git does not track yet.

    `--exclude-standard` leaves ignored build output out, so this is the set a
    control would have carried before #1570 replaced the per-suite
    `copytree(specs)` inventories: a pending repair that adds a data file does
    not `git add` it first (#1570 r15 concurrency). Ignored data files are
    deliberately not carried: they are build output by convention, and a
    missing one fails the control loudly rather than passing it.
    """
    listing = subprocess.check_output(
        ['git', 'ls-files', '-z', '--others', '--exclude-standard', '--', *paths], cwd=REPO)
    return [Path(os.fsdecode(entry)) for entry in listing.split(b'\0') if entry]


def _untracked_files(*paths):
    """Untracked plain files under `paths`; a link or a directory is refused.

    `git ls-files --others` lists a symlinked directory as a single entry and
    never descends into it, and `copyfile` follows whatever a link points at --
    a `.env.local`, a file under `~/.pilotdeck`. Neither may reach `/tmp`
    (#1570 r16 codex P0). A directory entry would otherwise land in the copy
    loop's "tracked member deleted" branch and be reported as something it is
    not (#1570 r16 codex P1).
    """
    files = []
    for path in _untracked(*paths):
        source = REPO / path
        if source.is_symlink() or not source.is_file():
            raise RuntimeError(f'source_copy: {path} is an untracked symlink or directory '
                               'in an input directory; the control copies plain files only, '
                               'so add it to git (or remove it) before running')
        _refuse_special(source, path, 'an input directory')
        files.append(path)
    return files


def _dotenv_part(path):
    """The path component that names a dotenv file, or None.

    `.env*` is the contract: `source_copy` never carries those bytes, because
    the controls' `environment()` assumes a clean scratch tree. The match is
    per component and case-insensitive -- `src/.ENV.production` is the same
    file on a macOS volume, and `specs/.env.production/credentials` names a
    dotenv *directory* whose children are dotenv content (#1570 r19 codex P0).
    `prod.env` is included too: the suffix is how `docker --env-file` and
    `python-dotenv` spell the same thing.
    """
    for part in path.parts:
        folded = part.lower()
        if folded.startswith('.env') or folded.endswith('.env'):
            return part
    return None


def _refuse_special(source, path, kind):
    """Refuse a file whose bytes are not a plain copy of the checkout's own.

    Three shapes can hand `/tmp` something the checkout never meant to share:
    a symbolic link (`copyfile` follows it), a hard link (same bytes under a
    second name, so the name says nothing about what it is), and a dotenv file
    (`source_copy` never copies `.env*`, tracked or not -- that is the promise
    the controls' `environment()` relies on). All three are refused loudly
    instead of copied, because a leak cannot be undone after the fact
    (#1570 r16/r17/r18 codex P0).
    """
    part = _dotenv_part(path)
    if part is not None:
        raise RuntimeError(f'source_copy: {path} is a dotenv file in {kind} ({part}); the '
                           'control never copies .env* (tracked or not), so move it out of '
                           'the workspace inputs or remove it before running')
    try:
        links = source.stat().st_nlink
    except OSError:
        return
    if links > 1:
        raise RuntimeError(f'source_copy: {path} is a hard link in {kind} ({links} names); '
                           'the control cannot tell what else it names, so replace it with '
                           'a real file or remove it before running')


def _escapes_repository(path, root=None):
    """True when `path` resolves (through symlinks) outside the repository.

    `shutil.copyfile` follows symlinks, so a link pointing at `~/.pilotdeck`
    would be copied into the scratch workspace -- the copy is outside the
    checkout and outlives the run in `/tmp` (#1570 r16 codex P0). The
    comparison is by file identity, not spelling: macOS volumes are
    case-insensitive, so a differently-cased alias of the same directory must
    not read as "outside" (#1570 r17 codex P1).
    """
    root = REPO.resolve() if root is None else root
    resolved = (REPO / path).resolve()
    if resolved == root or root in resolved.parents:
        return False
    while True:
        try:
            if os.path.samefile(resolved, root):
                return False
        except OSError:
            return True
        parent = resolved.parent
        if parent == resolved:
            return True
        resolved = parent


def _tracked_but_deleted(relative):
    """True when `relative` is in the git index, even if the tree deleted it.

    `:(literal)` matters: a plain pathspec treats `[`, `*` and `?` in a file
    name as glob syntax, so `src/a[b.rs` would silently stop matching (or
    match a sibling), and a real reference would slip through as fixture text.

    `--error-unmatch` answers 0 (tracked) or 1 (not tracked); anything else is
    a broken question, not an answer -- a corrupt index exits 128 (`fatal:
    .git/index: index file smaller than expected`, measured) and reading that
    as "not tracked" would drop a real reference or misclassify a deleted one.
    Same rule as `_ignored` below, which pins the rc the same way (#1570 r26
    zai boundary P1).
    """
    completed = subprocess.run(
        ['git', 'ls-files', '--error-unmatch', '--', f':(literal){relative.as_posix()}'],
        cwd=REPO, capture_output=True)
    if completed.returncode not in (0, 1):
        raise RuntimeError(f'source_copy: cannot tell whether git tracks {relative} '
                           f'(rc={completed.returncode}): '
                           f'{completed.stderr.decode("utf-8", "replace").strip()}')
    return completed.returncode == 0


def _ignored(relative):
    """True when `.gitignore` excludes `relative` (`--exclude-standard`).

    `check-ignore` takes pathnames rather than pathspecs, so `:(literal)` is
    rejected outright (`fatal: pathspec magic not supported by this command`)
    -- a rc other than 0/1 is a broken question, not an answer, and it fails
    closed (#1570 r20 codex P1).
    """
    completed = subprocess.run(['git', 'check-ignore', '-q', '--', relative.as_posix()],
                               cwd=REPO, capture_output=True)
    if completed.returncode not in (0, 1):
        raise RuntimeError(f'source_copy: cannot tell whether git ignores {relative} '
                           f'(rc={completed.returncode}): '
                           f'{completed.stderr.decode("utf-8", "replace").strip()}')
    return completed.returncode == 0


def _copyable(relative, tracked=None):
    """True when `source_copy` would carry `relative` as a plain file.

    Tracked files are carried, and so are untracked plain files in `src/` or
    in a derived input directory (`git ls-files --others --exclude-standard`)
    (#1570 r18). Links, dotenv paths and hard links are refused by the copy
    loop, so they are not copyable either: the scan must not derive an input
    the copy would then refuse to make, and it must not refuse one the copy
    would carry. Both directions were wrong at once -- an untracked
    `src/data.txt` named by an untracked `src/new.rs` was refused with a
    message about `git ls-files` that this same change had just made untrue
    (#1570 r19 codex P1). The ignored case is the mirror image: a plain
    untracked file that `--exclude-standard` leaves out passed every check
    and was still never copied, so the copy came up short on an input the
    scan had accepted (#1570 r20 codex P1). `tracked` is the `src/` set the
    caller already has; the external branch asks git per path instead, which
    is one subprocess for the three edges this tree has.
    """
    path = REPO / relative
    if path.is_symlink() or not path.is_file():
        return False
    if _dotenv_part(relative) is not None:
        return False
    try:
        if path.stat().st_nlink != 1:
            return False
    except OSError:
        return False
    return _in_index(relative, tracked) or not _ignored(relative)


def _in_index(relative, tracked=None):
    """True when `relative` is in the index the way the copy list reads it.

    `src/` members are looked up in the caller's `ls-files` listing (it
    already has one; asking git per file would be a subprocess each), and
    everything else asks git. One dispatch for both callers -- deriving and
    copying from two different rules is how #1570 came back twice
    (#1570 r21 zai simplify P2).
    """
    if relative.parts[:1] == ('src',):
        if tracked is None:
            tracked = _tracked('src')
        return relative in tracked
    return _tracked_but_deleted(relative)


def _indexed(path, tracked):
    """True when `path` names something in the git index, even if the tree deleted it.

    `git ls-files` lists the index, so a pending repair that deletes a tracked
    file leaves it listed, and a file behind a tracked symlink (`src/link ->
    real`) is listed under its own name rather than the path that names it.
    Both are real references, not fixture text: refusing them loudly is what
    keeps the copy from silently coming up short (#1570 r10/r11).
    """
    for base in (REPO, REPO.resolve()):
        try:
            relative = path.relative_to(base)
        except ValueError:
            continue
        return _in_index(relative, tracked)
    return False


def _repository_identity():
    """`(st_dev, st_ino)` of the checkout root, taken before the copy starts.

    The descriptor walk below protects every component *under* the root, but
    `os.open(REPO)` itself would follow a link if the whole checkout were
    replaced between the scan and the copy (#1570 r21 codex P0). The caller
    takes this identity once, before it reads anything, and every later open
    of the root must land on the same directory.
    """
    try:
        info = os.stat(REPO)
    except OSError as error:
        raise RuntimeError(f'source_copy: cannot stat the repository root {REPO}: '
                           f'{error}') from error
    return (info.st_dev, info.st_ino)


def _open_input(relative, root_identity=None):
    """Open a workspace input without following a link at any level.

    Opening `REPO / relative` with `O_NOFOLLOW` protects only the last
    component: swap a *parent* directory (`specs -> /tmp/elsewhere`) between
    the checks and the open, and the bytes still come from outside the
    checkout (#1570 r20 codex P0). Each component is therefore opened
    relative to the descriptor of the one above (`openat`), so every level is
    checked by the open that uses it and no path is resolved twice. The root
    itself is opened `O_NOFOLLOW` and compared against `root_identity`, so
    replacing the checkout root does not redirect the walk either
    (#1570 r21 codex P0).
    `O_NONBLOCK` matters too: opening a FIFO read-only waits for a writer
    forever, and the type check only happens on the descriptor
    (#1570 r20 codex P0). On a regular file it changes nothing.
    """
    identity = _repository_identity() if root_identity is None else root_identity
    parts = relative.parts
    directory = os.open(REPO, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = os.fstat(directory)
        if (info.st_dev, info.st_ino) != identity:
            raise RuntimeError('source_copy: the repository root changed while the control '
                               'was starting; the scratch copy must come from the tree the '
                               'scan read, so refusing to run the control')
        for part in parts[:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                            dir_fd=directory)
            os.close(directory)
            directory = child
        return os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                       dir_fd=directory)
    finally:
        os.close(directory)


def _sha256_of(descriptor):
    """sha256 of exactly the bytes one descriptor holds."""
    digest = hashlib.sha256()
    with os.fdopen(descriptor, 'rb', closefd=False) as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def _read_input(path, root_identity=None):
    """bytes of one workspace input, read from a single checked descriptor.

    The scan used to call `read_bytes()` on the path: a source swapped for a
    FIFO in that window blocks the control forever, and a source swapped for
    a link is read through it (#1570 r21 zai sibling P2). One open, one type
    check, one read -- the same rule as the copy and the final digest.
    """
    try:
        descriptor = _open_input(path, root_identity)
    except OSError as error:
        raise RuntimeError(f'source_copy: {path} changed while the control was starting '
                           f'({error}); refusing to run the control') from error
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            raise RuntimeError(f'source_copy: {path} is not a regular file; the control '
                               'reads plain files only, so refusing to run the control')
        with os.fdopen(descriptor, 'rb', closefd=False) as handle:
            return handle.read()
    except OSError as error:
        # The read itself can still fail (EIO, a revoked mount). A bare OSError
        # reaching the caller reads as "ran and failed" in the host triage
        # rather than "refused to start"; `_copy_input` already wraps this stage
        # (#1570 r22 concurrency/boundary P3).
        raise RuntimeError(f'source_copy: {path} changed while the control was starting '
                           f'({error}); refusing to run the control') from error
    finally:
        os.close(descriptor)


def _source_digest(path, root_identity=None):
    """sha256 of one workspace input, read from a single descriptor.

    The last line of defence used to call `read_bytes()` on the path again:
    a path swapped for a FIFO after the copy made it hang instead of refuse,
    and a path swapped for a link read through it (#1570 r21 codex P0). The
    descriptor is opened once, checked for type, and read.
    """
    try:
        descriptor = _open_input(path, root_identity)
    except OSError as error:
        raise RuntimeError(f'source_copy: {path} changed while it was being copied '
                           f'({error}); refusing to run the control against a moving tree'
                           ) from error
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            raise RuntimeError(f'source_copy: {path} is not a regular file any more; the tree '
                               'changed while the copy was being made, so refusing to run the '
                               'control against a moving tree')
        return _sha256_of(descriptor)
    except OSError as error:
        # `_sha256_of`'s read is the stage `_copy_input` already wraps: an EIO
        # here used to escape as a bare OSError, which the host triage reads as
        # "ran and failed" instead of "refused against a moving tree"
        # (#1570 r22 concurrency/boundary P3).
        raise RuntimeError(f'source_copy: {path} changed while it was being copied '
                           f'({error}); refusing to run the control against a moving tree'
                           ) from error
    finally:
        os.close(descriptor)


def _copy_input(path, destination, root_identity=None):
    """Copy one workspace input from a single opened descriptor.

    Checking the path and then calling `copyfile` is a check-then-use race: a
    concurrent editor can swap `src/a.rs` for a link to `.env.local` (or add a
    second hard-link name) after `is_symlink()` / `st_nlink` said the file was
    clean, and the final digest check reads through that same link, so it
    agrees with the copy instead of catching it (#1570 r19 concurrency P2).
    `_open_input` makes the open itself the link check at every level,
    `fstat` on the descriptor reports the inode whose bytes are about to be
    written, and the bytes copied are the ones that descriptor holds.
    """
    try:
        descriptor = _open_input(path, root_identity)
    except OSError as error:
        if error.errno in (errno.ELOOP, errno.ENOTDIR):
            raise RuntimeError(f'source_copy: {path} became a symlink or left the repository '
                               'while the control was starting; the control copies the tracked '
                               'file it names, never the bytes a link points at, so refusing '
                               'to run the control') from error
        raise RuntimeError(f'source_copy: cannot copy {path}: {error}') from error
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            raise RuntimeError(f'source_copy: {path} is not a regular file; the control '
                               'copies plain files only, so refusing to run the control')
        if info.st_nlink > 1:
            raise RuntimeError(f'source_copy: {path} is a hard link ({info.st_nlink} names); '
                               'the control cannot tell what else it names, so replace it '
                               'with a real file or remove it before running')
        with os.fdopen(descriptor, 'rb', closefd=False) as handle:
            with open(destination, 'wb') as copy:
                shutil.copyfileobj(handle, copy)
    except OSError as error:
        raise RuntimeError(f'source_copy: cannot copy {path}: {error}') from error
    finally:
        os.close(descriptor)


def root_level_inputs(directories, files):
    """The inputs `compile_inputs` leaves out of `directories` (repository root).

    They have no directory worth copying -- asking git for `.` would copy the
    whole tree -- so callers copy them as individual files. Kept here, not
    inlined at each call site: `source_copy` and every control with its own
    copy loop must agree on which files those are, and a copy loop that stops
    agreeing silently drops an input (#1570).
    """
    return [path for path in files
            if not any(directory in path.parents for directory in directories)]


def verify_copy(root, files):
    """Refuse a copy that lost a compile-time input.

    A copy that lacks an input does not fail as "the mutation survived": it
    fails to compile, every mutation reads INVALID, and the control silently
    stops discriminating -- the #1570 failure itself.
    """
    missing = sorted(str(path) for path in files if not (root / path).is_file())
    if missing:
        raise RuntimeError('compile-time inputs missing from the copy: '
                           + ', '.join(missing))


# `include_str!` / `include_bytes!` take any macro delimiter (`(` `[` `{`) and
# allow trivia -- whitespace, line comments, nested block comments -- between
# every token: `include_str /* why */ ! ( "…" )` compiles, and rustfmt keeps
# the bracket form. A regex for that gap cannot count nested comments, and a
# count that silently reads zero is exactly the #1570 failure (nothing copied,
# every mutation INVALID), so the gap is skipped by a scanner instead. The path
# is a plain string literal (raw strings included), optionally followed by the
# single trailing comma `source_inputs.rs` also accepts.
MACRO_NAME = re.compile(r'\b(include_str|include_bytes|include)\b')
MACRO_OPENERS = {'(': ')', '[': ']', '{': '}'}
IDENTIFIER = re.compile(r'(?:r#)?([A-Za-z_][A-Za-z0-9_]*)')
# Rust's token separator is `Pattern_White_Space`, exactly these eleven code
# points -- not the whole `Zs` category (`rustc` rejects `include_str<NBSP>!`
# with "unknown start of token"). `str.isspace()` is wrong in both directions
# here: it accepts `U+00A0`/`U+1680`/`U+2000`-`U+200A`/`U+202F`/`U+205F`/`U+3000`
# and `U+001C`-`U+001F`, and misses `U+200E`/`U+200F`. A narrower set (say
# `' \t\r\n'`) makes `include_str <VT> !` -- legal Rust -- fall through as
# "not a macro call" and copy nothing.
RUST_WHITESPACE = '\t\n\x0b\x0c\r \x85\u200e\u200f\u2028\u2029'


def _is_trivia_space(char):
    return char in RUST_WHITESPACE


LIFETIME_STOPS = frozenset("'\"\\") | frozenset(RUST_WHITESPACE)


def _lifetime_end(text, index):
    """Index just past the lifetime at `index`, or None when there is none.

    A lifetime is `'` plus an identifier, and Rust identifiers are Unicode
    (`'α` compiles), so an ASCII class would stop the scan and silently drop
    every later `path` in the same attribute. Python's `str.isidentifier` uses
    the same XID tables, but *its* copy can be older than the rustc reading
    the file: Python 3.9 (Unicode 13) does not know `U+11F02`, which rustc
    accepts as an identifier. "This Python does not know the character" must
    therefore not mean "no lifetime" -- that `break`s the attribute scan, the
    #1570 failure -- so the default is inverted: after `'`, anything but a
    quote, a backslash or one of Rust's eleven whitespace code points starts
    a lifetime, and a non-ASCII character continues one even when the local
    tables do not know it. Misreading one token only skips it; stopping loses
    the rest of the attribute.
    """
    position = index + 1
    if position >= len(text) or text[position] in LIFETIME_STOPS:
        return None
    position += 1
    while position < len(text):
        char = text[position]
        if char in LIFETIME_STOPS:
            break
        if char.isascii() and not ('a' + char).isidentifier():
            break
        position += 1
    return position


def _skip_trivia(text, index, limit):
    """Index of the first token after whitespace and Rust comments.

    Block comments nest (`/* a /* b */ c */`) and line comments run to the end
    of the line; both are legal inside a macro invocation, so neither can be
    modelled as "some whitespace". An unterminated block comment returns the
    limit, which makes the caller refuse rather than read past it into code.
    """
    while index < limit:
        char = text[index]
        if _is_trivia_space(char):
            index += 1
        elif text.startswith('//', index):
            newline = text.find('\n', index, limit)
            index = limit if newline < 0 else newline + 1
        elif text.startswith('/*', index):
            depth, index = 1, index + 2
            while index < limit and depth:
                if text.startswith('/*', index):
                    depth, index = depth + 1, index + 2
                elif text.startswith('*/', index):
                    depth, index = depth - 1, index + 2
                else:
                    index += 1
            if depth:
                return limit
        else:
            return index
    return limit


def _skip_string(text, start, limit):
    """Index just past the string literal at `start`, or None if there is none.

    Covers every string form a macro argument or an attribute can hold: plain,
    raw (`r#"…"#`, any `#` count), byte (`b"…"`) and C (`c"…"`). A plain string
    may span lines (`#[doc = "first\nsecond"]` compiles), so a newline does not
    end the scan -- running to the limit returns None, and every caller refuses
    rather than reading past an unterminated literal into code.
    """
    index, raw = start, False
    if text.startswith(('br', 'cr'), index):
        raw, index = True, index + 2
    elif text[index:index + 1] == 'r':
        raw, index = True, index + 1
    elif text[index:index + 1] in ('b', 'c'):
        index += 1
    hashes = 0
    while text[index:index + 1] == '#':
        hashes, index = hashes + 1, index + 1
    if text[index:index + 1] != '"' or (hashes and not raw):
        return None
    if raw:
        closing = '"' + '#' * hashes
        end = text.find(closing, index + 1, limit)
        return None if end < 0 else end + len(closing)
    index += 1
    while index < limit:
        char = text[index]
        if char == '\\':
            index += 2
            continue
        if char == '"':
            return index + 1
        index += 1
    return None


def _skip_char(text, start, limit):
    """Index just past the char literal at `start`, or None (a lifetime)."""
    index = start + 1
    if index >= limit:
        return None
    if text[index] == '\\':
        index += 2
        escape = text[index - 1:index]
        if escape == 'u':                      # \u{1F600}
            close = text.find('}', index, limit)
            if close < 0:
                return None
            index = close + 1
        elif escape == 'x':                    # \x41
            index += 2
    else:
        index += 1
    return index + 1 if text[index:index + 1] == "'" else None


def _skip_literal(text, start, limit):
    """Index just past the literal at `start`, or None when none starts there.

    Literals are skipped whole so that a backtick, a `]` or a `\\` *inside* one
    is never read as syntax -- that is how a real call used to pass for text
    (`include!(concat!(/* ` */ …))`, `concat!("a", '`', "b")`, a raw string
    holding `]"#` inside a `#[cfg_attr]`).
    """
    if text[start:start + 1] == "'":
        return _skip_char(text, start, limit)
    if text[start:start + 1] in ('b', 'c') and text[start + 1:start + 2] == "'":
        return _skip_char(text, start + 1, limit)
    return _skip_string(text, start, limit)


def _read_path_literal(text, start, limit):
    """`(value, end, raw)` for the path string literal at `start`, else None.

    Accepts plain strings and raw strings (`r"…"`, `r#"…"#`). A plain string
    keeps its escapes undecoded and comes back with `raw=False`; `compile_inputs`
    refuses that case, because decoding `\\x66` by hand is how a scan starts
    guessing at paths.
    """
    index, raw = start, False
    if text[index:index + 1] == 'r':
        raw, index = True, index + 1
    hashes = 0
    while text[index:index + 1] == '#':
        hashes, index = hashes + 1, index + 1
    if text[index:index + 1] != '"' or (hashes and not raw):
        return None
    quote, closing = index, '"' + '#' * hashes
    index += 1
    while index < limit:
        char = text[index]
        if raw:
            if text.startswith(closing, index):
                return text[quote + 1:index], index + len(closing), True
            index += 1
            continue
        if char == '\\':
            index += 2
            continue
        if char == '"':
            return text[quote + 1:index], index + 1, False
        if char == '\n':
            return None
        index += 1
    return None


def _call_argument(text, match, limit):
    """`(opener, argument_start)` when the identifier at `match` is a macro call.

    Returns None for a bare identifier (a name or prose mentioning
    `include_str`), so the scan only judges something the compiler expands.
    """
    index = _skip_trivia(text, match.end(), limit)
    if text[index:index + 1] != '!':
        return None
    index = _skip_trivia(text, index + 1, limit)
    opener = text[index:index + 1]
    if opener not in MACRO_OPENERS:
        return None
    return opener, _skip_trivia(text, index + 1, limit)


def _include_argument_can_be_a_literal(text, start, limit=8192):
    """Whether the `include!` argument at `start` could be a string literal.

    A working `include!` needs an argument that evaluates to a string literal:
    a literal itself or a macro such as `concat!`/`env!`. Anything else is a
    compile error, so an occurrence that cannot be one is text rather than a
    call -- and `unsafe_boundary.rs` is full of such text on purpose. Two
    shapes cannot be code whatever surrounds them:

    * a leading `{` is a block expression, never a string literal;
    * a backslash or a backtick outside a string literal and outside a comment
      is not a Rust token at all, so the occurrence sits inside a comment or a
      string literal.

    Comments are skipped rather than scanned: `include!(concat!(/* ` */
    "../specs/", "frag.rs"))` is a real call, and the backtick inside its
    block comment must not pass for text.

    Everything else is treated as a real call, because `include!` expands its
    argument at compile time and a silently skipped splice is exactly the
    failure #1570 is about: the copy misses the fragment, the crate does not
    compile, and every mutation reads INVALID.
    """
    limit = min(len(text), start + limit)
    if text[start:start + 1] == '{':
        return False
    depth, index = 1, start
    while index < limit:
        char = text[index]
        literal = _skip_literal(text, index, limit)
        if literal is not None:
            index = literal
            continue
        if char in ('"', "'"):
            # Unterminated literal: not code any tree reports as compiled.
            return True
        if text.startswith('/*', index) or text.startswith('//', index):
            index = _skip_trivia(text, index, limit)
            continue
        if char in '([{':
            depth += 1
        elif char in ')]}':
            depth -= 1
            if depth == 0:
                return True
        elif char in '\\`':
            return False
        elif char == '\n':
            # An unterminated single-line call, or text spanning lines: both
            # are safer to refuse than to resolve by guessing.
            return True
        index += 1
    return True


def _closing_index(text, start, limit):
    """Index of the token that closes the invocation, skipping a trailing comma."""
    index = _skip_trivia(text, start, limit)
    if text[index:index + 1] == ',':
        index = _skip_trivia(text, index + 1, limit)
    return index


def _path_attributes(text, limit):
    """Yield `(path, raw)` for every `#[path = "…"]`-shaped attribute.

    `#[path]`, `#![path]`, `#[r#path]` and the `cfg_attr(…, path = …)` form are
    one token sequence: `#`, an optional `!`, `[`, then a `path` key before the
    closing `]`. Scanning tokens keeps a newline or a comment between `path`
    and `=` from hiding the edge, and keeps a `path = ` inside a doc string
    from inventing one. Brackets are depth-counted, so a nested group such as
    `doc = stringify!(v[0])` does not end the attribute early.

    The scan over the file is textual, not a lexer: a `#[path = …]` written
    inside a comment or a raw string is still read as an edge (the tree does
    this on purpose -- `unsafe_boundary.rs` carries that text). Every invented
    edge fails loudly rather than silently: a target that does not exist is
    left alone like fixture text, and one that exists but is one of the shapes
    `source_copy` refuses makes the run refuse too.
    """
    for match in re.finditer(r'#', text):
        index = _skip_trivia(text, match.end(), limit)
        if text[index:index + 1] == '!':
            index = _skip_trivia(text, index + 1, limit)
        if text[index:index + 1] != '[':
            continue
        index += 1
        depth = 0
        while index < limit:
            index = _skip_trivia(text, index, limit)
            char = text[index:index + 1]
            # `]` closes the attribute only at the outer level: `stringify!(v[0])`
            # opens a nested bracket group, and ending the scan on its `]` would
            # drop a `path = "…"` that follows -- a silent short copy (#1570
            # r14 boundary).
            if char in ('(', '[', '{'):
                depth += 1
                index += 1
                continue
            if char in (')', ']', '}'):
                if depth:
                    depth -= 1
                    index += 1
                    continue
                if char == ']':
                    break
                index += 1
                continue
            if char == '':
                break
            literal = _skip_literal(text, index, limit)
            if literal is not None:
                index = literal
                continue
            if char == "'":
                # `'a` is a lifetime, not an unterminated char literal:
                # `#[cfg_attr(all(), doc = stringify!('a), path = "…")]`
                # compiles, and stopping here would drop the real edge.
                lifetime = _lifetime_end(text, index)
                if lifetime is not None:
                    index = lifetime
                    continue
                # Unterminated literal: nothing past it can be proven.
                break
            if char == '"':
                break
            word = IDENTIFIER.match(text, index)
            if word is None:
                index += 1
                continue
            index = word.end()
            if word.group(1) != 'path':
                continue
            index = _skip_trivia(text, index, limit)
            if text[index:index + 1] != '=':
                continue
            literal = _read_path_literal(text, _skip_trivia(text, index + 1, limit), limit)
            if literal is None:
                continue
            index = literal[1]
            yield literal[0], literal[2]


def compile_inputs(*, sources=None, read=None, root_identity=None):
    """Repository paths the scratch copy needs beyond `src/` (#1570).

    `src/` alone is not a build: tracked sources include
    `specs/gateway-coverage/coverage-manifest.json` from pilotdeck-gateway and
    `scripts/cli-parity/{help.txt,scenarios.json}` from pilotdeck-cli. Without
    them the copy does not compile, `run()` raises "compilation ... is not
    evidence", and every mutation that rebuilds those crates is recorded
    INVALID -- the control has no discriminating power at that point while the
    summary reads like an environment problem (#1985 block 1: 8/8 INVALID).

    Returns (directories, files). The directories are the inputs' parent
    directories, not the single files: a data directory is read as a unit at
    run time too (`specs/gateway-coverage/ledger.json` is read by a lib test),
    so copying one file out of it only moves the breakage to the next reader.
    Deriving the list from the sources rather than hardcoding it keeps a new
    include from silently falling back out of the copy.

    Fail-closed on shapes this scan cannot resolve, because a silently skipped
    input is the exact failure #1570 is about: same policy as
    `tests/common/source_inputs.rs` ("fail closed on non-literal paths"). Two
    consequences worth knowing before editing a fixture:

    * the scan is textual, so a comment or a test string that spells a whole
      call such as `include_str!("data")` is indistinguishable from a real one.
      Both directions are safe: an existing target is copied as if used (a
      little extra copying, nothing missed), a non-existent one stops the
      control loudly. Masking comments instead would need a Rust lexer, and a
      wrong mask copies nothing while the run still looks green.
    * an input is derived only when the copy would carry it, by one rule
      (`_copyable`): tracked or untracked plain file, in `src/` or in a derived
      directory, and not one of the shapes the copy refuses (a link, a hard
      link, a dotenv path). Anything else -- a deleted file, a link -- is
      refused here rather than half-copied. Deriving and copying from two
      different manifests is how #1570 came back twice (#1570 r19 codex P1).
    * `include!` and `#[path = "…"]` targets are followed only when they exist
      and stay inside the repository. A missing one is fixture text in the
      workspace scanners (`unsafe_boundary.rs`) or an error every tree reports,
      so refusing there would stop controls that work today; a present one is
      copied when it lives outside `src/`, because it is just as much a
      compile-time input. A present one under `src/` follows the same
      `_copyable` rule as every other input.
    * an `include!` argument that is not a plain literal is refused, because
      `include!(concat!("…"))` resolves at compile time and the scan cannot
      follow it. The one exception is an argument that could not be a string
      literal at all -- text in a comment or a fixture string; see
      `_include_argument_can_be_a_literal`.

    Two shapes resolve in rustc's name resolution rather than at the token
    level, and this scan does not follow either (#2119): a bare `mod name;`
    inside a fragment pulled from outside `src/` (rustc looks for
    `name.rs` beside that fragment), and a macro bound to another name
    (`use std::include_str as load;`, then `load!("…")`). Neither occurs in
    this tree today; a real one would show up as a copy that does not compile,
    which is loud in the log but reads as INVALID in the summary -- the #1570
    shape. `tests/common/source_inputs.rs` already refuses the alias form, so
    a Rust lexer is the durable fix.

    `sources` pins the `src/` file list this scan reads; `read` collects a
    sha256 of every file the scan actually read. `source_copy` passes both so
    the scan and the copy work from one list, and so the copy can be checked
    against the bytes the derivation was made from -- an index that gains a
    file after the list was taken would otherwise be scanned by nobody and
    copied unscanned.

    `root_identity` is the `(st_dev, st_ino)` `source_copy` took before it
    read anything. Without it every read here re-trusted whatever occupied the
    root path at that moment: swapping the checkout for another real checkout
    during the scan scanned the *impostor's* source, and its text came back
    out in the refusal message, which the host triage writes to the log
    (#1570 r22 codex P0). A standalone caller (a test) may leave it out; the
    copy path never does.
    """
    tracked = set(_tracked('src'))
    repository = REPO.resolve()
    directories, files, scanned = set(), set(), set()
    # A pending repair can delete a tracked file. It is not a source any more,
    # and reading it here would raise a bare `FileNotFoundError` before the
    # copy loop -- a traceback the host triage reads as "ran and failed"
    # rather than "never started" (#1570 r9 concurrency P2). An `include!` or
    # `#[path]` that named the deleted file still refuses below: a required
    # target must exist.
    pending = sorted(path for path in (tracked if sources is None else set(sources))
                     if path.suffix == '.rs' and (REPO / path).is_file())
    while pending:
        source = pending.pop()
        if source in scanned:
            continue
        scanned.add(source)
        data = _read_input(source, root_identity)
        if read is not None:
            read[source] = hashlib.sha256(data).hexdigest()
        text = data.decode('utf-8', 'surrogateescape')
        # `include_str!` / `include_bytes!` must resolve: their target is the
        # input. `include!` splices a Rust fragment into the crate and
        # `#[path = "…"]` redirects a module file; either can name something
        # outside `src/`, and the fragment can hold a further `include_str!`,
        # so an existing in-repo target is treated exactly like an
        # `include_str!` target: copied when it lives outside `src/`, scanned
        # for includes in both cases. A target that does not exist is left
        # alone -- fixture text (`unsafe_boundary.rs` writes
        # `include!("../../outside.rs")` into a string on purpose) or an error
        # every tree reports.
        edges = []  # (required, path text) for every edge this file declares
        for match in MACRO_NAME.finditer(text):
            call = _call_argument(text, match, len(text))
            if call is None:
                continue
            macro, (opener, argument) = match.group(1), call
            literal = _read_path_literal(text, argument, len(text))
            if literal is None:
                if macro != 'include':
                    raise RuntimeError(f'{source}: a {macro}! argument is not a plain '
                                       'string literal, or only appears inside a comment or '
                                       'a test string; the scratch copy cannot prove it '
                                       'carries the file, so refusing to run the control')
                if not _include_argument_can_be_a_literal(text, argument):
                    continue
                raise RuntimeError(f'{source}: an include! argument is not a plain string '
                                   'literal; the scratch copy cannot prove it carries the '
                                   'file, so refusing to run the control')
            value, end, raw = literal
            cursor = _closing_index(text, end, len(text))
            if text[cursor:cursor + 1] != MACRO_OPENERS[opener]:
                raise RuntimeError(f'{source}: {macro}! opens with {opener} and does not '
                                   'close with its pair; the scratch copy cannot resolve '
                                   'that shape, so refusing to run the control')
            if not raw and '\\' in value:
                # `include!("../specs/\x66rag.rs")` reads `frag.rs`: an escape
                # hides the real name from a textual scan, and decoding Rust
                # string escapes here would be guessing. Refuse instead.
                raise RuntimeError(f'{source}: {macro}! names {value!r}, which contains a '
                                   'string escape; the scratch copy cannot prove which file '
                                   'it reads, so refusing to run the control')
            edges.append((macro != 'include', value))
        for path, raw in _path_attributes(text, len(text)):
            if not raw and '\\' in path:
                raise RuntimeError(f'{source}: #[path = {path!r}] contains a string escape; '
                                   'the scratch copy cannot prove which file it reads, so '
                                   'refusing to run the control')
            edges.append((False, path))
        for required, relative_path in edges:
            target = Path(os.path.normpath(REPO / source.parent / relative_path))
            resolved = target.resolve()
            if repository not in resolved.parents:
                # A target that resolves outside the repository cannot be
                # carried. Existing is what makes it an input rather than
                # fixture text: `include!("../../outside.rs")` written into a
                # string names nothing, while a symlink to a real file outside
                # does -- and skipping that one would leave the copy short.
                if resolved.is_file():
                    raise RuntimeError(f'{source}: an include reads {target}, which resolves '
                                       f'to {resolved} outside the repository; the scratch '
                                       'copy cannot carry it')
                if required or _indexed(target, tracked):
                    raise RuntimeError(f'{source}: an include reads {target}, which is not a '
                                       'file in this checkout')
                continue
            if not resolved.is_file():
                # A name that is tracked in the index but deleted from the
                # working tree is a real reference (a pending repair can
                # delete it), not fixture text: the copy would come up short,
                # so refuse it even for the edge kinds that tolerate a name
                # nothing reads. The resolved path is checked too: a tracked
                # symlink (`src/link -> real`) puts the real file in the index
                # under its own name, not under the path that names it.
                if required or _indexed(target, tracked) or _indexed(resolved, tracked):
                    raise RuntimeError(f'{source}: an include reads {target}, which is not a '
                                       'file in this checkout')
                continue
            try:
                relative = target.relative_to(REPO)
            except ValueError:
                raise RuntimeError(f'{source}: an include reads {target}, which reaches the '
                                   'repository only through a link; the scratch copy cannot '
                                   'name it')
            if SRC in target.parents:
                if not _copyable(relative, tracked):
                    raise RuntimeError(f'{source}: an include reads {relative}, which the '
                                       'scratch copy would not carry -- it is untracked and '
                                       'ignored, deleted from the working tree, or one of the '
                                       'shapes the copy refuses (a link, a hard link, a dotenv '
                                       'file); refusing to run the control')
            else:
                if relative.parent != Path('.'):
                    # The copy is built from `git ls-files` plus the untracked
                    # plain files, so anything it would not carry must be
                    # refused here: a file dropped between derivation and copy
                    # surfaces later as a misleading "missing from the copy".
                    # Root-level files are different -- they are copied by
                    # name, so an uncommitted one is still carried.
                    if not _copyable(relative):
                        raise RuntimeError(f'{source}: an include reads {relative}, which the '
                                           'scratch copy would not carry -- it is untracked '
                                           'and ignored, deleted from the working tree, or one '
                                           'of the shapes the copy refuses (a link, a hard '
                                           'link, a dotenv file); refusing to run the control')
                    # A repository-root file has no directory worth copying:
                    # `git ls-files -- .` would drag in the whole tree.
                    directories.add(relative.parent)
                files.add(relative)
            if not required and relative not in scanned:
                # Only `include!` / `#[path]` pull in a Rust fragment. An
                # `include_str!` target is data: scanning it for macros would
                # invent calls out of a JSON payload.
                pending.append(relative)
    return tuple(sorted(directories)), tuple(sorted(files))


def source_copy(root):
    """Copy tracked workspace inputs, including pending fixes, never target/.env.

    "Workspace inputs" is `src/` plus whatever `src/` reads at compile time
    from elsewhere in the repository (#1570, see `compile_inputs`).
    """
    # Bind every open below to *this* checkout: taken before anything is read,
    # so a root replaced mid-copy is refused instead of copied from
    # (#1570 r21 codex P0).
    root_identity = _repository_identity()
    # One list for both halves: the scan is told exactly which `src/` files to
    # read, and the copy is built from that same list. Listing the index twice
    # would let a file added in between be copied without ever being scanned,
    # so an `include!` it holds would never become an edge (#1570 r14 codex).
    # Untracked `src/` files belong in that list for the same reason: copying
    # them without scanning them compiles a `mod new;` edge whose own
    # `include_str!` never became an input, so the copy comes up short and the
    # mutation reads INVALID (#1570 r18 codex P1).
    try:
        tracked = _tracked('src')
        # The untracked half is a git call too: a failure here used to escape
        # the same way `_tracked` did, one line lower (#1570 r23 codex P2).
        untracked_src = _untracked_files('src')
    except (subprocess.SubprocessError, OSError) as error:
        # A git failure before anything is copied leaves a bare traceback,
        # which the host triage reads as "ran and failed" instead of "refused
        # to start" -- the #1570 shape. The re-lists after the copy already
        # normalise theirs; this is the same failure one stage earlier
        # (#1570 r22 boundary/concurrency P3).
        raise RuntimeError(f'cannot list the workspace inputs ({error}); '
                           'refusing to run the control') from error
    sources = tracked + [path for path in untracked_src
                         if path not in set(tracked)]
    # sha256 of every file the scan read, filled by the scan itself: comparing
    # the copy against these bytes is what proves the derivation and the copy
    # came from the same tree, including fragments outside `src/`.
    read = {}
    try:
        directories, files = compile_inputs(sources=sources, read=read,
                                            root_identity=root_identity)
    except (subprocess.SubprocessError, OSError) as error:
        raise RuntimeError(f'cannot derive the workspace inputs ({error}); '
                           'refusing to run the control') from error
    paths = list(sources)
    seen = set(paths)
    # No derived directory is normal (nothing outside `src/` is read); asking
    # git for nothing lists the *whole* repository, which would copy `state/`,
    # `.github/` and tracked `.env*` into every scratch workspace (#1570 r15
    # boundary).
    listed = set()
    try:
        tracked_inputs = _tracked(*directories) if directories else ()
        untracked_inputs = _untracked_files(*directories) if directories else []
    except (subprocess.SubprocessError, OSError) as error:
        raise RuntimeError(f'cannot list the input directories ({error}); '
                           'refusing to run the control') from error
    for path in tracked_inputs:
        listed.add(path)
        if path not in seen:
            seen.add(path)
            paths.append(path)
    # The `copytree(specs)` inventories this replaced carried untracked files
    # too, and a pending repair adds a data file without `git add`. Copying
    # them cannot hide an input (they land in the copy) and refusing would stop
    # every control in a tree with a new fixture, so carry the plain ones along
    # (#1570 r15 concurrency). `src/` gets the same rule: a `mod new;` edge is
    # not followed by the scan (#2119), so a pending `src/new.rs` that is not
    # `git add`ed would otherwise reach the copy as a missing file and every
    # mutation would read INVALID (#1570 r17 sibling).
    # `src/` is already covered by `sources`; the derived directories get the
    # same carry-along rule. Both are filtered by `_untracked_files`, which
    # refuses links, directories and dotenv files.
    untracked = list(untracked_inputs)
    for path in untracked:
        listed.add(path)
        if path not in seen:
            seen.add(path)
            paths.append(path)
    # `compile_inputs` leaves root-level inputs out of `directories`, so add
    # those files back instead of asking git for everything under `.`.
    for path in root_level_inputs(directories, files):
        if path not in seen:
            seen.add(path)
            paths.append(path)
    # A pending repair can delete a tracked file; `git ls-files` still lists
    # it. Inside `src/` the deleted file is skipped: the tree does not compile
    # either way, so a faithful copy is INVALID for a reason that is printed
    # below (#1570 r11 arbitration). An input *directory* outside `src/` is
    # copied whole because its siblings are read at run time
    # (`specs/gateway-coverage/ledger.json`), and such a read is not in the
    # include graph: skipping a deleted member there leaves a copy that fails
    # at run time and reads as INVALID with nothing in the log (#1570 r14
    # concurrency). Refuse instead.
    # Everything that actually reached the scratch tree, so the check at the end
    # can compare each copy against the source *now*: `read` only covers what
    # the scan looked at, and a data file that no include edge points at
    # (`specs/gateway-coverage/ledger.json`, `LICENSE`) can move between the
    # copy and the first test run (#1570 r15 concurrency).
    written = []
    repository = REPO.resolve()
    for path in paths:
        if (REPO / path).is_symlink():
            # `copyfile` follows the link, so a tracked path swapped for a link
            # to `.env.local` (or to `~/.pilotdeck`) puts those bytes in `/tmp`
            # and the digest check below still passes -- it compares the copy
            # with the same followed bytes (#1570 r17 codex P0).
            raise RuntimeError(f'source_copy: {path} is a symlink; the control copies the '
                               'tracked file it names, never the bytes a link points at, so '
                               'replace it with the real file or remove it before running')
        if not (REPO / path).is_file():
            if any(directory in path.parents for directory in directories):
                raise RuntimeError(f'source_copy: {path} is a tracked member of an input '
                                   'directory but is deleted in the working tree; the copy '
                                   'would silently come up short, so refusing to run the '
                                   'control')
            print(f'source_copy: tracked file deleted in the working tree, not copied: {path}',
                  flush=True)
            continue
        _refuse_special(REPO / path, path, 'the workspace inputs')
        destination = root / path
        if _escapes_repository(path, repository):
            raise RuntimeError(f'source_copy: {path} resolves outside the repository; the '
                               'scratch copy must not carry anything from outside the '
                               'checkout, so refusing to copy it')
        destination.parent.mkdir(parents=True, exist_ok=True)
        _copy_input(path, destination, root_identity)
        written.append(path)
    # `rustup` must keep the repository toolchain when Cargo runs in /tmp, and
    # `LICENSE` rides along for the same "workspace inputs" promise. Both can
    # be deleted by a pending repair like any other tracked file, so they get
    # the same skip-and-say-it-out-loud treatment as the loop above instead of
    # a bare `FileNotFoundError` that reads as "ran and failed". A link is
    # tested before "is it a file": `LICENSE -> /etc` is not a file, and
    # skipping it as "missing" would give the same shape two answers in two
    # loops (#1570 r19 concurrency P3).
    for name in ('LICENSE', 'rust-toolchain.toml'):
        if (REPO / name).is_symlink():
            raise RuntimeError(f'source_copy: {name} is a symlink; the control copies the '
                               'tracked file it names, never the bytes a link points at, so '
                               'replace it with the real file or remove it before running')
        if not (REPO / name).is_file():
            print(f'source_copy: {name} is missing from the working tree, not copied',
                  flush=True)
            continue
        if _escapes_repository(name, repository):
            raise RuntimeError(f'source_copy: {name} resolves outside the repository; the '
                               'scratch copy must not carry anything from outside the '
                               'checkout, so refusing to copy it')
        _refuse_special(REPO / name, Path(name), 'the workspace inputs')
        _copy_input(Path(name), root / name, root_identity)
        written.append(Path(name))
    # #1570: prove the inputs landed instead of trusting the write loop.
    verify_copy(root, files)
    # The destination side is the control's own 0700 scratch tree, so this is
    # not the r21 link/FIFO window -- but a read error here (quota, a removed
    # tmpfs) used to escape as a bare `OSError` and read as "ran and failed",
    # while the same read four lines below is wrapped (#1570 r23 zai sibling P3).
    try:
        copied = {path: hashlib.sha256((root / path).read_bytes()).hexdigest()
                  for path in read if (root / path).is_file()}
    except OSError as error:
        raise RuntimeError(f'the scratch copy could not be read back ({error}); refusing '
                           'to run the control against a moving tree') from error
    if copied != read:
        raise RuntimeError('the scratch copy does not match the sources the scan read; '
                           'the tree changed while the copy was being made, so refusing '
                           'to run the control against a moving tree')
    # The same check for `src/`: a link or directory that appears there during
    # the copy window would otherwise surface as `_untracked_files`' own
    # "add it to git (or remove it)" message, which misdescribes a moving tree
    # as something the user is expected to fix (#1570 r20 zai concurrency P3).
    try:
        relisted_src = set(_tracked('src')) | set(_untracked_files('src'))
    except (RuntimeError, subprocess.SubprocessError, OSError) as error:
        raise RuntimeError('src/ changed while the scratch copy was being made '
                           f'({error}); refusing to run the control against a moving '
                           'tree') from error
    if relisted_src != set(sources):
        raise RuntimeError('src/ changed while the scratch copy was being made; refusing '
                           'to run the control against a moving tree')
    # The same check for the derived directories: a file that appears there
    # *during* the copy window is in neither `written` nor `files`, and a
    # run-time read of it (the `ledger.json` shape) then fails as INVALID with
    # nothing in the log -- the #1570 symptom itself (#1570 r19 concurrency
    # P2 / sibling P2). Re-listing also turns a link or directory that
    # appeared mid-copy into this message instead of a confusing one from
    # `_untracked_files`.
    if directories:
        try:
            relisted = set(_tracked(*directories)) | set(_untracked_files(*directories))
        except (RuntimeError, subprocess.SubprocessError, OSError) as error:
            raise RuntimeError('an input directory changed while the scratch copy was being '
                               f'made ({error}); refusing to run the control against a moving '
                               'tree') from error
        if relisted != listed:
            raise RuntimeError('an input directory changed while the scratch copy was being '
                               'made; refusing to run the control against a moving tree')
    # The third derivation is the same kind of re-list as the two above, so a
    # git failure here is refused the same way instead of escaping as a bare
    # `CalledProcessError` (#1570 r22 sibling P3). It runs against the identity
    # taken before the scan, so a checkout swapped in the copy window refuses
    # here even when the index it lists happens to match.
    try:
        unchanged = compile_inputs(sources=sources, root_identity=root_identity)
    except (RuntimeError, subprocess.SubprocessError, OSError) as error:
        raise RuntimeError('the workspace inputs changed while the scratch copy was being '
                           f'made ({error}); refusing to run the control against a moving '
                           'tree') from error
    if unchanged != (directories, files):
        raise RuntimeError('the workspace inputs changed while the scratch copy was being '
                           'made; refusing to run the control against a moving tree')
    # Last line of defence: every file in the copy must still equal its source.
    # The checks above prove the *include graph* did not move; this one also
    # covers the data files the graph does not name, which are read at run time
    # and would otherwise turn a real KILLED/SURVIVED into a confusing INVALID
    # when someone edits the tree while a control is starting up (#1570 r15
    # concurrency).
    for path in written:
        # Re-read the source the same way it was copied: `read_bytes()` here
        # followed links and blocked forever on a path swapped for a FIFO
        # after the copy, so the last line of defence could hang instead of
        # refusing (#1570 r21 codex P0).
        source = _source_digest(path, root_identity)
        try:
            duplicate = hashlib.sha256((root / path).read_bytes()).hexdigest()
        except OSError as error:
            raise RuntimeError(f'source_copy: {path} changed while it was being copied '
                               f'({error}); refusing to run the control against a moving tree')
        if source != duplicate:
            raise RuntimeError(f'the scratch copy of {path} does not match the working tree; '
                               'the tree changed while the copy was being made, so refusing '
                               'to run the control against a moving tree')
    return root / 'src'


# Opt-in #994 API. Legacy run/environment/scratch defaults above remain intact.
COMPILE_IDLE_SECONDS = 900
BASELINE_RUN_SECONDS = 900
RUN_FACTOR = 5
MIN_RUN_SECONDS = 20


def adaptive_timeout(seconds):
    if not math.isfinite(seconds) or seconds < 0:
        raise ValueError('#994 invalid baseline duration')
    return max(MIN_RUN_SECONDS, seconds * RUN_FACTOR)


def persistent_environment(directory):
    """Use the caller's persistent negative target, never the product target."""
    env = environment(directory)
    target = Path(env['CARGO_TARGET_DIR']).expanduser().resolve()
    forbidden = (REPO / '_build/cargo-target', SRC / 'target')
    if any(target == path.resolve() or path.resolve() in target.parents
           for path in forbidden):
        raise ValueError('#994 supply a separate persistent CARGO_TARGET_DIR, e.g. '
                         '<tree>/_build/negctl-target')
    if target == Path(directory).resolve() or Path(directory).resolve() in target.parents:
        raise ValueError('#994 negative target must outlive the scratch directory')
    env.update(CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL='1')
    return env


class CargoTests:
    """Build once per invocation, then time only the selected libtest binary.

    Commands deliberately name exactly one target; selection/libtest options
    are separate, so none can leak into --no-run or accidentally select zero
    tests. All phases, including restored, rebuild using the same target.
    """

    def __init__(self, source, output, env):
        self.source, self.output, self.env = Path(source), Path(output), env
        self.baselines = {}
        self.compiled = {}

    def save(self, name, value):
        with (self.output / name).open('x') as stream:
            stream.write(value)

    def phase(self, label, command, *, timeout, idle_timeout=None, cwd=None):
        started = time.monotonic()
        result = None
        try:
            result = run_owned(command, cwd=cwd or self.source, env=self.env,
                               timeout=timeout, idle_timeout=idle_timeout)
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes)
                              else value or '' for value in (error.stdout, error.stderr))
            self.save(label + '.log', raw + f'\n{type(error).__name__}: {error}\n')
            raise
        else:
            self.save(label + '.log', result.stdout + result.stderr)
            return result, time.monotonic() - started
        finally:
            self.save(label + '.json', json.dumps({
                'command': command, 'cwd': str(cwd or self.source),
                'target': self.env['CARGO_TARGET_DIR'], 'timeout': timeout,
                'idleTimeout': idle_timeout, 'elapsedSeconds': time.monotonic() - started,
                'returncode': result.returncode if result is not None else None,
            }, indent=2) + '\n')

    def run(self, label, target, arguments, *, positive=False, restored=False):
        """target is ('--lib',) or ('--test', name); arguments go to libtest."""
        if target != ('--lib',) and not (len(target) == 2 and target[0] == '--test'):
            raise ValueError('#994 expected exactly one Cargo test target')
        key = (tuple(target), tuple(arguments))
        self.compiled[label] = False
        if not positive and not restored and key not in self.baselines:
            raise ValueError('#994 no passing unmutated baseline for this selection')
        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=900)
        self.save(label + '.slots.log', slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', *target,
                   '--no-run', '--message-format=json']
        compiled, _ = self.phase(label + '-compile', command, timeout=None,
                                 idle_timeout=COMPILE_IDLE_SECONDS)
        if compiled.returncode:
            raise RuntimeError('#994 compilation failed; not a killed mutation')
        expected = 'pilotdeck_gateway' if target == ('--lib',) else target[1]
        artifacts = set()
        finished = False
        for line in compiled.stdout.splitlines():
            row = json.loads(line)
            if row.get('reason') == 'build-finished':
                finished = row.get('success') is True
            if (row.get('reason') == 'compiler-artifact'
                    and row.get('target', {}).get('name') == expected
                    and row.get('profile', {}).get('test') is True
                    and row.get('executable')):
                artifacts.add(row['executable'])
        if not finished or len(artifacts) != 1:
            raise RuntimeError('#994 missing/ambiguous current Cargo test artifact')
        executable = Path(artifacts.pop()).resolve()
        target_dir = Path(self.env['CARGO_TARGET_DIR']).resolve()
        if target_dir not in executable.parents or not executable.is_file():
            raise RuntimeError('#994 test artifact missing or outside caller target')
        self.compiled[label] = True
        limit = (BASELINE_RUN_SECONDS if key not in self.baselines
                 else adaptive_timeout(self.baselines[key]))
        result, elapsed = self.phase(label + '-run', [str(executable), *arguments],
                                     cwd=self.source / 'pilotdeck-gateway', timeout=limit)
        # The caller also checks the fixture's exact positive cardinality.
        if positive and result.returncode == 0 and 'test result: ok.' in result.stdout:
            self.baselines[key] = elapsed
        return result


def mutation_suite(label, rows, output, selection, passed, classify, *, guarded=(), script=None):
    """HEAD-guarded scratch mutations; account for every row, including errors.

    rows: (name, real source path, old, new, selections). Source writes are only
    to the disposable copy. The existing source guard owns the original bytes;
    no synthetic Git repository or alternate baseline is involved.
    """
    output = Path(output).resolve()
    # #1666/#1734: the caller target is a startup guard, so it is checked
    # before the evidence directory is claimed and before the source guard can
    # wait for a cargo slot: a missing target is "never started" (2), not a
    # run that failed. `environment()` below backstops only the presence half
    # for callers that reach it directly; the value half (dot / root / `src`)
    # is enforced here, before any directory exists (#1666 fix-r10).
    if missing_target():
        # absent, empty or all-space: all three are "never started" (#1666
        # fix-r8). Callers such as `background-output/negative.py` have no
        # guard of their own, so this one has to be the whole family's.
        refuse('CARGO_TARGET_DIR must be supplied by the caller')
    refuse_unusable_target()  # #1666 fix-r9: dot / root / src are unusable too
    # #1666: a taken evidence directory means "never started" (2). Letting
    # `FileExistsError` escape printed a traceback and exited 1, which the host
    # triage table reads as "ran and did not pass" -- an investigation of a run
    # that never began.
    try:
        output.mkdir(parents=True, exist_ok=False)
    except FileExistsError:
        print(f'INVALID: output directory already exists: {output}',
              file=sys.stderr, flush=True)
        return 2
    except OSError as error:
        print(f'INVALID: cannot create the output directory {output}: {error}',
              file=sys.stderr, flush=True)
        return 2
    records = []
    interrupted = None

    def publish():
        counts = pending.counts(records)
        # Atomic replacement also makes interrupted summaries readable.
        temporary = output / 'summary.json.tmp'
        temporary.write_text(json.dumps({'controls': records, 'counts': counts}, indent=2) + '\n')
        temporary.replace(output / 'summary.json')
        return counts

    try:
        registrations = pending.for_script(REPO, script)
        paths = list(dict.fromkeys([*(row[1] for row in rows), *guarded]))
        if registrations:
            paths.append(REPO / pending.FIXTURES / pending.REGISTRY)
        with mutating_sources(REPO, paths) as snapshots:
            with scratch(label, deadline=0) as directory:
                env = persistent_environment(directory)
                env['PDRS_553_NEGATIVE_SPILL_PATH'] = str(directory / 'unconfigured.log')
                source = source_copy(directory)
                runner = CargoTests(source, output, env)
                print(f'persistent target: {env["CARGO_TARGET_DIR"]}', flush=True)
                for name, path, old, new, tests in rows:
                    item = dict(name=name, status='INVALID', reason='not run',
                                compiled=False, restoredPass=False, selections=[])
                    entry = pending.match(registrations, name)
                    if entry is not None:
                        item.update(status=pending.SKIPPED, reason=entry.reason())
                        records.append(item)
                        publish()
                        print(f'{name}: {pending.SKIPPED}: {entry.reason()}', flush=True)
                        continue
                    original = snapshots[path]
                    copied = directory / path.relative_to(REPO)
                    try:
                        text = original.decode()
                        if text.count(old) != 1 or old == new or not tests:
                            raise ValueError('#994 stale/invalid mutation anchor or empty selections')
                        changed = text.replace(old, new, 1)
                        runner.save(name + '.patch', ''.join(difflib.unified_diff(
                            text.splitlines(True), changed.splitlines(True),
                            fromfile=str(path.relative_to(REPO)), tofile=str(path.relative_to(REPO)))))
                        for index, test in enumerate(tests):
                            result = runner.run(f'{name}-{index}-positive', *selection(test), positive=True)
                            if not passed(result, test):
                                raise RuntimeError(f'#994 positive selection did not pass: {test}')
                        copied.write_text(changed)
                        for index, test in enumerate(tests):
                            try:
                                result = runner.run(f'{name}-{index}-negative', *selection(test))
                                status, reason = classify(name, result, test)
                            except Exception as error:
                                status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                            item['selections'].append(dict(test=test, status=status, reason=reason,
                                compiled=runner.compiled.get(f'{name}-{index}-negative', False)))
                        item['compiled'] = all(row['compiled'] for row in item['selections'])
                        states = [row['status'] for row in item['selections']]
                        item['status'] = ('INVALID' if 'INVALID' in states else
                                          'SURVIVED' if 'SURVIVED' in states else 'KILLED')
                        item['reason'] = '; '.join(row['reason'] for row in item['selections'])
                    except Exception as error:
                        item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                    finally:
                        copied.write_bytes(original)
                        if copied.read_bytes() != original:
                            raise RuntimeError('#994 scratch restoration failed')
                    # Every mutation, including INVALID, recompiles pristine bytes
                    # in the same target before the successor can use it.
                    restored = True
                    for index, test in enumerate(tests):
                        try:
                            result = runner.run(f'{name}-{index}-restored', *selection(test),
                                                restored=True)
                            if not passed(result, test):
                                raise RuntimeError(f'#994 restored selection did not pass: {test}')
                        except Exception as error:
                            restored = False
                            item['reason'] += f'; restored: {type(error).__name__}: {error}'
                    item['restoredPass'] = restored
                    if not restored:
                        item['status'] = 'INVALID'
                    records.append(item)
                    publish()
                    print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except BaseException as error:
        if isinstance(error, SystemExit) and error.code == 2 and not records:
            # #1666/#1734: a startup refusal measured nothing. Re-raise without
            # publishing a summary and drop the directory this call just
            # claimed, so "never started" does not leave evidence that reads
            # like a finished run. Only an empty directory can be dropped; if
            # something already wrote into it, say where it was left instead
            # of hiding the residue.
            try:
                output.rmdir()
            except FileNotFoundError:
                pass  # already gone: nothing was left behind
            except OSError as residue:
                print(f'INVALID: startup refusal left {output} behind '
                      f'({type(residue).__name__}: {residue})', file=sys.stderr, flush=True)
            raise
        reason = f'suite/source guard: {type(error).__name__}: {error}'
        for row in records:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for row in rows[len(records):]:
            records.append(dict(name=row[0], status='INVALID', reason=reason,
                                compiled=False, restoredPass=False, selections=[]))
        if not isinstance(error, Exception):
            interrupted = error
    counts = publish()
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    if interrupted is not None:
        raise interrupted
    return int(not records or any(row['status'] != 'KILLED' for row in records))
