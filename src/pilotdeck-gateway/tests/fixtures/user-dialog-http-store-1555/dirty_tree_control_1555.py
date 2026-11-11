#!/usr/bin/env python3
# anchor-selfcheck: unsupported run-level negative control: holds the shared
# tree lock while the peak-RSS driver starts (the driver must stay blocked), and
# dirties one tracked source byte inside that same hold, handing the lock to
# the driver with the descriptor (the driver must refuse, then restore).
"""#1555 fix-r2 negative control: the peak-RSS driver takes the tree lock.

Two checks, one per half of the fix:

1. `lock-held` — on a clean tree the driver must first finish `status=OK` with
   a real minimum (`positive` below, so a driver that merely refuses everything
   cannot pass this control). The control then takes the same lock and starts
   the driver: it prints `starting`, then `lock-requested` immediately before
   the blocking `flock` (stamped with the driver's own `time.monotonic()`, so
   the measured wait is not inflated by the control's polling delay), then
   `lock-acquired` as the first statement after the
   request returns. The blocking request belongs to this leg only; the
   dirty-src leg below inherits the lock with the descriptor instead of
   requesting it (#1695). Seeing the request without the acquisition at the end of
   the hold is a state proof that it is blocked -- the only code between the
   two is the request itself, so neither a slow interpreter nor a git-heavy
   preflight can be mistaken for a lock wait (fix-r4: the old 3 s quiet-window
   inference could score a live no-lock mutant as KILLED on a cold cache).
   Delete the driver's `with driver_lock(...)` block (which is `tree_lock`
   here -- this leg always runs standalone) and `lock-acquired` follows the
   request at once: SURVIVED.
2. `dirty-src` — one tracked product source is dirtied while the control holds
   the lock, and the driver is started with that same lock handed over with
   the descriptor (`launch_under_lock`, #1695), so it owns the lock from its
   first statement and finds the bytes dirty the moment it looks; it never
   gets a clean window in which it could build. Delete the dirty
   gate and it builds and measures: SURVIVED. The refusal must be the driver's
   own `src_dirty` bit (a machine field), so an unrelated build failure cannot
   pass for a killed mutation. The marker is undone by the *driver*, still
   inside the lock, before it returns (`negctl_restore` in
   `rss_probe_1555.py`): a restore issued here after the driver exits would
   leave the mark on disk while other runners queue for the lock, and the
   winner would see a marked source and record a false `src/ is dirty`
   (fix-r5 review, concurrency P1). The fallback here only covers a driver that
   never reached the dirty branch.

   The handoff closes the seam fix-r5 had only narrowed (#1695): the control
   takes the `flock` on one descriptor (`handoff_lock`), writes the marker
   while it holds it, starts the driver with that descriptor in `pass_fds` and
   then closes its own copy *without unlocking* — a `flock` belongs to the
   open file description, which the child shares, so no sibling polling
   `LOCK_NB` can win the lock between the write and the driver's own restore.
   Releasing the old way (unlock here, let the driver re-acquire) left the
   marker exposed in that gap, recording a false `src/ is dirty`.
   This narrows the window; it does not remove it (fix-r1 review,
   concurrency P2 / boundary P2): a driver that dies before its
   `negctl_restore` takes the descriptor with it — the lock is free while the
   marker is still on disk — and a launch that fails after the marker write
   leaves a short gap between the caller closing the descriptor and
   `restore_locked` re-taking it. Both are exactly the residual windows
   `restore_locked` exists for; see its docstring.

The lock check does not trust the driver's own markers alone (fix-r5). A mutant
that deletes the whole `with driver_lock(...)` block *including* the
`lock-acquired` print would also show "requested, never acquired" and would
build for longer than the hold, which is indistinguishable from a blocked
driver by the two markers alone -- the exact false PASS the fix-r4 P1 was
about. Two independent state checks close it:

* the driver must not have produced `build.log` or `summary.json` while the
  control held the lock (a driver that is not blocked builds there), and
* after the release, its **full** output must contain `lock-acquired`: a real
  driver acquires the lock the moment it is released, so a run that finished
  `status=OK` without ever printing that line never took the lock at all.

Neither check is the whole verdict: a run with no `lock-acquired` and no
`provenance` is split by its exit code. Zero means the driver ran to the end
without ever entering the lock block (the mutant deleted it, prints included)
and is SURVIVED; non-zero means it died before the lock and is INVALID, since
infrastructure failure is not evidence a mutant survived. The `dirty-src`
control applies the same split: no `provenance` with a zero exit is SURVIVED,
non-zero is INVALID (fix-r9 review, three independent P1s -- the two chains
had drifted apart). Two differences between the chains are deliberate and
written down here so they are not read as drift again: the lock side applies
that split only after the clean-run positive control has passed (a mutant that
breaks the driver outright is INVALID, not a survivor), and the two sides read
`provenance` from different places -- the lock side scans the driver's log
text, the dirty side reads `summary['provenance']`. A restore that puts the
bytes back without its file mode makes the dirty row INVALID through
`provenance.negctl_restore_mode_lost`, and this file's own restore through a
`mode_lost` flag kept apart from the free-text complaint (the complaint also
carries the "restored without the lock" note, which says nothing about the
mode): git tracks only the exec bit, so the difference would otherwise be
invisible to every guard. The marker write is where the mode is still known
good, so a marker whose `chmod` fails is never swapped in -- both restore
paths read the mode back off the file and would otherwise preserve the wrong
one and report success (fix-r11 review).

The marker is normally undone by the driver itself, inside the lock (see
`negctl_restore` in `rss_probe_1555.py`); this file's `restore_locked` is the
fallback for a driver that never reached the dirty branch, and the final
byte-verification. It re-takes the tree lock with a bounded wait (fix-r4) and
falls back to the unlocked path -- saying so in the complaint -- if a sibling
holds it longer than the bound or the lock cannot be opened at all. The restore
only writes when the bytes on disk are still exactly this process's marker
(HEAD or a marker with the same pid; a bare constant let one concurrent control
undo another's marker): anything else is somebody else's uncommitted work, left
untouched with the run reported as INVALID. The write goes through a temp file
and `os.replace`, so a concurrent reader sees either the old bytes or the old
bytes plus marker, never a truncated source, and the temp is removed if the
replace fails. The temp file lives in the worktree root, outside the guarded
`src/` and under the `.negctl-*` ignore rule: a `src/`-local temp that survived
a kill would be an untracked build input, and one left in the root would dirty
`git status --porcelain` and block the frozen-ledger re-registration.

    CARGO_TARGET_DIR=... python3 dirty_tree_control_1555.py NEW_OUTPUT_DIR
"""
import fcntl
import json
import math
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import _head_bytes, _kill_group, run_owned, tree_lock

