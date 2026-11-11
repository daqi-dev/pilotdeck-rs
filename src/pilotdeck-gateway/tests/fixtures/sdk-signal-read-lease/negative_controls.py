#!/usr/bin/env python3
# anchor-selfcheck: unsupported two-phase concurrency lease control with one imperative self-mutation kill row, no declarative whole-file product anchors
"""#1662 paired read-lease control for the resident sdk_signal_exit golden test.

RED proves the #1662 false FAIL: with the fixture read lease's contended wait
removed, the resident ``signal_oracle_matches_recorded_source_and_local_files``
test reads a committed-mode mutant of ``capture.py`` and fails the golden
sha256 while the source guard still holds the tree lock. GREEN proves the fixed
test survives the same writer protocol: an independent writer child holds the
tree lock with the mutant on disk, the fixed test waits for the lease, and
reads only restored bytes. Every row is published after each source was
restored and re-verified; any non-KILLED row exits non-zero.

GREEN and RED use separate judges. In RED a golden-sha failure is the mutant
being killed; in GREEN only a *pass* counts, and the pass must carry the
resident test's own measured lease wait (``#1662 read lease wait ms=``) at or
above ``MIN_LEASE_WAIT_MS`` -- a pass without it never met the writer and
proves nothing. GREEN failures of any kind are INVALID, never KILLED.

The writer publishes the exact interval it holds the lock in; the resident test
reports the instant of its first *refused* attempt, and GREEN requires that
instant to fall inside the interval less ``WITNESS_MARGIN_MS`` at the upper end
(the truncated-millisecond bucket argument in the constant below). A wait
measured against some later lock holder, or a stall after an uncontended
acquisition, is therefore INVALID, never KILLED.

``--stop-check`` self-checks the writer's exit contract (a requested stop exits
0 *via* the graceful path, an own failure exits non-zero and is reported); it is
also the third row of the default run below.

#2513 fix-r7 (concurrency P1): this fixture runs under a host-pre-held
/tmp/pdrs-parity.lock (the 28999 lane contract, ``sdk-signals/README.md``), and
its mutation windows cover parity-guarded sources (``capture.py``,
``read_lease.rs``), so since the guarded set grew they would queue on the very
lease the host holds for us -- 60 s TimeoutError, every row INVALID. The
windows now redirect to ``out/parity-redirect.lock`` behind the fail-closed
``_require_host_lease`` gate (the sdk-signals fix-r4/r6 twin), and
``--window-lock-check`` machine-checks the condition the kill row
``--kill-window-lock`` pins: with the shared lease held by a live external
process, the entry must acquire the window lock on the redirect path instead
of blocking.

Run only after commit (mutating_sources refuses a dirty tree):
    CARGO_TARGET_DIR=<target> python3 -I .../sdk-signal-read-lease/negative_controls.py NEW_OUTPUT_DIR
"""
import importlib.util
import json
import os
import re
import signal
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import negative_process
from negative_process import (
    TreeLock,
    _collect,
    _ensure_workspace_scanner,
    _head_bytes,
    _kill_group,
    exclusive_write,
    mutating_sources,
    new_output_directory,
    run_owned,
    wait_for_slot,
    write_source_atomic,
)
from negative_suite import passed, publish_summary
from negative_panic import named_test_failed, panic_evidence

root = Path(__file__).resolve().parents[5]
# #1885: the lease moved from `sdk_signal_exit.rs` into this shared module, so
# the anchor, the marker guard and the RED mutation all follow it here. The
# resident test that reports the marker is still `RESIDENT_TEST` below; only the
# bytes being read/mutated moved.
test_source = root / 'src/pilotdeck-cli/tests/common/read_lease.rs'
capture = root / 'src/pilotdeck-cli/tests/fixtures/sdk-signals/capture.py'
wrapper = root / 'src/pilotdeck-cli/tests/fixtures/sdk-signals/sigint_default.py'
RESIDENT_TEST = 'signal_oracle_matches_recorded_source_and_local_files'
SHA_MARKER = '#1145 source file SHA256 matches disk: probeSha256/capture.py'
# The contended-wait sleep is the whole #1662 fix: returning the file instead
# keeps reading guarded bytes while the committed writer still holds the lock.
LEASE_WAIT_ANCHOR = 'thread::sleep(Duration::from_millis(50));'
LEASE_WAIT_REMOVED = 'return Some(file);'
CAPTURE_RESIDUE = '\n# 1662 committed-mode mutant residue\n'
# How long the writer holds the lock once its window opens. The reader is
# launched right after the window appears (its cargo slot is taken beforehand),
# so it reaches the lock well inside this hold. The writer also publishes the
# exact hold interval in epoch milliseconds; GREEN requires the reader's own
# acquisition timestamp to fall inside it, which is what binds the witness to
# *this* writer instead of any later lock holder (#1662 review r3 P0).
HOLD_SECONDS = 90
WRITER_HOLD = 'writer-hold.json'
# The writer fails fast instead of queueing behind a long committed suite: it
# only has to win the lock once, and a slow acquisition is a busy workspace,
# not evidence about the lease. 300 s stays well inside the 900 s default other
# devices use, so this control never becomes the longest lock holder.
WRITER_LOCK_TIMEOUT = 300
# Writer start -> lock acquisition (<= WRITER_LOCK_TIMEOUT) -> window open.
WRITER_ENTRY_BUDGET = WRITER_LOCK_TIMEOUT + 300
# Writer finish after the hold: the rest of the window plus the restore.
WRITER_EXIT_BUDGET = HOLD_SECONDS + 120
# The resident test reports its own lease wait on stderr (cargo test only shows
# it under --nocapture). That measurement is the contention witness: elapsed
# cargo time also covers startup, the package-cache lock and rebuilds, so it can
# be large without any lease contention (and small after a real, short wait).
# Any wait at all proves the reader met the writer; 500 ms is far above the
# 50 ms poll granularity and far below a window-scale wait.
LEASE_MARKER = '#1662 read lease wait ms='
MIN_LEASE_WAIT_MS = 500
# ``--stop-check`` only exercises the writer's exit contract, so it can use a
# short hold: nothing waits on the lease there.
STOP_CHECK_HOLD_SECONDS = 5
# The SIGTERM run must outlive ``STOP_CHECK_STOP_BUDGET`` so the controller's own
# *terminate* branch is exercised, and the stop has to land well inside the hold
# to prove the signal -- not the sleep ending -- is what stopped it
# (#1662 review r10 P2).
STOP_CHECK_STOP_HOLD_SECONDS = 30
STOP_CHECK_STOP_BUDGET = 5
# ``stop_writer`` and ``force_stop`` bound *each* wait, so this is not a total:
# a writer stuck in its restore costs two of these plus ``_collect``'s 30 s. It
# is derived from the hold rather than written as a literal so raising
# ``STOP_CHECK_HOLD_SECONDS`` cannot leave the first wait expiring while a
# healthy writer is still legitimately asleep -- that would SIGTERM it into a
# clean rc-0 stop and silently stop testing the own-failure path (#1662 review
# r10 P3 / r12 boundary P3).
STOP_CHECK_RESTORE_ALLOWANCE_SECONDS = 55
STOP_CHECK_EXIT_BUDGET = STOP_CHECK_HOLD_SECONDS + STOP_CHECK_RESTORE_ALLOWANCE_SECONDS
# Printed to stderr only when the writer takes the requested-stop path; the
# self-check greps for it so "rc 0 + restored" cannot be satisfied by a SIGTERM
# that was ignored while the writer ran out its hold (#1662 review r9 P2).
STOPPED_MARKER = '#1662 writer stopped'
# ``epoch_ms()`` truncates, so a timestamp is a whole millisecond *bucket*, not
# an instant: the writer's ``end_ms`` bucket also covers the release, and a
# reader refused right after that release by the *next* exclusive holder could
# still report the same number. GREEN therefore requires the refusal to sit at
# least this far *below* the published end. One second is ~1 % of the hold and
# far above any scheduling slop, yet it makes ``blocked <= end_ms - margin``
# mean "refused at least a second before this writer let go" -- while the writer
# demonstrably still held the exclusive lock (#1662 review r8 P0).
#
# Only the end side needs it. ``start_ms`` is sampled in the window body, i.e.
# after ``mutating_sources`` already holds the exclusive lock and after the
# mutant write, so no other holder can have been there a whole bucket earlier;
# a lower margin would only eat into the (much smaller) reader start-up gap and
# turn a perfectly good refusal into INVALID (#1662 review r9 P1).
WITNESS_MARGIN_MS = 1000
CARGO_TIMEOUT = 1800


