# anchor-selfcheck: unsupported positive-only lock-release controls; the source mutation table lives in negative_controls.py
#!/usr/bin/env python3
"""#209 E1 positive controls for the gated shared-lease release.

Offline only: each test uses a temporary directory as the lease root, so the
real /tmp/pdrs-parity.lock is never touched.  ``run_fixture`` is driven with a
fake child; no fixture, sidecar or full calibration is launched.

The source-mutation controls live in ``negative_controls.py`` and run after the
coordinator commits the patch: ``mutating_sources`` refuses a working tree that
differs from HEAD.  This file is the restored positive they bind to.
"""
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import tempfile

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-e1-lock-pyc-')
sys.dont_write_bytecode = True

import contextlib
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import signal
import subprocess
import unittest
from unittest.mock import MagicMock, patch

ROOT = Path(__file__).resolve().parents[5]
SESSION = ROOT / 'scripts/gateway-parity/session_domain.py'
DEFERRED = {signal.SIGINT, signal.SIGTERM}


def _load(name, path):
    """Load the apparatus entry by explicit path; never resolve via sys.path."""
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


S = _load('session_domain_lock_release', SESSION)
C = S.common


class FakeChild:
    """The small Popen surface run_fixture's cleanup path actually uses."""

    def __init__(self, pid, returncode=0):
        self.pid = pid
        self.returncode = returncode
        self.wait_timeouts = 0

    def poll(self):
        return self.returncode

    def wait(self, timeout=None):
        if self.returncode is None:
            # A successful bounded wait reaps the leader and records its code.
            self.returncode = 0
        return self.returncode