root = Path(__file__).resolve().parents[5]
RELATIVE = 'src/pilotdeck-gateway/src/user_dialog/http_store.rs'
source = root / RELATIVE
probe = Path(__file__).resolve().parent / 'rss_probe_1555.py'
# Process-unique: two controls in the same worktree write the same file, and a
# bare constant let one restore the other's marker (review, concurrency P2).
MARKER = f'\n// #1555 fix-r2 dirty-tree control pid={os.getpid()}\n'.encode()
HOLD_SECONDS = 10.0
# The driver must have been inside the request for at least this long when the
# lock is released; a request that lands in the last moment of the hold is a
# slow start, not a proven wait.
MIN_BLOCK_SECONDS = 2.0
# How far before the hold began a driver stamp may sit and still be believed:
# a stamp from earlier than this would make the measured wait longer than the
# whole hold, which is the direction that turns a slow start into a proven one.
STAMP_SLACK_SECONDS = 1.0
LOCK_TIMEOUT = 3600
RESTORE_LOCK_TIMEOUT = 120
WATCHDOG = 1800
# One `ps` census of the driver's descendants before a timeout kill (#1705).
# Bound on purpose: the kill answers an already-failed row, so the census must
# not be able to hang the control next to the dead driver.
PS_CENSUS_TIMEOUT = 30
PROBE = [sys.executable, '-I', str(probe)]
MODE = ['--modes', 'baseline', '--rounds', '1']


def read_summary(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return {}


def read_log(path):
    try:
        return path.read_text()
    except OSError:
        return ''


def early_lines(text, limit=3):
    """The first whole lines of an early window, for the row's `early_output`.

    `split('\n')` (never `splitlines()`, the shared rule — a U+2028 in some
    output must not tear a line in two) yields `['']` on an empty or
    whitespace-only window, where the old `splitlines()` shape produced `[]`:
    the row must keep reporting "the driver printed nothing" as an empty
    list (fix-r1 review, boundary P3 / simplify P3).
    """
    return [line for line in text.strip().split('\n') if line][:limit]


def scratch(name):
    """Temp path for the atomic swaps: same filesystem, outside `src/`.

    `os.replace` needs one filesystem. The worktree root qualifies and is not
    covered by `_assert_workspace_sources` (`src`, `.cargo`, `rust-toolchain*`),
    so a temp file left by a killed run cannot block later guard runs.
    """
    return root / f'.negctl-{os.getpid()}-{name}'


def launch(out, env, name, log, pass_fds=()):
    """Start the driver with its own output file, in its own session."""
    return subprocess.Popen(
        PROBE + [str(out / name)] + MODE, cwd=root, env=env, stdout=log,
        stderr=subprocess.STDOUT, text=True, start_new_session=True,
        pass_fds=tuple(pass_fds))


def handoff_lock(root_path, *, timeout=LOCK_TIMEOUT):
    """Take the shared tree lock on a raw descriptor, for the #1695 handoff.

    Same lock file, same `LOCK_EX` and the same bounded `LOCK_NB` polling as
    `negative_process.tree_lock` (which stays untouched: ~199 fixtures share
    it and it is M6-owned), but the holder is one descriptor this process can
    hand to the driver with `pass_fds` instead of a `TreeLock` whose `__exit__`
    always unlocks — unlocking through any copy of the descriptor would drop
    the lock for the child too, which is the one thing the handoff must not
    do. The file is created with the same mode and never removed, so both
    spellings lock the same inode.

    Raises `TimeoutError` (an `OSError`) at the deadline; the caller reads
    that as the control-side failure it is, never as a verdict.
    """
    path = Path(root_path) / '.negative-source.lock'
    deadline = time.monotonic() + timeout
    while True:
        # `O_CLOEXEC` matches the shared `TreeLock` invariant (children inherit
        # the lock only through `pass_fds`, which clears the flag for the one
        # handoff). A daemonized caller with stdio closed can otherwise see the
        # lock descriptor land on 0/1/2, where the child's stdio `dup2` would
        # clobber it (fix-r1 review, boundary P3) — move it out of that range
        # before it is ever handed over. `F_DUPFD_CLOEXEC` keeps the flag on
        # the copy, so the invariant survives the move.
        fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_CLOEXEC, 0o644)
        if fd <= 2:
            moved = fcntl.fcntl(fd, fcntl.F_DUPFD_CLOEXEC, 3)
            os.close(fd)
            fd = moved
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return fd
        except BlockingIOError:
            os.close(fd)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f'source lock busy: {path}')
            time.sleep(min(0.05, remaining))