class _Stopped(Exception):
    """The writer was asked to stop; its unwind restored and re-verified first.

    Raised from the SIGTERM handler and caught around ``mutating_sources``, so a
    requested stop is a *success* (exit 0) while any failure of its own -- a
    restore or a HEAD re-read that raises -- still exits non-zero and is
    reported by the controller (#1662 review r8 P1).
    """


def cargo_argv(suffix):
    return [sys.executable, '-I', '-B', str(wrapper), 'cargo', 'test', '--offline',
            '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-cli',
            '--test', 'sdk_signal_exit', *suffix]


def run_cargo(out, label, suffix, slot_taken=False):
    """Run one cargo invocation; return ``(result, seconds spent inside it)``.

    The elapsed time is reported for the evidence trail only; it is not a
    contention witness (see ``MIN_LEASE_WAIT_MS``). Waiting for a cargo slot is
    excluded either way: it is a scheduling delay, not lease contention.
    ``slot_taken`` skips the wait when the caller already took one (GREEN does
    that before the writer opens its window, so the hold only has to cover the
    test itself).
    """
    if not slot_taken:
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)
        exclusive_write(out / f'{label}.slots.log', slot.stdout + slot.stderr)
    started = time.monotonic()
    try:
        result = run_owned(cargo_argv(suffix), cwd=root, env=os.environ.copy(),
                           timeout=CARGO_TIMEOUT)
    except subprocess.TimeoutExpired as error:
        # Keep whatever was observed: an audit must be able to tell "never
        # started" from "started and was killed at the bound".
        exclusive_write(out / f'{label}.timeout.stdout', error.stdout or '')
        exclusive_write(out / f'{label}.timeout.stderr', error.stderr or '')
        raise
    elapsed = time.monotonic() - started
    exclusive_write(out / f'{label}.stdout', result.stdout)
    exclusive_write(out / f'{label}.stderr', result.stderr)
    return result, elapsed


def publish(path, text):
    """Write a rendezvous marker atomically: readers see the file or nothing."""
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(text)
    temporary.replace(path)


def epoch_ms():
    return int(time.time() * 1000)


def publish_hold(out, start_ms, end_ms):
    """Publish the writer's hold interval; ``end_ms`` is None while it is open."""
    publish(out / WRITER_HOLD, json.dumps({'start_ms': start_ms, 'end_ms': end_ms}) + '\n')


def read_hold(out):
    hold = json.loads((out / WRITER_HOLD).read_text())
    return int(hold['start_ms']), hold['end_ms'] and int(hold['end_ms'])


def read_released(out):
    """``(released_ms, restored)`` after the real release, or None if never published.

    ``restored`` is the writer's *direct observation* that the guarded file is
    back to its HEAD bytes at the moment it recorded the release -- not an
    inference from the exit status, and not implied by the record's existence.
    The controller checks it (#1662 review r8 P2).
    """
    path = out / 'writer-released.json'
    if not path.is_file():
        return None
    record = json.loads(path.read_text())
    return int(record['released_ms']), bool(record.get('restored'))


def capture_matches_head():
    """Is the guarded file byte-identical to its HEAD version right now?"""
    try:
        return capture.read_bytes() == _head_bytes(root, capture.relative_to(root), 'HEAD')
    except Exception:
        # Unreadable HEAD or unreadable file: no restoration claim either way.
        return False


# --- #2513 fix-r7 (concurrency P1): the window redirect --------------------
#
# This fixture's contract is a host-pre-held /tmp/pdrs-parity.lock (the host
# serializes every 28999 lane user and runs this fixture under that hold, the
# same README contract as sdk-signals). Its mutation windows cover
# parity-guarded sources, so they would take that same shared lease -- against
# the host holding it for us -- and fail after 60 s with zero evidence. The
# Python twin of the sdk-signals fix-r4 redirect: the window's
# ``common.parity_lock`` call point is redirected to ``out/parity-redirect.lock``,
# an unshared lease in this run's evidence directory, same owner/guard/
# stale-takeover protocol. Mutual exclusion against live rounds is not weakened
# -- the host pre-hold provides it for the whole run.

COMMON_SOURCE = root / 'scripts/gateway-parity/common.py'
# The redirect probe must acquire well inside this bound; the reverted mutant
# queues on the externally-held shared lease and fails after exactly it.
WINDOW_LOCK_PROBE_SECONDS = 5
# The check's own stand-in for the host pre-hold: one bounded external holder
# of the real shared lease, spawned only when nobody else holds it.
SHARED_HOLD_SECONDS = 30
SHARED_HOLD_ENTRY_SECONDS = 30
SHARED_HOLD_EXIT_SECONDS = 15
# The kill row's dedicated marker: the failure that credits the kill must
# carry it (an unrelated red in the same entry stays INVALID, never KILLED).
WINDOW_LOCK_MARKER = '#2513 该 fixture 必须能取到窗口锁'
# The one kill row (#2513 fix-r7): the redirect reverted to the shared lease.
WINDOW_LOCK_KILL_ROW = (
    'window-lock-redirect-reverted',
    # The anchors are concatenated so this very definition does not spell
    # them contiguously: the row counts occurrences in this file's text,
    # and a whole-literal constant would match its own definition.
    'timeout=timeout, ' + 'lock=out / "parity-redirect.lock"):',
    'timeout=timeout, ' + 'lock=common.LOCK):',
)


class _HoldStopped(Exception):
    """The shared-hold holder was asked to stop; the unwind releases first."""


