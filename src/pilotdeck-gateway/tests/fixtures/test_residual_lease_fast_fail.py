#!/usr/bin/env python3
# anchor-selfcheck: unsupported static residual-lease probe, no mutation definitions
"""#2929 fix-r3: a residual parity lease must fail the acquire fast.

A published owner (``owner.json`` present, ``cleanupPending`` set) is never
aged out by ``MAX_AGE`` and never taken over -- not even when its pid is
dead.  Before fix-r3 both takeover-capable acquirers
(``scripts/gateway-parity/common.py`` and ``scripts/parity/round.py``)
waited their whole acquire budget behind such a lease, which is guaranteed
zero progress: nobody but a human following the 「锁被保留时怎么解」
section of ``scripts/cli-parity/README.md`` can recover it.

These probes pin the fix from the outside, with private lock directories
only (never ``/tmp/pdrs-parity.lock``):

* both takeover-capable acquirers raise ``ResidualLeaseError`` (a
  ``TimeoutError`` subclass, so existing catchers keep classifying it as a
  lock-wait failure) within seconds, with pid/age/cleanupPending
  diagnostics and the manual-recovery pointer, and without deleting the
  residual lease (a fast-fail that also removed the lock would race a live
  recovery exactly like the hand-deletion hazard the README documents);
* ``common.parity_lock`` still waits the full budget behind a *live*
  owner -- the fast-fail must not loosen the budget semantics #1528
  established;
* the parity-lock-release runner's non-shared ``parity_lease`` fails fast
  when the lock directory simply exists (it never waits the family out).

``scripts/parity/round.py`` refuses to import without ``-I``; run this file
as CI does: ``python3 -I <this file>``.  ``round.py`` also pulls in
``scripts/parity/g03_compare.py`` at import time, whose top level resolves
the PilotDeck baseline and even loads ``tools/agent-loop-parity/trace.py``
from it; CI checkouts have no ``resources/PilotDeck`` (the baseline stays
repo-external), so ``load_round`` below imports it under a throwaway
baseline stub -- none of these lock-semantics probes call any comparison
helper, and the stub keeps the real ``parity_lock`` semantics under test.
"""

import contextlib
import faulthandler
import importlib.util
import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent
REPO_ROOT = FIXTURES.parents[3]

COMMON = REPO_ROOT / 'scripts/gateway-parity/common.py'
ROUND = REPO_ROOT / 'scripts/parity/round.py'
LOCK_RELEASE_RUNNER = FIXTURES / 'parity-lock-release/negative_controls.py'


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# #2929 fix-r7 (CI run 43548179459): g03_compare.py's module top level runs
# BASELINE = baseline_path() AND T = load_original_trace(), the latter reading
# <baseline>/tools/agent-loop-parity/trace.py -- an empty stub directory is
# NOT enough.  CI has no resources/PilotDeck and no PDRS_BASELINE, which made
# this file fail at import instead of running the probes.  A stub directory
# with an import-only trace.py lets round.py load everywhere; the probes only
# exercise round.parity_lock, which never touches G.*/T.* comparison state.
STUB_TRACE_PY = (
    '# import-only stub written by test_residual_lease_fast_fail.py;\n'
    '# the lock-semantics probes never call any T.* helper.\n'
)


def load_round():
    """Load round.py under a scoped throwaway PDRS_BASELINE stub.

    The env var is set only around the exec so the other probes (common.py,
    the lock-release runner) keep loading exactly as before, and a developer
    PDRS_BASELINE in the environment is restored afterwards.
    """
    with tempfile.TemporaryDirectory() as stub:
        tools = Path(stub) / 'tools' / 'agent-loop-parity'
        tools.mkdir(parents=True)
        (tools / 'trace.py').write_text(STUB_TRACE_PY)
        saved = os.environ.get('PDRS_BASELINE')
        os.environ['PDRS_BASELINE'] = stub
        try:
            return load('residual_round', ROUND)
        finally:
            if saved is None:
                os.environ.pop('PDRS_BASELINE', None)
            else:
                os.environ['PDRS_BASELINE'] = saved


def dead_pid():
    """A pid that is dead now and cannot be reanimated mid-probe.

    A just-reaped child's pid sits in the host's reuse window (#2929 fix-r4,
    MM concurrency): once the host reuses it, alive(pid) is true again, the
    fast-fail never fires and the probe waits its whole budget out -- a pure
    apparatus false red.  A pid above every host's allocation range (macOS
    pid_max 99999, Linux pid_max at most 4194304) is never handed out, so it
    stays dead; the precondition is asserted anyway so a host that does
    allocate it fails the probe loudly instead of hanging on a wait.
    """
    pid = 2 ** 31 - 1
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return pid
    raise AssertionError(
        f'pid {pid} is unexpectedly signalable; pick another dead pid')


def publish_residual(lock, pid, age=3600.0, groups=('suite-x',)):
    lock.mkdir()
    (lock / 'owner.json').write_text(json.dumps({
        'pid': pid,
        'timestamp': time.time() - age,
        'token': 'probe-token',
        'cleanupPending': sorted(groups),
    }))
    return lock / 'owner.json'