def launch_under_lock(out, env, name, log, fd):
    """Start the driver and complete the atomic lock handoff (#1695).

    The child is started with the lock descriptor in `pass_fds`, and only
    then is this process's copy closed — WITHOUT `flock(LOCK_UN)`. The lock
    belongs to the open file description, which the child now shares, so the
    close leaves it held by the driver: from this process's marker write,
    through the child's first statement, to the driver's own restore, the
    lock is held continuously and a sibling polling `LOCK_NB` cannot win it
    in between. Unlocking here (the pre-#1695 shape, where the driver
    re-acquired on its own) drops the child's lock as well and re-opens that
    window; closing before the child exists releases the lock outright.
    Narrowed, not eliminated (fix-r1 review, concurrency P2 / boundary P2):
    if the child dies before its restore the descriptor closes with it, and
    a launch failure between the write and this call is answered by the
    caller's `finally` closing the descriptor so `restore_locked` can re-take
    it — the descriptor must still be open here for exactly that path.
    """
    process = launch(out, env, name, log, pass_fds=(fd,))
    os.close(fd)
    return process


def process_census():
    """One `ps` snapshot: a list of `(pid, ppid, pgid, state)` rows (#1705).

    `pid=,ppid=,pgid=,state=` suppresses the header on both BSD and GNU `ps`,
    and the census is what makes the setsid descendants of the driver visible:
    they are in *their own* sessions, so no group signal aimed at the leader
    reaches them (#1705). The `state` column is how the caller tells a live
    leader from a zombie one: a zombie's children are already reparented, so
    the ppid walk below it is broken even though every row is still there
    (fix-r1 review, concurrency P2). A census with no parseable row is an
    error, not an empty tree — the caller falls back to the leader-group kill
    and says so.
    """
    result = subprocess.run(['ps', '-eo', 'pid=,ppid=,pgid=,state='],
                            capture_output=True, text=True,
                            timeout=PS_CENSUS_TIMEOUT)
    rows = []
    for line in result.stdout.split('\n'):
        fields = line.split()
        if len(fields) != 4:
            continue
        try:
            rows.append((int(fields[0]), int(fields[1]), int(fields[2]),
                         fields[3]))
        except ValueError:
            continue
    if not rows:
        raise ValueError(f'ps reported no parsable rows: rc={result.returncode} '
                          f'stderr={result.stderr.strip()[:200]!r}')
    return rows


def descendant_groups(leader, census):
    """Every process group at or below `leader`, from one census snapshot.

    Pure: the snapshot is taken by the caller (`process_census`) so a test can
    feed a synthetic tree. The leader itself is included (its own group dies
    with the plain `_kill_group`); every descendant contributes its group id,
    which is how a `start_new_session` child — invisible to the leader's group
    signal — is reached through *its* group instead. A descendant that exited
    between the census and the kill is simply gone; one that was *spawned* in
    that gap escapes this snapshot, which is why the census follows the leader
    group kill as closely as it does. The walk ignores the `state` column: a
    zombie descendant still owns its pgid, and signalling it is a no-op.
    """
    children = {}
    for pid, ppid, _pgid, _state in census:
        children.setdefault(ppid, []).append(pid)
    found = {leader}
    stack = [leader]
    while stack:
        pid = stack.pop()
        for child in children.get(pid, ()):
            if child not in found:
                found.add(child)
                stack.append(child)
    return {pgid for pid, _ppid, pgid, _state in census if pid in found}


def _kill_group_guarded(process):
    """`_kill_group` under `_kill_tree`'s never-raise contract (#1705).

    `negative_process._kill_group` re-raises its `PermissionError` after the
    reap-and-retry (the macOS sandbox EPERM on an unreaped exited leader), and
    that must not escape either: the cleanup answers an already-failed row,
    so a second failure is printed — the residue stays visible — instead of
    replacing the timeout the caller is about to report (fix-r1 review,
    boundary P2).
    """
    try:
        _kill_group(process)
    except OSError as error:
        print(f'#1705 the driver group kill failed too '
              f'({type(error).__name__}: {error}); orphan descendants '
              '(if any) are left running', file=sys.stderr, flush=True)