def _load_pristine_common(staged):
    """Load scripts/gateway-parity/common.py from staged verified bytes.

    Never a ``git`` re-read and never mutated disk code: the caller stages
    HEAD-proven bytes (the mutation-window snapshot, or ``_head_bytes`` for
    the check modes) exactly like ``negative_process._parity_mutation_lock``
    and the sdk-signals redirect do.
    """
    spec = importlib.util.spec_from_file_location(
        'negative_pristine_common_sdk_signal_read_lease', staged)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _require_host_lease(common, lock=None):
    """#2513 fix-r7 (concurrency P1): the host pre-hold, machine-checked.

    The sdk-signals fix-r6 gate, same shape: without it the redirect would
    also remove the old shared-lease 60 s TimeoutError as a side effect, and a
    run without the host's serialization could silently open a mutation
    window unguarded. This gate runs before the window opens (from
    ``redirected``, before the redirected lease is taken) and is fail-closed:
    the shared lease's owner.json must exist and name a live pid that is not
    this process itself, the README's 「取不到租约即退出稍后重跑」 contract.
    Otherwise one reason line goes to stderr and the run exits 2 — this
    file's existing refusal shape (a refusal, never a verdict). The owner
    and liveness reads come from the verified ``common`` module, so the gate
    cannot drift from the lease protocol it polices.
    """
    lock = common.LOCK if lock is None else lock
    previous = common.owner(lock)
    if previous is None:
        print(
            f"#2513 host pre-hold required: {lock / 'owner.json'} is missing "
            "or invalid; exit and rerun under the host hold",
            file=sys.stderr,
        )
        raise SystemExit(2)
    pid = previous['pid']
    if not common.alive(pid):
        print(
            f"#2513 host pre-hold required: owner pid {pid} is not alive; "
            "exit and rerun under the host hold",
            file=sys.stderr,
        )
        raise SystemExit(2)
    if pid == os.getpid():
        print(
            "#2513 host pre-hold required: owner pid is this fixture itself "
            "— the pre-hold must be the host's, not the fixture's own",
            file=sys.stderr,
        )
        raise SystemExit(2)


def _redirect_window_parity_lock(out):
    """#2513 fix-r7 (concurrency P1): redirect this fixture's window lease.

    Returns the restore callable; the caller must invoke it in a ``finally``.
    The pristine ruler module is still executed from verified snapshot bytes
    (never mutated disk code) and ``common.py`` still joins the guarded set,
    so the snapshot entry requirement is the original's, not a copy drifting
    behind negative_process. The fail-closed host pre-hold gate runs from
    ``redirected`` before the redirected lease is taken, so a run without the
    host's serialization refuses instead of opening an unguarded window.
    """
    original = negative_process._parity_mutation_lock

    @contextmanager
    def redirected(window_root, snapshot, timeout):
        if COMMON_SOURCE not in snapshot:
            raise RuntimeError(
                "#166 parity lock requires common.py in guarded sources")
        # The controller normally creates the writer's output directory before
        # spawning it (--stop-check / green do), but a directly-invoked writer
        # must not crash with a bare FileNotFoundError inside the redirect.
        out.mkdir(parents=True, exist_ok=True)
        staged = out / 'pristine-common.py'
        staged.write_bytes(snapshot[COMMON_SOURCE])
        common = _load_pristine_common(staged)
        common.guard = lambda lock: TreeLock(
            lock.with_name(lock.name + ".guard"), timeout=timeout)
        # The window opens only under the host's pre-held shared lease —
        # checked here, before anything is mutated.
        _require_host_lease(common)
        restored = [False]
        with common.parity_lock(
                timeout=timeout, lock=out / "parity-redirect.lock"):
            try:
                yield restored
            finally:
                if not restored[0]:
                    # Same fail-closed marker as the unredirected original:
                    # an unverified restoration must not leave a reusable lease.
                    common.LIVE_GROUPS.add("source-restore-unverified")

    negative_process._parity_mutation_lock = redirected

    def restore():
        negative_process._parity_mutation_lock = original

    return restore


def shared_hold(staged, hold):
    """``--shared-hold``: one bounded external holder of the real shared lease.

    The check's stand-in for the host pre-hold. SIGTERM unwinds through
    ``parity_lock`` so the lease is released before exit (a hard kill would
    leave the cleanupPending owner behind, which nothing may auto-take over).
    """
    common = _load_pristine_common(staged)

    def stop(_signum, _frame):
        raise _HoldStopped('#2513 shared-hold stopped')

    signal.signal(signal.SIGTERM, stop)
    try:
        with common.parity_lock(timeout=SHARED_HOLD_ENTRY_SECONDS):
            time.sleep(hold)
    except _HoldStopped as stopped:
        print(stopped, file=sys.stderr)
    return 0


def _stop_shared_hold(holder):
    """Terminate the holder and prove it released the lease."""
    holder.terminate()
    deadline = time.monotonic() + SHARED_HOLD_EXIT_SECONDS
    while holder.poll() is None and time.monotonic() < deadline:
        time.sleep(0.1)
    if holder.poll() is None:
        _kill_group(holder)
        _collect(holder, 30)
        raise RuntimeError('#2513 the external shared-hold did not stop; its '
                           'lease release is NOT confirmed')
    if holder.returncode != 0:
        raise RuntimeError(f'#2513 the external shared-hold exited '
                           f'{holder.returncode}; its lease release is NOT confirmed')
    _collect(holder, 30)


