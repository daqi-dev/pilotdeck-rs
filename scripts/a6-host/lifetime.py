#!/usr/bin/env python3
"""Parent-death and lifetime watchdog shared by the a6-host fixtures (#1779).

Why this exists
---------------
``Connection::spawn`` puts every peer in its own process group
(``src/pilotdeck-gateway/src/host/connection.rs:65``), so a worker-side
``killpg`` never reaches it. When the test process is SIGKILLed the peer is
reparented to init, and a mode that waits for SIGTERM then waits forever:
mbp2 had 20 such orphans, the oldest at 12 h 21 m (#1779).

The peer cannot tell "the host closed my stdin" from "the host died" by looking
at its own pipes -- both are EOF. That is exactly why ``close-term-cleanup``
must stay alive past EOF (its receipt asserts SIGTERM *after* EOF). So the one
thing that does differ, the parent process itself, is what gets watched here,
and every unbounded wait additionally gets a ceiling.

Three layers, cheapest first:
  * ``install`` -- a daemon thread polls ``os.getppid()``. Reparenting means the
    host is gone: write ``.orphan``, say why on stderr, kill the descendants
    registered through ``track`` (the ``grandchild-*`` sleepers), exit 70.
    Covers the mbp2 scenario within one poll interval.
  * ``bounded_close_wait`` -- the `close-*` waits get a hard ceiling after EOF
    instead of an unbounded ``signal.pause()``.
  * the lifetime ceiling (``PDRS_A6_FIXTURE_MAX_LIFETIME``) -- a backstop for
    anything the two above miss (e.g. a parent that dies before ``install``
    runs): write ``.lifetime`` and exit 71. Deliberately not a "kill every
    ppid=1 process" sweep: that would hit processes this fixture does not own.

What is *not* covered: a descendant nobody handed to ``track``. Every fixture
mode that spawns one registers it inside ``spawning()``, so today there is none;
a future mode has to opt in. ``os.killpg(0, SIGKILL)`` is deliberately not used
as a catch-all: the fixture is spawned as its own group leader
(``connection.rs:65``), so a group kill would SIGKILL this process too, and the
exit code is then no longer ours to choose.

Exit codes: 71 is asserted in ``host/tests.rs`` (the test is the peer's parent
there). 70 is *not* observable by the tests -- on the orphan path the parent is
gone by definition -- so ``.orphan`` is the receipt that proves this path ran,
and no test claims the number.

Nothing here sends signals to processes this fixture does not own: the watchdog
only kills itself, its registered descendants, and this process.
"""
import contextlib
import math
import os
import signal
import sys
import threading
import time

# 0.1 s keeps the orphan window far below any test's patience while costing one
# syscall per poll. A killed host is detected within this interval.
PARENT_POLL_SECONDS = 0.1

# Backstop for a peer whose parent cannot be observed. Ten minutes is an order
# of magnitude above the longest intentional wait in these fixtures (60 s in
# the grandchild/callback sleeps, 30 s in the host-cancel release waits), so no
# healthy test can reach it, while a leaked peer can never outlive a session.
DEFAULT_MAX_LIFETIME = 600.0

# The host escalates EOF -> SIGTERM -> SIGKILL inside `close_timeout`, which
# the tests set to 2-5 s (protocol_limits). 30 s is 6x that headroom: a peer
# still waiting after it is looking at a broken host, not a slow one.
CLOSE_WAIT_LIMIT = 30.0

EXIT_PARENT_DIED = 70
EXIT_MAX_LIFETIME = 71

# Descendants this fixture owns (see ``track``). Deliberately a registry, not a
# process-table scan.
_TRACKED = []
# A lock alone does not close the fork -> ``track`` window: the watchdog can
# publish ``_EXITING`` and reach ``os._exit`` before the spawning thread is
# scheduled again. So a spawn is announced *before* ``Popen`` forks
# (``spawning()``), and the watchdog waits for ``_SPAWNING`` to drain before it
# snapshots the registry. A ``track`` that runs after the watchdog gave up still
# kills its own process, which covers the spawn-after-exit case.
_COND = threading.Condition()
_SPAWNING = 0
_EXITING = False
# Bounded so a spawning thread that never returns cannot hang the exit; Popen
# returns in milliseconds, so this only trips on a wedged interpreter.
SPAWN_SETTLE_LIMIT = 1.0


@contextlib.contextmanager
def spawning():
    """Announce that a descendant is about to be forked and registered.

    Wrap both the ``Popen`` and the ``track`` call: the watchdog may fire while
    this block runs, and it waits for the block to finish before killing, so the
    descendant is always registered by the time the registry is read.
    """
    global _SPAWNING
    with _COND:
        _SPAWNING += 1
    try:
        yield
    finally:
        with _COND:
            _SPAWNING -= 1
            _COND.notify_all()