class ResidualLeaseFastFailTests(unittest.TestCase):

    def test_common_fails_fast_on_dead_cleanup_pending_owner(self):
        common = load('residual_common', COMMON)
        with tempfile.TemporaryDirectory() as tmp:
            lock = Path(tmp) / 'pdrs-parity.lock'
            owner = publish_residual(lock, dead_pid())
            before = owner.read_bytes()
            started = time.monotonic()
            with self.assertRaises(common.ResidualLeaseError) as caught, \
                    contextlib.redirect_stderr(sys.stderr):
                with common.parity_lock(lock=lock, timeout=60):
                    pass  # must never be reached
            elapsed = time.monotonic() - started
            message = str(caught.exception)
            self.assertLess(elapsed, 10.0,
                            f'fast-fail took {elapsed:.1f}s behind a residual lease')
            self.assertIn('residual lease', message)
            self.assertIn('can never be acquired automatically', message)
            self.assertIn('锁被保留时怎么解', message)
            self.assertIn('cleanupPending', message)
            # Fail fast, not steal fast: the residual lease is left for the
            # human recovery procedure the message points at.
            self.assertEqual(owner.read_bytes(), before,
                             'fast-fail must not delete or rewrite the residual lease')

    def test_common_residual_error_is_a_timeout_error(self):
        # Existing catchers (_guard policies -> BLOCKED, runner classifiers ->
        # INVALID) key on TimeoutError; reclassifying a residual as an unknown
        # crash would change failure shapes across the fleet.
        common = load('residual_common', COMMON)
        self.assertTrue(issubclass(common.ResidualLeaseError, TimeoutError))

    def test_common_still_waits_the_budget_behind_a_live_owner(self):
        common = load('residual_common', COMMON)
        with tempfile.TemporaryDirectory() as tmp:
            lock = Path(tmp) / 'pdrs-parity.lock'
            # pid 1 (launchd) is alive on every target host; cleanupPending
            # makes it non-takeoverable, so the wait must run its course.
            publish_residual(lock, 1, groups=('suite-y',))
            started = time.monotonic()
            with self.assertRaises(TimeoutError) as caught:
                with common.parity_lock(lock=lock, timeout=1.5):
                    pass
            elapsed = time.monotonic() - started
            self.assertGreaterEqual(elapsed, 1.2,
                                    f'live owner waited only {elapsed:.1f}s of the budget')
            self.assertNotIsInstance(caught.exception, common.ResidualLeaseError)

    def test_round_fails_fast_on_dead_cleanup_pending_owner(self):
        if not sys.flags.isolated:
            self.fail('run with -I: python3 -I '
                      + str(Path(__file__).resolve()))
        with tempfile.TemporaryDirectory() as tmp:
            lock = Path(tmp) / 'pdrs-parity.lock'
            owner = publish_residual(lock, dead_pid())
            before = owner.read_bytes()
            started = time.monotonic()
            # round.parity_lock has no timeout parameter: a reverted
            # fast-fail would poll forever, so the probe hangs only until this
            # watchdog dumps every traceback and exits nonzero (#2929 fix-r4,
            # DS boundary#2) -- a bounded loud failure, never a job-level
            # timeout.
            faulthandler.dump_traceback_later(15, exit=True)
            try:
                with self.assertRaises(TimeoutError) as caught:
                    with load_round().parity_lock(lock=lock):
                        pass
            finally:
                faulthandler.cancel_dump_traceback_later()
            elapsed = time.monotonic() - started
            message = str(caught.exception)
            self.assertLess(elapsed, 10.0,
                            f'fast-fail took {elapsed:.1f}s behind a residual lease')
            self.assertIn('residual lease', message)
            self.assertIn('锁被保留时怎么解', message)
            self.assertEqual(owner.read_bytes(), before,
                             'fast-fail must not delete or rewrite the residual lease')

    def test_lock_release_runner_lease_fails_fast_when_directory_exists(self):
        # The non-shared branch documented at negative_controls.py:523 does
        # NOT wait the family out: it only polls the .guard flock, and the
        # first lease-directory check after it fails immediately.
        runner = load('residual_lock_release', LOCK_RELEASE_RUNNER)
        with tempfile.TemporaryDirectory() as tmp:
            lock = Path(tmp) / 'pdrs-parity.lock'
            owner = publish_residual(lock, dead_pid())
            before = owner.read_bytes()
            started = time.monotonic()
            # parity_lease's default budget is the whole family
            # lease window: a reverted fail-fast would wait it out, so the
            # same watchdog bounds this probe too (#2929 fix-r4, DS
            # boundary#2).
            faulthandler.dump_traceback_later(15, exit=True)
            try:
                with self.assertRaises(TimeoutError) as caught:
                    with runner.parity_lease(lock=lock):
                        pass
            finally:
                faulthandler.cancel_dump_traceback_later()
            elapsed = time.monotonic() - started
            self.assertLess(elapsed, 10.0,
                            f'fail-fast took {elapsed:.1f}s behind an existing lease')
            self.assertIn('#209 parity lock held', str(caught.exception))
            self.assertEqual(owner.read_bytes(), before,
                             'fail-fast must not delete or rewrite the held lease')


if __name__ == '__main__':
    unittest.main(verbosity=2)