def window_lock_check(out):
    """``--window-lock-check``: the window redirect, machine-checked.

    With the shared lease held by a live external process — the host's own
    pre-hold when there is one, otherwise this check's bounded holder — the
    guarded entry must acquire its window lock on the redirect path
    (``out/parity-redirect.lock``) within the probe bound instead of queueing
    on the shared lease. This is the condition the kill row pins: revert the
    redirect and the acquisition queues behind the external holder, the
    bounded probe times out, and this check fails at its markered assertion
    (never a 60 s hang and never an unmarked panic).
    """
    staged = out / 'pristine-common.py'
    staged.write_bytes(_head_bytes(root, 'scripts/gateway-parity/common.py', 'HEAD'))
    common = _load_pristine_common(staged)
    holder = None
    previous = common.owner(common.LOCK)
    if previous is None or not common.alive(previous['pid']) or previous['pid'] == os.getpid():
        # No live external holder: stage one (own session, bounded hold).
        holder = subprocess.Popen(
            [sys.executable, '-I', str(Path(__file__).resolve()),
             '--shared-hold', str(staged), str(SHARED_HOLD_SECONDS)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            start_new_session=True)
        deadline = time.monotonic() + SHARED_HOLD_ENTRY_SECONDS
        while True:
            previous = common.owner(common.LOCK)
            if (previous is not None and common.alive(previous['pid'])
                    and previous['pid'] != os.getpid()):
                break
            if holder.poll() is not None:
                raise RuntimeError('#2513 the external shared-hold exited '
                                   'before acquiring the lease')
            if time.monotonic() > deadline:
                _kill_group(holder)
                raise RuntimeError('#2513 the external shared-hold never '
                                   'acquired the lease')
            time.sleep(0.2)
    try:
        _require_host_lease(common)
        restore_window_parity_lock = _redirect_window_parity_lock(out)
        try:
            snapshot = {COMMON_SOURCE: _head_bytes(
                root, 'scripts/gateway-parity/common.py', 'HEAD')}
            started = time.monotonic()
            try:
                with negative_process._parity_mutation_lock(
                        root, snapshot, WINDOW_LOCK_PROBE_SECONDS) as restored:
                    assert (out / 'parity-redirect.lock' / 'owner.json').is_file(), (
                        WINDOW_LOCK_MARKER + '：窗口锁必须在重定向路径 '
                        'out/parity-redirect.lock 上取到（取得的是别处的租约）')
                    # The probe mutates nothing; there is no restoration to
                    # verify, so the window's lease may be released cleanly.
                    restored[0] = True
            except TimeoutError as error:
                raise AssertionError(
                    WINDOW_LOCK_MARKER + '：外部持有共享租约时窗口仍在共享租约上排队'
                    f'（重定向失效），探针在 {WINDOW_LOCK_PROBE_SECONDS}s 内未取到窗口锁'
                ) from error
            elapsed = time.monotonic() - started
            assert elapsed < WINDOW_LOCK_PROBE_SECONDS, (
                WINDOW_LOCK_MARKER + f'：重定向路径取锁阻塞了 {elapsed:.1f}s，'
                f'超过 {WINDOW_LOCK_PROBE_SECONDS}s 探针界')
        finally:
            restore_window_parity_lock()
        assert not (out / 'parity-redirect.lock').exists(), (
            WINDOW_LOCK_MARKER + '：窗口退出后重定向租约必须按协议释放')
    finally:
        if holder is not None:
            _stop_shared_hold(holder)
    return ('KILLED', f'外部持锁下窗口锁在 {WINDOW_LOCK_PROBE_SECONDS}s 界内经'
            ' out/parity-redirect.lock 取到，未在共享租约上排队')


def kill_window_lock(out):
    """``--kill-window-lock``: the one kill row pinning 「该 fixture 必须能取到窗口锁」.

    Mutates this file's own redirect anchor (inside a ``mutating_sources``
    window), runs ``--window-lock-check`` as a subprocess and requires the
    mutant to fail at the row's dedicated marker — a reverted redirect queues
    on the externally-held shared lease and the bounded probe fails with the
    markered assertion, not a timeout and not an unmarked panic. Positive,
    negative and restored runs are all recorded; any non-KILLED row exits
    non-zero.
    """
    self_path = Path(__file__).resolve()
    name, before, after = WINDOW_LOCK_KILL_ROW
    rows = []

    def run_mode(label):
        return run_owned(
            [sys.executable, '-I', str(self_path), '--window-lock-check',
             str(out / label)],
            cwd=root, timeout=180)

    with mutating_sources(root, [self_path], lock_timeout=60) as originals:
        original = originals[self_path].decode()
        status, reason, compiled = 'INVALID', 'not run', False
        try:
            count = original.count(before)
            if count != 1:
                raise RuntimeError(f'#2513 stale anchor {name}: '
                                   f'expected 1, found {count}')
            positive = run_mode(name + '-positive')
            if positive.returncode != 0:
                raise RuntimeError('#2513 positive --window-lock-check failed '
                                   f'(rc={positive.returncode}): '
                                   f'{(positive.stdout + positive.stderr).strip()}')
            changed = original.replace(before, after, 1)
            compile(changed, str(self_path), 'exec')
            compiled = True
            write_source_atomic(root, self_path, changed.encode())
            try:
                result = run_mode(name + '-negative')
                log = result.stdout + result.stderr
                if result.returncode == 0:
                    status, reason = 'SURVIVED', '--window-lock-check passed on the mutant'
                elif WINDOW_LOCK_MARKER in log:
                    status, reason = 'KILLED', ('--window-lock-check failed at the '
                                               'row marker (the redirect queued on '
                                               'the shared lease)')
                else:
                    status, reason = 'INVALID', ('--window-lock-check failed without '
                                                 'the row marker')
            finally:
                write_source_atomic(root, self_path, originals[self_path])
                if self_path.read_bytes() != originals[self_path]:
                    raise RuntimeError(f'#2513 restore failed: {name}')
            restored = run_mode(name + '-restored')
            if restored.returncode != 0:
                status, reason = 'INVALID', (reason + '; restored '
                                             '--window-lock-check did not pass')
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        rows.append({'name': name, 'status': status, 'reason': reason,
                     'compiled': compiled, 'test': '--window-lock-check',
                     'assertion': WINDOW_LOCK_MARKER})
        print(f'{name}: {status}: {reason}', flush=True)
    publish_summary(out, rows)
    return 0 if rows and rows[0]['status'] == 'KILLED' else 1


def writer(out, hold):
    """Independent committed-mode writer: hold the tree lock over one mutant.

    The hold interval is published in epoch milliseconds (``writer-hold.json``,
    written once when the window opens and again just before the lock is
    released). GREEN compares the reader's own acquisition timestamp against
    that interval, so a wait measured against some later lock holder cannot be
    mistaken for this writer (#1662 review r3 P0).

    SIGTERM is acted on only while the mutation window is open, and the handler
    disarms *itself* before raising: the unwind then restores every guarded byte
    with the handler already off, so a second SIGTERM landing inside that
    restore cannot interrupt ``write_bytes`` (#1662 review r3 P0). A SIGKILL (or
    a failing restore) can still leave the mutant on disk; the next
    ``mutating_sources`` run then refuses the dirty tree instead of silently
    mutating a mutated baseline.

    A requested stop exits 0: the handler raises a private ``_Stopped`` and the
    unwind through ``mutating_sources.__exit__`` restores and re-reads every
    guarded byte before the exception reaches the handler here. Every other way
    out -- a failing restore, an unreadable HEAD, a signalling bug -- still exits
    non-zero, which is what lets the controller judge the exit status
    unconditionally (#1662 review r8 P1).
    """
    armed = [True]
    entered = [False]

    def stop(_signum, _frame):
        if armed[0]:
            armed[0] = False
            # A private exception, not ``SystemExit``: the controller must be able
            # to tell "we asked it to stop, it restored and left cleanly" from
            # "it failed on its own". Only the former exits 0 (#1662 r8 P1).
            raise _Stopped(f'{STOPPED_MARKER} before its window closed')

    signal.signal(signal.SIGTERM, stop)
    # #2513 fix-r7 (concurrency P1): this writer runs under the host's
    # pre-held shared lease (the 28999 lane contract), and its window covers
    # the parity-guarded capture.py — without the redirect it would queue on
    # the very lease the host holds for us (60 s TimeoutError, zero evidence).
    restore_window_parity_lock = _redirect_window_parity_lock(out)
    try:
        try:
            with mutating_sources(root, [capture], lock_timeout=WRITER_LOCK_TIMEOUT):
                try:
                    capture.write_text(capture.read_text() + CAPTURE_RESIDUE)
                    # Publish the window atomically: a reader that sees the file must
                    # also see the mutant bytes (#166 tmp-and-rename).
                    opened = epoch_ms()
                    publish_hold(out, opened, None)
                    # "Entered" means the hold start is on disk, so every
                    # "released" record has a published ``start_ms`` behind it and
                    # a stop landing between these two markers still leaves
                    # consistent evidence (#1662 review r7/r8/r9 P3). The
                    # filename marker is published right after; a signal caught
                    # in between publishes a release without the marker, which
                    # controllers read as "window never opened" -- fail-closed,
                    # and unreachable here because both callers signal only after
                    # seeing that marker (r10 P3).
                    entered[0] = True
                    publish(out / 'writer-window-open', 'open\n')
                    time.sleep(hold)
                    # Recorded while the lock is still held: the reader can only
                    # acquire it after this instant.
                    publish_hold(out, opened, epoch_ms())
                    if os.environ.get('PDRS_1662_WRITER_FAIL_AFTER_HOLD'):
                        # Negative-control hook for ``--stop-check``: a writer that
                        # fails on its own must exit non-zero, so the controller's
                        # unconditional exit-status rule can be shown to fire
                        # (#1662 review r8 P1).
                        raise RuntimeError('#1662 injected writer failure after the hold')
                finally:
                    # Disarm on every path out of the window, including an exception:
                    # restoration must never run with the handler still armed.
                    armed[0] = False
        except _Stopped as stopped:
            # ``mutating_sources.__exit__`` restored every guarded byte, re-read
            # it and only then released the lock -- the stop request is fulfilled
            # exactly as designed, so it is not a failure: exit 0.
            print(stopped, file=sys.stderr)
    finally:
        # The redirect is restored first on this unwind: it patched a module
        # global, so leaving it behind would steer every later window in this
        # process (the writer is a subprocess, but the discipline is the same).
        restore_window_parity_lock()
        # ``mutating_sources.__exit__`` has restored every guarded byte and
        # released the lock by now, so this instant is the earliest another
        # process can acquire it. Published on the normal path *and* on the
        # SIGTERM unwind, so each run can show, rather than argue, that the
        # recorded ``end_ms`` lies strictly inside the real hold
        # (#1662 review r7 arbitration evidence). Only once the hold start was
        # published: a SIGTERM during lock acquisition never published a window,
        # and a "released" record there would be fake evidence
        # (#1662 review r7 P3 / r8 P3). ``restored`` is measured here, not
        # assumed: the record itself says whether the bytes are back (#1662 r8 P2).
        if entered[0]:
            publish(out / 'writer-released.json',
                    json.dumps({'released_ms': epoch_ms(),
                                'restored': capture_matches_head()}) + '\n')
    return 0


def _drain(process, out):
    """Best effort: read and close a writer's pipes so nothing leaks.

    ``communicate()`` keeps whatever the writer printed; the point is that the
    streams are closed here rather than at GC time, and that a full pipe cannot
    leave the writer blocked in ``write`` (#1662 review r8 P3). Any failure is
    swallowed: this runs on an error path and must not mask the stop verdict.
    """
    try:
        stdout, stderr = _collect(process, 30)
        exclusive_write(out / 'writer.force-stop.stdout', stdout or '')
        exclusive_write(out / 'writer.force-stop.stderr', stderr or '')
    except Exception:
        pass


def force_stop(process, out, budget=WRITER_EXIT_BUDGET):
    """Recover when ``stop_writer`` itself was interrupted.

    Graceful first (the writer's SIGTERM handler unwinds ``mutating_sources``,
    which restores every guarded byte), then a bounded wait, and only then a
    kill -- a kill during restoration can leave a mutant or a truncated file on
    disk (#1662 review r5/r6). Returns True only when the writer exited *cleanly*
    (status 0), which is what makes the restoration claim true: a requested stop
    restores and then exits 0, so a non-zero status here means it did not, and
    the caller must not claim restoration (#1662 review r8/r9).

    ``budget`` bounds each wait; the self-check passes its own short one so a
    stuck writer cannot hold the interactive path for the production
    ``WRITER_EXIT_BUDGET`` (``HOLD_SECONDS + 120``)
    (#1662 review r12 concurrency/boundary P3).
    """
    if process.poll() is not None:
        _drain(process, out)
        return process.returncode == 0
    try:
        process.terminate()
        process.communicate(timeout=budget)
        # It exited: a requested stop restores and then leaves with status 0.
        return process.returncode == 0
    except subprocess.TimeoutExpired:
        pass
    except Exception:
        # Something other than a timeout (an IPC error, a closed pipe). The
        # writer may still be restoring, so poll it to the same bound instead of
        # killing it outright; ``poll()`` cannot raise the way ``wait()`` can, so
        # this path still ends in a *confirmed* timeout before any kill
        # (#1662 review r6/r7 P0-2).
        deadline = time.monotonic() + budget
        while process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.5)
        if process.poll() is not None:
            _drain(process, out)
            return process.returncode == 0
    try:
        _kill_group(process)
    finally:
        # Always reap, even when the kill itself fails.
        _collect(process, 30)
    return False