class LockReleaseControls(unittest.TestCase):
    PID = 505050

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-e1-lock-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.lock = self.root / 'parity.lock'
        C.LIVE_GROUPS.clear()
        self.addCleanup(C.LIVE_GROUPS.clear)
        self.masks = {}
        self.registered_at_cleanup = None
        self.run_number = 0

    def _owner(self):
        """Read the retained lease record without an un-messaged failure point."""
        path = self.lock / 'owner.json'
        self.assertTrue(path.is_file(),
                        '#209 a retained lease must publish owner.json')
        try:
            return json.loads(path.read_text())
        except ValueError as error:
            self.fail(f'#209 retained lease owner.json must be valid JSON: {error}')

    def _run(self, probe, *, wait_states=('EXITED',), child=None,
             kill_leader=None, acquire_lock=False, fixture_launch=False,
             read_error=None):
        """Drive the real run_fixture cleanup gate with a fake child."""
        child = child if child is not None else FakeChild(self.PID)
        self.run_number += 1
        folder = self.root / f'run-{self.run_number}'
        folder.mkdir()
        if fixture_launch:
            cells = folder / 'six-cells'
            cells.mkdir()
            (cells / 'launch.json').write_text('{}')
        states = iter(wait_states)

        def wait_state(pid, timeout):
            return next(states, wait_states[-1])

        def spawn(*args, **kwargs):
            self.masks['spawn'] = signal.pthread_sigmask(signal.SIG_BLOCK, set())
            return child

        def probe_group(actual):
            self.masks['cleanup'] = signal.pthread_sigmask(signal.SIG_BLOCK, set())
            self.registered_at_cleanup = self.PID in C.LIVE_GROUPS
            if isinstance(probe, BaseException):
                raise probe
            return dict(probe)

        stack = contextlib.ExitStack()
        stack.enter_context(patch.object(S.subprocess, 'Popen', side_effect=spawn))
        stack.enter_context(patch.object(S, 'wait_exit_unreaped', side_effect=wait_state))
        stack.enter_context(patch.object(S, 'probe_process_group', side_effect=probe_group))
        stack.enter_context(patch.object(S.os, 'killpg', return_value=None))
        stack.enter_context(patch.object(S.os, 'kill', return_value=None))
        if kill_leader is not None:
            stack.enter_context(patch.object(S.HeldProcessGroup, 'kill_leader',
                                             return_value=kill_leader))
        if read_error is not None:
            stack.enter_context(patch.object(S, 'read', side_effect=read_error))
        try:
            if acquire_lock:
                with C.parity_lock(timeout=2, lock=self.lock):
                    return S.run_fixture(folder, ROOT, command=['stub'])
            return S.run_fixture(folder, ROOT, command=['stub'])
        finally:
            stack.close()

    def test_verified_cleanup_releases_lock(self):
        audit = self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID},
                          acquire_lock=True)
        self.assertNotIn(self.PID, C.LIVE_GROUPS,
                         '#209 verified cleanup must discard the owned group')
        self.assertIs(audit.get('allStopped'), True,
                      '#209 verified cleanup must report allStopped')
        self.assertEqual(audit.get('ownedGroupsRemaining'), [],
                         '#209 verified cleanup must leave no owned group')
        self.assertFalse(self.lock.exists(),
                         '#209 verified cleanup must release the shared lock')

    def test_unverified_cleanup_retains_lock(self):
        audit = self._run({'status': S.CLEANUP_GROUP_NOT_EMPTY, 'pgid': self.PID},
                          acquire_lock=True)
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 unverified cleanup must keep the owned group registered')
        self.assertIs(audit.get('allStopped'), False,
                      '#209 unverified cleanup must not report allStopped')
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_GROUP_NOT_EMPTY,
                         '#209 the group finding must stay visible')
        self.assertTrue(self.lock.exists(),
                        '#209 unverified cleanup must retain the shared lock')
        owner = self._owner()
        self.assertEqual(owner.get('cleanupPending'), [self.PID],
                         '#209 retained groups must be explicit in owner.json')

    def test_takeover_pending_refused(self):
        self.lock.mkdir()
        retained = {'pid': 424242, 'timestamp': 0, 'token': 'retained',
                    'cleanupPending': [424242]}
        (self.lock / 'owner.json').write_text(json.dumps(retained))
        self.addCleanup(shutil.rmtree, self.lock, True)
        with self.assertRaises(TimeoutError,
                               msg='#209 cleanupPending lock must be refused'):
            with C.parity_lock(timeout=0.3, lock=self.lock):
                pass
        self.assertEqual(self._owner(), retained,
                         '#209 refused acquisition must preserve the owner record')

    def test_probe_oserror_retains_lock(self):
        audit = self._run(OSError('injected #209 probe'), acquire_lock=True)
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_NOT_VERIFIED,
                         '#209 probe OSError must leave cleanup NOT_VERIFIED')
        self.assertIn('injected #209 probe', audit.get('cleanupBlocked') or '',
                      '#209 probe OSError must be recorded')
        self.assertIs(audit.get('allStopped'), False,
                      '#209 probe OSError must not report allStopped')
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 probe OSError must keep the owned group registered')
        self.assertTrue(self.lock.exists(),
                        '#209 probe OSError must retain the shared lock')

    def test_leader_not_reaped_retains_lock(self):
        child = FakeChild(self.PID, returncode=None)
        audit = self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID},
                          wait_states=('REAPED',), child=child,
                          kill_leader=('sent', 'RUNNING'), acquire_lock=True)
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_NOT_VERIFIED,
                         '#209 a leader that was not reaped must not be verified')
        self.assertIs(audit.get('allStopped'), False,
                      '#209 a leader that was not reaped must not report allStopped')
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 a leader that was not reaped must stay registered')
        self.assertTrue(self.lock.exists(),
                        '#209 a leader that was not reaped must retain the lock')

    def test_leader_kill_refusal_retains_lock(self):
        child = FakeChild(self.PID, returncode=None)
        audit = self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID},
                          wait_states=('EXITED', 'REAPED'), child=child,
                          acquire_lock=True)
        self.assertIn('leader kill refused', audit.get('cleanupBlocked') or '',
                      '#209 a refused leader kill must be recorded')
        # Frozen-scoring regression (#209 E1 P0-1): main keeps the probe's
        # VERIFIED value; only the lock-lifecycle fields may say "not stopped".
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_VERIFIED,
                         '#209 P0-1 a verified probe must keep main scoring (no downgrade)')
        self.assertIs(audit.get('allStopped'), False,
                      '#209 a cleanup error must deny allStopped')
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 a cleanup error must keep the owned group registered')
        self.assertTrue(self.lock.exists(),
                        '#209 a cleanup error must retain the shared lock')
        self.assertEqual(self._owner().get('cleanupPending'), [self.PID],
                         '#209 a cleanup error must publish cleanupPending')

    def test_cleanup_raised_retains_lock(self):
        # A cleanup helper that raises after the leader settled must propagate,
        # keep the frozen probe value, and retain the lease.  The failing read
        # stands in for any helper error outside the inner OSError/ValueError
        # catches; the probe has already published VERIFIED at that point.
        child = FakeChild(self.PID, returncode=0)
        with self.assertRaises(RuntimeError,
                               msg='#209 a cleanup helper error must propagate'):
            self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID},
                      child=child, fixture_launch=True,
                      read_error=RuntimeError('injected #209 cleanup'),
                      acquire_lock=True)
        audit = json.loads(
            (self.root / f'run-{self.run_number}' / 'launch.json').read_text())
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_VERIFIED,
                         '#209 a cleanup helper error must keep the probe scoring')
        self.assertIn('injected #209 cleanup',
                      audit.get('cleanupRaised') or '',
                      '#209 a cleanup helper error must be recorded')
        self.assertIs(audit.get('allStopped'), False,
                      '#209 a cleanup helper error must deny allStopped')
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 a cleanup helper error must keep the group registered')
        self.assertTrue(self.lock.exists(),
                        '#209 a cleanup helper error must retain the shared lock')
        self.assertEqual(self._owner().get('cleanupPending'), [self.PID],
                         '#209 a cleanup helper error must publish cleanupPending')

    def test_exited_refused_kill_retains_lock(self):
        # An unreaped EXITED leader makes kill_leader refuse with state EXITED.
        # The r1 frozen-scoring restore keeps main's unconditional refused
        # branch, so the refusal is recorded and the lease stays retained
        # (fail-closed); this pins that deliberate main-equivalence.
        child = FakeChild(self.PID, returncode=None)
        audit = self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID},
                          wait_states=('EXITED', 'EXITED'), child=child,
                          acquire_lock=True)
        self.assertIn('leader kill refused: state EXITED',
                      audit.get('cleanupBlocked') or '',
                      '#209 a refused EXITED kill must be recorded')
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_VERIFIED,
                         '#209 a refused EXITED kill must keep the frozen probe value')
        self.assertIs(audit.get('allStopped'), False,
                      '#209 a refused EXITED kill must retain the lease')
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 a refused EXITED kill must stay registered')
        self.assertTrue(self.lock.exists(),
                        '#209 a refused EXITED kill must retain the shared lock')
        self.assertEqual(self._owner().get('cleanupPending'), [self.PID],
                         '#209 a refused EXITED kill must publish cleanupPending')

    def test_spawn_defers_signals(self):
        self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID})
        self.assertEqual(self.masks.get('spawn', set()) & DEFERRED, DEFERRED,
                         '#209 spawn and registration must block SIGINT/SIGTERM')
        self.assertFalse(signal.pthread_sigmask(signal.SIG_BLOCK, set()) & DEFERRED,
                         '#209 the signal mask must be restored after run_fixture')

    def test_cleanup_defers_signals(self):
        self._run({'status': S.CLEANUP_GROUP_NOT_EMPTY, 'pgid': self.PID})
        self.assertEqual(self.masks.get('cleanup', set()) & DEFERRED, DEFERRED,
                         '#209 cleanup must block SIGINT/SIGTERM')
        self.assertFalse(signal.pthread_sigmask(signal.SIG_BLOCK, set()) & DEFERRED,
                         '#209 the signal mask must be restored after run_fixture')

    def test_group_registered_before_cleanup(self):
        self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID})
        self.assertIs(self.registered_at_cleanup, True,
                      '#209 the owned group must be registered before cleanup')

    def test_real_clean_group_releases_lock(self):
        folder = self.root / 'real-clean'
        folder.mkdir()
        with C.parity_lock(timeout=2, lock=self.lock):
            audit = S.run_fixture(folder, ROOT,
                                  command=[sys.executable, '-I', '-c', 'pass'])
        expected = S.CLEANUP_VERIFIED if sys.platform == 'darwin' else S.CLEANUP_BLOCKED
        self.assertTrue(audit.get('processTreeCleanup') == expected,
                        '#1221 lease release preserves the host cleanup score')
        self.assertIs(audit.get('allStopped'), True,
                      '#209 a real empty group must be reported allStopped')
        self.assertNotIn(audit.get('pgid'), C.LIVE_GROUPS,
                         '#209 a real empty group must be discarded from the registry')
        self.assertFalse(self.lock.exists(),
                         '#209 a real empty group must release the shared lock')

    def test_existing_output_directory_is_not_reused(self):
        # The real admission, not pathlib: a directory that already holds
        # evidence is refused (#953) and keeps every byte.  The runner must
        # never retire an old summary before this check (r4 P0).
        folder = self.root / 'existing-output'
        folder.mkdir()
        (folder / 'summary.json').write_text('{"counts": {}}\n')
        before = {path.name: path.read_bytes() for path in folder.iterdir()}
        process = _load(
            'negative_process_lock_release',
            ROOT / 'src/pilotdeck-gateway/tests/fixtures/negative_process.py')
        message = '#209 output directory reuse must be refused'
        with self.assertRaises(SystemExit, msg=message) as caught:
            process.new_output_directory(folder)
        self.assertIn('#953 refusing to reuse a non-empty output directory',
                      str(caught.exception), message)
        self.assertEqual({path.name: path.read_bytes() for path in folder.iterdir()},
                         before, message + ' with every byte kept')


