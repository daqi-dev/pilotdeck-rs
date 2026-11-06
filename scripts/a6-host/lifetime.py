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
    host is gone: write ``.orphan``, say why on stderr, exit non-zero. Covers
    the mbp2 scenario within one poll interval.
  * ``bounded_close_wait`` -- the `close-*` waits get a hard ceiling after EOF
    instead of an unbounded ``signal.pause()``.
  * ``MAX_LIFETIME`` -- a backstop for anything the two above miss (e.g. a
    parent that dies before ``install`` runs). Deliberately not a "kill every
    ppid=1 process" sweep: that would hit processes this fixture does not own.

Nothing here sends signals to other processes; every exit is the fixture
terminating itself.
"""
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
EXIT_CLOSE_WAIT = 72


def _report(receipt, suffix, message):
    """Record why we are giving up: stderr for the host ring buffer, a receipt
    file for the test, so a timeout can never look like a quiet success."""
    sys.stderr.write(message + '\n')
    sys.stderr.flush()
    if receipt is not None:
        try:
            receipt.with_suffix(suffix).write_text(message + '\n')
        except OSError:
            pass


def install(receipt, mode, max_lifetime=None):
    """Start the watchdog. Call once, as early as possible, in every mode."""
    if max_lifetime is None:
        max_lifetime = float(os.environ.get('PDRS_A6_FIXTURE_MAX_LIFETIME',
                                            DEFAULT_MAX_LIFETIME))
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
                os._exit(EXIT_PARENT_DIED)
            if time.monotonic() - started > max_lifetime:
                _report(receipt, '.lifetime',
                        '{}: lifetime exceeded {:.0f}s; exiting (#1779)'
                        .format(mode, max_lifetime))
                os._exit(EXIT_MAX_LIFETIME)

    threading.Thread(target=watch, name='a6-parent-watch', daemon=True).start()


def bounded_close_wait(receipt, mode, seconds=None):
    """Block for a terminating signal, but never longer than `seconds`.

    Used by the modes whose contract *is* "stay alive after stdin EOF until the
    host signals". The SIGTERM handler installed by those modes still exits
    first in every healthy run; this only bounds the broken-host case, and it
    fails loudly (AssertionError, non-zero exit) rather than returning quietly.
    """
    if seconds is None:
        seconds = float(os.environ.get('PDRS_A6_CLOSE_WAIT_LIMIT', CLOSE_WAIT_LIMIT))

    def on_timeout(_signum, _frame):
        _report(receipt, '.timeout',
                '{}: no terminating signal within {:.0f}s after stdin EOF; '
                'the host never escalated to SIGTERM (#1779)'.format(mode, seconds))
        raise AssertionError(
            '{}: no terminating signal within {:.0f}s after stdin EOF'.format(mode, seconds))

    signal.signal(signal.SIGALRM, on_timeout)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        while True:
            signal.pause()
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