def stop_writer(process, out, budget=WRITER_EXIT_BUDGET):
    """Bounded stop: let the writer restore first, then terminate, then kill.

    ``budget`` bounds each of the two waits; the self-check passes a short one so
    the *terminate* branch is the one that runs (#1662 review r10 P2).

    The exit status is judged unconditionally: a writer we terminated ourselves
    restores and then exits 0, so anything else -- a restore that raised, a HEAD
    re-read that failed, a status set before our signal ever landed -- is a real
    failure and must be reported (#1662 review r8 P1). Only a kill we performed
    is exempt, and that raises on its own.
    """
    killed = False
    try:
        stdout, stderr = process.communicate(timeout=budget)
    except subprocess.TimeoutExpired:
        # Graceful first: the handler unwinds mutating_sources and restores.
        process.terminate()
        try:
            stdout, stderr = process.communicate(timeout=budget)
        except subprocess.TimeoutExpired:
            killed = True
            _kill_group(process)
            stdout, stderr = _collect(process, 30)
    exclusive_write(out / 'writer.stdout', stdout or '')
    exclusive_write(out / 'writer.stderr', stderr or '')
    if killed:
        raise RuntimeError('#1662 writer had to be killed; '
                           'source restoration is NOT confirmed')
    if process.returncode != 0:
        raise RuntimeError(f'#1662 writer exited {process.returncode}')


def judge(result):
    log = result.stdout + result.stderr
    evidence = panic_evidence(log, RESIDENT_TEST)
    if (result.returncode != 0 and named_test_failed(log, RESIDENT_TEST)
            and 'test result: FAILED' in log and 'could not compile' not in log
            and SHA_MARKER in evidence):
        return 'KILLED', 'resident test failed at its golden sha assertion'
    if passed(result, RESIDENT_TEST):
        return 'SURVIVED', 'resident test still passed'
    return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing test)'