class RunnerInvalidationControls(unittest.TestCase):
    """The delivered mutation runner must never keep a stale green summary."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-e1-runner-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.source = self.root / 'mutant_source.py'
        self.source.write_text('x = 0\n')
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative_controls
        self.runner = negative_controls

    def _drive(self, *, guard_error=None):
        """Run the real ``main`` with only the guard and subprocess mocked."""
        out = Path(tempfile.mkdtemp(prefix='out-', dir=self.root))
        source = self.source
        values = {
            1: subprocess.CompletedProcess([], 0, '', 'Ran 1 test\n'),
            2: subprocess.CompletedProcess(
                [], 1, '', 'FAIL: test_one\n#209 exclusive message\n'),
            3: subprocess.CompletedProcess([], 0, '', 'Ran 1 test\n'),
        }
        calls = []
        captured = {'mutating_calls': 0}

        def run_owned(command, **kwargs):
            calls.append(command)
            return values[len(calls)]

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                if guard_error is not None:
                    raise guard_error
                return False

        def mutating(root, paths, **kwargs):
            captured['mutating_calls'] += 1
            captured['paths'] = list(paths)
            return Guard()

        with patch.multiple(self.runner,
                            MUTATIONS=[('one', source, 'x = 0', 'x = 1')],
                            CHECKS={'one': ('Controls.test_one',
                                            '#209 exclusive message')},
                            mutating_sources=mutating,
                            run_owned=run_owned), \
                patch.object(sys, 'argv',
                             [str(Path(self.runner.__file__)), '--output',
                              str(out), '--only', 'one']):
            try:
                code = self.runner.main()
            except BaseException as error:
                if error is not guard_error:
                    raise
                # Only the injected cancellation is observed; a real Ctrl-C
                # (a different exception object) must still abort the test.
                code = error
        path = out / 'summary.json'
        if not path.is_file():
            summary = None
        else:
            try:
                summary = json.loads(path.read_text())
            except ValueError as error:
                self.fail(
                    f'#209 P0-2 runner summary.json must be valid JSON: {error}')
        return code, summary, captured.get('paths'), captured['mutating_calls']

    def test_final_guard_failure_rewrites_summary(self):
        """P0-2: a guard/restoration failure after the last row must void it."""
        code, summary, _, _ = self._drive(
            guard_error=RuntimeError('restore failed'))
        self.assertEqual(code, 2, '#209 P0-2 guard failure must exit 2')
        self.assertIsNotNone(
            summary, '#209 P0-2 a guard failure must publish an INVALID summary')
        self.assertEqual(summary['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#209 P0-2 the on-disk summary must not keep KILLED')
        self.assertTrue(all(row['status'] == 'INVALID'
                            for row in summary['controls']),
                        '#209 P0-2 every recorded row must be invalidated')
        self.assertIn('source guard', summary['controls'][0]['reason'],
                      '#209 P0-2 invalidation reason must be recorded')

    def test_final_guard_interrupt_rewrites_summary(self):
        """P0-2: a cancellation after the last row must void it, then propagate."""
        for canceled in (KeyboardInterrupt('restore interrupted'),
                         SystemExit('restore exited')):
            with self.subTest(canceled=type(canceled).__name__):
                code, summary, _, _ = self._drive(guard_error=canceled)
                self.assertIs(type(code), type(canceled),
                              '#209 P0-2 the cancellation must be re-raised')
                self.assertIsNotNone(
                    summary,
                    '#209 P0-2 a cancellation after the last row must not keep KILLED')
                self.assertEqual(
                    summary['counts'],
                    {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                    '#209 P0-2 a cancellation after the last row must not keep KILLED')
                self.assertTrue(all(row['status'] == 'INVALID'
                                    for row in summary['controls']),
                                '#209 P0-2 every recorded row must be invalidated')
                self.assertIn('source guard', summary['controls'][0]['reason'],
                              '#209 P0-2 invalidation reason must be recorded')

    def test_interrupt_during_void_publish_leaves_no_killed_summary(self):
        """Codex r3 P0: a SIGINT inside the void publish must not keep KILLED.

        The publish side is the review's read-only in-memory model: file
        operations are mocks and the SIGINT is a real Python signal raised
        from ``tmp.replace`` while the INVALID payload is being published.
        Admission and pre-existing evidence are deliberately out of scope
        here: ``new_output_directory`` is replaced by the model output, and
        the target-directory lifecycle is pinned by
        ``test_existing_evidence_is_refused_byte_identical`` (#209 E1 r4 P0).
        """
        message = ('#209 P0-3 an interrupt during the void publish must not '
                   'leave a readable KILLED summary')
        disk = {}
        files = {}

        def entry(name):
            if name not in files:
                files[name] = MagicMock(name=name)
            return files[name]

        out = MagicMock(name='out')
        out.__truediv__.side_effect = entry
        tmp = entry('summary.json.tmp')
        tmp.write_text.side_effect = lambda payload: disk.update(staged=payload)

        def publish(_destination):
            # The review's window: the INVALID temp is complete, and the
            # SIGINT lands before the rename.
            if json.loads(disk['staged'])['counts']['INVALID']:
                signal.raise_signal(signal.SIGINT)
            disk['summary'] = disk['staged']

        tmp.replace.side_effect = publish
        source = MagicMock(name='source')
        values = {
            1: subprocess.CompletedProcess([], 0, '', 'Ran 1 test\n'),
            2: subprocess.CompletedProcess(
                [], 1, '', 'FAIL: test_one\n#209 exclusive message\n'),
            3: subprocess.CompletedProcess([], 0, '', 'Ran 1 test\n'),
        }
        calls = []

        def run_owned(command, **kwargs):
            calls.append(command)
            return values[len(calls)]

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                raise RuntimeError('restore verification failed')

        def mutating(root, paths, **kwargs):
            return Guard()

        with patch.multiple(
                self.runner,
                MUTATIONS=[('one', source, 'x = 0', 'x = 1')],
                CHECKS={'one': ('Controls.test_one',
                                '#209 exclusive message')},
                new_output_directory=lambda _: out,
                mutating_sources=mutating,
                run_owned=run_owned), \
                patch.object(sys, 'argv',
                             [str(Path(self.runner.__file__)), '--output',
                              str(self.root / 'void-publish')]):
            try:
                code = self.runner.main()
            except BaseException as error:
                code = error
        try:
            summary = json.loads(disk['summary']) if 'summary' in disk else None
        except ValueError as error:
            self.fail(f'{message}: unreadable summary: {error}')
        self.assertIsInstance(code, KeyboardInterrupt, message)
        self.assertTrue(
            summary is None
            or summary['counts'] != {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0},
            message)

    def test_existing_evidence_is_refused_byte_identical(self):
        """P0-4: a pre-existing evidence directory is refused, never edited.

        r4 P0: the runner used to delete ``summary.json`` before admission, so
        a mistyped ``--output`` destroyed another run's result and only then
        got the #953 refusal.  Both shapes are pinned here — the old summary
        alone and the old summary next to other evidence — and every byte
        (sha256) of the directory must survive.  The bound mutant puts the
        pre-admission unlink back.
        """
        message = ('#209 P0-4 a pre-existing evidence directory must be refused '
                   'with every byte preserved')
        stale = json.dumps({
            'controls': [{'mutation': 'one', 'status': 'KILLED',
                          'reason': '#209 stale success'}],
            'counts': {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0}}) + '\n'
        for label, extra in (
                ('summary-only', {}),
                ('summary-and-evidence',
                 {'e1-release-ungated-positive.log': 'Ran 1 test\nOK\n'})):
            with self.subTest(case=label):
                folder = Path(tempfile.mkdtemp(prefix='reuse-', dir=self.root))
                (folder / 'summary.json').write_text(stale)
                for name, text in extra.items():
                    (folder / name).write_text(text)

                def snapshot():
                    return {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                            for path in sorted(folder.iterdir())}

                before = snapshot()

                def refuse(*_args, **_kwargs):
                    # Any admitted directory would start the run; fail loudly
                    # instead of touching this tree (#209 P0-4 message).
                    raise RuntimeError('#209 P0-4 mutation must not start')

                with patch.multiple(self.runner, mutating_sources=refuse), \
                        patch.object(sys, 'argv',
                                     [str(Path(self.runner.__file__)),
                                      '--output', str(folder),
                                      '--only', 'e1-release-ungated']):
                    try:
                        code = self.runner.main()
                    except BaseException as error:
                        code = error
                after = snapshot()
                self.assertEqual(
                    after, before,
                    f'{message}: the {label} directory was edited')
                self.assertEqual(
                    after.get('summary.json'), before['summary.json'],
                    f'{message}: summary.json bytes changed ({label})')
                self.assertIsInstance(code, SystemExit, f'{message} ({label})')
                self.assertIn('#953 refusing to reuse a non-empty output directory',
                              str(code), f'{message} ({label})')

    def test_runner_guards_session_and_common_without_outer_lease(self):
        """P1: one lease from mutating_sources and both protected sources."""
        code, summary, paths, mutating_calls = self._drive()
        self.assertEqual(code, 0, '#209 P1 the sample runner must finish green')
        self.assertEqual(summary['counts'],
                         {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0},
                         '#209 P1 the sample mutant must be killed')
        self.assertIn(self.runner.SESSION, paths,
                      '#209 P1 session_domain.py must be protected')
        self.assertIn(self.runner.COMMON, paths,
                      '#209 P1 common.py must be protected with session_domain.py')
        self.assertIn(self.runner.RUNNER, paths,
                      '#209 P1 the delivered runner must be protected for its own row')
        self.assertEqual(mutating_calls, 1,
                         '#209 P1 no second outer lease may wrap mutating_sources')


if __name__ == '__main__':
    unittest.main()