def _kill_tree(process):
    """Kill the driver, its sessions, and every setsid descendant (#1705).

    `negative_process.run_owned` starts each cargo / rustc / timing child with
    `start_new_session=True`, so the old `_kill_group` alone reached only the
    driver's own session: the rest survived as orphans, still compiling the
    marked source into the shared `CARGO_TARGET_DIR` while the restore below
    put HEAD bytes back. The census is taken while the leader is still alive
    (reparenting on its death would hide the children), the leader's group is
    killed first to stop new spawns, then each enumerated group — never this
    control's own — gets `SIGKILL`. Best effort by design, fail-closed on the
    error paths, all of which *print* instead of raising so cleanup can never
    mask the timeout it answers; the row is already INVALID on this path:

    * the census itself — `subprocess.run(..., timeout=PS_CENSUS_TIMEOUT)`
      times out with `subprocess.TimeoutExpired`, a `SubprocessError` that is
      *not* an `OSError`, so the except tuple catches the whole
      `SubprocessError` family instead of enumerating members (fix-r1
      review, sibling P2 / concurrency P2 / boundary P2 — three independent
      hits on the same hole); a plain `OSError`/`ValueError` rides along;
    * a leader that exited before the census — the caller's `poll()`/`wait()`
      liveness check and the census are two instants, and a leader that dies
      in between is a zombie (or already reaped) whose children are
      reparented to init: the ppid walk would enumerate nobody while the
      setsid orphans stay invisible. The census's `state` column makes that
      visible instead of silent (fix-r1 review, concurrency P2), and the
      leader-group fallback still reaches the driver's direct children,
      which never left its group;
    * the leader-group kill — see `_kill_group_guarded`.
    """
    try:
        census = process_census()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f'#1705 ps census failed ({type(error).__name__}: {error}); '
              'killed the driver group only', file=sys.stderr, flush=True)
        _kill_group_guarded(process)
        return
    leader_state = next((state for pid, _ppid, _pgid, state in census
                         if pid == process.pid), None)
    if leader_state is None or leader_state.startswith('Z'):
        print('#1705 the driver exited before the ps census; its descendants '
              'were reparented and are not enumerable; killed the driver '
              'group only', file=sys.stderr, flush=True)
        _kill_group_guarded(process)
        return
    _kill_group_guarded(process)
    own = os.getpgrp()
    for pgid in sorted(descendant_groups(process.pid, census)):
        if pgid == own or pgid == process.pid:
            continue  # already signalled above / never this control's group
        try:
            os.killpg(pgid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass  # raced exit or reaping; the census is a snapshot


def requested_clock(text, not_before=None):
    """Return the driver's own monotonic timestamp from its `lock-requested` line.

    Stamping the request with the control's read time biases the measured wait
    *late*, which makes it come out **shorter** than the real one: the control
    polls every 50 ms and `read_log` can be delayed further on a cold cache, so
    a genuinely blocked driver can land under the proof threshold and be
    reported INVALID (fail-closed -- the bias is never towards a false pass;
    fix-r5 boundary P2). The driver prints `time.monotonic()` on the same line
    and both processes read the host's monotonic clock, so the difference is
    the real wait. `None` when the line carries no parseable number -- an older driver,
    or a driver whose marker line was rewritten -- and the caller falls back to
    its read time, which is the pre-fix behaviour and never turns a wait into a
    pass on its own.

    Only *complete* lines count (fix-r6 review, boundary P2): the control polls
    a file the driver is still writing, so the trailing newline-less segment
    can be half a number (`lock-requested 12`), which would freeze a bogus
    stamp and blow the measured wait up to the whole monotonic epoch -- the
    direction that lets a slow no-lock mutant pass. A non-finite value (nan
    compares False against every threshold) or one from the future is treated
    the same as no value at all: fall back to the read time. `not_before` is
    the control's own `time.monotonic()` when it started the hold: a stamp
    from before that (or `0.0`, which `float('1e-400')` yields) would make the
    measured wait look longer than the whole epoch, and `MIN_BLOCK_SECONDS` is
    the only proof of a wait -- so anything earlier than `STAMP_SLACK_SECONDS`
    before the hold began is rejected too (fix-r7 review, boundary P2).
    """
    lines = text.split('\n')
    if lines and lines[-1] != '':
        lines = lines[:-1]  # the last segment may still be being written
    for line in lines:
        if line.startswith('lock-requested'):
            fields = line.split()
            if len(fields) == 2:
                try:
                    value = float(fields[1])
                except ValueError:
                    return None
                if not math.isfinite(value) or value > time.monotonic():
                    return None
                if (not_before is not None
                        and value < not_before - STAMP_SLACK_SECONDS):
                    return None
                return value
            return None
    return None


def watch(process, path, not_before=None):
    """Return what the driver printed while the caller still holds the lock.

    `lock-requested` is printed immediately before the driver's blocking
    `flock` call and `lock-acquired` as the first statement after it returns,
    so the pair is a state proof, not a quiet-window inference: the only code
    between the two lines is the request. Seeing the request without the
    acquisition at the end of the hold means the driver is inside the request;
    it cannot have acquired the lock the control holds, unless the lock does
    not exclude -- which is SURVIVED.

    Returns `(text, requested_at)`. No grace window: a driver that is slow to
    start simply never prints the request during the hold, and the caller
    reports INVALID rather than guessing that it was blocked.
    """
    deadline = time.monotonic() + HOLD_SECONDS
    text = ''
    requested_at = None
    while True:
        text = read_log(path)
        if requested_at is None and 'lock-requested' in text:
            stamp = requested_clock(text, not_before)
            requested_at = stamp if stamp is not None else time.monotonic()
        if 'lock-acquired' in text or process.poll() is not None:
            return text, requested_at
        if time.monotonic() >= deadline:
            return text, requested_at
        time.sleep(0.05)


def settle(process, log):
    """Wait for the driver after the lock is released; return its returncode."""
    try:
        process.wait(timeout=WATCHDOG)
    except subprocess.TimeoutExpired:
        # Kill the whole driver tree, not the leader alone (#1705): the
        # driver starts cargo / rustc / the timing tool through
        # `negative_process.run_owned`, which uses `start_new_session=True`,
        # so each is in its own session that a group signal aimed at the
        # driver's own session never reached -- they survived as orphans,
        # still compiling the marked source into the shared target
        # directory. `_kill_tree` enumerates those sessions from a `ps`
        # census taken while the leader is alive and signals each group;
        # `_kill_group` alone remains the fallback when the census fails.
        _kill_tree(process)
        process.wait(timeout=60)
        raise
    finally:
        log.close()
    return process.returncode


def restore(path, original, marked=None):
    """Undo this process's marker only; never overwrite someone else's bytes.

    Returns `(restored, mode_lost, complaint)`. `restored` is False when the
    bytes are not HEAD and not *this* process's marker (the marker carries our
    pid, so a concurrent control's marker is not mistaken for ours): another
    writer touched the file and the run must neither report success nor clobber
    that work. `mode_lost` is a separate machine flag, not a reading of
    `complaint`: `complaint` also carries the "restored without the lock"
    fallback note, which says nothing about the mode and must not invalidate
    the row (fix-r11 review, boundary P1 / sibling P2). The write is a temp
    file plus `os.replace`, which is atomic for a reader on the same
    filesystem, and the temp file is removed if the replace itself fails.
    """
    marked = original + MARKER if marked is None else marked
    try:
        current = path.read_bytes()
    except OSError as error:
        return (False, False,
                f'{type(error).__name__}: {error} (could not read the source back)')
    if current == original:
        return True, False, ''
    if current != marked:
        return (False, False,
                'the product source no longer matches HEAD or this '
                'control\'s marker; left untouched for inspection')
    temp = scratch('restore')
    try:
        # Read the mode first, but never let a failed read abandon the byte
        # restore: leaving the marker on disk is the worse failure (it is
        # process-tagged, so no other control will ever clear it and the whole
        # worktree stays dirty for this fixture family). A missing mode is
        # carried out as a complaint instead (fix-r9 review, boundary P3 /
        # concurrency P2; the earlier version swallowed the stat failure and
        # reported success, which was just as wrong in the other direction).
        try:
            mode = stat.S_IMODE(path.stat().st_mode)
        except OSError:
            mode = None
        temp.write_bytes(original)
        # `os.replace` swaps in the temp file's inode, so without this the
        # tracked source comes back with the umask's mode instead of its own
        # (fix-r6 review, sibling P2 -- the driver's `negctl_restore` path got
        # the same fix; both write the same tracked file).
        if mode is not None:
            try:
                os.chmod(temp, mode)
            except OSError:
                mode = None
        os.replace(temp, path)
    except OSError as error:
        try:
            temp.unlink()
        except OSError:
            pass
        return (False, False,
                f'{type(error).__name__}: {error} (the rewrite failed)')
    try:
        readback = path.read_bytes()
    except OSError as error:
        return (False, False,
                f'{type(error).__name__}: {error} (rewrite not readable)')
    if readback == original:
        if mode is None:
            # The bytes are back but the mode is not HEAD's, and git tracks
            # only the exec bit: this difference is invisible to every guard.
            # Report it instead of claiming a clean restore.
            return (True, True,
                    'restored the bytes but not the file mode (stat or chmod '
                    'failed); the worktree may carry a mode-only difference '
                    'that no guard can see')
        return True, False, ''
    return False, False, 'the restore did not read back as HEAD bytes'


def restore_locked(path, original):
    """Restore the marker under the tree lock, bounded, then release.

    The primary path is now the driver's own restore (`negctl_restore` in
    `rss_probe_1555.py`), and since #1695 the lock travels with the descriptor
    from the marker write to that restore (see the module docstring), so this
    fallback is off the normal path. It is still required, not redundant: the
    handoff narrows the window, it does not remove it — a driver that never
    reaches the dirty branch (a mutant without the dirty gate), crashes, or
    is killed exits with the marker still on disk *and the lock already free*
    (the descriptor died with the process), and it would stay marked forever
    -- the marker is process-tagged, so no other control will ever clear it.
    This function is the fallback for exactly
    that, and for the second call from `main`. It re-takes the tree lock with
    a bounded wait; a sibling that holds the lock for longer than the bound
    must not be able to skip the restore, so the timeout falls back to the
    unlocked path and says so. Any `OSError` from the lock itself (EMFILE,
    ENOSPC, a missing worktree) takes the same fallback: a restore that
    silently did not happen would leave the product source marked forever.
    """
    try:
        with tree_lock(root, timeout=RESTORE_LOCK_TIMEOUT):
            return restore(path, original, original + MARKER)
    except OSError as error:
        restored, mode_lost, complaint = restore(path, original, original + MARKER)
        # The fallback note goes into the free-text channel on purpose: taking
        # the lock failed, but the bytes may have been restored perfectly, and
        # that must not read as a mode loss (fix-r11 review, boundary P1).
        return restored, mode_lost, (f'{type(error).__name__}: {error} '
                                     f'(restored without the lock); {complaint}')


def lock_verdict(*, early, full, returncode, driver_status, positive_ok,
                 positive_error, positive_rc, positive_status, positive_minima,
                 blocked, progressed, failure, early_exit):
    """(#1713) The `lock-held` verdict chain as one pure function.

    Extracted verbatim from `lock_check` -- same branches, same order, same
    reason strings -- so every cell can be self-tested from fake inputs
    (`test_controls.py`) without a cargo run. The first four verdicts do not
    depend on the driver at all; the timing band is deliberately *after* the
    two independent state checks and the rc/status split, so it can only turn
    a would-be KILLED row INVALID, never rescue a no-lock mutant. Returns
    `(status, reason)`.
    """
    acquired = 'lock-acquired' in full
    passed_lock = acquired or 'provenance ' in full
    if 'lock-acquired' in early:
        return 'SURVIVED', ('the driver acquired the shared lock while the '
                           'control held it')
    if progressed:
        return 'SURVIVED', ('the driver built while the control held the lock '
                           '(build.log or summary.json appeared during the hold)')
    if failure:
        # A control-side failure (lock timeout, launch error) dominates: it
        # says nothing about the driver's locking, so the row must not be read
        # as evidence either way.
        return 'INVALID', failure
    if not positive_ok:
        return 'INVALID', (f'clean-tree positive control failed: {positive_error} '
                           f'rc={positive_rc} '
                           f'status={positive_status!r} '
                           f'minima={positive_minima!r}')
    if 'lock-requested' not in early:
        return 'INVALID', (f'the driver never asked for the lock during the hold '
                           f'(early={early.strip()[:200]!r}, early_exit={early_exit})')
    if not acquired and passed_lock:
        # The driver printed `provenance`, which happens only after the lock
        # call returned, yet `lock-acquired` is nowhere in its output: the
        # mutant skipped the lock and ran on. Decisive and independent of the
        # timing band below, and checked before the rc / status branches on
        # purpose -- a no-lock mutant whose build also fails is still a
        # survivor, and reporting it as INVALID would hide the mutation behind
        # a build error (fix-r5 review, concurrency P2).
        return 'SURVIVED', ('the driver asked for the shared lock, never printed '
                           'lock-acquired, and went on to print provenance -- it ran '
                           f'without the lock (rc={returncode} '
                           f'status={driver_status!r})')
    if not acquired and returncode == 0:
        # No `lock-acquired`, no `provenance`, and the driver still exited
        # cleanly: it ran to the end without ever touching the lock. That is
        # the mutant that deletes the whole `with tree_lock(...)` block,
        # provenance print included -- it builds nothing and writes no
        # summary, so it has to be caught here rather than blamed on the
        # infrastructure (fix-r7 review, concurrency P2 / boundary P3).
        return 'SURVIVED', ('the driver exited cleanly (rc=0) without ever printing '
                           'lock-acquired or provenance -- it ran without the lock')
    if not acquired:
        # No `lock-acquired` *and* no `provenance` *and* a non-zero exit: the
        # driver never got past the lock call at all, which is what a crash or
        # an external kill looks like (`os.open` failing in
        # `tree_lock.__enter__`, EMFILE, a stray signal). Blaming the mutant
        # for that would be a false FAIL with the wrong attribution (fix-r6
        # review, concurrency P2).
        return 'INVALID', ('the driver never reached the lock: no lock-acquired and no '
                           f'provenance in its output (rc={returncode} '
                           f'status={driver_status!r})')
    if blocked < MIN_BLOCK_SECONDS:
        return 'INVALID', (f'the request appeared {blocked:.2f}s before the release, '
                           f'shorter than the {MIN_BLOCK_SECONDS}s needed to prove a wait')
    if returncode != 0:
        return 'INVALID', f'driver exit {returncode} after the release'
    if driver_status != 'OK':
        return 'INVALID', f'driver status {driver_status!r} after the release'
    return 'KILLED', ''


def lock_check(out, env):
    """The lock makes the driver wait; the clean run proves the driver works."""
    positive = None
    positive_error = ''
    try:
        positive = run_owned(PROBE + [str(out / 'probe-clean')] + MODE,
                             cwd=root, env=env, timeout=WATCHDOG)
    except (subprocess.TimeoutExpired, OSError) as error:
        positive_error = f'{type(error).__name__}: {error}'
    positive_summary = read_summary(out / 'probe-clean' / 'summary.json')
    positive_minima = positive_summary.get('minima') or {}
    positive_ok = (positive is not None and positive.returncode == 0
                   and positive_summary.get('status') == 'OK'
                   and isinstance(positive_minima.get('baseline'), int))
    path = out / 'probe-lock.log'
    log = path.open('w')
    process = None
    early = ''
    failure = ''
    requested_at = None
    held = 0.0
    blocked = 0.0
    progressed = False
    try:
        with tree_lock(root, timeout=LOCK_TIMEOUT):
            started = time.monotonic()
            process = launch(out, env, 'probe-lock', log)
            early, requested_at = watch(process, path, not_before=started)
            released = time.monotonic()
            held = released - started
            # A lower bound on the true wait: the driver stamps the request
            # microseconds before the blocking call and the release is sampled
            # after the hold. A cold start that reaches the request in the last
            # two seconds of the hold therefore lands under MIN_BLOCK_SECONDS
            # and is reported INVALID -- fail-closed on purpose, because the
            # same shortfall would otherwise be the shape of a slow no-lock
            # mutant. The verdict does not rest on this number alone: the two
            # independent state checks below (`progressed`, `acquired`) hold it
            # even when the timing band is unusable (fix-r5 boundary P2).
            if requested_at is not None:
                blocked = released - requested_at
            # A driver that is not blocked builds while the control still holds
            # the lock, and both files are written only after the acquisition.
            # This is a *fast path*, not the state proof: `build.log` lands when
            # the build ends and `summary.json` when the measurement ends, so a
            # cold target directory can keep both absent for the whole hold and
            # leave this False on a mutant (fix-r5 review, concurrency P2). The
            # verdict does not rest on it -- the decisive check is the absence of
            # `lock-acquired` in the driver's full output, which is independent
            # of build duration and is evaluated before the rc / status branches
            # below so a mutant whose build also fails still reads SURVIVED.
            progressed = ((out / 'probe-lock' / 'build.log').exists()
                          or (out / 'probe-lock' / 'summary.json').exists())
        returncode = settle(process, log)
    except (subprocess.TimeoutExpired, OSError) as error:
        failure = f'{type(error).__name__}: {error}'
        if process is not None and process.poll() is None:
            _kill_tree(process)
            process.wait(timeout=60)
        log.close()
        returncode = None
    summary = read_summary(out / 'probe-lock' / 'summary.json')
    # The full output, not just the hold window: a real driver prints
    # `lock-acquired` as soon as the control releases the lock. `provenance`
    # is printed a few statements later (under the lock), so its presence is
    # the "the driver got past the lock call" bit that separates a mutant from
    # a driver that crashed before ever asking (fix-r6 review, concurrency P2).
    full = read_log(path)
    acquired = 'lock-acquired' in full
    # #1713: the verdict chain itself now lives in `lock_verdict`, extracted
    # verbatim, so its every branch is self-testable without a cargo run.
    status, reason = lock_verdict(
        early=early, full=full, returncode=returncode,
        driver_status=summary.get('status'), positive_ok=positive_ok,
        positive_error=positive_error,
        positive_rc=None if positive is None else positive.returncode,
        positive_status=positive_summary.get('status'),
        positive_minima=positive_summary.get('minima'),
        blocked=blocked, progressed=progressed, failure=failure,
        early_exit=process is not None and process.returncode is not None
                   and not early)
    return dict(control='lock-held-during-driver', status=status,
                held_seconds=round(held, 3), blocked_seconds=round(blocked, 3),
                progressed_under_hold=progressed, acquired_after_release=acquired,
                positive=dict(returncode=None if positive is None else positive.returncode,
                              status=positive_summary.get('status'),
                              minima=positive_summary.get('minima')),
                early_output=early_lines(early),
                returncode=returncode, driver_status=summary.get('status'),
                minima=summary.get('minima'), reason=reason)


def dirty_verdict(*, restored, mode_lost, complaint, failure, provenance,
                  returncode, driver_status, failures, built, refused):
    """(#1713) The `dirty-src` verdict chain as one pure function.

    Extracted verbatim from `dirty_check` -- same branches, same order, same
    reason strings -- so every cell can be self-tested from fake inputs
    (`test_controls.py`) without a cargo run. The two restore branches come
    first on purpose: a worktree left marked (or left in a mode-only drifted
    state) is damage the control itself caused, and no driver verdict can
    outrank it. Returns `(status, reason)`.
    """
    if not restored or mode_lost:
        # Either the bytes are not HEAD again (`not restored`, always with a
        # complaint), or they are but the file mode is not (`mode_lost`):
        # git tracks only the exec bit and `_assert_workspace_sources` only
        # byte-compares, so the second case leaves the worktree dirty in a way
        # no guard can see. Attribute it instead of riding on a green row
        # (fix-r9 review). `mode_lost` is a machine flag of its own, not a
        # reading of `complaint`, so the "restored without the lock" fallback
        # note cannot trip this branch (fix-r11 review, boundary P1).
        return 'INVALID', complaint
    if provenance.get('negctl_restore_mode_lost'):
        # The driver put the bytes back but lost the mode; the fallback below
        # then reads that wrong mode off the file and "preserves" it, so this
        # machine field is the only place the loss is visible (fix-r10 review,
        # sibling P3). Attribute it rather than ride on a green row.
        return 'INVALID', ('the driver restored the marker bytes but not the file '
                           f'mode: {provenance.get("negctl_restore")!r}')
    if failure:
        # A control-side failure (handoff timeout, launch error, watchdog
        # kill) dominates for the same reason as in `lock_verdict`.
        return 'INVALID', failure
    if not provenance and returncode == 0:
        # The `lock_check` clean-exit branch, mirrored (fix-r9 review, three
        # independent P1s): no `provenance` and a zero exit means the driver
        # ran to the end without ever reaching the dirty check -- exactly the
        # mutant that deletes the whole lock block, summary write included.
        # Its own failure to check the tree is the mutation, so this is
        # SURVIVED, not an infrastructure INVALID. A non-zero exit with no
        # summary is still ambiguous and stays INVALID below.
        return 'SURVIVED', ('the driver exited cleanly (rc=0) without ever printing '
                           'provenance -- it ran to the end without reaching the '
                           'dirty check')
    if not provenance:
        # Same rule as `lock_check`'s "never reached the lock" branch (fix-r7
        # review, sibling P1): a driver that died before it could check the
        # tree leaves no summary, so `refused` is False -- but that is an
        # infrastructure failure, not evidence that the mutant survived.
        return 'INVALID', ('the driver never reached the dirty check (no provenance '
                           f'in its summary: rc={returncode} '
                           f'status={driver_status!r}); a crash before the '
                           'lock says nothing about the gate')
    if refused:
        return 'KILLED', ''
    return 'SURVIVED', (f'rc={returncode} status={driver_status!r} '
                        f'src_dirty={provenance.get("src_dirty")!r} '
                        f'failures={failures[:1]} '
                        f'built={built}')


def dirty_check(out, env, original):
    """A dirty product source must be refused before any build happens.

    #1695: the tree lock is taken on one raw descriptor (`handoff_lock`) and
    handed to the driver with `pass_fds` (`launch_under_lock`), so the marker's
    life on disk -- write, driver observation, driver restore -- is spent
    inside one continuous hold, with no release-and-reacquire gap for a
    sibling to squeeze into. That closed #1695's seam; it is not a
    crash-proofing claim (fix-r1 review, concurrency P2 / boundary P2): a
    driver that dies before its restore frees the lock with the marker still
    on disk, and a launch failure between the write and the handoff is closed
    by the `finally` below. Both residual windows are what `restore_locked`
    answers; see its docstring.
    """
    path = out / 'probe-dirty.log'
    log = path.open('w')
    process = None
    fd = None
    early = ''
    failure = ''
    restored, mode_lost, complaint = True, False, ''
    try:
        fd = handoff_lock(root, timeout=LOCK_TIMEOUT)
        if source.read_bytes() != original:
            raise RuntimeError('product source differs from HEAD before the dirty check')
        marked = original + MARKER
        temp = scratch('marker')
        mode = stat.S_IMODE(source.stat().st_mode)
        temp.write_bytes(marked)
        # Same inode swap as the two restore paths: keep the source's own
        # mode instead of the umask's (fix-r6 review, sibling P2). Both the
        # read above and this `chmod` are fatal *here*, while the source
        # has just been verified byte-identical to HEAD -- the one moment
        # in the whole chain when its mode is known good. A swallowed
        # `chmod` would land a umask-mode marker, and both restore paths
        # read the mode back off the file: they would then faithfully
        # write that wrong mode home, leaving a permanent mode difference
        # that git and every guard are blind to, with `mode_lost` False
        # (fix-r11 review, sibling P2 / boundary P2). Aborting before the
        # replace leaves the tracked source untouched; only the temp goes.
        try:
            os.chmod(temp, mode)
        except OSError as error:
            try:
                temp.unlink()
            except OSError:
                pass
            raise RuntimeError(
                'the marker could not carry the source file mode '
                f'({type(error).__name__}: {error}); refusing to swap a '
                'umask-mode marker into the tracked source') from error
        os.replace(temp, source)
        # The driver undoes the marker itself, under the lock, before it
        # returns (`negctl_restore` in rss_probe_1555.py): a restore issued
        # after this process exits would race the runners queued behind the
        # driver and let them see a marked source. The lock travels with the
        # descriptor (`launch_under_lock`), so "under the lock" is the same
        # hold this branch is already inside (#1695).
        original_file, marked_file = out / 'restore-original', out / 'restore-marked'
        original_file.write_bytes(original)
        marked_file.write_bytes(marked)
        driver_env = dict(env)
        driver_env['PDRS_NEGCTL_RESTORE'] = '\x1f'.join(
            (RELATIVE, str(original_file), str(marked_file)))
        driver_env['PDRS_NEGCTL_LOCK_FD'] = str(fd)
        process = launch_under_lock(out, driver_env, 'probe-dirty', log, fd)
        fd = None  # the driver's copy is now the only reference to the hold
        # Recorded for the row only; the dirty verdict below never reads it.
        # The handoff driver owns the lock from its first statement, so
        # `lock-requested` and `lock-acquired` print back to back.
        early, _requested_at = watch(process, path)
        returncode = settle(process, log)
    except (subprocess.TimeoutExpired, OSError, RuntimeError) as error:
        failure = f'{type(error).__name__}: {error}'
        if process is not None and process.poll() is None:
            _kill_tree(process)
            process.wait(timeout=60)
        log.close()
        returncode = None
    finally:
        if fd is not None:
            # The marker is on disk and this process still holds the only
            # reference — the launch failed before the handoff, or an error
            # landed between the write and `launch_under_lock` (fix-r1 review,
            # simplify P3: this close used to be duplicated in the except body
            # above; `finally` always runs, so one site is enough, and the
            # descriptor now also rides out the kill cleanup above — the lock
            # stays held for that bounded window, which only delays, never
            # deadlocks, the siblings queued behind it). Release before
            # `restore_locked` re-takes the lock, or the restore would
            # deadlock against this very fd.
            os.close(fd)
            fd = None
        restored, mode_lost, complaint = restore_locked(source, original)
    summary = read_summary(out / 'probe-dirty' / 'summary.json')
    failures = [str(item) for item in (summary.get('failures') or [])]
    provenance = summary.get('provenance') or {}
    # The driver's own machine field, not a human-readable message prefix: the
    # wording of the refusal drifts between rounds, and `rc=2 + INVALID + no
    # build.log` is also the shape of a plain build failure (which reports
    # `src_dirty=false`). `provenance.src_dirty` separates the two.
    refused = (returncode == 2
               and summary.get('status') == 'INVALID'
               and not summary.get('minima')
               and not (out / 'probe-dirty' / 'build.log').exists()
               and provenance.get('src_dirty') is True)
    # #1713: the verdict chain itself now lives in `dirty_verdict`, extracted
    # verbatim, so its every branch is self-testable without a cargo run.
    status, reason = dirty_verdict(
        restored=restored, mode_lost=mode_lost, complaint=complaint,
        failure=failure, provenance=provenance, returncode=returncode,
        driver_status=summary.get('status'), failures=failures,
        built=(out / 'probe-dirty' / 'build.log').exists(), refused=refused)
    return dict(control='dirty-src-driver-refused', status=status,
                returncode=returncode, driver_status=summary.get('status'),
                built=(out / 'probe-dirty' / 'build.log').exists(),
                early_output=early_lines(early),
                failures=summary.get('failures'), restored=restored,
                restore_complaint=complaint, reason=reason)


def sanitized_env():
    """The caller's environment minus the control-injection variables.

    Both `PDRS_NEGCTL_LOCK_FD` and `PDRS_NEGCTL_RESTORE` are set per-launch by
    `dirty_check` only (#1695 hygiene; the restore spec joined the drop in
    fix-r1 review, sibling P3). A `PDRS_NEGCTL_LOCK_FD` inherited from the
    caller would make the `lock_check` leg's driver (which must prove it
    *blocks*) silently own an inherited lock instead and print `lock-acquired`
    during the hold -- the exact false SURVIVED that leg exists to catch --
    and a leftover `PDRS_NEGCTL_RESTORE` (say, a marker environment left by a
    SIGKILLed dirty run, the very case `restore_locked` answers) would make a
    driver that finds the tree dirty run `negctl_restore` on files that are
    not this control's, outside this control's lock. Nothing legit ever
    passes either in, so both are dropped unconditionally.
    """
    env = os.environ.copy()
    env.pop('PDRS_NEGCTL_LOCK_FD', None)
    env.pop('PDRS_NEGCTL_RESTORE', None)
    return env


def main():
    if len(sys.argv) != 2:
        raise SystemExit('usage: dirty_tree_control_1555.py NEW_OUTPUT_DIR')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = sanitized_env()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be provided by the caller')
    original = _head_bytes(root, RELATIVE, 'HEAD')
    rows = []
    restored = False
    mode_lost = False
    complaint = ''
    try:
        rows.append(lock_check(out, env))
        rows.append(dirty_check(out, env, original))
    finally:
        restored, mode_lost, complaint = restore_locked(source, original)
    counts = {state: sum(row['status'] == state for row in rows)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    record = dict(controls=rows, counts=counts, restored=restored,
                  restore_mode_lost=mode_lost, restore_complaint=complaint)
    (out / 'summary.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record), flush=True)
    return (0 if restored and not mode_lost
            and counts['KILLED'] == len(rows) else 1)


if __name__ == '__main__':
    raise SystemExit(main())