def observations(log):
    """Lease acquisitions the resident test reported.

    Each row is ``(wait ms, first attempt, first refused attempt, acquisition)``
    in epoch milliseconds; ``blocked`` is 0 when the first attempt succeeded.
    """
    return [(int(waited), int(first), int(blocked), int(at))
            for waited, first, blocked, at in
            re.findall(re.escape(LEASE_MARKER)
                       + r'(\d+) first=(\d+) blocked=(\d+) at=(\d+)', log)]


def green(out):
    """Fixed test under the writer protocol: wait for the lease, read restored bytes.

    GREEN has its own verdicts on purpose. ``judge()`` is the RED direction: a
    golden-sha failure there means the mutant killed the test. Here the same
    failure means the fix stopped working, so reusing ``judge()`` would report
    a broken lease as a passing control (#1662 review P0). Only a pass counts,
    and only when the test itself reports a lease wait.
    """
    prewarm, _elapsed = run_cargo(out, 'prewarm-compile', ['--no-run'])
    if prewarm.returncode != 0:
        return 'INVALID', (f'prewarm compile failed (rc={prewarm.returncode}); the '
                           f'timed run would rebuild and its timing means nothing')
    # Build the #1166 workspace scanner outside the writer's budget: otherwise
    # mutating_sources builds it inside WRITER_ENTRY_BUDGET on a cold target.
    _ensure_workspace_scanner()
    # Field-family drift guard: the marker is written by the Rust test and read
    # here, so a rename on either side would silently turn every GREEN into
    # INVALID (fail-closed, but the control would quietly stop working).
    if LEASE_MARKER not in test_source.read_text():
        return 'INVALID', (f'marker {LEASE_MARKER!r} is no longer in '
                           f'{test_source.name}; the two sides drifted apart')
    # Take the cargo slot *before* the writer opens its window: the slot wait is
    # the only unbounded delay between the window and the test's first attempt
    # at the lock, and HOLD_SECONDS has to cover it (#1662 review r3 P3).
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)
    exclusive_write(out / 'green.slots.log', slot.stdout + slot.stderr)
    process = subprocess.Popen(
        [sys.executable, '-I', str(Path(__file__).resolve()), '--writer', str(out),
         str(HOLD_SECONDS)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        start_new_session=True)
    failure = None
    stop_error = None
    hard_interrupt = None
    result = None
    try:
        deadline = time.monotonic() + WRITER_ENTRY_BUDGET
        while not (out / 'writer-window-open').is_file():
            if process.poll() is not None:
                raise RuntimeError('#1662 writer exited before opening its window')
            if time.monotonic() > deadline:
                raise TimeoutError('#1662 writer never opened its mutation window')
            time.sleep(0.5)
        result, _elapsed = run_cargo(out, 'fixed-test-survives-writer-window',
                                     [RESIDENT_TEST, '--', '--exact', '--nocapture'],
                                     slot_taken=True)
    except BaseException as error:
        failure = error
    finally:
        # ``finally``, not a sequential call: BaseException (KeyboardInterrupt,
        # SystemExit) must stop the writer too, or it outlives us holding the
        # lock with the mutant on disk (#1662 review r3 P1).
        try:
            stop_writer(process, out)
        except BaseException as error:
            stop_error = error
            # Keep the interrupt itself: rewriting ``stop_error`` below must not
            # turn a Ctrl-C into a plain INVALID row (#1662 review r6).
            if not isinstance(error, Exception):
                hard_interrupt = error
            # The stop itself can be interrupted (SIGINT during communicate).
            # Recover gracefully and only kill as a last resort; ``stopped``
            # records whether we can still claim the sources were restored.
            fallback_note = ''
            try:
                stopped = force_stop(process, out)
            except BaseException as stop_failure:
                # The fallback itself failed (kill or reap raised). Keep both
                # errors and, above all, still re-raise an interrupt below
                # instead of losing it here (#1662 review r7 P1).
                stopped = False
                if not isinstance(stop_failure, Exception):
                    if hard_interrupt is None:
                        hard_interrupt = stop_failure
                else:
                    # An ordinary failure of the fallback is not an interrupt to
                    # re-raise, but it must not vanish either: the audit trail is
                    # the only place the operator can see why the stop is
                    # unconfirmed (#1662 review r8 P3).
                    fallback_note = (f'; the stop fallback itself failed: '
                                     f'{type(stop_failure).__name__}: {stop_failure}')
            if not stopped:
                # ``force_stop`` answers by exit status, so this covers both a
                # writer that had to be killed and one that exited non-zero on
                # its own (restore or HEAD verification failed). Say both, or
                # the audit trail claims a kill that never happened (r10 P3).
                stop_error = RuntimeError(
                    f'{type(error).__name__}: {error}; the writer did not stop cleanly '
                    f'(killed, or exited non-zero), source restoration is NOT confirmed'
                    f'{fallback_note}')
    for interrupt in (failure, hard_interrupt):
        if isinstance(interrupt, BaseException) and not isinstance(interrupt, Exception):
            # Interrupts still abort the run; the writer is already stopped above.
            raise interrupt
    if failure is not None or stop_error is not None:
        # Report both: a stop failure must not hide the primary reason (and the
        # other way round) -- the summary is the audit trail (#1662 review r3 P2).
        parts = []
        if failure is not None:
            parts.append(f'{type(failure).__name__}: {failure}')
        if stop_error is not None:
            parts.append(f'stop failed: {type(stop_error).__name__}: {stop_error}')
        return 'INVALID', '; '.join(parts)
    log = result.stdout + result.stderr
    if not passed(result, RESIDENT_TEST):
        return 'INVALID', (
            f'fixed test did not survive the writer window (rc={result.returncode}, '
            f'golden-sha failure={SHA_MARKER in panic_evidence(log, RESIDENT_TEST)}); '
            f'GREEN only accepts the lease wait followed by a pass')
    waits = observations(log)
    if not waits:
        return 'INVALID', (f'resident test reported no "{LEASE_MARKER}" line, so it never '
                           f'waited on the writer lease; this row is no evidence for the fix')
    start_ms, end_ms = read_hold(out)
    if end_ms is None:
        return 'INVALID', ('the writer never published the end of its hold; the acquisition '
                           'timestamp cannot be attributed to it')
    if end_ms - start_ms <= WITNESS_MARGIN_MS:
        # Derived-quantity guard: shortening HOLD_SECONDS (the neighbouring
        # ``STOP_CHECK_HOLD_SECONDS`` is easy to mix up) would make the hold no
        # wider than the margin, so no refusal could ever sit inside
        # ``[start_ms, end_ms - margin]`` and every row would be INVALID with a
        # misleading reason (#1662 review r9 P3 / r10 P3).
        return 'INVALID', (f'the published writer hold spans {end_ms - start_ms} ms, which '
                           f'does not clear the {WITNESS_MARGIN_MS} ms witness margin')
    # The witness is the *refused* attempt: ``try_lock_shared`` returning
    # WouldBlock is a direct observation that an exclusive holder was there, and
    # nothing but a real holder produces it. Requiring that instant to fall
    # inside this writer's hold leaves no room for a scheduler pause (before the
    # call, or after an uncontended acquisition) and none for a later lock
    # holder (#1662 review r4/r5 P0). The upper end needs ``WITNESS_MARGIN_MS``
    # of slack for the truncation bucket, and only the upper end: see the
    # constant (#1662 review r8 P0 / r9 P1).
    waited, first, blocked, at = max(waits, key=lambda row: row[0])
    if blocked == 0:
        return 'INVALID', ('the resident test never observed a refused attempt, so it never '
                           'met an exclusive holder; this row is no evidence for the lease')
    if not (start_ms <= blocked <= end_ms - WITNESS_MARGIN_MS):
        return 'INVALID', (f'resident test was refused at {blocked} ms, outside the writer '
                           f'hold [{start_ms}, {end_ms}] less the {WITNESS_MARGIN_MS} ms '
                           f'witness margin: that refusal was not this writer')
    released = read_released(out)
    if released is None:
        # The writer never published the instant it released the lock, so the
        # hold interval cannot be shown to be conservative. Missing evidence is
        # not evidence (#1662 review r7 P1).
        return 'INVALID', ('the writer never published writer-released.json; the hold '
                           'interval cannot be shown to end before the real release')
    released_ms, restored = released
    if not restored:
        # The record exists but the writer's own restore/HEAD re-read did not
        # complete: the lock was dropped, the bytes may not be back. A record
        # alone is not a restoration claim (#1662 review r8 P2).
        return 'INVALID', (f'writer released the lock at {released_ms} ms but did not '
                           f'confirm that the guarded sources were restored')
    if released_ms <= end_ms:
        # Would mean the writer released the lock at or before the instant it
        # recorded as inside its hold; the whole boundary argument rests on this
        # being false, so refuse to publish a verdict if it ever is not.
        return 'INVALID', (f'writer recorded end_ms={end_ms} but released at '
                           f'{released_ms} ms; the hold interval is not conservative')
    if not first <= blocked <= at:
        return 'INVALID', (f'inconsistent marker: first={first} blocked={blocked} at={at}')
    if waited < MIN_LEASE_WAIT_MS:
        return 'INVALID', (f'resident test waited {waited} ms, under the '
                           f'{MIN_LEASE_WAIT_MS} ms floor: the lease was not contended')
    return 'KILLED', (f'resident test was refused inside the writer hold '
                      f'[{start_ms}, {end_ms}], waited {waited} ms and then passed'
                      f' (lock released at {released_ms} ms)')


def red(out):
    """Lease removed while the source guard holds the lock: the #1662 false FAIL."""
    # #2513 fix-r7 (concurrency P1): the window covers parity-guarded sources
    # and this fixture runs under the host's pre-held shared lease, so the
    # window's lease is the redirected one (see ``_redirect_window_parity_lock``).
    restore_window_parity_lock = _redirect_window_parity_lock(out)
    try:
        with mutating_sources(root, [test_source, capture]):
            original = test_source.read_text()
            if original.count(LEASE_WAIT_ANCHOR) != 1:
                raise RuntimeError(f'#1662 stale lease anchor: '
                                   f'{original.count(LEASE_WAIT_ANCHOR)} matches')
            test_source.write_text(
                original.replace(LEASE_WAIT_ANCHOR, LEASE_WAIT_REMOVED, 1))
            capture.write_text(capture.read_text() + CAPTURE_RESIDUE)
            result, _elapsed = run_cargo(out, 'lease-removed-false-fail',
                                         [RESIDENT_TEST, '--', '--exact'])
    finally:
        restore_window_parity_lock()
    return judge(result)


def stop_check(out):
    """Self-check the writer's exit contract against the real tree lock.

    Two runs, no cargo:

    * ``sigterm`` -- the writer outlives a short ``stop_writer`` budget, so the
      controller takes its own *terminate* branch. A requested stop must restore
      the guarded bytes, record ``restored: true`` and exit 0; that is what lets
      ``stop_writer`` judge the exit status unconditionally.
    * ``own-failure`` -- the writer fails on its own after the hold
      (``PDRS_1662_WRITER_FAIL_AFTER_HOLD``). It must exit non-zero, and
      ``stop_writer`` must *report* it instead of returning cleanly (#1662
      review r8 P1).
    """
    _ensure_workspace_scanner()
    before = capture.read_bytes()
    rows = []
    for label, hold, signal_it in (
            ('sigterm', STOP_CHECK_STOP_HOLD_SECONDS, True),
            ('own-failure', STOP_CHECK_HOLD_SECONDS, False)):
        run_out = out / label
        run_out.mkdir(parents=True, exist_ok=False)
        environment = dict(os.environ)
        if not signal_it:
            environment['PDRS_1662_WRITER_FAIL_AFTER_HOLD'] = '1'
        process = subprocess.Popen(
            [sys.executable, '-I', str(Path(__file__).resolve()), '--writer',
             str(run_out), str(hold)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            start_new_session=True, env=environment)
        reported = None
        cleanup_error = None
        try:
            deadline = time.monotonic() + WRITER_ENTRY_BUDGET
            while not (run_out / 'writer-window-open').is_file():
                if process.poll() is not None:
                    break
                if time.monotonic() > deadline:
                    raise TimeoutError(f'#1662 {label}: writer never opened its window')
                time.sleep(0.05)
            try:
                # The SIGTERM run goes through the controller's own stop path, with
                # a budget far below its hold so the *terminate* branch is the one
                # exercised (a manual terminate() here would leave that branch
                # untested; #1662 review r10 P2).
                stop_writer(process, run_out,
                            budget=STOP_CHECK_STOP_BUDGET if signal_it
                            else STOP_CHECK_EXIT_BUDGET)
            except RuntimeError as error:
                reported = str(error)
        finally:
            if process.poll() is None:
                # Never a bare kill here: an interrupt landing mid-restore would
                # abandon the writer's unwind and leave the mutant on disk. This
                # is the bounded graceful-then-kill path, not SIGKILL first
                # (#1662 review r9 P0).
                try:
                    force_stop(process, run_out, budget=STOP_CHECK_EXIT_BUDGET)
                except BaseException as error:
                    # Recorded, never swallowed: the writer may still hold the
                    # lock with the mutant on disk, and the caller must say so
                    # instead of reporting a clean row (#1662 review r10 P1).
                    cleanup_error = error
        row = {'label': label, 'rc': process.returncode,
               'window': (run_out / 'writer-window-open').is_file(),
               'bytes_back': capture.read_bytes() == before,
               'start_ms': (read_hold(run_out)[0]
                            if (run_out / WRITER_HOLD).is_file() else None),
               'stderr': (run_out / 'writer.stderr').read_text()
               if (run_out / 'writer.stderr').is_file() else '',
               'released': read_released(run_out), 'reported': reported,
               'cleanup': cleanup_error}
        rows.append(row)
        print(f'{label}: rc={row["rc"]} window={row["window"]} '
              f'bytes_back={row["bytes_back"]} released={row["released"]} '
              f'reported={row["reported"]!r}', flush=True)
    for row in rows:
        if row['cleanup'] is not None and not isinstance(row['cleanup'], Exception):
            # A real interrupt (Ctrl-C / SystemExit) still aborts the run, as in
            # ``green()``: the writer may be alive, and turning that into a quiet
            # INVALID row would hide it (#1662 review r10 P1).
            raise row['cleanup']
    sigterm, own_failure = rows
    failed = []
    for row in rows:
        if row['cleanup'] is not None:
            failed.append(f'{row["label"]}: the stop fallback was itself interrupted '
                          f'({type(row["cleanup"]).__name__}: {row["cleanup"]}); the writer '
                          f'may still hold the lock and its restore is NOT confirmed')
    if not sigterm['window']:
        failed.append('the SIGTERM run never published its window')
    if sigterm['reported'] is not None:
        failed.append(f'stop_writer rejected a requested stop: {sigterm["reported"]!r}')
    if sigterm['rc'] != 0:
        failed.append(f'a requested stop exited {sigterm["rc"]}, not 0')
    # The writer prints the ``_Stopped`` text on the graceful path only. Without
    # it, "rc 0 + restored" cannot tell a handled stop from a SIGTERM that was
    # ignored and simply ran out its hold (#1662 review r9 P2).
    if STOPPED_MARKER not in sigterm['stderr']:
        failed.append(f'the SIGTERM run did not take the stop path: '
                      f'{STOPPED_MARKER!r} missing from its stderr')
    if not sigterm['bytes_back']:
        failed.append('a requested stop left the guarded bytes mutated')
    if not (sigterm['released'] and sigterm['released'][1]):
        failed.append(f'a requested stop did not record restored: true '
                      f'(record={sigterm["released"]})')
    # The terminate branch is only *proven* if the writer stopped well before its
    # hold would have ended on its own (#1662 review r10 P2).
    if sigterm['released'] is None or sigterm['start_ms'] is None:
        failed.append('the SIGTERM run published no hold interval to time the stop against')
    else:
        stopped_after_ms = sigterm['released'][0] - sigterm['start_ms']
        if not 0 < stopped_after_ms < STOP_CHECK_STOP_HOLD_SECONDS * 1000:
            failed.append(f'the requested stop took {stopped_after_ms} ms, which does not '
                          f'land inside the {STOP_CHECK_STOP_HOLD_SECONDS} s hold')
    if not own_failure['window']:
        failed.append('the own-failure run never opened its window')
    if own_failure['rc'] is None:
        # Never reaped: no exit status exists, so "!= 0" and the text match below
        # would both be comparing nothing (#1662 review r10 P3).
        failed.append('the own-failure writer never exited; there is no status to judge')
    elif own_failure['rc'] == 0:
        failed.append('a writer that failed on its own exited 0')
    elif own_failure['reported'] is None:
        failed.append('stop_writer accepted a non-zero writer exit')
    elif not re.search(rf'exited {own_failure["rc"]}\b', own_failure['reported']):
        # Exact status, not a substring: ``exited 1`` must not be satisfied by
        # ``exited 10`` (#1662 review r10 P3). One chain, not three overlapping
        # ``if``s, so an unreaped writer cannot also be described as "accepted a
        # non-zero exit" -- there is no status to accept (#1662 review r12
        # simplify P3 / concurrency P3).
        failed.append(f'the reported failure does not name the exit status: '
                      f'{own_failure["reported"]!r}')
    if not own_failure['bytes_back']:
        # The failure restores through the same unwind; bytes left mutated mean
        # the restore itself broke (#1662 review r9 P2).
        failed.append('the own-failure run left the guarded bytes mutated')
    return ('KILLED' if not failed else 'INVALID'), (
        '; '.join(failed) if failed else
        f'requested stop exited 0 and restored, own failure exited '
        f'{own_failure["rc"]} and was reported by stop_writer')


def main():
    arguments = sys.argv[1:]
    if arguments[:1] == ['--writer']:
        if len(arguments) != 3:
            raise SystemExit('#1662 writer usage: --writer OUT HOLD_SECONDS')
        return writer(Path(arguments[1]), float(arguments[2]))
    if arguments[:1] == ['--shared-hold']:
        if len(arguments) != 3:
            raise SystemExit('#2513 shared-hold usage: --shared-hold STAGED_COMMON HOLD_SECONDS')
        return shared_hold(Path(arguments[1]), float(arguments[2]))
    if arguments[:1] == ['--window-lock-check']:
        if len(arguments) != 2:
            raise SystemExit('#2513 window-lock-check usage: --window-lock-check NEW_OUTPUT_DIR')
        out = new_output_directory(arguments[1])
        status, reason = 'INVALID', 'not run'
        try:
            status, reason = window_lock_check(out)
        except Exception as error:
            # Same shape as stop-check: a crash (including the kill row's
            # markered AssertionError) still leaves an audit row.
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        print(f'window-lock-check: {status}: {reason}', flush=True)
        publish_summary(out, [{'name': 'window-lock-check',
                               'status': status, 'reason': reason}])
        return 0 if status == 'KILLED' else 1
    if arguments[:1] == ['--kill-window-lock']:
        if len(arguments) != 2:
            raise SystemExit('#2513 kill-window-lock usage: --kill-window-lock NEW_OUTPUT_DIR')
        return kill_window_lock(new_output_directory(arguments[1]))
    if arguments[:1] == ['--stop-check']:
        if len(arguments) != 2:
            raise SystemExit('#1662 stop-check usage: --stop-check NEW_OUTPUT_DIR')
        out = new_output_directory(arguments[1])
        status, reason = 'INVALID', 'not run'
        try:
            status, reason = stop_check(out)
        except Exception as error:
            # Same shape as green/red: a crash still leaves an audit row
            # (#1662 review r9 P3).
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        print(f'writer exit contract: {status}: {reason}', flush=True)
        publish_summary(out, [{'name': 'writer-exit-contract',
                               'status': status, 'reason': reason}])
        return 0 if status == 'KILLED' else 1
    if len(arguments) != 1 or arguments[0].startswith('-'):
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT_DIR | '
                          '--stop-check NEW_OUTPUT_DIR | '
                          '--window-lock-check NEW_OUTPUT_DIR | '
                          '--kill-window-lock NEW_OUTPUT_DIR | '
                          '--writer OUT HOLD_SECONDS | '
                          '--shared-hold STAGED_COMMON HOLD_SECONDS')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1662 CARGO_TARGET_DIR must be supplied by the caller')
    out = new_output_directory(arguments[0])
    rows = []
    # The exit contract is part of the control, not an optional extra: it is the
    # only thing that shows a killed writer is reported rather than swallowed
    # (#1662 review r9 P2).
    for name, phase in (('fixed-test-survives-writer-window', green),
                         ('lease-removed-false-fail', red),
                         ('writer-exit-contract', stop_check)):
        status, reason = 'INVALID', 'not run'
        try:
            status, reason = phase(out)
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        rows.append({'name': name, 'status': status, 'reason': reason})
        print(f'{name}: {status}: {reason}', flush=True)
    publish_summary(out, rows)
    counts = {state: sum(row['status'] == state for row in rows)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(f'#1662 {counts}', flush=True)
    return 0 if all(row['status'] == 'KILLED' for row in rows) else 1


if __name__ == '__main__':
    sys.exit(main())