def track(process):
    """Register a descendant so it dies with us on the orphan/lifetime paths."""
    global _EXITING
    with _COND:
        _TRACKED.append(process)
        exiting = _EXITING
    if exiting:
        _kill_one(process)


def _kill_one(process):
    try:
        process.kill()
    except Exception:
        pass


def _knob(name, default):
    """Read a ceiling from the environment without ever disabling it.

    A blank, NaN, zero or negative override must not turn a bounded wait back
    into an unbounded one: ``setitimer(ITIMER_REAL, 0)`` *cancels* the timer
    and ``elapsed > nan`` is always false. Anything unusable keeps the default
    rather than silently removing the ceiling this module exists to add.
    """
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        value = float(raw)
    except ValueError:
        return default
    if not math.isfinite(value) or value <= 0:
        return default
    return value


def _report(receipt, suffix, message):
    """Record why we are giving up: stderr for the host ring buffer, a receipt
    file for the test, so a timeout can never look like a quiet success.

    Never raises. The host owns our stderr pipe (``connection.rs:71``
    ``Stdio::piped()``), so by the time a dead host is detected that pipe has
    no reader and ``sys.stderr.write`` raises ``BrokenPipeError``. Letting that
    escape would kill the watchdog thread before it exits the process -- the
    failure would be silent, which is the one outcome this module forbids.
    """
    try:
        sys.stderr.write(message + '\n')
        sys.stderr.flush()
    except Exception:
        pass
    if receipt is not None:
        try:
            receipt.with_suffix(suffix).write_text(message + '\n')
        except Exception:
            pass


def _kill_tracked():
    """Kill the descendants this fixture owns, so 父死子死 covers them too.

    Only pids handed to ``track`` are touched -- never a scan of the process
    table, which would reach processes this fixture does not own. Called from
    the watchdog thread; ``Popen.kill`` is safe there.

    Publishes ``_EXITING`` and first waits (bounded) for an in-flight
    ``spawning()`` block to finish, so a descendant that was forked but not yet
    registered is in the snapshot instead of being left behind.
    """
    global _EXITING
    with _COND:
        _EXITING = True
        _COND.wait_for(lambda: _SPAWNING == 0, timeout=SPAWN_SETTLE_LIMIT)
        tracked = list(_TRACKED)
    for process in tracked:
        _kill_one(process)


def install(receipt, mode):
    """Start the watchdog. Call once, as early as possible, in every mode."""
    max_lifetime = _knob('PDRS_A6_FIXTURE_MAX_LIFETIME', DEFAULT_MAX_LIFETIME)
    parent = os.getppid()
    started = time.monotonic()

    def watch():
        while True:
            time.sleep(PARENT_POLL_SECONDS)
            now = os.getppid()
            if now != parent:
                _report(receipt, '.orphan',
                        '{}: parent {} is gone (reparented to {}); exiting instead of '
                        'waiting for a signal that will never come (#1779)'
                        .format(mode, parent, now))
                _kill_tracked()
                os._exit(EXIT_PARENT_DIED)
            if time.monotonic() - started > max_lifetime:
                _report(receipt, '.lifetime',
                        '{}: lifetime exceeded {:.0f}s; exiting (#1779)'
                        .format(mode, max_lifetime))
                _kill_tracked()
                os._exit(EXIT_MAX_LIFETIME)

    threading.Thread(target=watch, name='a6-parent-watch', daemon=True).start()


def bounded_close_wait(receipt, mode):
    """Block for a terminating signal, but never longer than the ceiling.

    Used by the modes whose contract *is* "stay alive after stdin EOF until the
    host signals". The SIGTERM handler installed by those modes still exits
    first in every healthy run; this only bounds the broken-host case, and it
    fails loudly (AssertionError, non-zero exit) rather than returning quietly.
    """
    seconds = _knob('PDRS_A6_CLOSE_WAIT_LIMIT', CLOSE_WAIT_LIMIT)

    def on_timeout(_signum, _frame):
        # close-hang keeps running after SIGTERM on purpose, so the missing
        # step there is the SIGKILL escalation, not the SIGTERM itself.
        if receipt is not None and receipt.with_suffix('.term').exists():
            why = 'SIGTERM arrived but no SIGKILL within {:.0f}s'.format(seconds)
        else:
            why = 'no terminating signal within {:.0f}s after stdin EOF'.format(seconds)
        _report(receipt, '.timeout',
                '{}: {}; the host stopped driving the close (#1779)'.format(mode, why))
        raise AssertionError('{}: {}'.format(mode, why))

    signal.signal(signal.SIGALRM, on_timeout)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        while True:
            signal.pause()
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
