#!/usr/bin/env python3
"""Bounded offline regressions for lock lifecycle, corrupt evidence and observer."""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is builtin; os is imported only after the frozen-module preflight below.
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import importlib.util
from pathlib import Path
import tempfile

# -B only stops cache writes, so redirect the bytecode prefix too (fix-r5).
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code

# Load the one guard by explicit path, never sys.path: a same-named package or
# extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing the guard is what this block defends.
_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
_guard_path = _apparatus / '_guard.py'
_guard = sys.modules.get('_guard')
if not (getattr(_guard, '__file__', None)
        and Path(_guard.__file__).resolve() == _guard_path
        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):
    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)
    _guard = importlib.util.module_from_spec(_spec)
    sys.modules['_guard'] = _guard
    _guard_source = _guard_path.read_bytes()
    try:
        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)
    except BaseException:
        sys.modules.pop('_guard', None)
        raise
    _guard._apparatus_source = _guard_source
if Path(_guard.__file__).resolve() != _guard_path:
    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')

_load = _guard.load
_has_git_checkout = _guard._has_git_checkout
_verify_cli_imports = _guard.verify_cli_imports
run_entry = _guard.run_entry
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

import json
import multiprocessing
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import threading
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch


APPARATUS_PROBES = common.APPARATUS_PROBES
DATA_FILES = common.DATA_FILES
DEGENERATE_PROBES = common.DEGENERATE_PROBES
MAX_AGE = common.MAX_AGE
ProbeSkipped = common.ProbeSkipped
apparatus_whitelist = common.apparatus_whitelist
baseline_dependency_hashes = common.baseline_dependency_hashes
cargo_config_problems = common.cargo_config_problems
guard = common.guard
materialize_probe = common.materialize_probe
node_modules_entry = common.node_modules_entry
owner = common.owner
parity_lock = common.parity_lock
probe_expected = common.probe_expected
remove_probe = common.remove_probe
save = common.save
unknown_apparatus_files = common.unknown_apparatus_files
compare_module = _load('compare', common.SCRIPT / 'compare.py')
Normalizer = compare_module.Normalizer
compare = compare_module.compare
wire_frame = compare_module.wire_frame
observer_module = _load('observer', common.SCRIPT / 'observer.py')
Decoder = observer_module.Decoder

BOOTSTRAP_START = '# >>> apparatus bootstrap'
BOOTSTRAP_END = '# <<< apparatus bootstrap'
# Derived from both registered-entry tables, never hand-maintained: a new A2/A3
# module (run_all, build_proxy, ...) is covered by the sys.path-import checks
# without a second edit.
APPARATUS_MODULES = tuple(sorted({
    Path(name).stem for name in common.A2_SCRIPTS + common.A3_SCRIPTS if name.endswith('.py')
}))
# ``import common`` and ``import os, common`` are both bare-name imports that
# would resolve through sys.path; match the whole import list, not just the
# first name after ``import``.
APPARATUS_IMPORT = (r'(?m)^\s*(?:from\s+(?:' + '|'.join(APPARATUS_MODULES) + r')\b'
                    r'|import\s+[^\n#]*\b(?:' + '|'.join(APPARATUS_MODULES) + r')\b)')


def contender(lock, events):
    os.setsid()
    with parity_lock(timeout=3, lock=Path(lock)):
        with open(events, 'a') as file:
            file.write(f'{os.getpid()} enter\n')
        time.sleep(.04)
        with open(events, 'a') as file:
            file.write(f'{os.getpid()} exit\n')


class Safety(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-gateway-ruler-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.lock = self.root / 'lock'

    def test_cleanup_pending_retains_lock_and_cannot_be_taken_over(self):
        with patch.object(common, 'LIVE_GROUPS', {424242}):
            with parity_lock(lock=self.lock):
                pass
        self.assertTrue(self.lock.exists(), '#209 unverified cleanup must retain lock')
        retained = owner(self.lock)
        self.assertEqual(retained.get('cleanupPending'), [424242], '#209 retained groups are explicit')
        with patch.object(common, 'alive', return_value=False):
            with self.assertRaises(TimeoutError, msg='#209 retained lock cannot be stolen as dead owner'):
                with parity_lock(.01, lock=self.lock):
                    self.fail('#209 cleanup-pending owner was stolen')
        self.assertEqual(owner(self.lock), retained, '#209 failed takeover preserves owner')

    def test_acquisition_interrupt_releases_empty_owned_lock(self):
        real_save = common.save

        def interrupt_after_save(path, value):
            real_save(path, value)
            os.kill(os.getpid(), common.signal.SIGINT)

        with patch.object(common, 'LIVE_GROUPS', set()), patch.object(common, 'save', interrupt_after_save):
            with self.assertRaises(KeyboardInterrupt, msg='#209 acquisition interrupt delivered'):
                with parity_lock(lock=self.lock):
                    self.fail('#209 acquisition signal was lost')
        self.assertFalse(self.lock.exists(), '#209 cancelled acquisition must release empty lock')

    def test_release_guard_interrupt_publishes_pending_before_delivery(self):
        real_guard = common.guard
        calls = []

        @common.contextmanager
        def interrupt_at_release(lock):
            calls.append(lock)
            if len(calls) == 2:
                os.kill(os.getpid(), common.signal.SIGINT)
            with real_guard(lock):
                yield

        with patch.object(common, 'LIVE_GROUPS', {424242}), \
                patch.object(common, 'guard', interrupt_at_release):
            with self.assertRaises(KeyboardInterrupt, msg='#209 release interrupt delivered after publication'):
                with parity_lock(lock=self.lock):
                    pass
        self.assertEqual(owner(self.lock).get('cleanupPending'), [424242],
                         '#209 release entry interrupt must publish pending groups')
        with patch.object(common, 'alive', return_value=False):
            with self.assertRaises(TimeoutError, msg='#209 cancelled release retains lock'):
                with parity_lock(.01, lock=self.lock):
                    self.fail('#209 cancelled release lock was stolen')

    def test_cleanup_record_write_failure_keeps_preexisting_marker(self):
        real_save = common.save

        def fail_cleanup_write(path, value):
            if isinstance(value.get('cleanupPending'), list):
                raise OSError('#209 save denied')
            real_save(path, value)

        with patch.object(common, 'LIVE_GROUPS', {424242}), \
                patch.object(common, 'save', fail_cleanup_write):
            with self.assertRaises(OSError, msg='#209 cleanup record write failure remains visible'):
                with parity_lock(lock=self.lock):
                    acquisition_marker = owner(self.lock).get('cleanupPending')
        self.assertIs(acquisition_marker, True, '#209 lock must be marked before spawning')
        self.assertIs(owner(self.lock).get('cleanupPending'), True,
                      '#209 failed cleanup write must preserve acquisition marker')
        with patch.object(common, 'alive', return_value=False):
            with self.assertRaises(TimeoutError, msg='#209 failed write retains dead-owner lock'):
                with parity_lock(.01, lock=self.lock):
                    self.fail('#209 failed-write lock was stolen')

    def test_damaged_published_owner_is_not_stale_unpublished_lock(self):
        self.lock.mkdir()
        (self.lock / 'owner.json').write_text('{partial')
        stamp = time.time() - MAX_AGE - 1
        os.utime(self.lock, (stamp, stamp))
        with self.assertRaises(TimeoutError, msg='#209 damaged owner requires inspection'):
            with parity_lock(.01, lock=self.lock):
                self.fail('#209 damaged published owner was stolen')
        self.assertEqual((self.lock / 'owner.json').read_text(), '{partial', '#209 damaged owner preserved')

    def test_missing_or_extra_registration_fails_closed(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        child = SimpleNamespace(pid=424242, returncode=None)
        with patch.object(common, 'LIVE_GROUPS', set()), patch.object(runner.os, 'killpg') as killpg:
            audit = runner.stop_owned(child)
            self.assertFalse(audit['allStopped'], '#209 unregistered process is not proven stopped')
            self.assertIn(child.pid, common.LIVE_GROUPS, '#209 unregistered process retains lock')
            self.assertEqual(killpg.call_count, 0, '#209 unregistered process receives no signal')
        with patch.object(common, 'LIVE_GROUPS', {424243}):
            audit = runner.cleanup_owned([])
            self.assertEqual(audit['ownedGroupsRemaining'], [424243], '#209 extra registration is reported')
            self.assertFalse(audit['allStopped'], '#209 extra registration blocks success')

    def test_real_held_leaders_cleanup_after_exit_and_term(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        for body in ('pass', 'import time; time.sleep(30)'):
            with self.subTest(body=body), patch.object(common, 'LIVE_GROUPS', set()):
                entries = []
                log = (self.root / ('exited.log' if body == 'pass' else 'running.log')).open('w')
                child = runner.start_owned(entries, log, {}, [sys.executable, '-I', '-c', body],
                                           stdout=log, stderr=log)
                try:
                    if body == 'pass':
                        self.assertEqual(runner.wait_owned(child, 5), 0, '#209 real held exit observed')
                        self.assertIsNone(child.returncode, '#209 real leader still unreaped')
                    audit = runner.cleanup_owned(entries)
                    self.assertTrue(audit['allStopped'], f'#209 real cleanup must prove absence: {audit}')
                    self.assertFalse(common.LIVE_GROUPS, '#209 real clean groups release ownership')
                finally:
                    # Only this direct child; no signal after it has been reaped.
                    if child.returncode is None:
                        child.kill()
                        child.wait(timeout=5)
                    log.close()

    def test_zombie_denial_requires_final_esrch(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        for gone in (True, False):
            with self.subTest(gone=gone):
                child = unittest.mock.Mock(pid=424242, returncode=0)

                def denied(_pid, sig):
                    if sig == 0 and gone:
                        raise ProcessLookupError()
                    raise PermissionError('#209 group signal denied')

                with patch.object(common, 'LIVE_GROUPS', {child.pid}), \
                        patch.object(runner, 'wait_owned', return_value=0), \
                        patch.object(runner.os, 'killpg', denied):
                    audit = runner.stop_owned(child)
                    self.assertEqual(audit['allStopped'], gone,
                                     '#209 zombie-only EPERM may clear only after final ESRCH')
                    self.assertEqual(child.pid in common.LIVE_GROUPS, not gone,
                                     '#209 denied live group retains ownership')
                    self.assertEqual(len(audit['unreapedLeaderSignalDenials']), 2,
                                     '#209 zombie signal denials remain in raw audit')

    def test_owned_wait_holds_identity_and_stop_never_signals_reaped_child(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        child = SimpleNamespace(pid=424242, returncode=None)
        status = SimpleNamespace(si_status=7, si_code=os.CLD_EXITED)
        with patch.object(runner.os, 'waitid', return_value=status) as waitid:
            self.assertEqual(runner.wait_owned(child, 0), 7, '#209 held wait reports exit without reap')
            self.assertTrue(waitid.call_args.args[2] & os.WNOWAIT, '#209 wait must reserve leader identity')
        with patch.object(common, 'LIVE_GROUPS', {child.pid}), \
                patch.object(runner.os, 'waitid', side_effect=ChildProcessError('#209 identity lost')), \
                patch.object(runner.os, 'killpg', side_effect=ProcessLookupError()) as killpg:
            result = runner.stop_owned(child)
            self.assertFalse(result['allStopped'], '#209 lost ownership cannot certify cleanup')
            self.assertTrue(all(call.args[1] == 0 for call in killpg.call_args_list),
                            '#209 never send TERM/KILL to a reaped leader')
            self.assertIn(child.pid, common.LIVE_GROUPS, '#209 lost identity keeps lock ownership')

    def test_start_registers_before_pending_signal_and_unblocks_child(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        entries = []
        log = unittest.mock.Mock()
        child = SimpleNamespace(pid=424242)

        def popen(*_args, **kwargs):
            blocked = common.signal.pthread_sigmask(common.signal.SIG_BLOCK, set())
            self.assertTrue(common.DEFERRED_SIGNALS <= blocked, '#209 spawn-registration signal window')
            self.assertIs(kwargs.get('preexec_fn'), common.unblock_child_signals, '#209 child mask restored')
            os.kill(os.getpid(), common.signal.SIGINT)
            return child

        with patch.object(common, 'LIVE_GROUPS', set()), patch.object(runner.subprocess, 'Popen', popen):
            with self.assertRaises(KeyboardInterrupt, msg='#209 pending interrupt must be delivered'):
                runner.start_owned(entries, log, {}, ['fixture'])
            self.assertEqual(common.LIVE_GROUPS, {child.pid}, '#209 register before signal delivery')
            self.assertEqual([row[0] for row in entries], [child], '#209 cleanup list published before signal')
        with patch.object(common.signal, 'pthread_sigmask') as mask:
            common.unblock_child_signals()
            self.assertEqual(mask.call_args_list,
                             [unittest.mock.call(common.signal.SIG_UNBLOCK, common.DEFERRED_SIGNALS)],
                             '#209 child signals must be unblocked')

    def test_spawn_failure_closes_log_without_registering(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        log, entries = unittest.mock.Mock(), []
        with patch.object(common, 'LIVE_GROUPS', set()), \
                patch.object(runner.subprocess, 'Popen', side_effect=OSError('#209 spawn failed')):
            with self.assertRaisesRegex(OSError, '#209 spawn failed'):
                runner.start_owned(entries, log, {}, ['fixture'])
            self.assertFalse(common.LIVE_GROUPS, '#209 failed spawn has no owned group')
            self.assertFalse(entries, '#209 failed spawn has no process entry')
        self.assertEqual(log.close.call_count, 1, '#209 failed spawn closes log')

    def test_cleanup_continues_after_oserror_and_defers_whole_batch(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        entries = [(SimpleNamespace(pid=pid, returncode=0), unittest.mock.Mock(), {})
                   for pid in (424241, 424242)]
        calls = []

        def stop(proc):
            calls.append(proc.pid)
            if len(calls) == 1:
                os.kill(os.getpid(), common.signal.SIGINT)
                raise OSError('#209 cleanup failed')
            return {'allStopped': True, 'errors': []}

        with patch.object(common, 'LIVE_GROUPS', {row[0].pid for row in entries}), \
                patch.object(runner, 'stop_owned', stop):
            with self.assertRaises(KeyboardInterrupt, msg='#209 deferred cleanup interrupt delivered'):
                runner.cleanup_owned(entries)
            self.assertEqual(calls, [424242, 424241], '#209 cleanup must reach every group before interrupt')
            self.assertIn(424242, common.LIVE_GROUPS, '#209 failed group remains owned')
        for _, log, _ in entries:
            self.assertEqual(log.close.call_count, 1, '#209 cleanup closes every log')

    def test_cleanup_log_error_restores_all_registrations(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        log = unittest.mock.Mock()
        log.close.side_effect = OSError('#209 close failed')
        child = SimpleNamespace(pid=424242, returncode=0)
        with patch.object(common, 'LIVE_GROUPS', set()), \
                patch.object(runner, 'stop_owned', return_value={'allStopped': True, 'errors': []}):
            audit = runner.cleanup_owned([(child, log, {})])
            self.assertEqual(common.LIVE_GROUPS, {child.pid}, '#209 cleanup error restores registrations')
            self.assertFalse(audit['allStopped'], '#209 cleanup log error cannot pass')

    def test_stop_owned_proves_absence_and_keeps_all_failure_shapes(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        # No real foreign PID is signalled: killpg/wait/time are deterministic hooks.
        for kind in ('clean', 'kill-error', 'wait-error', 'probe-error', 'never-exits'):
            with self.subTest(kind=kind):
                child = unittest.mock.Mock(pid=424242, returncode=0 if kind != 'never-exits' else None)
                if kind == 'wait-error':
                    child.wait.side_effect = OSError('#209 wait denied')
                if kind == 'never-exits':
                    child.wait.side_effect = subprocess.TimeoutExpired('fixture', 3)
                calls = []

                def killpg(_pid, sig):
                    calls.append(sig)
                    if sig == 0:
                        if kind == 'probe-error':
                            raise PermissionError('#209 probe denied')
                        if kind != 'never-exits':
                            raise ProcessLookupError()
                    elif kind == 'kill-error':
                        raise OSError('#209 kill failed')

                with patch.object(common, 'LIVE_GROUPS', {child.pid}), \
                        patch.object(runner, 'wait_owned', return_value=0), \
                        patch.object(runner.os, 'killpg', killpg), \
                        patch.object(runner.time, 'monotonic', side_effect=(0, 3)):
                    audit = runner.stop_owned(child)
                    self.assertEqual(audit['allStopped'], kind == 'clean', '#209 only proven cleanup passes')
                    self.assertEqual(child.pid in common.LIVE_GROUPS, kind != 'clean',
                                     '#209 failed cleanup remains registered')
                self.assertEqual(calls[:2], [common.signal.SIGTERM, common.signal.SIGKILL],
                                 '#209 kill/wait errors cannot skip the KILL attempt')
                self.assertEqual(child.wait.call_count, 1, '#209 reap only after all signals')

    def test_live_owner_times_out_without_removal(self):
        self.lock.mkdir()
        previous = {'pid': os.getpid(), 'timestamp': time.time(), 'token': 'live'}
        save(self.lock / 'owner.json', previous)
        with self.assertRaises(TimeoutError):
            with parity_lock(.01, lock=self.lock):
                self.fail('must not acquire live owner')
        self.assertEqual(owner(self.lock), previous)

    def test_dead_pid_takeover_and_release(self):
        child = subprocess.Popen([sys.executable, '-c', 'pass'], start_new_session=True)
        child.wait(timeout=3)
        self.lock.mkdir()
        save(self.lock / 'owner.json', {'pid': child.pid, 'timestamp': time.time(), 'token': 'dead'})
        takeovers = []
        with parity_lock(takeovers=takeovers, lock=self.lock):
            self.assertEqual(owner(self.lock)['pid'], os.getpid())
        self.assertFalse(self.lock.exists())
        self.assertEqual(takeovers[0]['reason'], 'dead_pid')

    def test_old_release_cannot_remove_replacement_owner(self):
        replacement = {'pid': os.getpid(), 'timestamp': time.time(), 'token': 'replacement'}
        with parity_lock(lock=self.lock):
            save(self.lock / 'owner.json', replacement)
        self.assertEqual(owner(self.lock), replacement)

    def test_stale_unpublished_lock_takeover(self):
        self.lock.mkdir()
        stamp = time.time() - MAX_AGE - 1
        os.utime(self.lock, (stamp, stamp))
        takeovers = []
        with parity_lock(lock=self.lock, takeovers=takeovers):
            pass
        self.assertEqual(takeovers[0]['reason'], 'expired')

    def test_lease_timestamp_starts_after_wait(self):
        self.lock.mkdir()
        save(self.lock / 'owner.json', {'pid': os.getpid(), 'timestamp': time.time(), 'token': 'earlier-owner'})
        released = []

        def release():
            time.sleep(.2)
            with guard(self.lock):
                (self.lock / 'owner.json').unlink()
                self.lock.rmdir()
                released.append(time.time())

        thread = threading.Thread(target=release)
        thread.start()
        try:
            with parity_lock(timeout=3, lock=self.lock):
                self.assertGreaterEqual(owner(self.lock)['timestamp'], released[0])
        finally:
            thread.join(3)

    def test_concurrent_waiters_are_serialized(self):
        events = self.root / 'events'
        children = [multiprocessing.Process(target=contender, args=(str(self.lock), str(events))) for _ in range(3)]
        try:
            for child in children:
                child.start()
            for child in children:
                child.join(5)
                self.assertEqual(child.exitcode, 0)
        finally:
            for child in children:
                if child.is_alive():
                    child.terminate()
                    child.join(3)
        lines = events.read_text().splitlines()
        self.assertEqual(len(lines), 6)
        for start, end in zip(lines[::2], lines[1::2]):
            self.assertEqual(start.split()[0], end.split()[0])
            self.assertTrue(start.endswith('enter') and end.endswith('exit'))
        self.assertFalse(self.lock.exists())

    def test_two_corrupt_rounds_are_fail_not_exception(self):
        save(self.root / 'run.json', [])
        result = compare(self.root, self.root)
        self.assertEqual(result['status'], 'FAIL')
        self.assertIn('G0.round', {row['rule'] for row in result['failures']})

    def test_observer_decodes_split_masked_wire_without_rewriting(self):
        records = []
        decoder = Decoder(records.append, 'send')
        payload = b'{"type":"request","method":"supported_agents","id":"test"}'
        mask = b'1234'
        wire = bytes([0xc1, 128 | len(payload)]) + mask + bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        data = b'GET /ws HTTP/1.1\r\nUpgrade: websocket\r\n\r\n' + wire
        for byte in data:
            decoder.feed(bytes([byte]))
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]['payloadHex'], payload.hex())
        self.assertEqual(records[0]['frame']['method'], 'supported_agents')

    def test_business_times_and_generated_id_aliases_survive(self):
        a, b = Normalizer(), Normalizer()
        self.assertNotEqual(a.value({'durationMs': 1, 'runAt': '2099-01-01'}), b.value({'durationMs': 2, 'runAt': '2099-01-02'}))
        x = '00000000-0000-4000-8000-000000000001'
        y = '00000000-0000-4000-8000-000000000002'
        self.assertNotEqual(Normalizer().value([x, x]), Normalizer().value([x, y]))

    def test_uuid_definitions_pair_by_entity_not_list_position(self):
        x, y, z, w = [f'00000000-0000-4000-8000-{i:012d}' for i in range(1, 5)]
        a, b = Normalizer(), Normalizer()
        left = [{'id': x, 'name': 'alpha'}, {'id': y, 'name': 'beta'}]
        right = [{'id': w, 'name': 'beta'}, {'id': z, 'name': 'alpha'}]
        a.index(left)
        b.index(right)
        self.assertEqual(a.value([x, y]), b.value([z, w]))
        self.assertNotEqual(a.value([x, y]), b.value([w, z]))
        self.assertNotEqual(Normalizer().value([x, y]), Normalizer().value([y, x]))
        with self.assertRaises(ValueError):
            a.index({'id': z, 'name': 'alpha'})

    def test_binding_generation_cannot_change_on_same_entity(self):
        n = Normalizer()
        n.value({'connectionId': 'fixed-connection', 'generation': 1})
        with self.assertRaises(ValueError):
            n.value({'connectionId': 'fixed-connection', 'generation': 2})
        with self.assertRaises(ValueError):
            n.value({'connectionId': 'another-connection', 'generation': 1})

    def test_clock_container_boundaries(self):
        shapes = [({'name': 'business'}, ('createdAt', 'lastActivity')),
                  ({'requestId': 'business'}, ('createdAt',)),
                  ({'name': 'business', 'lastActivity': 1}, ('createdAt',)),
                  ({'projectKey': 'business'}, ('lastActivity',)),
                  ({'taskId': 'business'}, ('createdAt', 'updatedAt')),
                  ({'runId': 'business', 'ownerPid': 1}, ('updatedAt',)),
                  ({'runId': 'business'}, ('acceptedAt',)),
                  ({'firstPrompt': 'business'}, ('updatedAt',))]
        for parent, fields in shapes:
            for field in fields:
                with self.subTest(parent=parent, field=field):
                    self.assertNotEqual(Normalizer().value(dict(parent, **{field: 1})),
                                        Normalizer().value(dict(parent, **{field: 2})))
        containers = [({'projectKey': 'p', 'fullPath': 'p', 'name': 'n', 'sessionCount': 0, 'lastActivity': 1}, 'createdAt'),
                      ({'projectKey': 'p', 'path': 'p', 'name': 'n'}, 'lastActivity'),
                      ({'ownerId': 's', 'kind': 'question', 'toolCallId': 't', 'requestId': 'r'}, 'createdAt'),
                      ({'runId': 'r', 'ownerPid': 1, 'acceptedAt': 1}, 'updatedAt')]
        for parent, field in containers:
            self.assertEqual(Normalizer().value(dict(parent, **{field: 1})),
                             Normalizer().value(dict(parent, **{field: 2})))

    def test_receive_header_prefix_and_metadata_are_exact(self):
        for code, extension in [(2, b''), (126, b'\x00\x02'), (127, (2).to_bytes(8, 'big'))]:
            for first in (0x81, 0x01, 0xc1):
                for mask in (b'', b'1234'):
                    payload = b'{}'
                    if mask:
                        payload = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
                    header = bytes([first, code | (128 if mask else 0)]) + extension + mask
                    row = {'direction': 'receive', 'wireHex': (header + payload).hex(), 'frame': {},
                           'wire': {'opcode': 1, 'fin': bool(first & 128), 'rsv': first & 0x70,
                                    'masked': bool(mask), 'lengthCode': code, 'payloadLength': 2}}
                    self.assertEqual(wire_frame(row), header.hex())
                    row['wire']['masked'] = not bool(mask)
                    with self.assertRaises(ValueError):
                        wire_frame(row)
        for data in ('', '81', '817e00', '817f0000', '8182'):
            with self.assertRaises(ValueError):
                wire_frame({'wireHex': data})


class CheckoutProvenance(unittest.TestCase):
    """Static exports bypass only HEAD provenance, never checkout failures."""

    ENTRIES = ('build_proxy.py', 'check_frozen.py', 'check_ruler.py', 'compare.py',
               'native_evidence.py', 'observer.py', 'run_round.py',
               'session_domain.py', 'test_safety.py',
               'test_channel_provenance.py', 'test_retirement_channel_v2.py',
               'test_retirement_public_http.py', 'test_project_clock_scope.py',
               'controls/run_all.py', 'controls/prerequisites.py',
               'controls/session_domain.py')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='pdrs-166-checkout-')
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name).resolve()

    def verifiers(self, root):
        # Use the one guard's detector for every entry, plus the library copy.
        apparatus = root / 'scripts/gateway-parity'
        for index, name in enumerate(self.ENTRIES):
            alias = 'checkout_entry_' + str(index)
            self.addCleanup(sys.modules.pop, alias, None)
            entry = _load(alias, common.SCRIPT / name)
            with patch.object(entry, '_apparatus', apparatus), \
                    patch.object(_guard, 'APPARATUS', apparatus):
                yield name, entry._verify_cli_imports
        with patch.object(common, 'ROOT', root):
            yield 'common.verify_loaded_apparatus', common.verify_loaded_apparatus

    def test_static_exports_do_not_read_head(self):
        class HeadRead(Exception):
            pass
        for name, verify in self.verifiers(self.folder):
            with self.subTest(entry=name), patch.dict(os.environ, {}, clear=True), \
                    patch.object(subprocess, 'check_output', side_effect=HeadRead):
                read_head = False
                try:
                    verify()
                except HeadRead:
                    read_head = True
                self.assertFalse(read_head, '#166 static export must not read HEAD')

    def test_checkout_metadata_never_skips_head(self):
        class HeadRead(Exception):
            pass
        for kind in ('directory', 'worktree-file', 'dangling-symlink', 'ancestor', 'git-dir-env'):
            root = self.folder / kind / 'tree'
            root.mkdir(parents=True)
            marker = (root.parent if kind == 'ancestor' else root) / '.git'
            if kind in ('directory', 'ancestor'):
                marker.mkdir()
            elif kind == 'worktree-file':
                marker.write_text('gitdir: missing-dir\n')
            elif kind == 'dangling-symlink':
                marker.symlink_to(root / 'missing-dir')
            env = {'GIT_DIR': str(root / 'missing-dir')} if kind == 'git-dir-env' else {}
            for name, verify in self.verifiers(root):
                with self.subTest(kind=kind, entry=name), patch.dict(os.environ, env, clear=True), \
                        patch.object(subprocess, 'check_output', side_effect=HeadRead):
                    with self.assertRaises(HeadRead, msg='#166 checkout metadata must require HEAD'):
                        verify()

    def test_unreadable_checkout_metadata_fails_closed(self):
        lstat = Path.lstat
        def unreadable(path, *args, **kwargs):
            if path == self.folder / '.git':
                raise PermissionError('controlled metadata read denial')
            return lstat(path, *args, **kwargs)
        for name, verify in self.verifiers(self.folder):
            with self.subTest(entry=name), patch.dict(os.environ, {}, clear=True), \
                    patch.object(Path, 'lstat', unreadable):
                with self.assertRaises(PermissionError,
                                       msg='#166 unreadable checkout metadata must fail closed'):
                    verify()

    def test_explicit_head_is_verified_even_without_checkout_metadata(self):
        class HeadSourceRead(Exception):
            pass
        with patch.dict(os.environ, {}, clear=True), patch.object(common, 'ROOT', self.folder), \
                patch.object(common, '_head_source', side_effect=HeadSourceRead):
            with self.assertRaises(HeadSourceRead,
                                   msg='#166 explicit pinned HEAD must always be verified'):
                common.verify_loaded_apparatus('explicit-pinned-head')

    def test_cli_and_library_share_checkout_detection(self):
        import ast
        def detector(path):
            tree = ast.parse(path.read_bytes())
            matches = [node for node in tree.body
                       if isinstance(node, ast.FunctionDef) and node.name == '_has_git_checkout']
            self.assertEqual(len(matches), 1, '#166 checkout detector must have one definition')
            return ast.dump(matches[0], include_attributes=False)
        expected = detector(common.SCRIPT / 'common.py')
        # The CLI trust root and the library lock share one copied function so
        # the two checkout rules cannot drift apart; entries only alias it.
        self.assertEqual(detector(common.SCRIPT / '_guard.py'), expected,
                         '#166 CLI and library checkout detectors must match: _guard.py')
        for name in self.ENTRIES:
            tree = ast.parse((common.SCRIPT / name).read_bytes())
            self.assertFalse(any(isinstance(node, ast.FunctionDef)
                                 and node.name == '_has_git_checkout' for node in tree.body),
                             '#991 entry must not carry a private checkout detector: ' + name)


class ImportIsolation(unittest.TestCase):
    """#166 exact imported bytes, real lease protocol, no alternate repository."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='pdrs-166-import-')
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name).resolve()
        self.lock = self.folder / 'parity.lock'
        self.path = self.folder / 'probe.py'
        self.clean = b"VALUE = 'clean'\n"
        self.path.write_bytes(self.clean)
        # Only the dependency reader is injected. The lock comes from real HEAD
        # bytes, and both lease directory and .guard flock run on this host.
        self.head = common._apparatus_head()
        self.lock_source = common._head_source(self.head, common.SCRIPT / 'common.py')
        self.expected = {self.path: self.clean, self.folder / 'common.py': self.lock_source}
        self.addCleanup(sys.modules.pop, 'import_isolation_probe', None)

    def environment(self):
        from contextlib import ExitStack
        stack = ExitStack()
        stack.enter_context(patch.object(common, 'SCRIPT', self.folder))
        stack.enter_context(patch.object(common, '_apparatus_head', return_value=self.head))
        stack.enter_context(patch.object(common, '_head_source', side_effect=lambda head, path: self.expected[Path(path)]))
        return stack

    def test_imported_mutant_is_rejected_after_disk_restore(self):
        message = '#166 pre-lock mutant must be rejected after disk restoration'
        waiting, restored = threading.Event(), threading.Event()
        outcomes = []
        real_sleep = time.sleep

        def sleep_until_restore(delay):
            if threading.current_thread() is worker:
                waiting.set()
                if not restored.wait(3):
                    raise RuntimeError('#166 restore hook exceeded deadline')
            else:
                real_sleep(delay)

        def consume():
            try:
                with common.verified_parity_lock(timeout=3, lock=self.lock):
                    outcomes.append('WORK')
            except ValueError as error:
                outcomes.append('REJECTED' if '#166 loaded apparatus differs from HEAD' in str(error)
                                else repr(error))
            except BaseException as error:
                outcomes.append(repr(error))

        worker = threading.Thread(target=consume)
        with self.environment(), patch.object(time, 'sleep', side_effect=sleep_until_restore):
            try:
                with parity_lock(lock=self.lock):
                    self.path.write_bytes(b"VALUE = 'mutant'\n")
                    imported = _load('import_isolation_probe', self.path)
                    self.assertEqual(imported.VALUE, 'mutant', message + ': import hook')
                    worker.start()
                    self.assertTrue(waiting.wait(3), message + ': consumer must wait for predecessor')
                    self.path.write_bytes(self.clean)
                    self.assertIs(_load('import_isolation_probe', self.path), imported,
                                  message + ': cached import')
                restored.set()
            finally:
                restored.set()
                if worker.ident is not None:
                    worker.join(4)
        self.assertFalse(worker.is_alive(), message + ': bounded completion')
        self.assertEqual(outcomes, ['REJECTED'], message)
        self.assertFalse(self.lock.exists(), message + ': own lease released')

    def test_loader_records_executed_bytes_not_later_disk(self):
        source = ("from pathlib import Path\nPath(__file__).write_bytes(" + repr(self.clean) + ")\nVALUE = 'mutant'\n").encode()
        self.path.write_bytes(source)
        imported = _load('import_isolation_probe', self.path)
        self.assertEqual(self.path.read_bytes(), self.clean, '#166 import hook must restore disk')
        self.assertEqual(imported._apparatus_source, source, '#166 provenance must be exactly the compiled bytes')

    def test_cli_preflight_uses_head_verifier_for_mutated_common(self):
        from types import ModuleType
        entry = _load('trusted_cli_entry', common.SCRIPT / 'run_round.py')
        sources = {p.relative_to(common.ROOT).as_posix(): p.read_bytes()
                   for p in common.SCRIPT.rglob('*.py') if 'node_modules' not in p.parts}
        original = sources['scripts/gateway-parity/common.py']
        old = b"raise ValueError(f'#166 loaded apparatus differs from HEAD: {path}')"
        self.assertEqual(original.count(old), 1, '#166 trusted-verifier fixture anchor must be unique')
        source = original.replace(old, b'pass  # controlled corrupt verifier')
        mutant = ModuleType('common')
        mutant.__file__ = common.__file__
        exec(compile(source, mutant.__file__, 'exec'), mutant.__dict__)
        mutant._apparatus_source = source

        def git_read(command, **kwargs):
            if command[3] == 'rev-parse':
                return (self.head + '\n').encode()
            return sources[command[-1].split(':', 1)[1]]

        rejected = False
        detail = 'verifier accepted'
        with patch.dict(sys.modules, {'common': mutant}), patch.object(entry, 'common', mutant), \
                patch.object(subprocess, 'check_output', side_effect=git_read):
            try:
                entry._verify_cli_imports()
            except ValueError as error:
                detail = str(error)
                rejected = '#166 loaded apparatus differs from HEAD' in detail
        self.assertTrue(rejected, '#166 CLI preflight must use HEAD verifier even when disk helper is mutant: ' + detail)

    def test_controls_late_import_is_verified_before_return(self):
        entry = _load('late_controls_entry', common.SCRIPT / 'controls/prerequisites.py')
        sources = {p.relative_to(common.ROOT).as_posix(): p.read_bytes()
                   for p in common.SCRIPT.rglob('*.py') if 'node_modules' not in p.parts}
        target = common.SCRIPT / 'common.py'
        original = sources['scripts/gateway-parity/common.py']
        self.assertEqual(original.count(b'MAX_AGE = 2400'), 1,
                         '#166 late-import fixture anchor must be unique')
        mutant = original.replace(b'MAX_AGE = 2400', b'MAX_AGE = 2401', 1)
        alias = 'late_import_probe'
        self.addCleanup(sys.modules.pop, alias, None)
        read = Path.read_bytes

        def git_read(command, **kwargs):
            return ((self.head + '\n').encode() if command[3] == 'rev-parse'
                    else sources[command[-1].split(':', 1)[1]])

        with patch.object(subprocess, 'check_output', side_effect=git_read):
            entry._verify_cli_imports()  # The independent CLI's initial check passes.
            for source in (original, mutant):
                sys.modules.pop(alias, None)
                rejected = False
                with patch.object(Path, 'read_bytes',
                                  lambda path: source if path.resolve() == target else read(path)):
                    try:
                        entry.module_at(alias, target)
                    except ValueError as error:
                        rejected = '#166 loaded apparatus differs from HEAD' in str(error)
                self.assertEqual(rejected, source == mutant,
                                 '#166 controls late import must reject mutant before returning module')

    def test_controls_saved_tree_import_keeps_existing_semantics(self):
        entry = _load('saved_controls_entry', common.SCRIPT / 'controls/prerequisites.py')
        alias = 'saved_tree_probe'
        self.addCleanup(sys.modules.pop, alias, None)
        with patch.object(entry, '_verify_cli_imports') as verify:
            module = entry.module_at(alias, self.path)
        self.assertEqual((module.VALUE, verify.call_count), ('clean', 0),
                         '#166 saved code-root import must retain its existing semantics')

    def test_cli_dispatch_verifies_before_main(self):
        from contextlib import redirect_stdout, redirect_stderr
        from types import ModuleType
        import io
        head_verifier = b"def verify_loaded_apparatus(head):\n    raise RuntimeError('#166 controlled HEAD preflight reached')\n"
        for name in ('run_round.py', 'check_ruler.py', 'session_domain.py',
                     'controls/run_all.py', 'controls/session_domain.py',
                     'check_frozen.py', 'compare.py', 'build_proxy.py',
                     'observer.py', 'native_evidence.py'):
            with self.subTest(entry=name):
                path = common.SCRIPT / name
                module = ModuleType('__main__')
                module.__file__ = str(path)
                output = io.StringIO()
                reached = False
                def git_read(command, **kwargs):
                    return (self.head + '\n').encode() if command[3] == 'rev-parse' else head_verifier
                with patch.dict(sys.modules, {'__main__': module}), \
                        patch.object(subprocess, 'check_output', side_effect=git_read), \
                        patch.object(common, 'verify_loaded_apparatus', return_value=None), \
                        patch.object(sys, 'argv', [str(path), '--help']), \
                        redirect_stdout(output), redirect_stderr(output):
                    # Isolate CLI dispatch from the separately tested inner
                    # verifier; deleting preflight must reach our assertion,
                    # not die in a fallback HEAD check with an unrelated error.
                    try:
                        exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
                    except RuntimeError as error:
                        reached = '#166 controlled HEAD preflight reached' in str(error)
                    except SystemExit:
                        reached = '#166 controlled HEAD preflight reached' in output.getvalue()
                self.assertTrue(reached, '#166 CLI dispatch must verify HEAD imports before main')


    def session_dispatch_process(self, body, name='session_domain.py'):
        """Run the actual __main__ block in a child; inject only its dependencies.

        Candidate bytes cannot pass their own HEAD check until committed. This
        focused subprocess still executes the unmodified dispatch/SystemExit
        statements, so neither return-code mapping nor exception handling is
        replaced by a test stub.
        """
        import ast
        path = common.SCRIPT / name
        source = path.read_bytes().decode()
        tree = ast.parse(source)
        blocks = [node for node in tree.body if isinstance(node, ast.If)
                  and ast.unparse(node.test) == "__name__ == '__main__'"]
        self.assertEqual(len(blocks), 1, '#166 CLI wrapper fixture must find exactly one dispatch')
        command = (
            'import json, subprocess\n'
            'import importlib.util, sys\n'
            'from pathlib import Path\n'
            f"_spec = importlib.util.spec_from_file_location('_guard', {str(common.SCRIPT / '_guard.py')!r})\n"
            "_guard = importlib.util.module_from_spec(_spec)\n"
            "_spec.loader.exec_module(_guard)\n"
            "run_entry = _guard.run_entry\n"
            "common = _guard.load('common', _guard.APPARATUS / 'common.py')\n"
            + body + '\n'
            # Inject the candidate preflight only; the dispatch and classifier
            # under test stay the real ones.
            "_guard.verify_cli_imports = _verify_cli_imports\n"
            + ast.get_source_segment(source, blocks[0]))
        return subprocess.run([sys.executable, '-I', '-B', '-c', command],
                              capture_output=True, text=True, timeout=10)

    def test_session_cli_dispatch_preserves_exit_codes(self):
        message = '#166 real CLI wrapper must preserve main exit code'
        for status, code in (('PASS', 0), ('FAIL', 1), ('BLOCKED', 2), ('NOT_RUN', 3)):
            with self.subTest(status=status):
                body = ("def _verify_cli_imports(): pass\n"
                        "def main():\n"
                        f"    print(json.dumps({{'status': {status!r}}}))\n    return {code}\n")
                result = self.session_dispatch_process(body)
                self.assertEqual(result.returncode, code, message + ': ' + result.stderr)
                self.assertEqual(result.stdout.strip(), json.dumps({'status': status}), message + ': JSON')
                self.assertEqual(result.stderr, '', message + ': no traceback')

    def test_session_cli_none_or_false_return_is_not_success(self):
        message = '#1685 a None/False main return must not become exit 0'
        for value in ('None', 'False'):
            with self.subTest(value=value):
                body = ("def _verify_cli_imports(): pass\n"
                        "def main():\n"
                        "    print(json.dumps({'status': 'PASS'}))\n"
                        f"    return {value}\n")
                result = self.session_dispatch_process(body)
                self.assertEqual(result.returncode, 2, message + ': ' + result.stderr)
                self.assertEqual(result.stdout.strip(), json.dumps({'status': 'PASS'}),
                                 message + ': PASS JSON must still be emitted')
                self.assertEqual(result.stderr, '', message + ': no traceback')

    def test_guard_run_entry_success_path_is_fail_closed(self):
        message = '#1685 run_entry must fail closed when main returns None/False'
        cases = ((None, 2), (False, 2), (0, 0), (1, 1), (2, 2), (3, 3), (True, True))
        with patch.object(_guard, 'verify_cli_imports', return_value=None):
            for value, expected in cases:
                with self.subTest(value=repr(value)):
                    with self.assertRaises(SystemExit, msg=message) as raised:
                        _guard.run_entry(lambda: value, 'traceback')
                    self.assertIs(raised.exception.code, expected,
                                  message + ': ' + repr(value))

    def test_session_cli_preflight_errors_are_blocked(self):
        message = '#166 CLI preflight failure must produce BLOCKED JSON/2'
        for error in ("ValueError('controlled HEAD drift')", "OSError('controlled missing git')",
                      "subprocess.CalledProcessError(1, ['git'])",
                      "subprocess.TimeoutExpired(['git'], 30)", "KeyboardInterrupt('controlled cancel')"):
            with self.subTest(error=error):
                body = (f"def _verify_cli_imports(): raise {error}\n"
                        "def main(): raise AssertionError('main reached after preflight refusal')\n")
                result = self.session_dispatch_process(body)
                self.assertEqual(result.returncode, 2, message + ': ' + result.stderr)
                self.assertIn('"status": "BLOCKED"', result.stdout, message)
                self.assertNotIn('main reached', result.stdout, message + ': main must not run')
                self.assertEqual(result.stderr, '', message + ': no traceback')

    def test_tools_cli_preflight_errors_are_blocked(self):
        message = '#166 every CLI preflight failure must produce BLOCKED JSON/2'
        for name in ('check_frozen.py', 'compare.py', 'build_proxy.py', 'run_round.py',
                     'check_ruler.py', 'controls/run_all.py', 'controls/session_domain.py',
                     'observer.py', 'native_evidence.py'):
            for error in ("ValueError('controlled HEAD drift')", "OSError('controlled missing git')",
                          "subprocess.CalledProcessError(1, ['git'])",
                          "subprocess.TimeoutExpired(['git'], 30)", "KeyboardInterrupt('controlled cancel')"):
                with self.subTest(entry=name, error=error):
                    body = (f"def _verify_cli_imports(): raise {error}\n"
                            "def main(): raise AssertionError('main reached after preflight refusal')\n")
                    result = self.session_dispatch_process(body, name)
                    self.assertEqual(result.returncode, 2, message + ': ' + result.stderr)
                    try:
                        report = json.loads(result.stdout)
                    except ValueError as escaped:
                        self.fail(message + ': invalid JSON: ' + str(escaped))
                    self.assertEqual(report.get('status'), 'BLOCKED', message + ': status')
                    self.assertTrue(report.get('error'), message + ': diagnostic')
                    self.assertNotIn('main reached', result.stdout, message + ': main must not run')
                    self.assertEqual(result.stderr, '', message + ': no traceback')

    def test_tools_cli_dispatch_preserves_exit_codes(self):
        message = '#166 every CLI wrapper must preserve main exit code'
        for name in ('check_frozen.py', 'compare.py', 'build_proxy.py', 'run_round.py',
                     'check_ruler.py', 'controls/run_all.py', 'controls/session_domain.py',
                     'observer.py', 'native_evidence.py'):
            for status, code in (('PASS', 0), ('FAIL', 1), ('BLOCKED', 2), ('NOT_RUN', 3)):
                with self.subTest(entry=name, status=status):
                    body = ("def _verify_cli_imports(): pass\n"
                            "def main():\n"
                            f"    print(json.dumps({{'status': {status!r}}}))\n    return {code}\n")
                    result = self.session_dispatch_process(body, name)
                    self.assertEqual(result.returncode, code, message + ': ' + result.stderr)
                    self.assertEqual(result.stdout.strip(), json.dumps({'status': status}), message + ': JSON')
                    self.assertEqual(result.stderr, '', message + ': no traceback')

    def test_cli_main_failures_preserve_pre_refactor_contract(self):
        message = '#991 every entry must preserve its measured main exception contract'
        fixture = (common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/'
                   'gateway-entry-guard/main-exit-contract.json')
        contract = json.loads(fixture.read_text())
        entries = {name for name in common.A2_SCRIPTS if name.endswith('.py')
                   and name not in ('common.py', '_guard.py', 'test_safety.py',
                                    'controls/prerequisites.py', 'linux_group_cleanup.py', 'host_suite.py',
                                    'retirement_native.py', 'retirement_cold.py', 'retirement_entries.py',
                                    'retirement_standard_http.py', 'retirement_public_http.py', 'test_retirement_standard_http.py',
                                    'test_retirement_public_http.py', 'test_project_clock_scope.py',
                                    'retirement_channel_v2.py', 'test_retirement_channel_v2.py',
                                    'test_channel_provenance.py')}
        self.assertEqual(set(contract['errors']),
                         {'ordinary', 'interrupt', 'timeout', 'permission', 'subprocess_timeout',
                          'apparatus', 'rule_only', 'system_exit'}, message + ': required exception classes')
        self.assertEqual(set(contract['entries']), entries, message + ': complete entry set')
        for name, cases in contract['entries'].items():
            self.assertEqual(set(cases), set(contract['errors']), message + ': complete exception set')
            for label, expected in cases.items():
                with self.subTest(entry=name, error=label):
                    body = ("class RuleOnly(ValueError): rule = 'APPARATUS_IMPORTS'\n"
                            "def _verify_cli_imports(): pass\n"
                            f"def main(): raise {contract['errors'][label]}\n")
                    result = self.session_dispatch_process(body, name)
                    self.assertEqual(result.returncode, expected['exit'], message + ': exit: ' + result.stderr)
                    self.assertTrue(result.stdout == expected['stdout'], message + ': exact stdout')
                    self.assertEqual('Traceback (most recent call last):' in result.stderr,
                                     expected['traceback'], message + ': traceback disposition')
                    last = result.stderr.splitlines()[-1] if result.stderr else ''
                    self.assertTrue(last == expected['stderr_last'], message + ': exact exception diagnostic')

    def test_guard_exit_codes_use_shared_mapping(self):
        message = '#991 guard must use shared exit codes including historical BLOCKED/1'
        codes = {'BLOCKED': 0, 'FAIL': 17}
        with patch.dict(common.BATCH_EXIT_CODES, codes):
            for policy, expected in [('round', 17), ('session', 0), ('ruler', 17),
                                     ('controls', 0)]:
                error = (common.ApparatusVerificationError('controlled') if policy == 'controls'
                         else ValueError('controlled'))
                child = 'FAIL' if policy in ('round', 'ruler') else 'BLOCKED'
                with patch.object(common, 'aggregate_status', wraps=common.aggregate_status) as shared:
                    self.assertEqual(_guard._failure(error, policy, common)[1], expected, message)
                self.assertEqual(shared.call_args_list, [unittest.mock.call(child)],
                                 '#991 guard failure must call the single shared aggregator: ' + policy)
            with patch.object(common, 'aggregate_status', wraps=common.aggregate_status) as shared:
                self.assertEqual(_guard._failure(TimeoutError('controlled'), 'ruler', common)[1], 0, message)
            self.assertEqual(shared.call_args_list, [unittest.mock.call('BLOCKED')],
                             '#991 guard failure must call the single shared aggregator: ruler blocked')
            with patch.object(_guard, 'verify_cli_imports', side_effect=ValueError('controlled')), \
                    patch('builtins.print'), self.assertRaises(SystemExit, msg=message) as raised:
                _guard.run_entry(lambda: None, 'traceback')
            self.assertEqual(raised.exception.code, 2,
                             '#991 untrusted shared mapping must never turn preflight refusal into success')
        with self.assertRaisesRegex(ValueError, '#991 unknown entry failure policy', msg=message):
            _guard._failure(ValueError('controlled'), 'unknown', common)

    def test_build_proxy_prerequisites_are_blocked_but_build_failure_is_fail(self):
        from contextlib import redirect_stdout
        import io
        message = '#166 build prerequisites must be BLOCKED/2 while compiler failure stays FAIL/1'
        entry = _load('build_proxy_status_entry', common.SCRIPT / 'build_proxy.py')
        for problems, error, expected in ((['unfrozen config'], None, 2),
                                          ([], OSError('controlled missing cargo'), 2),
                                          ([], None, 1)):
            with self.subTest(problems=problems, error=error), \
                    patch.object(entry, 'cargo_config_problems', return_value=problems), \
                    patch.object(subprocess, 'call', side_effect=error, return_value=1) as build, \
                    patch.object(sys, 'argv', ['build_proxy']), redirect_stdout(io.StringIO()) as output:
                code = entry.main()
                self.assertEqual(code, expected, message)
                self.assertEqual(build.call_count, 0 if problems else 1, message + ': build dispatch')
                if expected == 2:
                    self.assertIn('"status": "BLOCKED"', output.getvalue(), message + ': JSON')

    def test_session_lock_validation_errors_are_blocked(self):
        from contextlib import redirect_stdout
        import io
        message = '#166 lock verification failure must produce BLOCKED JSON/2'
        entry = _load('validation_entry', common.SCRIPT / 'session_domain.py')
        for error in (ValueError('controlled HEAD drift'), OSError('controlled missing git'),
                      subprocess.CalledProcessError(1, ['git']), subprocess.TimeoutExpired(['git'], 30)):
            with self.subTest(error=type(error).__name__), \
                    patch.object(entry, 'parity_lock', side_effect=error), \
                    patch.object(sys, 'argv', ['session_domain', '--suite', 'deadline', '--output', str(self.folder / 'out')]), \
                    redirect_stdout(io.StringIO()) as output:
                try:
                    code = entry.main()
                except Exception as escaped:
                    self.fail(message + ': escaped ' + repr(escaped))
                self.assertEqual(code, 2, message)
                self.assertIn('"status": "BLOCKED"', output.getvalue(), message)
                self.assertFalse((self.folder / 'out').exists(), message + ': no output directory')

    def test_session_deadline_uses_real_lease(self):
        from contextlib import contextmanager, redirect_stdout
        import io
        message = '#166 deadline must respect real lease and preserve predecessor'
        entry = _load('lease_entry', common.SCRIPT / 'session_domain.py')
        _load('import_isolation_probe', self.path)
        entered = []

        @contextmanager
        def private_lock(**kwargs):
            with common.verified_parity_lock(timeout=.02, lock=self.lock):
                entered.append(True)
                yield

        input_hashes = entry.hashes()
        with self.environment(), patch.object(entry, 'parity_lock', private_lock), \
                patch.object(entry, 'hashes', return_value=input_hashes):
            for mode in ('free', 'busy', 'cancelled'):
                with self.subTest(mode=mode):
                    from contextlib import ExitStack
                    output_path = self.folder / mode
                    with ExitStack() as stack:
                        if mode != 'free':
                            stack.enter_context(parity_lock(lock=self.lock))
                            before = (self.lock / 'owner.json').read_bytes()
                        if mode == 'cancelled':
                            stack.enter_context(patch.object(time, 'sleep', side_effect=KeyboardInterrupt('controlled cancellation')))
                        stack.enter_context(patch.object(sys, 'argv', ['session_domain', '--suite', 'deadline', '--output', str(output_path)]))
                        output = stack.enter_context(redirect_stdout(io.StringIO()))
                        code = entry.main()
                        self.assertEqual(code, 3 if mode == 'free' else 2, message + ': ' + output.getvalue())
                        self.assertIn('"status": "NOT_RUN"' if mode == 'free' else '"status": "BLOCKED"',
                                      output.getvalue(), message + ': JSON')
                        if mode != 'free':
                            self.assertFalse(output_path.exists(), message + ': no busy output')
                            self.assertEqual((self.lock / 'owner.json').read_bytes(), before, message + ': owner unchanged')
                    self.assertFalse(self.lock.exists(), message + ': lease released')
            self.assertEqual(entered, [True], message + ': only free request entered')

    def test_real_entries_verify_late_controls_before_execution(self):
        from contextlib import ExitStack, contextmanager, redirect_stdout
        import io
        message = '#166 late controls must be HEAD-verified before execution'
        sources = {p.relative_to(common.ROOT).as_posix(): p.read_bytes()
                   for p in common.SCRIPT.rglob('*.py') if 'node_modules' not in p.parts}
        def git_read(command, **kwargs):
            return ((self.head + '\n').encode() if command[3] == 'rev-parse'
                    else sources[command[-1].split(':', 1)[1]])
        class WorkReached(Exception):
            pass
        read = Path.read_bytes
        for name, control, alias in (('session_domain.py', 'session_domain.py', 'session_controls'),
                                     ('check_ruler.py', 'run_all.py', 'gateway_controls'),
                                     ('controls/prerequisites.py', '../common.py', 'prerequisite_common')):
            entry = _load('late_real_' + name[:-3], common.SCRIPT /
                          ('check_ruler.py' if name.startswith('controls/') else name))
            late_entry = (_load('late_real_prerequisites', common.SCRIPT / name)
                          if name.startswith('controls/') else entry)
            target = (common.SCRIPT / 'controls' / control).resolve()
            original = read(target)
            for mutant in (False, True):
                with self.subTest(entry=name, mutant=mutant), ExitStack() as stack:
                    stack.enter_context(patch.dict(sys.modules))
                    sys.modules.pop(alias, None)
                    executed = []
                    held = False
                    events = []
                    lease_states = []
                    lease_message = '#166 late controls load and verification must hold the lease'
                    @contextmanager
                    def recorded_lock(*args, **kwargs):
                        nonlocal held
                        held = True
                        events.append('acquired')
                        try:
                            yield
                        finally:
                            held = False
                            events.append('released')
                    def work(*args, **kwargs):
                        lease_states.append(('work', held))
                        executed.append(True)
                        events.append('work')
                        raise WorkReached
                    load = late_entry._load
                    verify = late_entry._verify_cli_imports
                    def late_load(module_name, path):
                        lease_states.append(('load', held))
                        events.append('load')
                        module = load(module_name, path)
                        module.run_all = module.run_controls = work
                        return module
                    def late_verify():
                        lease_states.append(('verify', held))
                        events.append('verify')
                        return verify()
                    if late_entry is not entry:
                        def prerequisite_work(*args):
                            late_entry.module_at(alias, target)
                            return work()
                        stack.enter_context(patch.object(entry, '_load', return_value=SimpleNamespace(run_controls=prerequisite_work)))
                        stack.enter_context(patch.object(entry, '_verify_cli_imports'))
                    stack.enter_context(patch.object(late_entry, '_load', side_effect=late_load))
                    changed = original + b'\n# controlled passive late-import mutant\n' if mutant else original
                    stack.enter_context(patch.object(Path, 'read_bytes', lambda path: changed if path.resolve() == target else read(path)))
                    stack.enter_context(patch.object(subprocess, 'check_output', side_effect=git_read))
                    stack.enter_context(patch.object(entry, 'parity_lock', recorded_lock))
                    stack.enter_context(patch.object(late_entry, '_verify_cli_imports', side_effect=late_verify))
                    stack.enter_context(patch.object(entry, 'baseline_dependency_hashes', return_value={}))
                    output_path = self.folder / (name + str(mutant))
                    args = ['--output', str(output_path)]
                    if name == 'session_domain.py':
                        args += ['--suite', 'calibrate']
                        for attr, value in [('E1_SOURCES', ()), ('hashes', lambda: {}),
                                            ('frozen_check', lambda: {'exitCode': 0}),
                                            ('run_fixture', lambda *a: {'exitCode': 0, 'launcherAndFixtureReaped': True}),
                                            ('validate_calibration', lambda *a: {})]:
                            stack.enter_context(patch.object(entry, attr, value))
                    else:
                        for attr, value in [('cargo_config_problems', lambda: []), ('apparatus_hashes', lambda: {}),
                                            ('run_round', lambda *a: None),
                                            ('compare', lambda *a: {'status': 'PASS', 'failures': []})]:
                            stack.enter_context(patch.object(entry, attr, value))
                    stack.enter_context(patch.object(sys, 'argv', [name, *args]))
                    output = stack.enter_context(redirect_stdout(io.StringIO()))
                    rejected = False
                    code = None
                    try:
                        code = entry.main()
                        rejected = code != 0 and '#166 loaded apparatus differs from HEAD' in output.getvalue()
                    except WorkReached:
                        pass
                    except (Exception, KeyboardInterrupt) as error:
                        self.fail(message + ': escaped ' + repr(error))
                    self.assertTrue(lease_states and all(state for _, state in lease_states),
                                    lease_message + ': ' + repr(lease_states))
                    self.assertEqual((rejected, executed), (True, []) if mutant else (False, [True]),
                                     message + ': ' + output.getvalue())
                    self.assertEqual(events, ['acquired', 'load', 'verify'] + ([] if mutant else ['work']) + ['released'],
                                     lease_message + ': ' + output.getvalue())
                    self.assertFalse(held, lease_message + ': released on either outcome')
                    if mutant:
                        blocked_message = '#166 late verification failure must be BLOCKED/2, not parity FAIL'
                        self.assertEqual(code, 2, blocked_message + ': ' + output.getvalue())
                        try:
                            summary = json.loads(output.getvalue())
                            report = (json.loads((output_path / 'report.json').read_text())
                                      if name == 'session_domain.py' else summary)
                        except (OSError, ValueError) as error:
                            self.fail(blocked_message + ': invalid report: ' + str(error))
                        self.assertEqual((report.get('status'), summary.get('status')), ('BLOCKED', 'BLOCKED'), blocked_message)
                        self.assertEqual(report.get('failure', {}).get('rule'), 'APPARATUS_IMPORTS', blocked_message)

    def test_session_late_validation_errors_are_distinct_from_evidence_failures(self):
        from contextlib import ExitStack, contextmanager, redirect_stdout
        import io
        message = '#166 late validation errors are BLOCKED/2 and ordinary evidence errors remain FAIL/1'
        entry = _load('late_error_entry', common.SCRIPT / 'session_domain.py')
        for stage in ('load', 'verify', 'work'):
            for error_type in (ValueError, OSError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt):
                # The ordinary evidence boundary has never caught generic RuntimeError.
                if stage == 'work' and error_type not in (ValueError, OSError):
                    continue
                with self.subTest(stage=stage, error=error_type.__name__), ExitStack() as stack:
                    held = False
                    def refuse(*args, **kwargs):
                        self.assertTrue(held, message + ': error injection holds lease')
                        raise error_type('controlled ' + stage + ' failure')
                    @contextmanager
                    def recorded_lock(*args, **kwargs):
                        nonlocal held
                        held = True
                        try:
                            yield
                        finally:
                            held = False
                    controls = SimpleNamespace(run_all=refuse)
                    stack.enter_context(patch.object(entry, 'parity_lock', recorded_lock))
                    stack.enter_context(patch.object(entry, '_load', side_effect=refuse if stage == 'load' else None,
                                                    return_value=controls))
                    stack.enter_context(patch.object(entry, '_verify_cli_imports',
                                                    side_effect=refuse if stage == 'verify' else None))
                    for attr, value in [('E1_SOURCES', ()), ('hashes', lambda: {}),
                                        ('baseline_dependency_hashes', lambda *a: {}),
                                        ('frozen_check', lambda: {'exitCode': 0}),
                                        ('run_fixture', lambda *a: {'exitCode': 0, 'launcherAndFixtureReaped': True}),
                                        ('validate_calibration', lambda *a: {})]:
                        stack.enter_context(patch.object(entry, attr, value))
                    path = self.folder / (stage + error_type.__name__)
                    stack.enter_context(patch.object(sys, 'argv', ['session_domain', '--suite', 'calibrate', '--output', str(path)]))
                    output = stack.enter_context(redirect_stdout(io.StringIO()))
                    try:
                        code = entry.main()
                        report = json.loads((path / 'report.json').read_text())
                        summary = json.loads(output.getvalue())
                    except (Exception, KeyboardInterrupt) as error:
                        self.fail(message + ': escaped ' + repr(error))
                    expected = ('FAIL', 1) if stage == 'work' else ('BLOCKED', 2)
                    self.assertEqual((report.get('status'), code), expected, message)
                    self.assertEqual(summary.get('status'), expected[0], message + ': JSON')
                    self.assertEqual(report.get('failure', {}).get('rule'),
                                     'EVIDENCE' if stage == 'work' else 'APPARATUS_IMPORTS', message + ': rule')
                    self.assertFalse(held, message + ': released')

    def test_ruler_late_validation_errors_are_distinct_from_evidence_failures(self):
        from contextlib import ExitStack, contextmanager, redirect_stdout, redirect_stderr
        import ast
        import io
        message = '#166 ruler late validation errors are BLOCKED/2/APPARATUS_IMPORTS; evidence stays exit 1'
        entry = _load('ruler_late_error_entry', common.SCRIPT / 'check_ruler.py')
        prerequisites = _load('ruler_late_error_prerequisites', common.SCRIPT / 'controls/prerequisites.py')
        source = (common.SCRIPT / 'check_ruler.py').read_text()
        blocks = [node for node in ast.parse(source).body if isinstance(node, ast.If)
                  and ast.unparse(node.test) == "__name__ == '__main__'"]
        self.assertEqual(len(blocks), 1, message + ': dispatch anchor')
        dispatch = compile(ast.get_source_segment(source, blocks[0]), entry.__file__, 'exec')
        for route in ('ruler', 'prerequisites'):
            for stage in ('load', 'verify', 'work'):
                for error_type in (ValueError, OSError, RuntimeError, subprocess.SubprocessError,
                                   PermissionError, TimeoutError, KeyboardInterrupt):
                    with self.subTest(route=route, stage=stage, error=error_type.__name__), ExitStack() as stack:
                        held = False
                        reached = []
                        @contextmanager
                        def recorded_lock(*args, **kwargs):
                            nonlocal held
                            held = True
                            try:
                                yield
                            finally:
                                held = False
                        def refuse(*args, **kwargs):
                            reached.append((stage, held))
                            raise error_type('controlled ' + stage + ' failure')
                        def work(*args, **kwargs):
                            if route == 'prerequisites':
                                prerequisites.module_at('ruler_error_probe', common.SCRIPT / 'common.py')
                            return refuse()
                        controls = SimpleNamespace(run_controls=work)
                        stack.enter_context(patch.object(entry, 'parity_lock', recorded_lock))
                        for attr, value in [('cargo_config_problems', lambda: []), ('apparatus_hashes', lambda: {}),
                                            ('baseline_dependency_hashes', lambda *a: {}), ('run_round', lambda *a: None),
                                            ('compare', lambda *a: {'status': 'PASS', 'failures': []})]:
                            stack.enter_context(patch.object(entry, attr, value))
                        stack.enter_context(patch.object(entry, '_load', return_value=controls,
                                                        side_effect=refuse if route == 'ruler' and stage == 'load' else None))
                        stack.enter_context(patch.object(entry, '_verify_cli_imports',
                                                        side_effect=refuse if route == 'ruler' and stage == 'verify' else None))
                        stack.enter_context(patch.object(prerequisites, '_load', return_value=SimpleNamespace(),
                                                        side_effect=refuse if stage == 'load' else None))
                        stack.enter_context(patch.object(prerequisites, '_verify_cli_imports',
                                                        side_effect=refuse if stage == 'verify' else None))
                        # The one guard owns the initial preflight; only that
                        # hook is stubbed so main's late verifier stays real.
                        stack.enter_context(patch.object(_guard, 'verify_cli_imports', lambda: None))
                        path = self.folder / (route + stage + error_type.__name__)
                        stack.enter_context(patch.object(sys, 'argv', ['check_ruler', '--output', str(path)]))
                        output = stack.enter_context(redirect_stdout(io.StringIO()))
                        errors = stack.enter_context(redirect_stderr(io.StringIO()))
                        code = None
                        try:
                            # Only initial preflight is stubbed; main and its late verifier
                            # retain their module globals and execute the real exception paths.
                            exec(dispatch, dict(entry.__dict__, __name__='__main__', _verify_cli_imports=lambda: None))
                        except SystemExit as exit_error:
                            code = exit_error.code
                        except (Exception, KeyboardInterrupt) as error:
                            self.fail(message + ': escaped ' + repr(error))
                        self.assertEqual(reached, [(stage, True)], message + ': injected path reached while held')
                        try:
                            summary = json.loads(output.getvalue())
                        except ValueError as error:
                            self.fail(message + ': invalid JSON: ' + str(error))
                        expected = ('FAIL', 1) if (stage == 'work' and error_type not in
                                                           (PermissionError, TimeoutError, KeyboardInterrupt)) else ('BLOCKED', 2)
                        self.assertEqual((summary.get('status'), code), expected, message)
                        self.assertEqual(summary.get('failure', {}).get('rule'),
                                         'EVIDENCE' if stage == 'work' else 'APPARATUS_IMPORTS', message + ': rule')
                        self.assertEqual(errors.getvalue(), '', message + ': no traceback')
                        self.assertFalse(held, message + ': lease released')

    def test_ruler_cargo_preflight_is_blocked_before_collection(self):
        from contextlib import ExitStack, contextmanager, redirect_stdout
        import io
        message = '#166 fix-r10 cargo preflight must return BLOCKED/2/CARGO_CONFIG before collection'
        entry = _load('ruler_cargo_error_entry', common.SCRIPT / 'check_ruler.py')
        for shared_code in (2, 17):
            with self.subTest(shared_code=shared_code), ExitStack() as stack:
                held = False
                @contextmanager
                def recorded_lock(*args):
                    nonlocal held
                    held = True
                    try:
                        yield
                    finally:
                        held = False
                stack.enter_context(patch.object(entry, 'install_shutdown', lambda: None))
                stack.enter_context(patch.object(entry, 'parity_lock', recorded_lock))
                stack.enter_context(patch.object(entry, 'cargo_config_problems', return_value=['controlled']))
                collect = stack.enter_context(patch.object(entry, 'run_round'))
                shared = stack.enter_context(patch.object(entry.common, 'aggregate_status',
                                                          return_value=('BLOCKED', shared_code)))
                stack.enter_context(patch.object(sys, 'argv', ['check_ruler', '--output', str(self.folder / 'out')]))
                output = stack.enter_context(redirect_stdout(io.StringIO()))
                try:
                    code = entry.main()
                    report = json.loads(output.getvalue())
                except (Exception, KeyboardInterrupt) as error:
                    self.fail('#166 fix-r10 cargo setup/execution error; invalid control: ' + repr(error))
                self.assertEqual((report.get('status'), code), ('BLOCKED', shared_code), message)
                self.assertEqual(report.get('failure', {}).get('rule'), 'CARGO_CONFIG', message)
                self.assertIn('controlled', report.get('failure', {}).get('detail', ''), message)
                self.assertEqual(collect.call_count, 0, message)
                self.assertEqual([call.args for call in shared.call_args_list], [('BLOCKED',)], message)
                self.assertFalse(held, message + ': lease released')

    def test_ruler_work_signals_are_blocked_after_lease_release(self):
        import signal
        message = '#166 fix-r10 work signals must return BLOCKED/2/EVIDENCE after lease release'
        # A fresh interpreter receives a real signal at an exact main() hook;
        # no polling or external signal-file race. Each child has a 15s bound.
        program = r'''
import ast, importlib.util, json, os, signal, sys
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace
path, folder, phase, signum = sys.argv[1:]
spec = importlib.util.spec_from_file_location('ruler_signal_entry', path)
entry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(entry)
held = False
@contextmanager
def lease(*args):
    global held
    held = True
    try:
        yield
    finally:
        held = False
        print('released', file=sys.stderr)
def cancel(*args):
    print('hook=' + phase + ';held=' + str(held), file=sys.stderr, flush=True)
    os.kill(os.getpid(), int(signum))
    raise AssertionError('signal did not interrupt hook')
checks = 0
def hashes():
    global checks
    checks += 1
    if checks == 2 and phase == 'postverify':
        cancel()
    return {}
real_save = entry.save
def save(path, value):
    if path.name == 'ruler.json' and phase == 'save':
        cancel()
    real_save(path, value)
entry.parity_lock = lease
entry.cargo_config_problems = lambda: []
entry.baseline_dependency_hashes = lambda *a: {}
entry.apparatus_hashes = hashes
entry.digest = lambda *a: 'audit'
entry.run_round = cancel if phase == 'collect' else lambda *a: None
entry.compare = cancel if phase == 'compare' else lambda *a: {'status': 'PASS', 'failures': []}
entry.save = save
entry._load = lambda *a: SimpleNamespace(run_controls=cancel if phase == 'controls' else lambda *a: {
    'cases': {}, 'prerequisites': {'passed': True, 'inputSha256': {}, 'auditSha256': 'audit'}})
entry._verify_cli_imports = lambda: None
entry._guard.verify_cli_imports = lambda: None
source = Path(path).read_text()
block, = [node for node in ast.parse(source).body if isinstance(node, ast.If)
          and ast.unparse(node.test) == "__name__ == '__main__'"]
sys.argv = ['check_ruler', '--output', folder]
exec(compile(ast.get_source_segment(source, block), path, 'exec'),
     dict(entry.__dict__, __name__='__main__'))
'''
        for phase in ('collect', 'compare', 'controls', 'postverify', 'save'):
            for signum in (signal.SIGINT, signal.SIGTERM):
                with self.subTest(phase=phase, signal=signum):
                    try:
                        result = subprocess.run([sys.executable, '-I', '-B', '-',
                            str(common.SCRIPT / 'check_ruler.py'),
                            str(self.folder / (phase + str(signum))), phase, str(signum)],
                            input=program, capture_output=True, text=True, timeout=15)
                    except (Exception, KeyboardInterrupt) as error:
                        self.fail('#166 fix-r10 signal child startup/timeout; invalid control: ' + repr(error))
                    hook = f'hook={phase};held=True\n'
                    self.assertIn(hook, result.stderr, '#166 fix-r10 signal hook not reached; invalid control')
                    if result.returncode != 2 and 'KeyboardInterrupt' not in result.stderr:
                        self.fail('#166 fix-r10 unrelated child failure; invalid control: ' + result.stderr)
                    self.assertEqual(result.returncode, 2, message + ': cancellation escaped')
                    try:
                        report = json.loads(result.stdout)
                    except ValueError as error:
                        self.fail('#166 fix-r10 invalid signal JSON; invalid control: ' + str(error))
                    self.assertEqual(result.stderr, hook + 'released\n', message)
                    self.assertEqual(report.get('status'), 'BLOCKED', message)
                    self.assertEqual(report.get('failure', {}).get('rule'), 'EVIDENCE', message)

    def test_standalone_controls_late_validation_is_blocked(self):
        from contextlib import ExitStack, redirect_stdout, redirect_stderr
        import ast
        import io
        message = '#166 fix-r10 standalone late imports must return BLOCKED/2/APPARATUS_IMPORTS only'
        entry = _load('standalone_controls_error_entry', common.SCRIPT / 'controls/run_all.py')
        prerequisites = entry.prerequisites_module
        source = Path(entry.__file__).read_text()
        blocks = [node for node in ast.parse(source).body if isinstance(node, ast.If)
                  and ast.unparse(node.test) == "__name__ == '__main__'"]
        self.assertEqual(len(blocks), 1, message + ': dispatch anchor')
        dispatch = compile(ast.get_source_segment(source, blocks[0]), entry.__file__, 'exec')
        for route in ('offline', 'source'):
            for stage in ('load', 'verify', 'work'):
                for error_type in (ValueError, OSError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt):
                    with self.subTest(route=route, stage=stage, error=error_type.__name__), ExitStack() as stack:
                        reached = []
                        def refuse(*args, **kwargs):
                            reached.append(stage)
                            raise error_type('controlled ' + stage)
                        def work(*args):
                            prerequisites.module_at('standalone_late_probe', common.SCRIPT / 'common.py')
                            return refuse()
                        # Keep the real module_at wrapping and real dispatch; only
                        # source/services work and HEAD preflight are test hooks.
                        stack.enter_context(patch.object(entry.common, 'verify_loaded_apparatus', lambda: None))
                        stack.enter_context(patch.object(entry, 'run_prerequisites', side_effect=work))
                        stack.enter_context(patch.object(entry, 'run_controls', side_effect=work))
                        stack.enter_context(patch.object(prerequisites, '_load', return_value=SimpleNamespace(),
                                                        side_effect=refuse if stage == 'load' else None))
                        stack.enter_context(patch.object(prerequisites, '_verify_cli_imports',
                                                        side_effect=refuse if stage == 'verify' else None))
                        # The one guard owns the initial preflight; the real
                        # run_entry classifier stays under test below.
                        stack.enter_context(patch.object(_guard, 'verify_cli_imports', lambda: None))
                        stack.enter_context(patch.object(sys, 'argv', ['run_all', '--output', str(self.folder / 'out'),
                            *(['--offline'] if route == 'offline' else ['--source', str(self.folder / 'source')])]))
                        output = stack.enter_context(redirect_stdout(io.StringIO()))
                        errors = stack.enter_context(redirect_stderr(io.StringIO()))
                        code = escaped = None
                        try:
                            exec(dispatch, dict(entry.__dict__, __name__='__main__', _verify_cli_imports=lambda: None))
                        except SystemExit as error:
                            code = error.code
                        except (Exception, KeyboardInterrupt) as error:
                            escaped = error
                        self.assertEqual(reached, [stage], message + ': real module_at reached')
                        if stage == 'work':
                            # The original controls CLI catches only provenance
                            # errors; ordinary work/cancellation still escapes.
                            self.assertIsInstance(escaped, error_type, message + ': original work exception escapes')
                            self.assertEqual(str(escaped), 'controlled work', message + ': original diagnostic')
                            self.assertIsNone(code, message + ': no synthetic exit code')
                            self.assertEqual(output.getvalue(), '', message + ': no synthetic JSON')
                        else:
                            self.assertIsNone(escaped, message + ': provenance error classified')
                            try:
                                report = json.loads(output.getvalue())
                            except ValueError as error:
                                self.fail(message + ': invalid standalone JSON: ' + str(error))
                            self.assertEqual((report.get('status'), code), ('BLOCKED', 2), message)
                            self.assertEqual(report.get('failure', {}).get('rule'), 'APPARATUS_IMPORTS', message)
                        self.assertEqual(errors.getvalue(), '', message + ': no traceback')

    def test_ruler_summary_uses_shared_status_without_changing_pending_review(self):
        from contextlib import ExitStack, nullcontext, redirect_stdout
        from unittest.mock import call
        import io
        message = '#166 fix-r9b ruler summary must share aggregation and preserve PENDING_REVIEW/2 or FAIL/1'
        entry = _load('ruler_summary_entry', common.SCRIPT / 'check_ruler.py')
        gates = ('self_compare', 'positive_controls', 'prerequisite_controls',
                 'same_bad_rejected', 'proxy_parity')
        for sentinel in (None, 17):
            for failed_gate in (None, *gates):
                with self.subTest(failed_gate=failed_gate, sentinel=sentinel), ExitStack() as stack:
                    path = self.folder / str(sentinel) / (failed_gate or 'all-green')
                    for index, label in enumerate(('ts1', 'ts2', 'ts3', 'ts4', 'ts5', 'proxy')):
                        common.save(path / label / 'run.json', {
                            'apparatusSha256': {}, 'baselineDepsSha256': {},
                            'side': 'rust' if label == 'proxy' else 'ts', 'label': label})
                        common.save(path / label / 'processes.json', [{
                            'name': 'gateway', 'pid': 100 if failed_gate == 'self_compare' else 100 + index}])
                    cases = {name: {'passed': failed_gate != 'same_bad_rejected', 'bothSides': True}
                             for name in ('both-sides-bad', 'corrupt-fingerprint', 'both-bad-disk',
                                          'both-bad-http', 'both-bad-wire-metadata')}
                    cases['positive'] = {'passed': failed_gate != 'positive_controls'}
                    controls = SimpleNamespace(run_controls=lambda *a: {
                        'cases': cases, 'prerequisites': {
                            'passed': failed_gate != 'prerequisite_controls',
                            'inputSha256': {}, 'auditSha256': 'audit'}})
                    for attr, value in [('install_shutdown', lambda: None),
                                        ('parity_lock', lambda *a: nullcontext()),
                                        ('cargo_config_problems', lambda: []),
                                        ('apparatus_hashes', lambda: {}),
                                        ('baseline_dependency_hashes', lambda *a: {}),
                                        ('digest', lambda *a: 'audit'),
                                        ('_load', lambda *a: controls),
                                        ('_verify_cli_imports', lambda: None),
                                        ('compare', lambda a, b: {
                                            'status': 'FAIL' if failed_gate == 'proxy_parity' and b.name == 'proxy' else 'PASS',
                                            'failures': []})]:
                        stack.enter_context(patch.object(entry, attr, value))
                    stack.enter_context(patch.object(entry, 'run_round',
                                                    side_effect=AssertionError('#166 fix-r9b unexpected live round; invalid test setup')))
                    aggregate = common.aggregate_status
                    shared = stack.enter_context(patch.object(entry.common, 'aggregate_status',
                        side_effect=lambda *states: (aggregate(*states)[0], sentinel)
                        if sentinel is not None else aggregate(*states)))
                    stack.enter_context(patch.object(sys, 'argv', ['check_ruler', '--output', str(path)]))
                    output = stack.enter_context(redirect_stdout(io.StringIO()))
                    try:
                        code = entry.main()
                        report = json.loads(output.getvalue())
                        saved = json.loads((path / 'ruler.json').read_text())
                    except (Exception, KeyboardInterrupt) as error:
                        self.fail('#166 fix-r9b main/report execution error; not a contract kill: ' + repr(error))
                    expected = ('PENDING_REVIEW', 2) if failed_gate is None else ('FAIL', 1)
                    if sentinel is not None:
                        expected = (expected[0], sentinel)
                    self.assertEqual((report.get('status'), code), expected, message)
                    self.assertEqual(saved, report, message + ': persisted report')
                    self.assertIsNone(report.get('independent_review'), message + ': review remains pending')
                    self.assertEqual([report.get(gate) for gate in gates],
                                     [gate != failed_gate for gate in gates], message + ': real gates exercised')
                    self.assertEqual(shared.call_args_list, [
                        call(*('PASS' if gate != failed_gate else 'FAIL' for gate in gates)),
                        call('BLOCKED' if failed_gate is None else 'FAIL')], message + ': shared status and exit')

    def test_main_code_is_verified_after_entry_disk_restore(self):
        from types import ModuleType
        clean = (b"import sys\n_apparatus_code = sys._getframe().f_code\nVALUE = 'clean'\n"
                 b"def slices(value): return value[1:5]\n"
                 b"NESTED = ('slice', (None, 1, 2))\nSET = {1, 2, 3}\n")
        self.expected[self.path] = clean
        for value in ('clean', 'mutant', 'metadata', 'missing-code'):
            with self.subTest(value=value):
                module = ModuleType('__main__')
                module.__file__ = str(self.path)
                source = clean.replace(b"'clean'", b"'mutant'") if value == 'mutant' else clean
                self.path.write_bytes(source)
                exec(compile(source, str(self.path), 'exec'), module.__dict__)
                if value == 'missing-code':
                    del module._apparatus_code
                elif value == 'metadata':
                    module._apparatus_code = module._apparatus_code.replace(co_qualname='mutant-main')
                self.path.write_bytes(clean)
                rejected = False
                with self.environment(), patch.dict(sys.modules, {'__main__': module}):
                    try:
                        with common.verified_parity_lock(timeout=.1, lock=self.lock):
                            pass
                    except ValueError as error:
                        rejected = '#166 loaded apparatus differs from HEAD' in str(error)
                self.assertEqual(rejected, value != 'clean',
                                 '#166 CLI entry code must be verified after disk restoration')

    def test_failed_import_does_not_poison_cache(self):
        self.path.write_bytes(b"raise RuntimeError('controlled import failure')\n")
        with self.assertRaisesRegex(RuntimeError, 'controlled import failure',
                                    msg='#166 failed import must reach controlled exception'):
            _load('import_isolation_probe', self.path)
        self.assertNotIn('import_isolation_probe', sys.modules,
                         '#166 failed import must not retain a partial module')
        self.path.unlink()
        with self.assertRaises(FileNotFoundError, msg='#166 missing source must fail closed'):
            _load('import_isolation_probe', self.path)
        self.assertNotIn('import_isolation_probe', sys.modules,
                         '#166 failed import must not retain a partial module')

    def test_unknown_cache_is_refused(self):
        imported = _load('import_isolation_probe', self.path)
        del imported._apparatus_source
        rejected = False
        try:
            _load('import_isolation_probe', self.path)
        except RuntimeError as error:
            rejected = '#166 unverified apparatus module cache' in str(error)
        self.assertTrue(rejected, '#166 cache without executed bytes must fail closed')
        with self.environment():
            with self.assertRaisesRegex(ValueError, '#166 loaded apparatus differs from HEAD',
                                        msg='#166 verifier must reject missing provenance too'):
                common.verify_loaded_apparatus()

    def test_clean_controls_do_not_reacquire_parent_lease(self):
        _load('import_isolation_probe', self.path)
        with self.environment(), parity_lock(lock=self.lock):
            before = (self.lock / 'owner.json').read_bytes()
            self.assertEqual(common.verify_loaded_apparatus(), self.head,
                             '#166 clean controls must verify while parent owns lease')
            self.assertEqual((self.lock / 'owner.json').read_bytes(), before,
                             '#166 controls must not replace parent lease')
        with self.environment(), common.verified_parity_lock(timeout=.1, lock=self.lock):
            self.assertTrue(self.lock.exists(), '#166 clean round must hold real parity lease')
        self.assertFalse(self.lock.exists(), '#166 clean round releases lease')

    def test_busy_or_cancelled_waiter_never_enters(self):
        _load('import_isolation_probe', self.path)
        with self.environment(), parity_lock(lock=self.lock):
            before = (self.lock / 'owner.json').read_bytes()
            with self.assertRaises(TimeoutError, msg='#166 never-finishing predecessor must time out'):
                with common.verified_parity_lock(timeout=.01, lock=self.lock):
                    self.fail('#166 busy predecessor must prevent work')
            with patch.object(time, 'sleep', side_effect=KeyboardInterrupt('controlled cancellation')):
                with self.assertRaises(KeyboardInterrupt, msg='#166 cancelled waiter must propagate'):
                    with common.verified_parity_lock(timeout=.1, lock=self.lock):
                        self.fail('#166 cancelled waiter must prevent work')
            self.assertEqual((self.lock / 'owner.json').read_bytes(), before,
                             '#166 rejected waiter must preserve predecessor')

    def test_pending_cleanup_keeps_verified_lease(self):
        _load('import_isolation_probe', self.path)
        with patch.object(common, 'LIVE_GROUPS', set()), self.environment():
            with common.verified_parity_lock(timeout=.1, lock=self.lock):
                common.LIVE_GROUPS.add('controlled-pending-child')
            pending = json.loads((self.lock / 'owner.json').read_text()).get('cleanupPending') if self.lock.exists() else None
            self.assertEqual(pending, ['controlled-pending-child'],
                             '#166 verified lock must preserve running cleanup registry')

    def test_session_busy_or_cancelled_lock_is_blocked(self):
        from contextlib import redirect_stdout
        import io
        entry = _load('blocked_entry', common.SCRIPT / 'session_domain.py')
        for error in (TimeoutError('controlled busy lock'), KeyboardInterrupt('controlled cancellation')):
            with self.subTest(error=type(error).__name__), \
                    patch.object(entry, 'parity_lock', side_effect=error), \
                    patch.object(sys, 'argv', ['session_domain', '--suite', 'deadline', '--output', str(self.folder / 'out')]), \
                    redirect_stdout(io.StringIO()) as output:
                code = entry.main()
                self.assertEqual(code, 2, '#166 session lock refusal must remain BLOCKED/2')
                self.assertIn('"status": "BLOCKED"', output.getvalue(), '#166 session lock refusal must report BLOCKED')
                self.assertFalse((self.folder / 'out').exists(), '#166 busy session must not write output')

    def test_entry_installs_shutdown_before_lock(self):
        from contextlib import contextmanager
        calls = []
        entry = _load('shutdown_entry', common.SCRIPT / 'session_domain.py')

        class StopBeforeWork(Exception):
            pass

        @contextmanager
        def stop_at_lock(**kwargs):
            self.assertEqual(calls, ['shutdown'], '#166 shutdown handler must precede lease acquisition')
            raise StopBeforeWork
            yield

        with patch.object(entry, 'install_shutdown', side_effect=lambda: calls.append('shutdown')), \
                patch.object(entry, 'parity_lock', stop_at_lock), \
                patch.object(sys, 'argv', ['session_domain', '--suite', 'deadline', '--output', str(self.folder / 'out')]):
            with self.assertRaises(StopBeforeWork, msg='#166 controlled lock boundary must be reached'):
                entry.main()

    def test_real_and_control_entries_enforce_import_verification(self):
        from contextlib import contextmanager

        class WorkReached(Exception):
            pass

        cases = [('run_round.py', ['--side', 'ts', '--label', 'probe'], 'run_round'),
                 ('check_ruler.py', [], 'cargo_config_problems'),
                 ('session_domain.py', ['--suite', 'deadline', '--output', str(self.folder / 'out')], 'hashes'),
                 ('controls/run_all.py', ['--offline', '--output', str(self.folder / 'out')], 'run_prerequisites'),
                 ('controls/session_domain.py', ['--case', 'healthy', '--output', str(self.folder / 'control-out')], 'check_case')]
        entries = [(name, args, hook, _load('isolation_entry_' + str(index), common.SCRIPT / name))
                   for index, (name, args, hook) in enumerate(cases)]
        self.path.write_bytes(b"VALUE = 'mutant'\n")
        _load('import_isolation_probe', self.path)
        self.path.write_bytes(self.clean)

        @contextmanager
        def isolated_lock(*args, **kwargs):
            with common.verified_parity_lock(timeout=.1, lock=self.lock):
                yield

        for name, args, hook, entry in entries:
            with self.subTest(entry=name):
                if not name.startswith('controls/'):
                    self.assertIs(entry.parity_lock, common.verified_parity_lock,
                                  '#166 real entry must use HEAD-backed verified parity lock')
                with self.environment(), patch.object(sys, 'argv', [name, *args]), \
                        patch.object(entry, hook, side_effect=WorkReached), \
                        patch.object(entry, 'parity_lock', isolated_lock, create=True):
                    rejected = False
                    try:
                        from contextlib import redirect_stdout
                        import io
                        with redirect_stdout(io.StringIO()) as output:
                            code = entry.main()
                        rejected = (code == 2 and '#166 loaded apparatus differs from HEAD' in output.getvalue())
                    except ValueError as error:
                        rejected = '#166 loaded apparatus differs from HEAD' in str(error)
                    except WorkReached:
                        pass
                    self.assertTrue(rejected, '#166 every real/control entry must reject imported mutant before work')


class ApparatusInputs(unittest.TestCase):
    """#52 / #85: unregistered files, installed deps and cargo configs are inputs."""

    def test_guard_failure_is_registered_aggregation_point(self):
        message = '#991 guard failure must remain a registered shared aggregation point'
        fixture = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py'
        audit = _load('guard_aggregation_audit', fixture)
        with patch.object(audit, 'md', SimpleNamespace(common=common)):
            report = audit.aggregation_audit()
        self.assertTrue(report['passed'], message + ': ' + repr(report['failures']))
        self.assertEqual(report['points'].get('guard-failure', {}).get('shared'), True, message)

    def test_deadline_not_run_has_dedicated_exit_code(self):
        # Keep shared-contract assertions in the existing audited test entry.
        for children in ((), [], (None,), (None, None)):
            with self.subTest(children=children):
                self.assertTrue(common.aggregate_status(*children) == ('FAIL', 1),
                                '#952 empty or all-None aggregation must be FAIL/1')

        from itertools import product
        states = ('PASS', 'PASS_WITH_SKIPS', 'NOT_RUN', 'BLOCKED', 'FAIL')
        codes = (0, 1, 3, 2, 1)
        for size in (1, 2, 3):
            for children in product(states, repeat=size):
                worst = max(states.index(child) for child in children)
                expected = (states[worst], codes[worst])
                for optional in (children, (None, *children, None)):
                    with self.subTest(children=optional):
                        self.assertTrue(common.aggregate_status(*optional) == expected,
                                        '#952 nonempty aggregation must preserve status order and exit codes')
        for unknown in ('', 'UNKNOWN', False, 0, [], {}):
            for children in ((unknown,), ('PASS', None, unknown), (unknown, 'FAIL')):
                with self.subTest(children=children):
                    self.assertTrue(common.aggregate_status(*children) == ('FAIL', 1),
                                    '#952 unknown children must remain FAIL/1')

        # State/exit mutations run under the negative runner's parity lease.
        # Exercise the actual main body in-process; CLI isolation is tested by
        # ImportIsolation, so a HEAD refusal cannot masquerade as a status kill.
        from contextlib import nullcontext, redirect_stdout
        import io
        entry = _load('session_domain', common.SCRIPT / 'session_domain.py')
        # Isolate status aggregation from the separately tested frozen gate.
        with tempfile.TemporaryDirectory(prefix='pdrs-166-deadline-') as folder, \
                patch.object(entry, 'frozen_check', return_value={'exitCode': 0}):
            output = Path(folder) / 'deadline'
            with patch.object(entry, 'parity_lock', return_value=nullcontext()), \
                    patch.object(sys, 'argv', ['session_domain', '--suite', 'deadline', '--output', str(output)]), \
                    redirect_stdout(io.StringIO()):
                code = entry.main()
            self.assertEqual(code, 3, '#166 NOT_RUN must exit 3')
            report = json.loads((output / 'report.json').read_text())
            for key in ('status', 'executionStatus', 'acceptanceStatus'):
                self.assertEqual(report[key], 'NOT_RUN', f'#166 {key} must disclose no execution')
            self.assertEqual(report['structureStatus'], 'PASS', '#166 structure validation remains separate')
            self.assertEqual(len(report['cells']), 4, '#166 all four unexecuted cells remain')
            for cell in report['cells']:
                self.assertEqual(cell['executedCases'], [], '#166 no execution is fabricated')
            with patch.object(entry, 'parity_lock', return_value=nullcontext()), \
                    patch.object(sys, 'argv', ['session_domain', '--suite', 'calibrate', '--baseline',
                                              str(Path(folder) / 'missing'), '--output', str(Path(folder) / 'blocked')]), \
                    redirect_stdout(io.StringIO()):
                code = entry.main()
            self.assertEqual(code, 2, '#166 BLOCKED remains distinct from NOT_RUN')
            # #1140 native scoring wiring: a witness capture scores the
            # rust-host/rust-sidecar cell against the registered A19-HOST
            # expectations while the three cells without evidence stay NOT_RUN,
            # so the batch cannot be PASS.  The observed batch status is also
            # re-derived through the one shared aggregator (#892 runtime
            # observation, never a static exit proof).
            controls = _load('session_controls', common.SCRIPT / 'controls/session_domain.py')
            with tempfile.TemporaryDirectory(prefix='pdrs-1140-native-') as native:
                fixture = controls.native_capture_fixture(Path(native))
                output = Path(native) / 'deadline-native'
                with patch.object(entry, 'parity_lock', return_value=nullcontext()), \
                        patch.object(sys, 'argv', ['session_domain', '--suite', 'deadline',
                                                   '--native-capture', str(fixture),
                                                   '--output', str(output)]), \
                        redirect_stdout(io.StringIO()):
                    code = entry.main()
                self.assertTrue(code == 1 and json.loads((output / 'report.json').read_text())['failure']['rule'] == 'NATIVE_CAPTURE',
                                '#1140 synthetic control cannot become formal native evidence')
                report = controls.control_deadline_report(fixture)
                self.assertTrue(report['status'] == common.aggregate_status(*[cell['status'] for cell in report['cells']])[0],
                                '#892 observed batch status equals the one shared aggregation')
                self.assertTrue(report['cells'][-1]['status'] == 'PASS' and report['status'] == 'NOT_RUN',
                                '#1140 labelled control only tests native cell aggregation')
                with patch.object(entry, 'parity_lock', return_value=nullcontext()), \
                        patch.object(sys, 'argv', ['session_domain', '--suite', 'deadline',
                                                   '--native-capture', str(Path(native) / 'missing-capture'),
                                                   '--output', str(Path(native) / 'deadline-blocked')]), \
                        redirect_stdout(io.StringIO()):
                    code = entry.main()
                self.assertEqual(code, 2, '#1140 a missing capture directory is BLOCKED')
                self.assertFalse((Path(native) / 'deadline-blocked').exists(),
                                 '#1140 BLOCKED creates no output directory')
        for status, code in [('PASS', 0), ('FAIL', 1), ('PASS_WITH_SKIPS', 1),
                             ('BLOCKED', 2), ('NOT_RUN', 3)]:
            self.assertEqual(common.aggregate_status(status), (status, code), '#166 shared exit contract')
        for verdict, expected in [('KILLED', ('PASS', 0)), ('SURVIVED', ('FAIL', 1)),
                                  ('INVALID', ('BLOCKED', 2))]:
            self.assertEqual(common.aggregate_status(common.MUTATION_STATUS[verdict]), expected,
                             '#166 source control verdict mapping')

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-apparatus-inputs-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    # Shared probes (common.APPARATUS_PROBES): the kind says how to materialize
    # the shape, and every one must be rejected.  The same table and the same
    # materialize_probe()/remove_probe() implementation drive
    # controls/prerequisites.py, so the two machine controls cannot drift apart.
    def test_every_shared_probe_is_rejected(self):
        outside = Path(tempfile.mkdtemp(prefix='pdrs-apparatus-outside-', dir=self.root.parent))
        self.addCleanup(shutil.rmtree, outside, ignore_errors=True)
        (outside / '__init__.py').write_text('shadow package')
        (outside / 'evil.py').write_text('shadow module')
        skipped = {}
        for kind, name in APPARATUS_PROBES:
            # Each shape is its own subTest.  A host-degenerate skip records the
            # entry and continues: raising SkipTest here would abort the whole
            # method and silently stop the remaining probes (fix-r6 P2).
            with self.subTest(kind=kind, name=name):
                try:
                    path = materialize_probe(self.root, kind, name, outside)
                except ProbeSkipped as error:
                    skipped[(kind, name)] = str(error)
                    print(f'[SKIPPED] {kind}:{name}: {error}', file=sys.stderr)
                    continue
                # The scan reports the first __pycache__ directory itself, not
                # each file below it (it never descends into one).
                expected = probe_expected(name)
                try:
                    if kind == 'unreadable-dir':
                        # onerror must surface the failure, never skip the tree.
                        with self.assertRaises(OSError):
                            unknown_apparatus_files(self.root)
                    else:
                        self.assertEqual(unknown_apparatus_files(self.root), [expected])
                finally:
                    remove_probe(kind, path)
        self.assertLessEqual(set(skipped), set(DEGENERATE_PROBES),
                             f'undeclared probe skip: {skipped}')
        print(f'probes: {len(APPARATUS_PROBES)} shapes, {len(skipped)} declared '
              f'degenerate skips, {len(APPARATUS_PROBES) - len(skipped)} exercised',
              file=sys.stderr)
        self.assertEqual(unknown_apparatus_files(self.root), [])

    def test_unknown_probe_kind_is_an_error(self):
        # A new table kind must fail loudly in both consumers instead of being
        # silently materialized as a plain file (the old else-branch drift).
        outside = Path(tempfile.mkdtemp(prefix='pdrs-probe-kind-', dir=self.root.parent))
        self.addCleanup(shutil.rmtree, outside, ignore_errors=True)
        with self.assertRaises(ValueError):
            materialize_probe(self.root, 'fifo', 'pipe', outside)
        with self.assertRaises(ValueError):
            remove_probe('fifo', self.root / 'pipe')
        self.assertFalse((self.root / 'pipe').exists())
        self.assertEqual(unknown_apparatus_files(self.root), [])

    def test_a2_entries_share_one_verbatim_path_bootstrap(self):
        # Every A2 entry that touches common carries one identical inline
        # preamble: -I before other stdlib imports, bytecode redirect, capture
        # the executing code object, load _guard by explicit path.  It cannot
        # move into common.py (importing common is what it defends), so the
        # machine check stops a future entry from reverting to ``from common
        # import``/``sys.path`` or from copying the guard body again.
        folder = Path(__file__).resolve().parent
        blocks = {}
        for name in common.A2_SCRIPTS + common.A3_SCRIPTS:
            if name in ('common.py', '_guard.py') or not name.endswith('.py'):
                continue
            text = (folder / name).read_text(encoding='utf-8')
            if 'common' not in text and '_load(' not in text:
                continue
            start = text.find(BOOTSTRAP_START)
            self.assertNotEqual(start, -1, f'#701 {name}: missing apparatus bootstrap')
            end = text.find(BOOTSTRAP_END, start)
            self.assertNotEqual(end, -1, f'#1140 {name}: unterminated apparatus bootstrap')
            blocks[name] = text[start:end + len(BOOTSTRAP_END)]
            prefix = text[:start]
            self.assertNotRegex(prefix, r'sys[.]path[.]insert', '#1140 no path insertion before bootstrap: ' + name)
            self.assertNotRegex(prefix, APPARATUS_IMPORT, '#1140 no apparatus import before bootstrap: ' + name)
        self.assertTrue(sorted(blocks) == sorted([
            'build_proxy.py', 'check_frozen.py', 'check_ruler.py', 'compare.py',
            'native_evidence.py', 'observer.py', 'run_round.py', 'session_domain.py', 'test_safety.py',
            'controls/run_all.py', 'controls/prerequisites.py',
            'controls/session_domain.py', 'host_suite.py', 'retirement_native.py',
            'retirement_cold.py', 'retirement_entries.py', 'retirement_standard_http.py',
            'retirement_public_http.py', 'test_retirement_standard_http.py', 'test_retirement_public_http.py',
            'test_project_clock_scope.py', 'retirement_channel_v2.py',
            'test_retirement_channel_v2.py', 'test_channel_provenance.py']),
            '#1140 complete standard bootstrap entry set')
        self.assertEqual(len(set(blocks.values())), 1,
                         '#1140 apparatus bootstrap differs between entries')
        block = next(iter(blocks.values()))
        self.assertIn('sys.flags.isolated', block)
        self.assertLess(block.index('sys.flags.isolated'),
                        block.index('sys.pycache_prefix'),
                        'isolated-mode guard must precede module loading')
        # #701 item 2: the -I guard must precede every other stdlib import, so a
        # shadowed argparse/pathlib cannot execute before it.  ``sys`` is the
        # only import allowed ahead of the check (the interpreter preloads it).
        import ast
        for name, text in blocks.items():
            tree = ast.parse(((folder / name).read_text(encoding='utf-8')))
            imports = [node for node in ast.walk(tree) if isinstance(node, (ast.Import, ast.ImportFrom))]
            first = imports[0]
            self.assertIsInstance(first, ast.Import, '#991 first import must be sys: ' + name)
            self.assertEqual([alias.name for alias in first.names], ['sys'],
                             '#991 first import must be sys: ' + name)
            checks = [node for node in ast.walk(tree)
                      if isinstance(node, ast.If)
                      and 'sys.flags.isolated' in ast.unparse(node.test)]
            self.assertEqual(len(checks), 1, '#991 one isolated-mode check: ' + name)
            later = [node.lineno for node in imports if node is not first]
            self.assertTrue(later, '#991 entry imports beyond sys: ' + name)
            self.assertLess(checks[0].lineno, min(later),
                            '#991 -I check must precede every other import: ' + name)

    def test_cli_main_is_only_the_one_guard_call(self):
        # The guard's single implementation is only useful if no entry keeps a
        # private bootstrap/classifier.  Each product CLI __main__ is exactly
        # ``run_entry(main, policy)``; the test runner test_safety.py stays
        # unittest.main() so the matrix itself can run before commit.
        import ast
        folder = Path(__file__).resolve().parent
        entries = [name for name in common.A2_SCRIPTS if name.endswith('.py')
                   and name not in ('common.py', '_guard.py', 'test_safety.py',
                                    'controls/prerequisites.py', 'linux_group_cleanup.py', 'host_suite.py',
                                    'retirement_native.py', 'retirement_cold.py', 'retirement_entries.py',
                                    'retirement_standard_http.py', 'retirement_public_http.py', 'test_retirement_standard_http.py',
                                    'test_retirement_public_http.py', 'test_project_clock_scope.py',
                                    'retirement_channel_v2.py', 'test_retirement_channel_v2.py',
                                    'test_channel_provenance.py')]
        for name in entries:
            with self.subTest(entry=name):
                text = (folder / name).read_text(encoding='utf-8')
                tree = ast.parse(text)
                blocks = [node for node in tree.body if isinstance(node, ast.If)
                          and ast.unparse(node.test) == "__name__ == '__main__'"]
                self.assertEqual(len(blocks), 1, '#991 one __main__ dispatch: ' + name)
                self.assertEqual(blocks[0].orelse, [], '#991 __main__ must have no alternate dispatch: ' + name)
                calls = blocks[0].body
                message = '#991 __main__ must be exactly run_entry(main, policy): ' + name
                self.assertEqual(len(calls), 1, message)
                self.assertIsInstance(calls[0], ast.Expr, message)
                call = calls[0].value
                self.assertIsInstance(call, ast.Call, message)
                self.assertEqual(ast.unparse(call.func), 'run_entry', message)
                self.assertEqual(len(call.args), 2, message)
                self.assertEqual(ast.unparse(call.args[0]), 'main', message)
                self.assertIsInstance(call.args[1], ast.Constant, message)
                self.assertIn(call.args[1].value,
                              ('traceback', 'round', 'session', 'ruler', 'controls'), message)
                self.assertEqual(call.keywords, [], message)
                self.assertFalse(any(isinstance(node, (ast.Try, ast.ExceptHandler))
                                     for node in ast.walk(blocks[0])),
                                 '#991 no private except classification: ' + name)
                entry = _load('guard_shape_' + name.replace('/', '_')[:-3], folder / name)
                self.assertIs(entry.run_entry, _guard.run_entry,
                              '#991 entry must bind the one run_entry: ' + name)
                for private in ('_load', '_has_git_checkout', '_verify_cli_imports'):
                    self.assertFalse(any(isinstance(node, ast.FunctionDef) and node.name == private
                                         for node in tree.body),
                                     f'#991 {name} must alias, not redefine, {private}')
        # A library-only entry has no dispatch but still aliases the guard.
        prerequisites = _load('guard_shape_prerequisites', folder / 'controls/prerequisites.py')
        self.assertIs(prerequisites._load, _guard.load, '#991 library must alias guard load')
        self.assertIs(prerequisites._verify_cli_imports, _guard.verify_cli_imports,
                      '#991 library must alias guard verifier')
        self.assertIs(_load, _guard.load, '#991 test runner must alias guard load')
        self.assertIs(run_entry, _guard.run_entry, '#991 test runner must alias guard dispatch')

    def test_suite_libraries_are_authenticated_and_have_no_unmeasured_cli(self):
        import ast
        for name in ('host_suite.py', 'retirement_native.py', 'retirement_cold.py', 'retirement_entries.py',
                     'retirement_standard_http.py', 'retirement_public_http.py',
                     'retirement_channel_v2.py'):
            with self.subTest(library=name):
                path = Path(__file__).resolve().parent / name
                tree = ast.parse(path.read_text(encoding='utf-8'))
                self.assertIn(name, common.A2_SCRIPTS)
                self.assertFalse(any(isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                                     and node.name == 'main' for node in tree.body),
                                 '#1314 suite library must not introduce an unmeasured CLI contract')
                self.assertFalse(any(isinstance(node, ast.If) and '__name__' in ast.unparse(node.test)
                                     for node in ast.walk(tree)),
                                 '#1314 suite library must have no independent dispatch')
                entry = _load('guard_shape_' + path.stem, path)
                self.assertEqual(Path(entry.__file__).resolve(), path.resolve(),
                                 '#1314 library must be loaded through authenticated explicit path')

    def test_native_runtime_receipt_rejects_library_substitution(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-native-runtime-') as folder:
            root = Path(folder).resolve()
            profile = 'pilotdeck-libs-' + 'a' * 64
            library = root / profile / 'libvips.so.42'
            library.parent.mkdir()
            library.write_bytes(b'original native library')
            native = Path(sys.executable).resolve().read_bytes()
            programs = ('pilotdeck', 'pilotdeck-control', 'pilotdeck-gateway', 'pilotdeck-agent-loop-sidecar')
            def executable(program, contents):
                (root / program).write_bytes(contents)
                (root / program).chmod(0o700)
            def binary_receipt():
                common.save(root / 'manifest.json', [
                    {'path': name, 'sha256': common.digest(root / name),
                     'bytes': (root / name).stat().st_size} for name in programs])
            for program in programs:
                executable(program, native + (profile.encode() if program != programs[-1] else b''))
            binary_receipt()
            rows = [{'path': str(library.relative_to(root)), 'sha256': common.digest(library),
                     'bytes': library.stat().st_size}]
            manifest = root / 'libraries.json'
            common.save(manifest, rows)
            source = {'files': [{'path': 'src/pilotdeck-image-native/Cargo.toml'}],
                      'runtimeLibraries': {'path': str(manifest), 'sha256': common.digest(manifest)}}
            check = lambda: common.verify_native_runtime_libraries(source, root / 'manifest.json')
            self.assertEqual(check(), rows)
            original_program = (root / 'pilotdeck').read_bytes()
            with (root / 'pilotdeck').open('wb') as large:
                large.write(b'\x7fELF')
                large.truncate(2 * 1024 * 1024 * 1024 + 1)
            with self.assertRaisesRegex(ValueError, 'invalid native executable'):
                check()
            executable('pilotdeck', original_program)
            self.assertEqual(check(), rows)
            original = library.read_bytes()
            library.write_bytes(b'changed native library!')
            with self.assertRaisesRegex(ValueError, 'library bytes'):
                check()
            library.write_bytes(original)
            extra = library.parent / 'libextra.so'
            extra.write_bytes(b'extra')
            with self.assertRaisesRegex(ValueError, 'unregistered runtime files'):
                check()
            extra.unlink()
            executable('pilotdeck', native + b'wrong profile pilotdeck-libs-' + b'b' * 64)
            binary_receipt()
            with self.assertRaisesRegex(ValueError, 'binary runtime profile'):
                check()
            executable('pilotdeck', native + profile.encode())
            binary_receipt()
            outside = root / 'replacement'
            outside.write_bytes(original)
            library.unlink()
            library.symlink_to(outside)
            with self.assertRaisesRegex(ValueError, 'library bytes'):
                check()
            library.unlink()
            library.write_bytes(original)
            common.save(manifest, rows + rows)
            source['runtimeLibraries']['sha256'] = common.digest(manifest)
            with self.assertRaisesRegex(ValueError, 'duplicate entry'):
                check()
            common.save(manifest, rows)
            source['runtimeLibraries']['sha256'] = common.digest(manifest)
            self.assertEqual(check(), rows)
            del source['runtimeLibraries']
            with self.assertRaisesRegex(ValueError, 'missing manifest binding'):
                check()
            source['files'] = []
            with self.assertRaisesRegex(ValueError, 'missing manifest binding'):
                check()
            for program in programs:
                executable(program, native)
            binary_receipt()
            self.assertEqual(check(), [])
            executable('pilotdeck-control', b'')
            binary_receipt()
            with self.assertRaisesRegex(ValueError, 'invalid native executable'):
                check()
            executable('pilotdeck-control', b'legacy binary without native images')
            binary_receipt()
            with self.assertRaisesRegex(ValueError, 'native executable format'):
                check()
            executable('pilotdeck-control', native)
            binary_receipt()
            executable('pilotdeck-control', native + b'modified after receipt')
            with self.assertRaisesRegex(ValueError, 'binary hash or size'):
                check()
            executable('pilotdeck-control', native)
            binary_receipt()
            self.assertEqual(check(), [])

    def test_a2_entries_never_resolve_apparatus_through_sys_path(self):
        # Covers entries that do not mention ``common`` but still import a
        # sibling module by name; sys.path lookup is the P1 bypass in both cases.
        folder = Path(__file__).resolve().parent
        for name in common.A2_SCRIPTS + common.A3_SCRIPTS:
            if not name.endswith('.py') or name == 'common.py':
                continue
            text = (folder / name).read_text(encoding='utf-8')
            self.assertNotRegex(text, r'sys[.]path[.]insert', '#701 apparatus path lookup: ' + name)
            self.assertNotRegex(text, APPARATUS_IMPORT, '#701 apparatus sibling import: ' + name)

    def test_a3_entries_are_scanned_by_both_guards(self):
        folder = Path(__file__).resolve().parent
        read = Path.read_text
        target = folder / 'check_schema.py'
        for method in ('test_a2_entries_share_one_verbatim_path_bootstrap',
                       'test_a2_entries_never_resolve_apparatus_through_sys_path'):
            with self.subTest(method=method):
                baseline = unittest.TestResult()
                ApparatusInputs(method).run(baseline)
                self.assertTrue(baseline.wasSuccessful(), '#701 A3 scanner positive must pass')
                result = unittest.TestResult()
                with patch.object(Path, 'read_text', lambda path, *a, **k:
                                  read(path, *a, **k) + ('\nimport common\n' if path == target else '')):
                    ApparatusInputs(method).run(result)
                self.assertFalse(result.errors, '#701 A3 scanner must fail its bound assertion')
                self.assertEqual(len(result.failures), 1, '#701 A3 scanner must inspect check_schema.py')
                self.assertIn('#701', result.failures[0][1], '#701 A3 scanner requires dedicated failure')

    def test_apparatus_import_pattern_covers_import_lists(self):
        # A hand-maintained tuple can silently miss an A2 module; the
        # derivation and the comma form of ``import a, b`` both need a machine
        # check, otherwise this guard stops covering the omitted module.
        self.assertIn('observer', APPARATUS_MODULES, '#701 A2 observer must be guarded')
        self.assertIn('run_all', APPARATUS_MODULES, '#701 A2 run_all must be guarded')
        self.assertIn('check_schema', APPARATUS_MODULES, '#701 A3 module must be guarded')
        self.assertIn('_guard', APPARATUS_MODULES, '#991 the one guard must be guarded')
        for name in common.A2_SCRIPTS + common.A3_SCRIPTS:
            if not name.endswith('.py'):
                continue
            stem = Path(name).stem
            for text in (f'import {stem}\n', f'import os, {stem} as m\n',
                         f'from {stem} import x\n'):
                self.assertRegex(text, APPARATUS_IMPORT, f'#701 registered module {stem}')
        for text in ('import common\n', 'import os, common\n',
                     'import os, sys, compare as c\n', 'from common import x\n'):
            self.assertRegex(text, APPARATUS_IMPORT)
        for text in ('import os, commonish\n', 'import os, mycommon\n'):
            self.assertNotRegex(text, APPARATUS_IMPORT)

    def test_controls_entry_ignores_controls_common_shadow(self):
        # The controls/ entries must resolve common.py from the apparatus dir,
        # never from their own directory: a controls/common.py is not a second
        # canonical module, and preferring it would be the same P1 bypass one
        # level down (fix-r6 P1).
        #
        # The shadow is written into a throwaway copy of the apparatus, never
        # the real tree: addCleanup does not run on SIGKILL, and a stray
        # controls/common.py would make every later scan report an unregistered
        # file (false FAIL) until someone deletes it by hand (fix-r7 P1).
        folder = Path(__file__).resolve().parent
        git_status = ['git', '-C', str(folder), 'status', '--porcelain', '--', '.']
        before = subprocess.check_output(git_status, timeout=30)
        self.assertFalse((folder / 'controls/common.py').exists(),
                         'real apparatus tree has a controls/common.py residue')
        tree = self.root / 'apparatus'
        # Dereference links while copying: a linked controls/ directory must
        # not turn the shadow write below into a write through to its target.
        shutil.copytree(folder, tree,
                        ignore=shutil.ignore_patterns('node_modules', '__pycache__'))
        shadow = tree / 'controls/common.py'
        marker = self.root / 'controls-common-executed'
        shadow.write_text("from pathlib import Path\n"
                          f"Path({str(marker)!r}).write_text('executed')\n"
                          "raise RuntimeError('controls/common.py must not be loaded')\n",
                          encoding='utf-8')
        saved = sys.modules.get('common')
        saved_guard = sys.modules.get('_guard')
        try:
            module = _load('probe_controls_prerequisites',
                           tree / 'controls/prerequisites.py')
            self.assertEqual(Path(module.common.__file__).resolve(),
                             (tree / 'common.py').resolve())
            self.assertFalse(marker.exists())
        finally:
            for name, previous in (('common', saved), ('_guard', saved_guard)):
                if previous is None:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = previous
        after = subprocess.check_output(git_status, timeout=30)
        self.assertEqual(after, before, 'test wrote into the real apparatus tree')

    def test_skipped_probe_is_not_counted_as_passed(self):
        # Three-state gate (fix-r6 P2): a skipped control is listed but never
        # satisfies ``all(passed)``.  Only declared host-degenerate skips are
        # tolerated; an undeclared skip fails, and a skip-only report cannot pass.
        prerequisites = _load('prerequisites',
                              Path(__file__).resolve().parent / 'controls/prerequisites.py')
        declared = 'apparatus-unregistered-case-variant-COMMON.PY'
        results = {
            'ok': {'passed': True, 'skipped': False, 'detail': ''},
            declared: {'passed': None, 'skipped': True, 'detail': 'SKIPPED: APFS folds case'},
        }
        passed, skipped, unexpected = prerequisites.summarize_results(results, {declared})
        self.assertTrue(passed)
        self.assertEqual(skipped, {declared: 'SKIPPED: APFS folds case'})
        self.assertEqual(unexpected, [])
        results['apparatus-unregistered-file-surprise'] = {
            'passed': None, 'skipped': True, 'detail': 'SKIPPED: unexpected'}
        passed, skipped, unexpected = prerequisites.summarize_results(results, {declared})
        self.assertFalse(passed)
        self.assertEqual(unexpected, ['apparatus-unregistered-file-surprise'])
        only_skip = {'x': {'passed': None, 'skipped': True, 'detail': 'SKIPPED'}}
        self.assertFalse(prerequisites.summarize_results(only_skip, {'x'})[0])
        # The pre-fix shape had no explicit state and ``passed: True``; the gate
        # must refuse it rather than silently counting it as an executed pass.
        legacy = {'old': {'passed': True, 'detail': 'SKIPPED (degenerate)'}}
        with self.assertRaises(ValueError):
            prerequisites.summarize_results(legacy, set())

    def test_probe_table_prefers_running_tree_over_old_code_root(self):
        # fix-r6 P2: --code-root may point at a pre-fix tree with an old table.
        # The running table must win, otherwise the new shapes are silently not
        # exercised in exactly the comparison that is meant to expose them.
        prerequisites = _load('prerequisites',
                              Path(__file__).resolve().parent / 'controls/prerequisites.py')
        running_path = Path(__file__).resolve().parent / 'common.py'
        old = type('OldCommon', (), {
            'APPARATUS_PROBES': (('file', 'old-only.py'),),
            'materialize_probe': None,
        })()
        # This test isolates table selection; ImportIsolation covers lazy-load
        # validation independently, including committed HEAD byte injection.
        with patch.object(prerequisites, '_verify_cli_imports'):
            module, table = prerequisites.select_probe_source(old, running_path)
        self.assertEqual(tuple(table), tuple(APPARATUS_PROBES))
        self.assertNotIn(('file', 'old-only.py'), table)
        current, current_table = prerequisites.select_probe_source(common, running_path)
        self.assertIs(current, common)
        self.assertEqual(tuple(current_table), tuple(APPARATUS_PROBES))

    def test_whitelisted_and_data_files_are_accepted_by_path(self):
        for name in apparatus_whitelist():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('registered apparatus input')
        self.assertEqual(unknown_apparatus_files(self.root), [])
        extra = self.root / 'fixtures/extra.json'
        extra.write_text('{}')
        self.assertEqual(unknown_apparatus_files(self.root), ['fixtures/extra.json'])
        self.assertIn('fixtures/gateway-protocol.fixtures.json', DATA_FILES)

    def test_node_modules_symlinks_are_digested_not_scanned(self):
        # The one exempt top-level tree stays pruned; links inside it are
        # covered by the node_modules digest, which records the link target.
        modules = self.root / 'node_modules'
        modules.mkdir()
        outside = Path(tempfile.mkdtemp(prefix='pdrs-node-modules-', dir=self.root.parent))
        self.addCleanup(shutil.rmtree, outside, ignore_errors=True)
        for name in ('first', 'second'):
            (outside / name).mkdir()
            (outside / name / '__init__.py').write_text('dependency')
        link = modules / 'pkg'
        link.symlink_to(outside / 'first', target_is_directory=True)
        self.assertEqual(unknown_apparatus_files(self.root), [])
        first = node_modules_entry(self.root)
        link.unlink()
        link.symlink_to(outside / 'second', target_is_directory=True)
        self.assertNotEqual(node_modules_entry(self.root), first)
        self.assertEqual(unknown_apparatus_files(self.root), [])

    def test_python_cache_is_always_unregistered(self):
        # fix-r5 P0: unchecked-hash (PEP 552) bytecode is loaded without
        # re-reading the source, so no .pyc/.pyo gets a tolerance class even
        # beside a registered source.  An empty __pycache__ is rejected too:
        # the scanner reports the directory itself and never descends into it.
        (self.root / 'common.py').write_text('registered member')
        (self.root / '__pycache__').mkdir()
        self.assertEqual(unknown_apparatus_files(self.root), ['__pycache__'])
        cache = self.root / '__pycache__/common.cpython-314.pyc'
        cache.write_bytes(b'x')
        self.assertEqual(unknown_apparatus_files(self.root), ['__pycache__'])
        self.root.joinpath('__pycache__/evil.py').write_text('not cache bytecode')
        self.assertEqual(unknown_apparatus_files(self.root), ['__pycache__'])
        shutil.rmtree(self.root / '__pycache__')
        (self.root / 'legacy.pyc').write_bytes(b'x')
        (self.root / 'legacy.pyo').write_bytes(b'x')
        self.assertEqual(unknown_apparatus_files(self.root), ['legacy.pyc', 'legacy.pyo'])
        # A cache below a subdirectory is rejected at its first __pycache__
        # segment too; controls/run_all.py is loaded by path and would shadow.
        (self.root / 'controls/__pycache__').mkdir(parents=True)
        (self.root / 'controls/__pycache__/run_all.cpython-314.pyc').write_bytes(b'x')
        self.assertEqual(unknown_apparatus_files(self.root), [
            'controls/__pycache__', 'legacy.pyc', 'legacy.pyo'])

    def test_ds_store_is_tolerated_only_as_a_regular_file(self):
        # Same exemption as scripts/ui-server-parity/apparatus.py: ordinary
        # Finder metadata only.  Links/directories stay rejected (the shared
        # probe table covers those shapes).
        (self.root / '.DS_Store').write_text('finder metadata')
        (self.root / 'fixtures').mkdir()
        (self.root / 'fixtures/.DS_Store').write_text('finder metadata')
        self.assertEqual(unknown_apparatus_files(self.root), [])
        (self.root / 'fixtures/.DS_Store').unlink()
        (self.root / 'fixtures/.DS_Store').mkdir()
        self.assertEqual(unknown_apparatus_files(self.root), ['fixtures/.DS_Store'])
        (self.root / 'fixtures/.DS_Store').rmdir()
        (self.root / 'common.py').write_text('registered member')
        (self.root / 'fixtures/.DS_Store').symlink_to(self.root / 'common.py')
        self.assertEqual(unknown_apparatus_files(self.root), ['fixtures/.DS_Store'])

    def test_registered_path_case_variant_requires_the_same_inode(self):
        (self.root / 'common.py').write_text('registered member')
        variant = self.root / 'COMMON.PY'
        variant.write_text('case variant')
        same = os.path.samefile(self.root / 'common.py', variant)
        # APFS is case-insensitive: the variant is the registered inode.  On a
        # case-sensitive filesystem it is a distinct, unregistered file.
        self.assertEqual(unknown_apparatus_files(self.root), [] if same else ['COMMON.PY'])

    def test_unreadable_directory_aborts_the_digest_too(self):
        if os.geteuid() == 0:
            self.skipTest('root can read mode-000 directories')
        modules = self.root / 'node_modules/locked'
        modules.mkdir(parents=True)
        (modules / 'index.js').write_text('x')
        os.chmod(modules, 0)
        try:
            with self.assertRaises(OSError):
                node_modules_entry(self.root)
        finally:
            os.chmod(modules, 0o700)

    def test_frozen_hash_rows_are_inside_the_whitelist(self):
        manifest = Path(__file__).resolve().parents[2] / 'specs/gateway-parity/FROZEN.md'
        rows = re.findall(r'^[0-9a-f]{64}\s+\.\./\.\./scripts/gateway-parity/(\S+)\s*$',
                          manifest.read_text(encoding='utf-8'), re.M)
        self.assertTrue(rows)
        self.assertEqual(sorted(set(rows) - set(apparatus_whitelist())), [])

    def test_real_apparatus_has_no_unregistered_files(self):
        # The frozen tree itself must pass the whitelist: no existing data file
        # or generated cache may be misclassified.
        self.assertEqual(unknown_apparatus_files(), [])

    def test_installed_dependency_bytes_change_digest(self):
        self.assertIsNone(node_modules_entry(self.root))
        (self.root / 'node_modules/pkg').mkdir(parents=True)
        (self.root / 'node_modules/pkg/index.js').write_text('a')
        first = node_modules_entry(self.root)
        self.assertIsNotNone(first)
        (self.root / 'node_modules/pkg/index.js').write_text('b')
        self.assertNotEqual(node_modules_entry(self.root), first)

    def test_baseline_dependencies_bind_loaded_packages(self):
        modules = self.root / 'baseline/node_modules'
        for name in ('tsx', 'ws'):
            package = modules / name
            package.mkdir(parents=True)
            (package / 'package.json').write_text(json.dumps({'name': name, 'dependencies': {}}))
            (package / 'index.js').write_text(name)
        before = baseline_dependency_hashes(self.root / 'baseline')
        self.assertEqual(sorted(before), ['node_modules/tsx', 'node_modules/ws'])
        (modules / 'tsx/index.js').write_text('tampered loader')
        self.assertNotEqual(baseline_dependency_hashes(self.root / 'baseline'), before)
        shutil.rmtree(modules / 'ws')
        with self.assertRaises(ValueError):
            baseline_dependency_hashes(self.root / 'baseline')

    def test_untracked_and_persistent_cargo_configs_block(self):
        environ = {'CARGO_HOME': str(self.root / 'cargo-home')}
        self.assertEqual(cargo_config_problems(self.root, environ), [])
        (self.root / '.cargo').mkdir()
        (self.root / '.cargo/config.toml').write_text('[build]\n')
        self.assertTrue(any('untracked worktree config' in item
                            for item in cargo_config_problems(self.root, environ)))
        (self.root / '.cargo/config.toml').unlink()
        (self.root / '.cargo').rmdir()
        home = self.root / 'cargo-home'
        home.mkdir()
        (home / 'config.toml').write_text('[registries]\n')
        self.assertTrue(any('persistent CARGO_HOME config' in item
                            for item in cargo_config_problems(self.root, environ)))
        (home / 'config.toml').unlink()
        self.assertEqual(cargo_config_problems(self.root, environ), [])

    def test_tracked_worktree_cargo_config_is_reviewable(self):
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True, timeout=30)
        config = self.root / '.cargo/config.toml'
        config.parent.mkdir()
        config.write_text('[build]\n')
        subprocess.run(['git', '-C', str(self.root), 'add', '.cargo/config.toml'],
                       check=True, timeout=30)
        environ = {'CARGO_HOME': str(self.root / 'cargo-home')}
        self.assertEqual(cargo_config_problems(self.root, environ), [])


class WsProjection(unittest.TestCase):
    """#1372: the WS envelope -> in-process projection is a pure function.

    The partition key is the protocol field ``final`` (one final:false envelope
    per iterator yield, one trailing final:true carrying lastError ??
    lastCompleted ?? the empty-stream fallback).  Event content never decides
    placement and equal payloads are never collapsed.  The chain-level
    rejections live in ``controls/session_domain.py``; these pin the projection
    itself, which is also what the collector calls.
    """

    @classmethod
    def setUpClass(cls):
        cls.native = _load('native_evidence', common.SCRIPT / 'native_evidence.py')

    @staticmethod
    def envelope(seq, event, final=False, request='req-1'):
        return {'type': 'event', 'id': request, 'seq': seq, 'final': final, 'event': event}

    def project(self, *envelopes, request='req-1'):
        return self.native.project_envelopes(request, list(envelopes))

    def test_duplicate_yields_are_kept_and_terminal_is_not_a_frame(self):
        event = {'type': 'assistant_text_delta', 'text': 'same'}
        projected = self.project(self.envelope(0, event), self.envelope(1, dict(event)),
                                 self.envelope(2, self.native.WS_TERMINAL_FALLBACK, final=True))
        self.assertEqual(projected['frames'], [event, event], '#1372 equal payloads must not collapse')
        self.assertEqual(projected['terminal'], {'source': 'fallback',
                                                 'event': self.native.WS_TERMINAL_FALLBACK})

    def test_terminal_must_repeat_the_last_error(self):
        error = {'type': 'error', 'code': 'turn_timeout'}
        projected = self.project(self.envelope(0, error), self.envelope(1, dict(error), final=True))
        self.assertEqual(projected['frames'], [error])
        self.assertEqual(projected['terminal']['source'], 'error')
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, error),
                         self.envelope(1, {'type': 'error', 'code': 'fabricated'}, final=True))

    def test_completed_terminal_loses_to_an_earlier_error(self):
        error = {'type': 'error', 'code': 'turn_timeout'}
        completed = {'type': 'turn_completed'}
        projected = self.project(self.envelope(0, error), self.envelope(1, completed),
                                 self.envelope(2, dict(error), final=True))
        self.assertEqual(projected['terminal']['source'], 'error')
        # lastError ?? lastCompleted: a completion after the error is still not
        # the terminal, so a completed terminal here is fabricated.
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, error), self.envelope(1, completed),
                         self.envelope(2, dict(completed), final=True))

    def test_fallback_needs_an_empty_terminal_surface(self):
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, {'type': 'error', 'code': 'turn_timeout'}),
                         self.envelope(1, dict(self.native.WS_TERMINAL_FALLBACK), final=True))

    def test_terminal_types_are_not_coerced(self):
        # #1372 review P0: Python's ``==`` makes True == 1, so a terminal that
        # turned a JSON boolean into a number still satisfied the derivation
        # rule and the audit scored a stream whose terminal is not the frame it
        # claims to repeat.  Types the wire distinguishes must stay distinct.
        coerced = dict(self.native.WS_TERMINAL_FALLBACK, recoverable=1)
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, coerced, final=True))
        error = {'type': 'error', 'code': 'turn_timeout', 'retryable': True}
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, error), self.envelope(1, dict(error, retryable=1), final=True))
        completed = {'type': 'turn_completed', 'usage': {'inputTokens': 3}}
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, completed),
                         self.envelope(1, {'type': 'turn_completed', 'usage': {'inputTokens': '3'}}, final=True))
        # Number spelling stays a value, not a type: JSON.parse gives JavaScript
        # one number type, so 3 and 3.0 are the same terminal.
        self.project(self.envelope(0, completed),
                     self.envelope(1, {'type': 'turn_completed', 'usage': {'inputTokens': 3.0}}, final=True))

    def test_deeply_nested_terminals_compare_without_recursing(self):
        # #1372 r4 review P1: json.loads accepts shapes deeper than the
        # interpreter's recursion limit, so a recursive comparison raised
        # RecursionError on a legal terminal instead of answering.  The walk
        # must stay flat and give the same verdicts a shallow pair would.
        def nested(leaf, depth):
            value = {'leaf': leaf}
            for _ in range(depth):
                value = {'next': [value]}
            return value

        depth = 4000
        self.assertTrue(self.native.same_json(nested(1, depth), nested(1, depth)))
        self.assertFalse(self.native.same_json(nested(1, depth), nested(2, depth)))
        self.assertFalse(self.native.same_json(nested(True, depth), nested(1, depth)))

        deep = nested(1, depth)
        projected = self.project(self.envelope(0, dict(deep, type='error')),
                                 self.envelope(1, dict(deep, type='error'), final=True))
        self.assertEqual(projected['terminal']['source'], 'error')
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, dict(deep, type='error')),
                         self.envelope(1, nested(1, depth), final=True))

    def test_exactly_one_terminal_and_it_must_be_last(self):
        event = {'type': 'assistant_text_delta', 'text': 'x'}
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, event, final=True), self.envelope(1, event))
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, dict(event), final=True), self.envelope(1, event, final=True))
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, event))

    def test_envelope_identity_and_sequence_are_checked(self):
        event = {'type': 'assistant_text_delta', 'text': 'x'}
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, event, request='other'),
                         self.envelope(1, dict(event), final=True))
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, event), self.envelope(2, dict(event), final=True))
        with self.assertRaises(self.native.Incomplete):
            self.project(self.envelope(0, event, final='no'))


def count_name_bindings(source, name):
    """Count every binding of ``name`` in ``source``, semantically.

    r5-r8 showed a lexical pin on one spelling of an assignment always
    leaves an equivalent-spelling hole open (tail anchor, left space,
    delimiter, no-space spelling -- the same spot four times).  r9's first
    cut enumerated binding node types (Assign/AugAssign/AnnAssign/
    NamedExpr) and DS r10 opened it a fifth time through the forms that
    enumeration misses: tuple/list unpacking targets, ``for``/``with-as``
    and comprehension targets, ``def``/``class`` names, ``import ... as``,
    ``except ... as`` and match captures.  So this counts bindings by
    context, not by node type: every ``ast.Name`` in ``Store`` context
    (which covers plain/chained/unpacking assignment, augmented and
    annotated assignment, the walrus, for/with/comprehension targets),
    plus the str-field binding forms (function/class names, except and
    match captures, import aliases -- ``import a.b`` binds ``a``).
    Attribute/subscript targets (``self.clock``, ``globals()['clock']``)
    are different names as far as binding goes and are not counted -- the
    guarded fixtures carry a separate byte ban on dynamic-namespace writes
    (``globals(`` / ``setattr(``) for those.  Function parameters are not
    counted: a parameter binds the name in a new scope and cannot rebind
    the outer name the pins read.
    """
    import ast
    tree = ast.parse(source)
    count = 0
    for node in ast.walk(tree):
        if isinstance(node, ast.Name):
            if node.id == name and isinstance(node.ctx, ast.Store):
                count += 1
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            if node.name == name:
                count += 1
        elif isinstance(node, ast.ExceptHandler):
            if node.name == name:
                count += 1
        elif isinstance(node, ast.alias):
            bound = node.asname if node.asname is not None else node.name.split('.')[0]
            if bound == name:
                count += 1
        elif isinstance(node, (ast.MatchAs, ast.MatchStar)):
            if node.name == name:
                count += 1
        elif isinstance(node, ast.MatchMapping):
            if node.rest == name:
                count += 1
    return count


class SameJsonSurfaces(unittest.TestCase):
    """#2039: registered expectations and replay ledgers are JSON values, so
    their comparisons go through the strict ``same_json`` walk (#1372), never
    Python's ``==``/``!=``: a boolean that drifted into a number must surface
    as a difference instead of passing silently.  These pin the surface list
    the A19 scope gate and the per-case verdict consume, plus the native
    cleanup and prelaunch replay comparisons; chain-level behaviour stays in
    ``controls/session_domain.py``.

    #2425/#2431 pins on the same-json fixture's own machinery: the atomic
    publish/restore spelling and the pre-mutation terminate handlers (#2425),
    and the startup-refusal contract — a guard that fails before the first
    mutation is rc 2 with the claim directory withdrawn, never a red run
    (#2431).

    #2552 (PR #2544 review follow-up, DS P2 six): the lease budget's tail
    reserve is derived from the real exit-side worst case and carries its own
    derivation pin, the budget guard itself refuses rc 2 on an unreadable
    common.py, the spent-budget branch has a committed regression pin, and
    the terminate handler body is pinned functionally, not just by the
    call-site count.
    """

    @classmethod
    def setUpClass(cls):
        cls.native = _load('native_evidence', common.SCRIPT / 'native_evidence.py')
        cls.session = _load('session_domain', common.SCRIPT / 'session_domain.py')

    @staticmethod
    def _expectation(frames=('turn_completed:stop',), durable=('turn_timeout',),
                     registry=None):
        return {'frames': list(frames), 'durableStatus': list(durable),
                'registry': registry if registry is not None else {'applicable': False}}

    def _surfaces(self, ts, rust):
        return self.session._expectation_diff_surfaces(ts, rust)

    def test_label_strings_keep_the_registered_verdict(self):
        # Real data is label strings and JSON booleans: equal values of equal
        # type keep the empty surface list, so the #1372 train's registered
        # expectations (a19DiffCases etc.) are unchanged by this fix.
        registry = {'applicable': True, 'events': ['4:turn_completed:stop'],
                    'state': 'failed', 'result': 'turn_timeout:timeout'}
        self.assertTrue(self._surfaces(self._expectation(registry=registry),
                                       self._expectation(registry=dict(registry))) == [],
                        '#2039 equal-type expectations keep the empty surface list')

    def test_boolean_drift_surfaces_on_frames(self):
        # Python's ``!=`` made ``True == 1``, so a rust expectation whose JSON
        # boolean drifted into a number passed as identical and the A19 scope
        # gate (validate_expectations) never saw the surface.
        ts = self._expectation(frames=[True])
        rust = self._expectation(frames=[1])
        self.assertTrue(self._surfaces(ts, rust) == ['frames'],
                        '#2039 boolean drift must surface on frames')

    def test_boolean_drift_surfaces_on_durable_status(self):
        ts = self._expectation(durable=[False])
        rust = self._expectation(durable=[0])
        self.assertTrue(self._surfaces(ts, rust) == ['durableStatus'],
                        '#2039 boolean drift must surface on durableStatus')

    def test_applicable_drift_surfaces_before_the_field_guards(self):
        # A drifted ``applicable`` used to fall through to the field guards,
        # which then compared the sub-registry as if it applied.
        ts = self._expectation(registry={'applicable': True})
        rust = self._expectation(registry={'applicable': 1})
        self.assertTrue(self._surfaces(ts, rust) == ['registry.applicable'],
                        '#2039 registry.applicable boolean drift must surface')

    def test_registry_field_drift_surfaces(self):
        # The guarded ``registry.events``/``state``/``result`` comparisons sit
        # behind ``applicable``; their boolean drifts must surface too.
        registry = {'applicable': True, 'state': 'failed', 'result': 'turn_timeout:timeout'}
        ts = self._expectation(registry=dict(registry, events=[True]))
        rust = self._expectation(registry=dict(registry, events=[1]))
        self.assertTrue(self._surfaces(ts, rust) == ['registry.events'],
                        '#2039 registry.events boolean drift must surface')
        ts = self._expectation(registry=dict(registry, events=['4:turn_completed:stop'], result=True))
        rust = self._expectation(registry=dict(registry, events=['4:turn_completed:stop'], result=1))
        self.assertTrue(self._surfaces(ts, rust) == ['registry.result'],
                        '#2039 registry.result boolean drift must surface')

    def test_cleanup_replay_drift_is_not_coerced(self):
        # ``checked_evidence`` re-derives the ledger from the raw trace;
        # ``True == 1`` used to certify a replay whose cleanup flag had
        # drifted into a number.  parse_trace is patched because a real
        # strace round is the chain fixture's job, not this unit pin's.
        ledger = {'processes': [], 'execWitness': {'complete': True},
                  'networkWitness': {'complete': True},
                  'transcriptWitness': {'complete': True},
                  'cleanup': {'allStopped': 1}}
        replay = {'processes': [], 'execWitness': {'complete': True},
                  'networkWitness': {'complete': True},
                  'transcriptWitness': {'complete': True},
                  'cleanup': {'allStopped': True}}
        with patch.object(self.native, 'parse_trace', return_value=replay):
            checked = self.native.checked_evidence('raw', {}, ledger)
        self.assertTrue(checked['cleanup']['allStopped'] is False,
                        '#2039 cleanup allStopped=1 must not coerce to True')
        with patch.object(self.native, 'parse_trace', return_value=replay):
            checked = self.native.checked_evidence('raw', {}, dict(ledger, cleanup={'allStopped': True}))
        self.assertTrue(checked['cleanup']['allStopped'] is True,
                        '#2039 cleanup allStopped stays True on equal type')

    def test_prelaunch_drift_is_not_coerced(self):
        # ``load_capture`` re-checks the launch contract against the prelaunch
        # mirror; a boolean that became a number used to satisfy ``==``.
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            contract = {'launcherPid': 42, 'execTrace': True, 'binaries': {}}

            def capture(prelaunch):
                (folder / 'trace.log').write_text('1 +++ exited with 0 ++++\n')
                (folder / 'launch.json').write_text(json.dumps(contract))
                (folder / 'prelaunch.json').write_text(json.dumps(prelaunch))
                (folder / 'evidence.json').write_text('{}')
                # dict() spelling, not a literal: this is fixture data, not a
                # status computation, so it must not read as an aggregation
                # point to the #991/#164 audit (whose registry is shared).
                receipt = dict(status='WITNESS_CAPTURED', tracerExitCode=0)
                for name, key in self.native.receipt_artifacts('capture').items():
                    receipt[key] = self.native.sha(folder / name)
                (folder / 'receipt.json').write_text(json.dumps(receipt))
                return folder

            loaded = self.native.load_capture(capture({'execTrace': True, 'binaries': {}}))
            self.assertTrue(loaded[1] == contract, '#2039 equal-type prelaunch contract loads')
            try:
                self.native.load_capture(capture({'execTrace': 1, 'binaries': {}}))
                rejected = False
            except self.native.Incomplete:
                rejected = True
            self.assertTrue(rejected, '#2039 boolean drift in the prelaunch mirror is rejected')

    def test_tracer_exit_code_drift_is_not_coerced(self):
        # receipt.json is a retained JSON original whose bytes are not
        # sha-pinned (receipt_artifacts covers the other four artifacts), so
        # a tracerExitCode that drifted into a boolean must not satisfy == 0.
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            contract = {'launcherPid': 42, 'execTrace': True, 'binaries': {}}

            def capture(tracer_exit_code):
                (folder / 'trace.log').write_text('1 +++ exited with 0 ++++\n')
                (folder / 'launch.json').write_text(json.dumps(contract))
                (folder / 'prelaunch.json').write_text(json.dumps({'execTrace': True, 'binaries': {}}))
                (folder / 'evidence.json').write_text('{}')
                receipt = dict(status='WITNESS_CAPTURED', tracerExitCode=tracer_exit_code)
                for name, key in self.native.receipt_artifacts('capture').items():
                    receipt[key] = self.native.sha(folder / name)
                (folder / 'receipt.json').write_text(json.dumps(receipt))
                return folder

            loaded = self.native.load_capture(capture(0))
            self.assertTrue(loaded[1] == contract, '#2039 equal-type tracer exit code loads')
            try:
                self.native.load_capture(capture(False))
                rejected = False
            except self.native.Incomplete:
                rejected = True
            self.assertTrue(rejected, '#2039 tracer exit code boolean drift is rejected')

    def test_ws_profile_opcode_drift_is_not_coerced(self):
        # ws_profile gates observer JSONL wire records; a boolean that drifted
        # into a number used to satisfy ``==`` and pass as a complete text
        # frame without RSV, the same coercion the six batch comparisons fix.
        def rejected(wire):
            try:
                self.native.ws_profile('send', wire)
                return False
            except self.native.Incomplete:
                return True
        self.assertTrue(rejected({'opcode': True, 'fin': True, 'rsv': 0, 'masked': True}),
                        '#2039 ws profile opcode boolean drift is rejected')
        self.assertTrue(rejected({'opcode': 1, 'fin': True, 'rsv': False, 'masked': True}),
                        '#2039 ws profile rsv boolean drift is rejected')
        self.assertTrue(not rejected({'opcode': 1, 'fin': True, 'rsv': 0, 'masked': True}),
                        '#2039 equal-type wire record passes the ws profile')

    def test_session_profile_connections_drift_is_not_coerced(self):
        # session_profile audits the launch contract, a JSON original on the
        # live audit path: a connections boolean must not satisfy == 1.
        argv = ['bin', '--listen', '127.0.0.1:28999', '--home', '/h', '--project', '/h']
        contract = {'cases': ['demo/session'], 'processRoles': ['host', 'sidecar'],
                    'binaries': {'host': {'argv': argv}, 'sidecar': {}},
                    'connections': 1, 'externalEndpoints': ['127.0.0.1:29000'],
                    'capabilities': {'provider': 'loopback-mock', 'tools': []},
                    'project': '/h'}
        self.assertTrue(self.native.session_profile(contract, ['demo/session'])
                        == ('/h', '127.0.0.1:29000'), '#2039 equal-type session profile loads')
        try:
            self.native.session_profile(dict(contract, connections=True), ['demo/session'])
            rejected = False
        except self.native.Incomplete:
            rejected = True
        self.assertTrue(rejected, '#2039 session profile connections boolean drift is rejected')

    def test_memory_rehearsal_holds_tree_lock(self):
        import io
        marker = '#2039 same-json memory rehearsal holds the source tree lock'
        path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/same-json-surfaces/negative_controls.py'
        try:
            spec = importlib.util.spec_from_file_location('samejson_memory_lock', path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            seen, order = [], []

            class recording_lock:
                def __enter__(self):
                    order.append('lock')

                def __exit__(self, *_exc):
                    return None

            def recording_tree_lock(*arguments, **keywords):
                seen.append((arguments, keywords))
                return recording_lock()

            def fake_run_owned(command, **_keywords):
                order.append('child')
                return subprocess.CompletedProcess(command, 0, '', 'Ran 1 test in 0.001s\n\nOK\n')

            previous_target = os.environ.get('CARGO_TARGET_DIR')
            if previous_target is None:
                os.environ['CARGO_TARGET_DIR'] = str(common.ROOT / '_build/cargo-target')
            try:
                with tempfile.TemporaryDirectory(prefix='pdrs-2039-samejson-') as folder:
                    with patch.object(module, 'tree_lock', recording_tree_lock), \
                            patch.object(module, 'run_owned', fake_run_owned), \
                            patch('sys.stdout', new_callable=io.StringIO), \
                            patch.object(sys, 'argv', [str(path), str(Path(folder) / 'out'), '--memory',
                                                       'memory-rehearsal-without-lock']):
                        module.main()
            finally:
                if previous_target is None:
                    del os.environ['CARGO_TARGET_DIR']
            self.assertTrue(seen == [((module.root,), {'timeout': 900})]
                            and order[:1] == ['lock'] and 'child' in order, marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_mutant_bytes_publish_in_one_rename(self):
        # #2425: the guarded publish and the restore are one rename each
        # (write_source_atomic, #1903); a truncating path.write_* leaves a
        # half-file behind on SIGKILL.  Both call sites are pinned plus the
        # absence of any truncating spelling, so the self-mutations
        # (atomic-publish-reverted-to-truncating and the restore twin) turn
        # this red either way.
        marker = '#2425 mutant bytes are published in one atomic rename'
        path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/same-json-surfaces/negative_controls.py'
        try:
            source = path.read_bytes().decode()
            self.assertTrue(source.count('write_source_atomic(root, path, changed.encode())') == 1
                            and source.count('write_source_atomic(root, path, originals[path])') == 1
                            and 'path.write_text(' not in source
                            and 'path.write_bytes(' not in source, marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_guarded_run_installs_terminate_handlers(self):
        # #2425: the guarded branch installs terminate_cleanly on SIGTERM/
        # SIGHUP/SIGQUIT before the first mutation, so an interrupted batch
        # unwinds through the finally blocks and restores the tree instead of
        # leaving a mutant behind under the parity lease.  The handler *body*
        # is pinned separately, by
        # test_terminate_cleanly_raises_the_signal_exit_code (#2552, DS
        # boundary#1): this call-site count alone cannot tell a gutted body
        # from a real one.  The depth-only count also cannot tell *where*
        # the call sits: statements inside one()'s try block sit at the
        # same 12-space depth, so the call is additionally pinned by
        # adjacency to the mutating_sources entry
        # line -- moving it past the restore window (DS r5 sibling#1, the
        # same position-semantics gap the clock pin closed in r4) goes red.
        # All three registration rows are pinned by bytes: a SIGTERM-only
        # count let the SIGHUP/SIGQUIT hasattr blocks be deleted for a
        # false green (DS r7 sibling#1, P1 -- the comment above claims all
        # three signals).
        marker = '#2425 the guarded run installs terminate handlers before mutating'
        path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/same-json-surfaces/negative_controls.py'
        try:
            source = path.read_bytes().decode()
            self.assertTrue(source.count('            install_terminate_handlers()') == 1
                            and source.count('\n            install_terminate_handlers()\n'
                                            '            with mutating_sources(root, GUARDED_PATHS,\n') == 1
                            and source.count('signal.signal(signal.SIGTERM, terminate_cleanly)') == 1
                            and source.count('signal.signal(signal.SIGHUP, terminate_cleanly)') == 1
                            and source.count('signal.signal(signal.SIGQUIT, terminate_cleanly)') == 1, marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_install_terminate_handlers_leaves_live_registrations(self):
        # #2552 (DS r8 sibling#1 + concurrency#1, Claude r7 sibling#1 +
        # concurrency#1, two families converging): the byte counts above prove
        # the registration rows exist in the file, not that
        # install_terminate_handlers() executes them.  An 'if False:' guard,
        # a renamed attribute ('SIGHUP_'), or a move into a dead function
        # all keep every count at 1 while the guarded batch actually runs on
        # default dispositions -- an interrupted run would leave the mutant
        # behind, the exact failure #2425 pins.  Ask the signal table
        # itself: exec the module, call install_terminate_handlers(), and
        # require each handler to be live, restoring the saved dispositions
        # afterwards (the save/restore form the #2431 pins already use).
        #
        # #2552 r9 (DS r9 sibling#2 + boundary#1, coordinator arbitration
        # design-change round): two more legs.  (a) Binding drift -- the
        # direct call above proves the *module-level* function registers;
        # a local shadow inside main() (install_terminate_handlers =
        # lambda: None before the call site), a globals()/setattr rebind,
        # or a dead main would leave every pin green while the guarded
        # batch runs on default dispositions.  So main() itself is driven
        # through a guard that refuses at entry, and the live dispositions
        # are *recorded* at that moment (asserting inside __enter__ would
        # be swallowed by main's except into an rc2 refusal) and checked
        # afterwards.  Scope note (DS r10 concurrency#2): the dispositions
        # are sampled once, at guard entry -- overwriting a disposition
        # *inside* the mutation window (signal.signal(SIGTERM, SIG_DFL)
        # after entry) is a different, already-registered vector (#2624)
        # and is NOT closed by this leg.
        # (b) Structure -- count_name_bindings now counts
        # every binding form (DS r10 concurrency#1/boundary#1/sibling#1:
        # def/class names included), so the name must have exactly one
        # binding in the whole file: the module-level def, with no
        # expression binding, nested def, import alias or capture
        # anywhere else.  The fixture must also not contain dynamic-
        # namespace writes (globals( / setattr( / vars( / exec( /
        # __dict__ -- zero in the healthy file), which would rebind
        # names invisibly to every AST and byte pin here (the same
        # vector as the clock count, DS r8 boundary#1).  These are
        # substring bans and would fire on a comment mention too --
        # fail-closed, preferred over the hole they close (DS r10
        # boundary#2/#3).
        import io
        import signal
        import sys
        marker = '#2425/#2552 install_terminate_handlers registers all three signals'
        path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/same-json-surfaces/negative_controls.py'
        try:
            source = path.read_bytes().decode()
            spec = importlib.util.spec_from_file_location('samejson_install_pins', path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            names = [signal.SIGTERM] + [getattr(signal, name) for name in ('SIGHUP', 'SIGQUIT')
                                        if hasattr(signal, name)]
            saved = {name: signal.getsignal(name) for name in names}
            try:
                module.install_terminate_handlers()
                self.assertTrue(all(signal.getsignal(name) is module.terminate_cleanly
                                     for name in names), marker)
            finally:
                for name, disposition in saved.items():
                    # getsignal returns None when a non-Python handler holds
                    # the signal; signal.signal refuses None, so map that
                    # back to the default disposition (Claude r8 boundary#1
                    # P2b, DS r9 boundary#3 -- one-line guard).
                    signal.signal(name, disposition if disposition is not None
                                  else signal.SIG_DFL)

            import ast
            tree = ast.parse(source)
            module_defs = [node for node in tree.body
                           if isinstance(node, ast.FunctionDef)
                           and node.name == 'install_terminate_handlers']
            self.assertTrue(len(module_defs) == 1
                           and count_name_bindings(source, 'install_terminate_handlers') == 1
                           and 'globals(' not in source
                           and 'setattr(' not in source
                           and 'vars(' not in source
                           and 'exec(' not in source
                           and '__dict__' not in source, marker)

            with tempfile.TemporaryDirectory(prefix='pdrs-2552-r9-') as folder:
                out = Path(folder) / 'out'
                spec = importlib.util.spec_from_file_location('samejson_install_main', path)
                main_module = importlib.util.module_from_spec(spec)
                exec(compile(path.read_bytes(), str(path), 'exec'), main_module.__dict__)
                recorded = {}

                class recording_guard:
                    def __enter__(self):
                        # plain subscript writes: a .update({...}) call here
                        # reads as an incremental status write to the #991
                        # aggregation audit, which then demands registration
                        # for a test that aggregates nothing.
                        for name in names:
                            recorded[name] = signal.getsignal(name)
                        raise RuntimeError('probe: refuse before the first mutation')

                    def __exit__(self, *_exc):
                        return False

                try:
                    with patch.object(main_module, 'preflight', lambda: True), \
                            patch.object(main_module, 'mutating_sources',
                                         lambda *_a, **_k: recording_guard()), \
                            patch('sys.stdout', new_callable=io.StringIO), \
                            patch('sys.stderr', new_callable=io.StringIO), \
                            patch.object(sys, 'argv', [str(path), str(out),
                                                       'surfaces-frames-bare-not-equal']):
                        try:
                            main_module.main()
                            raised = None
                        except (SystemExit, RuntimeError) as error:
                            raised = error
                    # A dead or shadowed call site records nothing (or records
                    # the default dispositions); the guard refuses before the
                    # first mutation, so main exits rc 2 either way and only
                    # the recorded table tells the two apart.  The refusal is
                    # pinned to SystemExit(2): a mid-window SIGTERM lands in
                    # terminate_cleanly's SystemExit(128+signum), which must
                    # not read as this leg's expected refusal (DS r10
                    # concurrency#3 -- same shape as the #2431 sibling pins).
                    self.assertTrue(isinstance(raised, SystemExit)
                                    and raised.code == 2
                                    and len(recorded) == len(names)
                                    and all(recorded[name] is main_module.terminate_cleanly
                                             for name in names), marker)
                finally:
                    # main() installs without uninstalling; restore so the
                    # handler does not leak into the tests that follow.
                    for name, disposition in saved.items():
                        signal.signal(name, disposition if disposition is not None
                                      else signal.SIG_DFL)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_startup_refusal_is_rc2_memory_branch(self):
        # #2431: a startup-phase failure (the tree lock becoming unavailable
        # before the first mutation) must be a refusal, not a red run -- rc 2,
        # the claimed directory withdrawn, no summary.json that reads like a
        # finished run.  preflight is stubbed so the kill lands on the rc2
        # semantics itself, not on anchor bookkeeping; the case never runs,
        # the guard refuses before it.
        import io
        marker = '#2431 the memory branch refuses a startup failure with rc 2'
        path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/same-json-surfaces/negative_controls.py'
        try:
            with tempfile.TemporaryDirectory(prefix='pdrs-2431-rc2-') as folder:
                out = Path(folder) / 'out'
                spec = importlib.util.spec_from_file_location('samejson_rc2_memory', path)
                module = importlib.util.module_from_spec(spec)
                exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)

                class refusing_lock:
                    def __enter__(self):
                        raise OSError('probe: tree lock unavailable')

                    def __exit__(self, *_exc):
                        return None

                with patch.object(module, 'preflight', lambda: True), \
                        patch.object(module, 'tree_lock', lambda *_a, **_k: refusing_lock()), \
                        patch('sys.stdout', new_callable=io.StringIO), \
                        patch('sys.stderr', new_callable=io.StringIO) as stderr, \
                        patch.object(sys, 'argv', [str(path), str(out), '--memory',
                                                   'memory-rehearsal-without-lock']):
                    try:
                        module.main()
                        raised = None
                    except SystemExit as exit_code:
                        raised = exit_code
                self.assertTrue(raised is not None and raised.code == 2
                                and not out.exists()
                                and not (out / 'summary.json').exists()
                                and 'guard refused to start' in stderr.getvalue(), marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_startup_refusal_is_rc2_guarded_branch(self):
        # #2431: the guarded branch refuses the same way, for every exception
        # class the startup phase can raise -- the scanner staging is a
        # RuntimeError and a subprocess.TimeoutExpired shape, tree_lock an
        # OSError (covered by the memory pin above).  The real terminate
        # handlers run here, so their dispositions are saved and restored
        # around main().
        import io
        import signal
        marker = '#2431 the guarded branch refuses a startup failure with rc 2'
        path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/same-json-surfaces/negative_controls.py'
        try:
            for probe in (RuntimeError('probe: scanner staging failed'),
                          subprocess.TimeoutExpired('workspace scanner', 60)):
                with self.subTest(probe=probe), \
                        tempfile.TemporaryDirectory(prefix='pdrs-2431-rc2-') as folder:
                    out = Path(folder) / 'out'
                    spec = importlib.util.spec_from_file_location('samejson_rc2_guarded', path)
                    module = importlib.util.module_from_spec(spec)
                    exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)

                    class refusing_guard:
                        def __enter__(self):
                            raise probe

                        def __exit__(self, *_exc):
                            return None

                    names = [signal.SIGTERM] + [getattr(signal, name) for name in ('SIGHUP', 'SIGQUIT')
                                                 if hasattr(signal, name)]
                    saved = {name: signal.getsignal(name) for name in names}
                    try:
                        with patch.object(module, 'preflight', lambda: True), \
                                patch.object(module, 'mutating_sources',
                                             lambda *_a, **_k: refusing_guard()), \
                                patch('sys.stdout', new_callable=io.StringIO), \
                                patch('sys.stderr', new_callable=io.StringIO) as stderr, \
                                patch.object(sys, 'argv', [str(path), str(out),
                                                           'surfaces-frames-bare-not-equal']):
                            try:
                                module.main()
                                raised = None
                            except SystemExit as exit_code:
                                raised = exit_code
                    finally:
                        for name, disposition in saved.items():
                            # getsignal returns None for a non-Python
                            # handler; signal.signal refuses None (DS r10
                            # sibling#2 -- guard propagated from the r9
                            # legs above).
                            signal.signal(name, disposition if disposition is not None
                                          else signal.SIG_DFL)
                    self.assertTrue(raised is not None and raised.code == 2
                                    and not out.exists()
                                    and not (out / 'summary.json').exists()
                                    and 'guard refused to start' in stderr.getvalue(), marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_budget_guard_refuses_when_common_is_unreadable(self):
        # #2552 (DS concurrency#2 + boundary#3, two independent hits):
        # require_lease_budget_below_max_age sat outside the started zone
        # with no handler of its own, so a common.py the guard could not
        # read (missing file, syntax error, a renamed MAX_AGE) bubbled out
        # of main() as a traceback and read as a red run (rc 1).  Every
        # failure of that guard must be a startup refusal: rc 2, no
        # evidence directory, before the claim.  preflight is stubbed like
        # the #2431 pins so the kill lands on the rc2 semantics itself.
        marker = '#2552 the budget guard refuses when common.py is unreadable'
        path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/same-json-surfaces/negative_controls.py'
        try:
            import contextlib
            import io
            import signal
            with tempfile.TemporaryDirectory(prefix='pdrs-2552-budget-') as folder:
                folder = Path(folder)
                missing = folder / 'missing-common.py'
                renamed = folder / 'renamed-common.py'
                renamed.write_text('LEASE_MAX_AGE = 2400\n')
                # (label, common.py the guard reads, budget reader); the
                # violating budget is read from the pristine constants the
                # guard itself loads, never restated here (#2552 r1 DS
                # concurrency#1: a restated 2400 drifts with common.py).
                probes = (('missing common.py', missing, None),
                          ('renamed MAX_AGE', renamed, None),
                          ('budget above MAX_AGE', None,
                           lambda module: module.load_pristine_common().MAX_AGE))
                for label, common_path, budget_reader in probes:
                    with self.subTest(label=label), \
                            tempfile.TemporaryDirectory(prefix='pdrs-2552-out-') as out_folder:
                        out = Path(out_folder) / 'out'
                        spec = importlib.util.spec_from_file_location(
                            'samejson_budget_' + label.replace(' ', '_').replace('-', '_'), path)
                        module = importlib.util.module_from_spec(spec)
                        exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
                        budget = budget_reader(module) if budget_reader is not None else None

                        # Inert on the failure path (#2552 r1 DS concurrency#1
                        # + boundary#4): every probe must die inside
                        # require_lease_budget_below_max_age, before the claim
                        # and before install_terminate_handlers.  Stub
                        # mutating_sources with a sentinel that turns a broken
                        # guard into this pin's own failure instead of letting
                        # main() walk on and mutate this very tree, and
                        # save/restore the signal dispositions like the #2431
                        # pins.
                        def sentinel(*_args, **_kwargs):
                            raise RuntimeError('budget guard let main() reach mutating_sources')

                        names = [signal.SIGTERM] + [getattr(signal, name) for name in ('SIGHUP', 'SIGQUIT')
                                                     if hasattr(signal, name)]
                        saved = {name: signal.getsignal(name) for name in names}
                        try:
                            with contextlib.ExitStack() as stack:
                                stack.enter_context(patch.object(module, 'preflight', lambda: True))
                                stack.enter_context(patch.object(module, 'mutating_sources', sentinel))
                                if common_path is not None:
                                    stack.enter_context(patch.object(module, 'common', common_path))
                                if budget is not None:
                                    stack.enter_context(patch.object(module, 'LEASE_BUDGET_SECONDS', budget))
                                stack.enter_context(patch('sys.stdout', new_callable=io.StringIO))
                                stderr = stack.enter_context(
                                    patch('sys.stderr', new_callable=io.StringIO))
                                stack.enter_context(patch.object(sys, 'argv', [str(path), str(out),
                                                                               'surfaces-frames-bare-not-equal']))
                                try:
                                    module.main()
                                    raised = None
                                except BaseException as exit_code:
                                    # BaseException, not just SystemExit: a
                                    # mutant that lets the guard's error escape
                                    # main() must fail this pin's own
                                    # assertion below -- a ``with self.subTest``
                                    # swallows anything raised through it into
                                    # an unittest ERROR, which classify()
                                    # reads as INVALID, never KILLED (#2552,
                                    # same lesson as #2425's assertRaises
                                    # note).  The sentinel's RuntimeError
                                    # failing the 'lease budget' needle is
                                    # the same protection for the opposite
                                    # direction.
                                    raised = exit_code
                        finally:
                            for name, disposition in saved.items():
                                # getsignal returns None for a non-Python
                                # handler; signal.signal refuses None (DS
                                # r10 sibling#2 -- guard propagated from
                                # the r9 legs above).
                                signal.signal(name, disposition if disposition is not None
                                              else signal.SIG_DFL)
                        self.assertTrue(raised is not None
                                        and isinstance(raised, SystemExit)
                                        and raised.code == 2
                                        and not out.exists()
                                        and not (out / 'summary.json').exists()
                                        and 'lease budget' in stderr.getvalue(), marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_lease_budget_marks_spent_cases_invalid(self):
        # #2552 (DS concurrency#4): the spent-budget branch had no committed
        # regression pin -- deleting it left every SameJsonSurfaces test
        # green and the invariant had only a one-time drift probe.  Pin the
        # call site and the INVALID marking by bytes -- including the
        # append's 'status': 'INVALID' literal and the trailing continue,
        # which a KILLED-swap or a break would otherwise survive as a false
        # green (#2552 r1 DS concurrency#2 + boundary#2, two independent
        # hits), and the mutating_sources call site's use of GUARDED_PATHS,
        # so the reserve cannot drift from the guarded set (DS sibling#4) --
        # the reserve arithmetic and the below-MAX_AGE invariant by value,
        # and the tail reserve's derivation (guarded-path count times the
        # real _git worst case, imported constants) so it cannot silently
        # become a hand-picked number again (DS concurrency#1).  #2552 r2:
        # the imported-constant derivation is only as real as the import,
        # so the import line and the KILL_TIMEOUT alias are pinned by bytes
        # and the constants are checked for identity against a fresh exec of
        # negative_process itself (DS r2 concurrency#1 -- a local
        # redefinition would shrink both sides of the derivation together
        # and stay green), and the clock line is pinned so the branch
        # cannot be made unreachable by shifting the clock origin
        # (DS r2 boundary#1), by moving it (adjacency to the guarded
        # for loop, whose first statement is the budget check -- the
        # memory-mode loop at the same depth is followed by one(row)
        # instead, so the anchor names the guarded branch, DS r4
        # boundary#2), or by duplicating it inside the loop body, where
        # a per-case reset would silently rearm the budget -- the
        # duplicate is counted semantically, by AST bindings
        # (count_name_bindings: every Store-context Name plus the
        # str-field binding forms, so any spacing, right-hand spelling,
        # unpacking target, for/with/comprehension target, def/class
        # name, import alias or capture counts -- DS r8 boundary#1, the
        # same spot a fourth time: no-space clock=time.monotonic() and
        # the RHS alias clock = monotonic() both survived the r7 regex,
        # live output in _tmp/20261109-2552-harvest/fix-r8-suspension-report.md;
        # design-change round authorised by coordinator arbitration,
        # fix-r9; DS r10 concurrency#1/boundary#1/sibling#1 reopened the
        # first cut's node-type enumeration through tuple-unpacking and
        # for-target resets, so the count is by binding context, not by
        # node type), while a different name such as saved_clock
        # (Claude r5 boundary#1) or an attribute form such as
        # self.clock is not a binding of clock and cannot false-red
        # it; the adjacency's clock and for
        # lines are 16-space with-body lines and its budget-check line
        # is a 20-space loop-body line, DS r4 simplify#2 + DS r5
        # boundary#2).
        marker = '#2552 a spent lease budget marks the remaining cases INVALID'
        path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/same-json-surfaces/negative_controls.py'
        try:
            source = path.read_bytes().decode()
            spec = importlib.util.spec_from_file_location('samejson_budget_drift', path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            negproc_path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/negative_process.py'
            negproc_spec = importlib.util.spec_from_file_location(
                'samejson_negproc_identity', negproc_path)
            negproc = importlib.util.module_from_spec(negproc_spec)
            exec(compile(negproc_path.read_bytes(), str(negproc_path), 'exec'), negproc.__dict__)
            reserve = (3 * (module.RUN_TIMEOUT_SECONDS + 2 * module.KILL_GRACE_SECONDS)
                       + module.TAIL_OVERHEAD_SECONDS)
            self.assertTrue(
                source.count('if lease_budget_left(clock, time.monotonic()) <= 0:') == 1
                and source.count("'s parity lease budget before this case") == 1
                and source.count("results.append({'name': row[0], 'status': 'INVALID',") == 1
                and source.count("' by name'})\n                        continue") == 1
                and source.count('mutating_sources(root, GUARDED_PATHS,') == 1
                and count_name_bindings(source, 'clock') == 1
                and source.count('\n                clock = time.monotonic()\n'
                                '                for row in selected:\n'
                                '                    if lease_budget_left(clock, '
                                'time.monotonic()) <= 0:\n') == 1
                and source.count('from negative_process import (GIT_TIMEOUT, KILL_TIMEOUT,') == 1
                and source.count('KILL_GRACE_SECONDS = KILL_TIMEOUT') == 1
                and module.GIT_TIMEOUT == negproc.GIT_TIMEOUT
                and module.KILL_GRACE_SECONDS == negproc.KILL_TIMEOUT
                and module.TAIL_OVERHEAD_SECONDS
                    == len(module.GUARDED_PATHS) * (module.GIT_TIMEOUT
                                                     + 2 * module.KILL_GRACE_SECONDS)
                    + module.FSYNC_AND_LEASE_SLACK_SECONDS
                and module.lease_budget_left(0, 0) == module.LEASE_BUDGET_SECONDS - reserve
                and module.lease_budget_left(0, module.LEASE_BUDGET_SECONDS - reserve) == 0
                and module.lease_budget_left(0, module.LEASE_BUDGET_SECONDS - reserve + 1) < 0
                and module.LEASE_BUDGET_SECONDS + module.TAIL_OVERHEAD_SECONDS
                    < module.load_pristine_common().MAX_AGE, marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_terminate_cleanly_raises_the_signal_exit_code(self):
        # #2552 (DS boundary#1): terminate_cleanly's body had
        # no pin at all, so replacing the raise with ``pass`` kept every
        # SameJsonSurfaces test green while a comment claimed the body was
        # pinned.  Call the real handler here for every signal it is
        # registered for: outside a restore window it must raise
        # SystemExit(128+signum) so the finally blocks still unwind, and
        # while a signal is deferred (restore in progress) it must return
        # without exiting -- negative_process re-raises it after the window
        # closes.
        marker = '#2552 terminate_cleanly exits 128+signum outside the restore window'
        path = common.ROOT / 'src/pilotdeck-gateway/tests/fixtures/same-json-surfaces/negative_controls.py'
        try:
            import signal
            spec = importlib.util.spec_from_file_location('samejson_terminate_pin', path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            outcomes = []
            for name in ('SIGTERM', 'SIGHUP', 'SIGQUIT'):
                number = getattr(signal, name, None)
                if number is None:
                    continue
                try:
                    module.terminate_cleanly(number, None)
                    outcomes.append((name, None))
                except SystemExit as error:
                    outcomes.append((name, error.code))
            with patch.object(module, 'defer_signal_exit', lambda _signum: True):
                deferred = module.terminate_cleanly(signal.SIGTERM, None)
            expected = [(name, 128 + getattr(signal, name)) for name
                        in ('SIGTERM', 'SIGHUP', 'SIGQUIT')
                        if hasattr(signal, name)]
            self.assertTrue(outcomes == expected and deferred is None, marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))


class RetirementScoring(unittest.TestCase):
    """#1355 fix-r2/r3: E1 retirement scoring rules (R1/R3/R6/R7/R9 + timeout
    + ①final-frame/②budget/④every-round inventory).

    Bounded offline regressions on the frozen scoring helpers: each rule gets
    a positive control (real-shaped evidence passes) and a negative control
    (the mutation the rule exists to catch fails).  They never substitute for
    a real TS round (memory ruler-real-data).
    """

    @classmethod
    def setUpClass(cls):
        cls.session = _load('session_domain', common.SCRIPT / 'session_domain.py')

    @staticmethod
    def _frames(send_only=False, final=False):
        # One stream method: a send frame plus (unless stripped) receive event
        # frames under the same id — the minimal real-shaped wire record.
        # final adds the terminal final:true event frame an ok outcome needs
        # (① fix-r3); without it the record is the exact "stream merely ended"
        # shape that must not pass as ok.
        frames = [{'seq': 1, 'connection': 1, 'direction': 'send',
                   'frame': {'type': 'request', 'id': 'id-1', 'method': 'submit_turn'}}]
        if not send_only:
            frames.append({'seq': 2, 'connection': 1, 'direction': 'receive',
                           'frame': {'type': 'event', 'id': 'id-1', 'final': False,
                                     'event': {'type': 'turn_started'}}})
            if final:
                frames.append({'seq': 3, 'connection': 1, 'direction': 'receive',
                               'frame': {'type': 'event', 'id': 'id-1', 'final': True,
                                         'event': {'type': 'turn_completed'}}})
        return frames

    def _round_row(self, outcomes, frames, http=None):
        return {'label': 'round-test', 'driverExitCode': 0, 'outcomes': outcomes,
                'frames': frames, 'http': http or [{'path': '/health', 'status': 200}],
                'processes': [{'pid': 7}],
                'runtime': {'pilotHome': '/nonexistent-home'}}

    def test_stream_outcomes_need_a_receive_frame(self):
        # R1: pre-fix, a stream error outcome with zero received frames still
        # counted as executed and passed the round.  ① (fix-r3): the ok
        # positive control now carries a final:true event frame — the old
        # final:false-only positive was the exact fake-pass shape.
        expected = ['submit_turn']
        for status, final in (('ok', True), ('error', False)):
            outcome = {'method': 'submit_turn', 'outcome': status, 'shape': 'stream'}
            verdict = self.session.score_retirement_round(
                self._round_row([outcome], self._frames(send_only=True)), expected)
            self.assertTrue(verdict['status'] == 'FAIL', f'#1 stream {status} send-only must FAIL: {verdict}')
            violation = next(p for p in verdict['rules'] if p['rule'] == 'EVIDENCE_BACKING')['detail']
            problem = ('stream ok without a final event frame' if status == 'ok'
                       else f'stream {status} without any receive frame')
            self.assertTrue(violation == [{'method': 'submit_turn', 'problem': problem}],
                            f'#2 stream {status} send-only violation: {violation}')
            self.assertIn('submit_turn', verdict['executed'], status)
            verdict = self.session.score_retirement_round(
                self._round_row([outcome], self._frames(final=final)), expected)
            self.assertTrue([p['rule'] for p in verdict['rules']] == [],
                            f'#3 stream {status} with backing must PASS: {verdict["rules"]}')
            self.assertTrue(verdict['executed'] == ['submit_turn'],
                            f'#4 stream {status} executed set: {verdict["executed"]}')

    def test_stream_ok_requires_a_final_event_frame(self):
        # ① (#1355 fix-r3): a stream that only ever emitted non-final events and
        # ended is an error outcome at the driver; a forged ok over the same
        # wire must fail EVIDENCE_BACKING.  The pre-fix scorer passed exactly
        # this shape (arbitration P0).
        outcome = {'method': 'submit_turn', 'outcome': 'ok', 'shape': 'stream'}
        verdict = self.session.score_retirement_round(
            self._round_row([outcome], self._frames()), ['submit_turn'])
        self.assertTrue(verdict['status'] == 'FAIL', f'#5 non-final-only ok must FAIL: {verdict}')
        violation = next(p for p in verdict['rules'] if p['rule'] == 'EVIDENCE_BACKING')['detail']
        self.assertTrue(violation == [{'method': 'submit_turn',
                                        'problem': 'stream ok without a final event frame'}],
                        f'#6 non-final-only ok violation: {violation}')
        self.assertIn('submit_turn', verdict['executed'])
        verdict = self.session.score_retirement_round(
            self._round_row([outcome], self._frames(final=True)), ['submit_turn'])
        self.assertTrue(verdict['status'] == 'PASS', f'#7 final-backed ok must PASS: {verdict}')
        self.assertTrue(verdict['executed'] == ['submit_turn'],
                        f'#8 final-backed ok executed set: {verdict["executed"]}')

    def test_driver_final_evidence_comes_from_the_wire(self):
        # ① (#1355 fix-r3): the driver's finalSeen must come from recorded wire
        # evidence (wireFinals/requestIds in the wrapper), in both the success
        # path and the catch path — the pre-fix driver set finalSeen from
        # iterator `done`, which is exactly the shape that let a final-less
        # stream be recorded as a completed one.
        source = (common.SCRIPT / 'retirement_driver.mts').read_text()
        self.assertTrue(source.count('wireFinals.has(requestIds.get(entry.method)') == 2,
                        f'#21 finalSeen must derive from wire evidence in both stream '
                        f'branches, found {source.count("wireFinals.has(requestIds.get(entry.method)")}')
        self.assertNotIn('finalSeen = true', source)
        self.assertTrue('wireFinals.add(String(wire.id))' in source
                        and "requestIds.set(String(wire.method), String(wire.id))" in source,
                        '#22 the wrapper must record request ids and final:true frames')

    def test_round_budget_covers_every_legal_driver_path(self):
        # ② (#1355 fix-r3): the budget is the sum of its legal worst-case
        # components (methods + HTTP probes + socket close + readiness + connect
        # margin), not the old 101×15+90=1605 that sat below the 1730s worst
        # legal path; the driver's own timeout values must stay in sync with
        # the constants it is budgeted against (.mts ↔ .py cross-check).
        session = self.session
        worst = (session.RETIREMENT_METHOD_COUNT * session.RETIREMENT_PER_METHOD_TIMEOUT_S
                 + session.RETIREMENT_HTTP_PROBE_COUNT * session.RETIREMENT_HTTP_PROBE_TIMEOUT_S
                 + session.RETIREMENT_SOCKET_CLOSE_TIMEOUT_S
                 + 2 * session.RETIREMENT_READINESS_TIMEOUT_S
                 + session.RETIREMENT_CONNECT_MARGIN_S)
        self.assertTrue(session.RETIREMENT_ROUND_TIMEOUT == worst,
                        f'#9 budget {session.RETIREMENT_ROUND_TIMEOUT} != component sum {worst}')
        self.assertTrue(session.RETIREMENT_ROUND_TIMEOUT >= 1730,
                        f'#10 budget {session.RETIREMENT_ROUND_TIMEOUT} below the 1730s worst legal path')
        source = (common.SCRIPT / 'retirement_driver.mts').read_text()
        self.assertIn(f'PER_METHOD_TIMEOUT_MS = {session.RETIREMENT_PER_METHOD_TIMEOUT_S * 1000};', source)
        self.assertIn(f'AbortSignal.timeout({session.RETIREMENT_HTTP_PROBE_TIMEOUT_S * 1000})', source)
        self.assertIn(f"'closed sockets', {session.RETIREMENT_SOCKET_CLOSE_TIMEOUT_S * 1000})", source)
        self.assertTrue(source.count('await probe(') == session.RETIREMENT_HTTP_PROBE_COUNT,
                        f'#11 driver probes {source.count("await probe(")} != budgeted '
                        f'{session.RETIREMENT_HTTP_PROBE_COUNT}')

    def test_timeout_expired_classifies_blocked_with_full_faces(self):
        # ② (#1355 fix-r3): subprocess.TimeoutExpired is a SubprocessError
        # subclass and not a TimeoutError, so the pre-fix BLOCKED tuple let it
        # fall through to FAIL, and retirement_report let it escape main leaving
        # a 9-key report whose scoring faces were all missing.
        self.assertIsInstance(subprocess.TimeoutExpired('node', 1), self.session.BLOCKED_APPARATUS_ERRORS)
        with patch.object(self.session, '_retirement_report',
                          side_effect=subprocess.TimeoutExpired('node', 1760)):
            report = self.session.retirement_report(Path('out'), Path('baseline'))
        self.assertTrue(report['status'] == 'BLOCKED', f'#12 budget-blocked status: {report["status"]}')
        self.assertTrue(report['executionStatus'] == 'BLOCKED',
                        f'#13 budget-blocked executionStatus: {report["executionStatus"]}')
        self.assertTrue(report['failure']['rule'] == 'ROUND_BUDGET',
                        f'#14 budget-blocked failure rule: {report["failure"]}')
        for face in ('expected', 'executed', 'rounds', 'methodCoverage', 'entryInventory',
                     'hostAudit', 'controls', 'coldRestartRollback', 'topology', 'portAudit',
                     'processAudit', 'cleanupAudit', 'baseline', 'reason', 'failure'):
            self.assertIn(face, report, f'#15 budget-blocked report lacks face {face}')
        for face, status in (('methodCoverage', 'BLOCKED'), ('entryInventory', 'BLOCKED'),
                             ('hostAudit', 'BLOCKED'), ('controls', 'NOT_RUN'),
                             ('coldRestartRollback', 'NOT_RUN')):
            self.assertTrue(report[face]['status'] == status,
                            f'#16 budget-blocked face {face}: {report[face]["status"]} != {status}')

    def test_entry_inventory_reads_every_round(self):
        # ④ (#1355 fix-r3): an entry that answered in round 1 but 404'd in
        # round 2 is NO_HIT — the pre-fix face only read rounds[0] and hid the
        # second round's answer.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = self._fake_baseline(root)
            home = root / 'home'
            home.mkdir()
            (home / 'pilotdeck.yaml').write_text('schemaVersion: 1\n')

            def round_row(label, health_status):
                return {'label': label, 'http': [{'path': '/health', 'status': health_status}],
                        'frames': self._frames(final=True),
                        'outcomes': [{'method': 'submit_turn', 'outcome': 'ok', 'shape': 'stream'}],
                        'runtime': {'pilotHome': str(home)}}

            def aggregate(rows):
                return self.session._retirement_entry_inventory_aggregate(
                    [(row['label'], self.session._retirement_entry_inventory(baseline, row))
                     for row in rows])

            merged = aggregate([round_row('round-01', 200), round_row('round-02', 404)])
            health = next(entry for entry in merged['entries'] if entry['entry'] == '/health')
            self.assertTrue(health['status'] == 'NO_HIT', f'#17 mixed rounds must aggregate NO_HIT: {health}')
            self.assertTrue(health['rounds'] == [{'round': 'round-01', 'status': 'HIT'},
                                                 {'round': 'round-02', 'status': 'NO_HIT'}],
                            f'#18 per-round statuses must stay visible: {health["rounds"]}')
            self.assertIn('/health', merged['unhit'])
            self.assertTrue(merged['status'] == 'BLOCKED', f'#19 inventory face must stay BLOCKED: {merged["status"]}')
            merged = aggregate([round_row('round-01', 200), round_row('round-02', 200)])
            health = next(entry for entry in merged['entries'] if entry['entry'] == '/health')
            self.assertTrue(health['status'] == 'HIT', f'#20 all-answer rounds aggregate HIT: {health}')
            self.assertNotIn('/health', merged['unhit'])

    REPORT_FACES = ('expected', 'executed', 'rounds', 'methodCoverage', 'entryInventory',
                    'hostAudit', 'controls', 'coldRestartRollback', 'topology', 'portAudit',
                    'processAudit', 'cleanupAudit', 'baseline', 'reason', 'executionStatus',
                    'acceptanceStatus')

    def _with_round_faces(self, row, label):
        # The audit faces every legal round carries; _round_row covers the
        # scoring inputs only.
        row.update({
            'label': label,
            'ports': {'mock': 28761, 'gateway': 28762},
            'census': {'rows': [{'pid': 7}], 'declared': [7], 'declaredAlive': True},
            'connections': {'rows': [{'peer': '127.0.0.1:49153', 'raw': 'x'}],
                            'commandStatus': 'ok'},
            'listeners': {'pid': 7, 'addresses': ['127.0.0.1:28762'], 'loopbackOnly': True},
            'audit': {'allStopped': True},
        })
        return row

    def _degraded_round_row(self, label='round-01'):
        # A legal degraded round: the only stream method answered with an
        # error (backed by a receive frame) instead of ok, while a request
        # method still answered ok.  Round-01 therefore has no ok stream
        # outcome for the STREAM_OK_NON_FINAL control to mutate.
        outcomes = [
            {'method': 'submit_turn', 'outcome': 'error', 'errorCode': 'PROVIDER_UNAVAILABLE',
             'shape': 'stream'},
            {'method': 'run_get', 'outcome': 'ok', 'shape': 'request'},
        ]
        frames = [
            {'seq': 1, 'connection': 1, 'direction': 'send',
             'frame': {'type': 'request', 'id': 'id-turn', 'method': 'submit_turn'}},
            {'seq': 2, 'connection': 1, 'direction': 'receive',
             'frame': {'type': 'event', 'id': 'id-turn', 'final': True,
                       'event': {'type': 'error', 'code': 'PROVIDER_UNAVAILABLE'}}},
            {'seq': 3, 'connection': 1, 'direction': 'send',
             'frame': {'type': 'request', 'id': 'id-get', 'method': 'run_get'}},
            {'seq': 4, 'connection': 1, 'direction': 'receive',
             'frame': {'type': 'response', 'id': 'id-get', 'ok': True, 'result': {}}},
        ]
        return self._with_round_faces(self._round_row(outcomes, frames), label)

    def _all_error_round_row(self, label='round-01'):
        # Provider fully unavailable: every method answered with an error, so
        # there is no ok outcome (control 1) and no stream outcome (controls 5
        # and 6) to mutate.
        outcomes = [{'method': 'run_get', 'outcome': 'error',
                     'errorCode': 'PROVIDER_UNAVAILABLE', 'shape': 'request'}]
        frames = [
            {'seq': 1, 'connection': 1, 'direction': 'send',
             'frame': {'type': 'request', 'id': 'id-get', 'method': 'run_get'}},
            {'seq': 2, 'connection': 1, 'direction': 'receive',
             'frame': {'type': 'response', 'id': 'id-get', 'ok': False,
                       'error': {'code': 'PROVIDER_UNAVAILABLE', 'message': 'controlled'}}},
        ]
        return self._with_round_faces(self._round_row(outcomes, frames), label)

    def _stream_ok_round_row(self, *, send_frame=True, terminal_frame=True, duplicate_terminal=False,
                             label='round-01'):
        outcomes = [{'method': 'submit_turn', 'outcome': 'ok', 'shape': 'stream'},
                    {'method': 'run_get', 'outcome': 'ok', 'shape': 'request'}]
        frames = []
        if send_frame:
            frames.append({'seq': 1, 'connection': 1, 'direction': 'send',
                           'frame': {'type': 'request', 'id': 'id-turn', 'method': 'submit_turn'}})
        if terminal_frame:
            frames.append({'seq': 2, 'connection': 1, 'direction': 'receive',
                           'frame': {'type': 'event', 'id': 'id-turn', 'final': True,
                                     'event': {'type': 'done'}}})
        if duplicate_terminal:
            frames.append({'seq': 5, 'connection': 1, 'direction': 'receive',
                           'frame': {'type': 'event', 'id': 'id-turn', 'final': True,
                                     'event': {'type': 'done'}}})
        frames.append({'seq': 3, 'connection': 1, 'direction': 'send',
                       'frame': {'type': 'request', 'id': 'id-get', 'method': 'run_get'}})
        frames.append({'seq': 4, 'connection': 1, 'direction': 'receive',
                       'frame': {'type': 'response', 'id': 'id-get', 'ok': True, 'result': {}}})
        return self._with_round_faces(self._round_row(outcomes, frames), label)

    def _report_for(self, row, expected):
        # Assemble the real report around one synthetic round: only the round
        # inputs and the baseline-only faces are patched, so the controls run
        # against the real scorer/aggregator.
        with patch.object(self.session, '_retirement_expected', return_value=(expected, [])), \
                patch.object(self.session, '_retirement_baseline_revision', return_value='deadbeef'), \
                patch.object(self.session, '_retirement_ports_available', return_value=True), \
                patch.object(self.session, '_retirement_round', return_value=row), \
                patch.object(self.session, '_retirement_entry_inventory',
                             return_value={'status': 'PASS', 'unhit': [], 'gated': [], 'entries': []}), \
                patch.object(self.session, '_retirement_entry_inventory_aggregate',
                             return_value={'status': 'PASS', 'unhit': [], 'gated': [], 'entries': []}), \
                patch.object(self.session, '_retirement_js_host_census',
                             return_value={'status': 'PASS'}):
            return self.session.retirement_report(Path('out'), Path('baseline'))

    def _assert_full_faces(self, report, tag):
        for face in self.REPORT_FACES:
            self.assertIn(face, report, f'{tag} report lacks face {face}')
        # coldRestartRollback is BLOCKED until #1354, so the total verdict is
        # always at least BLOCKED; the controls face is asserted separately.
        self.assertEqual(report['controls']['status'], 'BLOCKED', report['controls'])
        self.assertNotEqual(report['executionStatus'], 'NOT_RUN')
        return report

    def test_degraded_round_keeps_full_report_and_marks_control_unavailable(self):
        # P1 (#1355 fix-r4): when round-01 has no ok stream outcome (a legal
        # degraded round), the STREAM_OK_NON_FINAL control cannot be mutated.
        # Pre-fix it raised EvidenceError and retirement_report returned a
        # 5-key BLOCKED stub with all 16 scoring faces missing.  The control is
        # now NOT_APPLICABLE (+reason), the controls face is BLOCKED (never
        # PASS / NOT_RUN), and the full report is still assembled.
        expected = ['submit_turn', 'run_get']
        row = self._degraded_round_row()
        controls = self.session._retirement_controls(row, expected)
        # Every class is still evaluated on the same round; only the
        # unestablishable one is marked unavailable (never PASS).
        self.assertEqual([(c['class'], c['status']) for c in controls['classes']],
                         [('TS_FALLBACK', 'PASS'), ('TS_HOST', 'PASS'),
                          ('UNIFORM_UNAVAILABLE', 'PASS'), ('FORWARDING_MISLABELED', 'PASS'),
                          ('STREAM_ERROR_NO_FRAME', 'PASS'),
                          ('STREAM_OK_NON_FINAL', 'NOT_APPLICABLE')])
        unavailable = next(c for c in controls['classes'] if c['class'] == 'STREAM_OK_NON_FINAL')
        self.assertIn('no ok stream outcome', unavailable['reason'])
        report = self._assert_full_faces(self._report_for(row, expected), '#31 degraded-round')
        self.assertTrue(report['controls']['reason'].startswith('positive mutation control(s)'),
                        report['controls'].get('reason'))

    def test_unestablishable_controls_keep_the_full_report(self):
        # P1 (#1355 fix-r4, second round): the same "cannot establish" rule
        # holds for every control class, not just the stream-ok one.  An
        # all-error round (a legal degraded round: provider fully unavailable)
        # has no ok outcome for TS_FALLBACK and no stream outcome for
        # STREAM_ERROR_NO_FRAME / STREAM_OK_NON_FINAL.  Pre-fix the first
        # `require` raised and the report collapsed to the 5-key stub.
        expected = ['run_get']
        controls = self.session._retirement_controls(self._all_error_round_row(), expected)
        self.assertEqual([(c['class'], c['status']) for c in controls['classes']],
                         [('TS_FALLBACK', 'NOT_APPLICABLE'), ('TS_HOST', 'PASS'),
                          ('UNIFORM_UNAVAILABLE', 'PASS'), ('FORWARDING_MISLABELED', 'PASS'),
                          ('STREAM_ERROR_NO_FRAME', 'NOT_APPLICABLE'),
                          ('STREAM_OK_NON_FINAL', 'NOT_APPLICABLE')])
        reasons = {c['class']: c.get('reason', '') for c in controls['classes']}
        self.assertIn('no ok outcome', reasons['TS_FALLBACK'])
        self.assertIn('no stream outcome', reasons['STREAM_ERROR_NO_FRAME'])
        report = self._assert_full_faces(self._report_for(self._all_error_round_row(), expected),
                                         '#32 all-error-round')
        # The round itself is still scored: the controls face is what turned
        # BLOCKED, never a silent PASS.
        self.assertEqual(report['executionStatus'], 'BLOCKED', report['executionStatus'])

    def test_stream_ok_control_unavailable_without_send_frame_or_single_terminal(self):
        # P1 (#1355 fix-r4): the two remaining unestablishable shapes of the
        # STREAM_OK_NON_FINAL control — no send frame to locate the terminal
        # event (pre-fix a KeyError escaped to main) and no single final:true
        # frame to downgrade (pre-fix an EvidenceError collapsed the report).
        expected = ['submit_turn', 'run_get']
        cases = (
            # Without the send frame neither wire-stripping class can mutate
            # anything; the nominal `no terminal frame` round still has the
            # send frame, so the TS_FALLBACK injection lands and is scored.
            ('no send frame', self._stream_ok_round_row(send_frame=False),
             'no recorded send frame', 'NOT_APPLICABLE'),
            ('no terminal frame', self._stream_ok_round_row(terminal_frame=False),
             'no terminal final:true event frame', 'PASS'),
        )
        for tag, row, reason, ts_fallback in cases:
            controls = self.session._retirement_controls(row, expected)
            class6 = next(c for c in controls['classes'] if c['class'] == 'STREAM_OK_NON_FINAL')
            self.assertEqual(class6['status'], 'NOT_APPLICABLE', f'#33 {tag}: {class6}')
            self.assertIn(reason, class6['reason'], f'#33 {tag}')
            # P1 (#1355 fix-r4b, both families): without a send frame the
            # wire-stripping classes mutate nothing; pre-fix they recorded PASS
            # off the round's own `send frame missing` failure.
            classes = {c['class']: c['status'] for c in controls['classes']}
            self.assertEqual(classes['TS_FALLBACK'], ts_fallback, f'#33 {tag}: {controls}')
            self.assertEqual(classes['STREAM_ERROR_NO_FRAME'], 'NOT_APPLICABLE',
                             f'#33 {tag}: {controls}')
            report = self._assert_full_faces(self._report_for(row, expected), f'#33 {tag}')
            # These rounds carry a real scored FAIL (stream ok with no wire
            # backing / no final:true frame); the report must surface it
            # instead of the pre-fix 5-key NOT_RUN stub.
            self.assertEqual(report['executionStatus'], 'FAIL', tag)

    def test_empty_mutation_is_never_a_control_pass(self):
        # P1 (#1355 fix-r4b, both families): the control round's *first* ok
        # method has no send frame while every other method is fully backed.
        # Pre-fix `_retirement_strip_wire` removed nothing and the round's own
        # `send frame missing` violation was credited to TS_FALLBACK as PASS —
        # the control never proved the scorer catches the injected fake.  Same
        # for the stream-error class when the stream method has no receive
        # frame under its send frame.
        expected = ['run_get', 'submit_turn']
        outcome = {'method': 'run_get', 'outcome': 'ok', 'shape': 'request'}
        rows = self._round_row(
            [outcome, {'method': 'submit_turn', 'outcome': 'ok', 'shape': 'stream'}],
            [{'seq': 1, 'connection': 1, 'direction': 'send',
              'frame': {'type': 'request', 'id': 'id-turn', 'method': 'submit_turn'}},
             {'seq': 2, 'connection': 1, 'direction': 'receive',
              'frame': {'type': 'event', 'id': 'id-turn', 'final': True,
                        'event': {'type': 'done'}}}])
        row = self._with_round_faces(rows, 'round-01')
        controls = self.session._retirement_controls(row, expected)
        classes = {c['class']: c for c in controls['classes']}
        self.assertEqual(classes['TS_FALLBACK']['status'], 'NOT_APPLICABLE', controls)
        self.assertIn('no send frame', classes['TS_FALLBACK']['reason'])
        # The request method's own outcome is already unbacked, so the round
        # itself fails; the control face must not claim a proof it never ran.
        self.assertNotEqual(classes['TS_FALLBACK']['status'], 'PASS')
        # The stream class is still establishable here: its receive frame is
        # really stripped, so that control keeps its PASS.
        self.assertEqual(classes['STREAM_ERROR_NO_FRAME']['status'], 'PASS', controls)
        report = self._assert_full_faces(self._report_for(row, expected), '#34 empty-mutation')
        self.assertEqual(report['controls']['status'], 'BLOCKED', report['controls'])

    def test_duplicate_terminal_frames_are_scored_not_unavailable(self):
        # P2a (anthropic boundary #2): a round with two final:true frames under
        # the same id is a legal round for the scorer.  The downgrade mutation
        # lands on both frames, so the control is established and must be
        # scored; marking it unavailable pinned the controls face to BLOCKED
        # and the total verdict could never reach PASS (false BLOCKED).
        expected = ['submit_turn', 'run_get']
        row = self._stream_ok_round_row(duplicate_terminal=True)
        controls = self.session._retirement_controls(row, expected)
        class6 = next(c for c in controls['classes'] if c['class'] == 'STREAM_OK_NON_FINAL')
        self.assertEqual(class6['status'], 'PASS', class6)
        report = self._report_for(row, expected)
        for face in self.REPORT_FACES:
            self.assertIn(face, report, f'#35 report lacks face {face}')

    def test_methodless_outcomes_make_the_controls_unavailable(self):
        # P2a (anthropic sibling #3 / boundary #3): the `and row.get('method')`
        # filters must skip nameless rows instead of selecting them; with no
        # named outcome at all, the three wire-mutation classes are
        # unavailable — never PASS, never a raised EvidenceError.
        expected = ['run_get']
        row = self._with_round_faces(self._round_row(
            [{'outcome': 'ok', 'shape': 'request'}, {'outcome': 'ok', 'shape': 'stream'}],
            [{'seq': 1, 'connection': 1, 'direction': 'send',
              'frame': {'type': 'request', 'id': 'id-get', 'method': 'run_get'}},
             {'seq': 2, 'connection': 1, 'direction': 'receive',
              'frame': {'type': 'response', 'id': 'id-get', 'ok': True, 'result': {}}}]), 'round-01')
        controls = self.session._retirement_controls(row, expected)
        classes = {c['class']: c['status'] for c in controls['classes']}
        for name in ('TS_FALLBACK', 'STREAM_ERROR_NO_FRAME', 'STREAM_OK_NON_FINAL'):
            self.assertEqual(classes[name], 'NOT_APPLICABLE', f'#36 {name}: {controls}')
        report = self._assert_full_faces(self._report_for(row, expected), '#36 methodless')
        # Same filters, positive side: a nameless row ahead of a named one must
        # be skipped, not selected — otherwise the class degrades to
        # NOT_APPLICABLE on a round where the mutation is perfectly
        # establishable (false BLOCKED).
        expected = ['run_get', 'submit_turn']
        row = self._with_round_faces(self._round_row(
            [{'method': '', 'outcome': 'ok', 'shape': 'request'},
             {'method': 'run_get', 'outcome': 'ok', 'shape': 'request'},
             {'method': '', 'outcome': 'ok', 'shape': 'stream'},
             {'method': 'submit_turn', 'outcome': 'ok', 'shape': 'stream'}],
            [{'seq': 1, 'connection': 1, 'direction': 'send',
              'frame': {'type': 'request', 'id': 'id-get', 'method': 'run_get'}},
             {'seq': 2, 'connection': 1, 'direction': 'receive',
              'frame': {'type': 'response', 'id': 'id-get', 'ok': True, 'result': {}}},
             {'seq': 3, 'connection': 1, 'direction': 'send',
              'frame': {'type': 'request', 'id': 'id-turn', 'method': 'submit_turn'}},
             {'seq': 4, 'connection': 1, 'direction': 'receive',
              'frame': {'type': 'event', 'id': 'id-turn', 'final': True,
                        'event': {'type': 'done'}}}]), 'round-01')
        controls = self.session._retirement_controls(row, expected)
        classes = {c['class']: c for c in controls['classes']}
        for name in ('TS_FALLBACK', 'STREAM_ERROR_NO_FRAME', 'STREAM_OK_NON_FINAL'):
            self.assertEqual(classes[name]['status'], 'PASS', f'#36b {name}: {controls}')
        self.assertIn("'run_get'", classes['TS_FALLBACK']['injection'])
        self.assertIn("'submit_turn'", classes['STREAM_OK_NON_FINAL']['injection'])

    def test_uniform_unavailable_control_unavailable_when_round_already_matches(self):
        # P1 (#1355 fix-r4c, anthropic sibling #1): a round that already
        # answered every method CAPABILITY_UNAVAILABLE with matching wire is
        # unchanged by the class-3 rewrite, so a PASS would only re-report the
        # round's own violation.  The class is NOT_APPLICABLE, never PASS.
        expected = ['run_get']
        row = self._with_round_faces(self._round_row(
            [{'method': 'run_get', 'outcome': 'error', 'errorCode': 'CAPABILITY_UNAVAILABLE',
              'shape': 'request'}],
            [{'seq': 1, 'connection': 1, 'direction': 'send',
              'frame': {'type': 'request', 'id': 'id-get', 'method': 'run_get'}},
             {'seq': 2, 'connection': 1, 'direction': 'receive',
              'frame': {'type': 'response', 'id': 'id-get', 'ok': False,
                        'error': {'code': 'CAPABILITY_UNAVAILABLE', 'message': 'x'}}}]), 'round-01')
        controls = self.session._retirement_controls(row, expected)
        class3 = next(c for c in controls['classes'] if c['class'] == 'UNIFORM_UNAVAILABLE')
        self.assertEqual(class3['status'], 'NOT_APPLICABLE', class3)
        self.assertIn('already answered', class3['reason'])
        report = self._assert_full_faces(self._report_for(row, expected), '#37 uniform-noop')

    def test_host_census_control_unavailable_without_gateway_pid(self):
        # P2a (#1355 fix-r4c, anthropic sibling #2): the class-2 injection is
        # "the gateway pid stays in the census but leaves the declared set".
        # Without that pid in the census rows the mutation proves nothing: an
        # empty census made the class FAIL (false FAIL) and a census holding
        # only some other pid made it PASS off a pre-existing undeclared row.
        expected = ['submit_turn', 'run_get']
        for tag, census_rows in (('empty census', []), ('other pid only', [{'pid': 999}])):
            row = self._stream_ok_round_row()
            row['census'] = {'rows': census_rows, 'declared': [7], 'declaredAlive': True}
            controls = self.session._retirement_controls(row, expected)
            class2 = next(c for c in controls['classes'] if c['class'] == 'TS_HOST')
            self.assertEqual(class2['status'], 'NOT_APPLICABLE', f'#38 {tag}: {class2}')
            self.assertIn('absent from the census rows', class2['reason'])
            self._assert_full_faces(self._report_for(row, expected), f'#38 {tag}')

    def test_host_control_survives_null_listeners_and_attributes_the_pid(self):
        # P1 (#1355 fix-r4d, anthropic boundary #1) + P2 (#1355 fix-r4d,
        # minimax boundary #3).  Three shapes:
        #  - `listeners: null` (the probe recorded nothing): pre-fix
        #    `round_row.get('listeners', {}).get('pid')` raised AttributeError
        #    and the report collapsed to the 5-key stub.
        #  - a census whose declared set never held the gateway pid: the
        #    injection removes nothing, so the class is unestablished.
        #  - a census holding a leftover undeclared row *besides* the gateway
        #    pid: emptying the declared set would fail on that row alone
        #    (false PASS); the class must be attributed to the gateway pid.
        expected = ['submit_turn', 'run_get']
        row = self._stream_ok_round_row()
        row['listeners'] = None
        controls = self.session._retirement_controls(row, expected)
        class2 = next(c for c in controls['classes'] if c['class'] == 'TS_HOST')
        self.assertEqual(class2['status'], 'NOT_APPLICABLE', f'#39 null listeners: {class2}')
        self.assertIn('absent from the census rows', class2['reason'])
        self._assert_full_faces(self._report_for(row, expected), '#39 null listeners')

        row = self._stream_ok_round_row()
        row['census'] = {'rows': [{'pid': 7}], 'declared': [], 'declaredAlive': True}
        controls = self.session._retirement_controls(row, expected)
        class2 = next(c for c in controls['classes'] if c['class'] == 'TS_HOST')
        self.assertEqual(class2['status'], 'NOT_APPLICABLE', f'#39 pid never declared: {class2}')
        self.assertIn("not in the round's declared owned set", class2['reason'])

        row = self._stream_ok_round_row()
        row['census'] = {'rows': [{'pid': 7}, {'pid': 59999}], 'declared': [7],
                         'declaredAlive': True}
        # The round's own census already fails on the leftover row; the class
        # is still establishable because exactly the gateway pid becomes
        # undeclared.  A blanket "census must be clean" rule would instead
        # report a false NOT_APPLICABLE here.
        self.assertEqual(
            self.session._retirement_census_verdict(row['census'])['status'], 'FAIL')
        controls = self.session._retirement_controls(row, expected)
        class2 = next(c for c in controls['classes'] if c['class'] == 'TS_HOST')
        self.assertEqual(class2['status'], 'PASS', f'#39 leftover row: {class2}')
        report = self._report_for(row, expected)
        for face in self.REPORT_FACES:
            self.assertIn(face, report, f'#39 leftover row lacks face {face}')
        # All six classes are established on this round, so the controls face
        # is a PASS even though the round's own census (correctly) fails.
        self.assertEqual(report['controls']['status'], 'PASS', report['controls'])

    def test_uniform_unavailable_control_judges_the_outcome_dimension(self):
        # P1 (#1355 fix-r4d, minimax boundary #1/#2): the "did the injection
        # land" test used to be a whole-round diff, so a frame flip made a
        # round with untouched outcomes look mutated.  Both sides were wrong:
        # outcomes already uniform + an ok:true frame scored the round's own
        # UNIFORM_UNAVAILABLE as a control PASS, and zero outcomes + an ok:true
        # frame scored an unestablishable class FAIL.
        expected = ['run_get']
        send = {'seq': 1, 'connection': 1, 'direction': 'send',
                'frame': {'type': 'request', 'id': 'id-get', 'method': 'run_get'}}
        ok_response = {'seq': 2, 'connection': 1, 'direction': 'receive',
                       'frame': {'type': 'response', 'id': 'id-get', 'ok': True, 'result': {}}}
        contradictory = [{'method': 'run_get', 'outcome': 'error',
                          'errorCode': 'CAPABILITY_UNAVAILABLE', 'shape': 'request'}]
        row = self._with_round_faces(self._round_row(contradictory, [send, ok_response]), 'round-01')
        original = self.session.score_retirement_round(row, expected)
        self.assertIn('UNIFORM_UNAVAILABLE', [p['rule'] for p in original['rules']],
                      f'#40 the violation must be the round\'s own: {original["rules"]}')
        controls = self.session._retirement_controls(row, expected)
        class3 = next(c for c in controls['classes'] if c['class'] == 'UNIFORM_UNAVAILABLE')
        self.assertEqual(class3['status'], 'NOT_APPLICABLE', f'#40 contradictory: {class3}')
        self.assertIn('changes no outcome', class3['reason'])
        self._assert_full_faces(self._report_for(row, expected), '#40 contradictory')

        for tag, frames in (('ok:true frame', [send, ok_response]),
                            ('no ok:true frame', [send])):
            row = self._with_round_faces(self._round_row([], frames), 'round-01')
            controls = self.session._retirement_controls(row, expected)
            class3 = next(c for c in controls['classes'] if c['class'] == 'UNIFORM_UNAVAILABLE')
            self.assertEqual(class3['status'], 'NOT_APPLICABLE', f'#40 zero outcomes {tag}: {class3}')
            self.assertIn('recorded no method outcome', class3['reason'])
            self._assert_full_faces(self._report_for(row, expected), f'#40 zero outcomes {tag}')

    def test_forwarding_control_unavailable_on_an_unobservable_sample(self):
        # P1 (#1355 fix-r4d, same family as minimax boundary #3): class 4 used
        # to rebuild the observation from `rows` only, dropping the round's
        # commandStatus — a failed lsof then scored a fabricated sample side.
        # P1 (#1355 fix-r4e, anthropic sibling #1 = minimax boundary #1): it
        # then judged observability on the *mutated* verdict, and because
        # `undeclared` is checked before `unparsed or not rows`, an empty or
        # unparsed sample flipped BLOCKED -> FAIL and the class recorded PASS.
        # Observability is now a property of the round's own sample: failed
        # lsof, zero parsed rows, an unparsed line, and a missing observation
        # all leave the class unestablished.  A round that already reports
        # undeclared peers stays establishable — the injected peer has its own
        # address, so attribution is the exact set delta (the real r2 round
        # reports provider egress, registered as #1478).
        expected = ['submit_turn', 'run_get']
        samples = (
            ('failed lsof', {'rows': [], 'commandStatus': 'failed', 'reason': 'lsof exited 1'},
             'unobservable'),
            ('empty sample', {'rows': [], 'commandStatus': 'ok'}, 'unobservable'),
            ('unparsed line', {'rows': [{'peer': '127.0.0.1:49153', 'raw': 'x'}],
                               'unparsed': [{'peer': None, 'raw': 'junk'}],
                               'commandStatus': 'ok'}, 'unobservable'),
            # P1 (#1355 fix-r4f, anthropic sibling #1): with a pre-existing
            # undeclared peer the round's own verdict is FAIL, not BLOCKED, so
            # a status-based observability test credited the injection against
            # an unparsed sample.  Observability is the observation's shape.
            ('unparsed line + own peer',
             {'rows': [{'peer': '180.101.242.203:443', 'raw': 'x'}],
              'unparsed': [{'peer': None, 'raw': 'junk'}], 'commandStatus': 'ok'},
             'unobservable'),
        )
        for tag, observation, reason in samples:
            row = self._stream_ok_round_row()
            row['connections'] = observation
            # The round's own network face must be non-PASS for these shapes —
            # that is exactly why the control cannot be credited to a PASS.
            self.assertNotEqual(
                self.session._retirement_network_verdict(observation)['status'], 'PASS', tag)
            controls = self.session._retirement_controls(row, expected)
            class4 = next(c for c in controls['classes'] if c['class'] == 'FORWARDING_MISLABELED')
            self.assertEqual(class4['status'], 'NOT_APPLICABLE', f'#41 {tag}: {class4}')
            self.assertIn(reason, class4['reason'], f'#41 {tag}: {class4}')
            self._assert_full_faces(self._report_for(row, expected), f'#41 {tag}')
        # A round with no network observation at all is the same family.
        row = self._stream_ok_round_row()
        del row['connections']
        controls = self.session._retirement_controls(row, expected)
        class4 = next(c for c in controls['classes'] if c['class'] == 'FORWARDING_MISLABELED')
        self.assertEqual(class4['status'], 'NOT_APPLICABLE', f'#41 no observation: {class4}')
        self.assertIn('recorded no network observation', class4['reason'])
        # P1 (#1355 fix-r4f, anthropic boundary #1): the round already reports
        # the injected peer itself, so the injection changes nothing and the
        # set delta would be empty — never PASS.
        row = self._stream_ok_round_row()
        row['connections'] = {'rows': [{'peer': '203.0.113.7:443', 'raw': 'x'}],
                              'commandStatus': 'ok'}
        controls = self.session._retirement_controls(row, expected)
        class4 = next(c for c in controls['classes'] if c['class'] == 'FORWARDING_MISLABELED')
        self.assertEqual(class4['status'], 'NOT_APPLICABLE', f'#41 own injected peer: {class4}')
        self.assertIn('already reports', class4['reason'])
        self._assert_full_faces(self._report_for(row, expected), '#41 own injected peer')
        # Positive side: the same round with a usable sample still passes, and
        # so does a round that already reports an undeclared peer of its own
        # (the r2 shape): the injected peer must be the exact set delta.
        for tag, observation in (
                ('usable sample', {'rows': [{'peer': '127.0.0.1:49153', 'raw': 'x'}],
                                   'commandStatus': 'ok'}),
                ('pre-existing peer', {'rows': [{'peer': '127.0.0.1:49153', 'raw': 'x'},
                                                {'peer': '180.101.242.203:443', 'raw': 'y'}],
                                       'commandStatus': 'ok'})):
            row = self._stream_ok_round_row()
            row['connections'] = observation
            controls = self.session._retirement_controls(row, expected)
            class4 = next(c for c in controls['classes'] if c['class'] == 'FORWARDING_MISLABELED')
            self.assertEqual(class4['status'], 'PASS', f'#41 {tag}: {class4}')

    def test_report_keeps_full_faces_when_a_round_omits_an_observation(self):
        # P1 (#1355 fix-r4e, minimax boundary #2; anthropic sibling #2 graded
        # it P2 — the higher grade wins): the controls path learned to tolerate
        # a missing/null observation, but the report path still hard indexed
        # `row['census']` / `row['connections']` / `row['listeners']`, so the
        # same round raised KeyError out of retirement_report and main's
        # fallback dropped all 16 scoring faces.  The registered criterion
        # (diffs/A8.6-RETIREMENT-001.md) promises the full report for these
        # shapes, and the missing observation must read as unobserved
        # (BLOCKED), never as a clean empty sample (PASS).
        expected = ['submit_turn', 'run_get']
        faces = {
            'census': ('processCensus', 'BLOCKED'),
            'connections': ('network', 'BLOCKED'),
            'listeners': ('listeners', 'FAIL'),
        }
        for key, (face, status) in faces.items():
            for tag, value in (('missing', KeyError), ('null', None)):
                row = self._stream_ok_round_row()
                if value is KeyError:
                    del row[key]
                else:
                    row[key] = value
                report = self._report_for(row, expected)
                for name in self.REPORT_FACES:
                    self.assertIn(name, report, f'#43 {tag} {key} lacks face {name}')
                self.assertNotEqual(report['executionStatus'], 'NOT_RUN', f'#43 {tag} {key}')
                self.assertEqual(report['hostAudit'][face][0]['status'], status,
                                 f'#43 {tag} {key}: {report["hostAudit"][face]}')
                self.assertNotEqual(report['hostAudit']['status'], 'PASS', f'#43 {tag} {key}')

    def test_present_but_malformed_observations_keep_their_faces(self):
        # #2519 (p2batch-d7 sibling #2, coord2 a档): the "present but
        # malformed" half — the observation key holds truthy garbage of the
        # wrong type.  #43 above pins missing/null; this pins truthy garbage
        # per observation key on both faces (report face keeps all 16 faces
        # and reads unobserved/FAIL, control face reads NOT_APPLICABLE), so
        # narrowing any of the isinstance guards turns this red.  Shapes are
        # measured current behavior, not reviewer inference (probe:
        # _tmp/20261109-p2batch-d7-harvest/probe-garbage-shapes.py):
        # census is guarded on both faces (any non-dict / non-list-rows shape
        # reads unobserved); connections and listeners read unobserved for
        # non-dict shapes — for listeners, the observation holds
        # {'pid','addresses','loopbackOnly'} (no rows), so the FAIL /
        # TS_HOST NOT_APPLICABLE below is the dropped pid leaving the
        # control unestablished, not any rows tolerance.  The unregistered
        # crash family — element-level garbage still raises out of the
        # report face (connections rows non-list or with non-dict elements
        # e.g. {'rows': 5} / {'rows': [1]}; census rows elements non-dict or
        # missing pid e.g. {'rows': [5]} / {'rows': [{}]}; census declared
        # non-iterable) — is pre-existing behavior outside this PR's two
        # registered criteria and is real-round unreachable; it is
        # documented here rather than pinned, so a future hardening is free
        # to fix it.
        expected = ['submit_turn', 'run_get']
        census = self._stream_ok_round_row()['census']
        cases = (  # (tag, key, value, hostAudit face, its status, control class)
            ('non-dict census', 'census', 'x',
             'processCensus', 'BLOCKED', 'TS_HOST'),
            ('garbage census rows', 'census', {**census, 'rows': 'garbage'},
             'processCensus', 'BLOCKED', 'TS_HOST'),
            ('int census rows', 'census', {**census, 'rows': 5},
             'processCensus', 'BLOCKED', 'TS_HOST'),
            ('non-dict connections', 'connections', 'x',
             'network', 'BLOCKED', 'FORWARDING_MISLABELED'),
            ('non-dict listeners', 'listeners', 'x',
             'listeners', 'FAIL', 'TS_HOST'),
            ('int listeners rows', 'listeners', {'rows': 5},
             'listeners', 'FAIL', 'TS_HOST'),
        )
        for tag, key, value, face, status, control in cases:
            row = self._stream_ok_round_row()
            row[key] = value
            report = self._report_for(row, expected)
            for name in self.REPORT_FACES:
                self.assertIn(name, report, f'#2519 {tag} lacks face {name}')
            self.assertNotEqual(report['executionStatus'], 'NOT_RUN', f'#2519 {tag}')
            self.assertEqual(report['hostAudit'][face][0]['status'], status,
                             f'#2519 {tag}: {report["hostAudit"][face]}')
            self.assertNotEqual(report['hostAudit']['status'], 'PASS', f'#2519 {tag}')
            classX = next(c for c in report['controls']['classes']
                          if c['class'] == control)
            self.assertEqual(classX['status'], 'NOT_APPLICABLE', f'#2519 {tag}: {classX}')

    def test_cleanup_audit_face_reads_the_shared_stopped_predicate(self):
        # #2519: the report face's cleanupAudit and the round face's CLEANUP
        # require both read `_cleanup_stopped`.  The False side had no pin
        # before (every synthetic round carried allStopped: True), so a
        # predicate that always read True would flip an unproven round's
        # cleanup face to PASS unseen.  The round face's require itself runs
        # inside a real retirement round (coordinator recalc); this pins the
        # shared predicate's report-face consumer offline.
        expected = ['submit_turn', 'run_get']
        report = self._report_for(self._stream_ok_round_row(), expected)
        self.assertEqual(report['cleanupAudit']['status'], 'PASS',
                         '#44 proven round cleanup must PASS the audit face')
        row = self._stream_ok_round_row()
        row['audit'] = {'allStopped': False}
        report = self._report_for(row, expected)
        self.assertEqual(report['cleanupAudit']['status'], 'FAIL',
                         '#44 unproven round cleanup must FAIL the audit face')
        self.assertEqual(report['processAudit'],
                         [row['audit']] * self.session.RETIREMENT_ROUNDS,
                         '#44 processAudit must carry the failing round audit')

    def test_round_face_cleanup_require_keeps_the_shared_predicate(self):
        # #2519 (p2batch-d7 boundary #2, coord2 a档; ds3 boundary#2 加固):
        # the round face's CLEANUP require is the only gate that rejects an
        # unproven round (the report face's cleanupAudit does not feed the
        # total verdict), and #44 pins only the report consumer.  The pins
        # bind to the executed bytes (`_apparatus_source`, the same bytes
        # #166 verified — a disk swap after load cannot forge a green), and
        # locate each call site among its owning function's own statements
        # (nested defs do not count — a dead inner helper is a different
        # scope) as an ast node whose first argument is the shared
        # `row['audit']` subscript, so commenting the require out,
        # reflowing lines, feeding the predicate a literal instead of the
        # round's audit, or parking the call in a dead helper (module-level
        # or nested) cannot keep a green run; the report face's argument is
        # pinned dynamically by #44 (allStopped: False must flip
        # cleanupAudit).  Counts pin the twins' call sites the same way
        # their dynamic pins pin their semantics: `_cleanup_stopped` is
        # called exactly twice (round-face require, report-face all()),
        # `_census_unobserved` exactly twice (verdict guard, controls rows
        # derivation), and each predicate keeps its single defining read.
        # (ast.parse precedent: test_safety.py:1940-1990; #2425 count-pin
        # precedent: :2670-2702.)
        import ast
        marker = '#2519 the round face reads the shared cleanup predicate'
        source = self.session._apparatus_source.decode()
        tree = ast.parse(source)
        functions = {n.name: n for n in tree.body
                     if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))}
        assert '_retirement_round' in functions and '_retirement_report' in functions

        def calls(node, name):
            return [c for c in ast.walk(node)
                    if isinstance(c, ast.Call) and isinstance(c.func, ast.Name) and c.func.id == name]

        def own_calls(func, name):
            # Only the function's own statements: do not descend into nested
            # defs/lambdas — a call parked in a dead inner helper belongs to
            # that inner scope and must not satisfy this pin.
            found, stack = [], list(ast.iter_child_nodes(func))
            while stack:
                node = stack.pop()
                if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
                    continue
                if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == name:
                    found.append(node)
                stack.extend(ast.iter_child_nodes(node))
            return found

        def audit_subscript(node):
            # `row['audit']` — the round's own audit, not a forged literal.
            return (isinstance(node, ast.Subscript) and isinstance(node.value, ast.Name)
                    and node.value.id == 'row' and isinstance(node.slice, ast.Constant)
                    and node.slice.value == 'audit')

        # Round face: in _retirement_round's own body, require(_cleanup_stopped(row['audit']), ...).
        round_face = any(
            isinstance(c.args[0], ast.Call) and isinstance(c.args[0].func, ast.Name)
            and c.args[0].func.id == '_cleanup_stopped' and c.args[0].args
            and audit_subscript(c.args[0].args[0])
            for c in own_calls(functions['_retirement_round'], 'require') if c.args)
        # Report face: in _retirement_report's own body, all(_cleanup_stopped(...) for ...).
        report_face = any(isinstance(arg, ast.GeneratorExp) and calls(arg, '_cleanup_stopped')
                          for c in own_calls(functions['_retirement_report'], 'all') for arg in c.args)
        self.assertTrue(
            round_face and report_face
            and len(calls(tree, '_cleanup_stopped')) == 2  # exactly the two call sites above
            and len(calls(tree, '_census_unobserved')) == 2  # verdict guard + controls rows
            and source.count("if _census_unobserved(census):") == 1
            and source.count("census_rows = [] if _census_unobserved(census) else census['rows']") == 1
            and source.count('return not isinstance(census, dict) '
                             'or not isinstance(census.get(\'rows\'), list)') == 1
            and source.count("return bool(audit['allStopped'])") == 1,
            marker)

    def test_control_attribution_requires_the_injected_object(self):
        # P1 (#1355 fix-r4d): a control PASS must be attributed to the object
        # the injection touched, not to a rule name.  Both directions are
        # exercised with a drifted scorer (a stand-in for the strip helpers
        # and the scorer disagreeing about what they report):
        #  - the rule reports the same problem for a *different* method: the
        #    injection was not caught, so the class is FAIL, never PASS;
        #  - the original round already reports the injected pair: the FAIL
        #    belongs to the round, so the class is NOT_APPLICABLE.
        expected = ['submit_turn', 'run_get']
        real = self.session.score_retirement_round

        def drifted_by_method(round_row, exp):
            verdict = real(round_row, exp)
            for problem in verdict['rules']:
                if problem['rule'] == 'EVIDENCE_BACKING':
                    problem['detail'] = [{'method': 'run_get', 'problem': 'send frame missing'}]
            return verdict

        row = self._stream_ok_round_row()
        with patch.object(self.session, 'score_retirement_round', side_effect=drifted_by_method):
            controls = self.session._retirement_controls(row, expected)
        classes = {c['class']: c['status'] for c in controls['classes']}
        for name in ('TS_FALLBACK', 'STREAM_ERROR_NO_FRAME', 'STREAM_OK_NON_FINAL'):
            self.assertEqual(classes[name], 'FAIL', f'#42a {name}: {controls}')

        def drifted_preexisting(round_row, exp):
            base = real(round_row, exp)
            rules = list(base['rules']) + [
                {'rule': 'EVIDENCE_BACKING',
                 'detail': [{'method': 'submit_turn', 'problem': 'send frame missing'}]}]
            return {'round': base['round'], 'status': 'FAIL', 'rules': rules,
                    'executed': base['executed'], 'methodVerdicts': base['methodVerdicts'],
                    'driverExitCode': base['driverExitCode']}

        with patch.object(self.session, 'score_retirement_round', side_effect=drifted_preexisting):
            controls = self.session._retirement_controls(row, expected)
        class1 = next(c for c in controls['classes'] if c['class'] == 'TS_FALLBACK')
        self.assertEqual(class1['status'], 'NOT_APPLICABLE', f'#42b: {class1}')
        self.assertIn('already reports', class1['reason'])

        #  - the census verdict stops naming the injected row: a FAIL whose
        #    reported object is not the gateway pid must not be credited to
        #    the declared-set injection.
        real_census = self.session._retirement_census_verdict

        def drifted_census(census):
            return {**real_census(census), 'undeclared': []}

        with patch.object(self.session, '_retirement_census_verdict', side_effect=drifted_census):
            controls = self.session._retirement_controls(self._stream_ok_round_row(), expected)
        class2 = next(c for c in controls['classes'] if c['class'] == 'TS_HOST')
        self.assertEqual(class2['status'], 'FAIL', f'#42c: {class2}')

        #  - the network verdict stops naming the injected peer (it reports
        #    some other undeclared address): a FAIL is not attribution to the
        #    object the injection added, so the class is FAIL, never PASS.
        real_network = self.session._retirement_network_verdict
        injected = '203.0.113.7:443'

        def drifted_network(observation):
            verdict = real_network(observation)
            if verdict.get('status') == 'FAIL':
                verdict['undeclared'] = [peer for peer in verdict['undeclared']
                                         if peer != injected] or ['198.51.100.9:443']
            return verdict

        with patch.object(self.session, '_retirement_network_verdict',
                          side_effect=drifted_network):
            controls = self.session._retirement_controls(self._stream_ok_round_row(), expected)
        class4 = next(c for c in controls['classes'] if c['class'] == 'FORWARDING_MISLABELED')
        self.assertEqual(class4['status'], 'FAIL', f'#42d: {class4}')

    def test_timeout_outcome_is_never_executed_and_fails_closed(self):
        # fix-r1 rule kept intact by the R1 edit: a timeout is missing for
        # COVERAGE and is itself an EVIDENCE_BACKING violation.
        outcome = {'method': 'submit_turn', 'outcome': 'timeout', 'shape': 'stream'}
        verdict = self.session.score_retirement_round(self._round_row([outcome], self._frames()), ['submit_turn'])
        rules = [p['rule'] for p in verdict['rules']]
        self.assertIn('EVIDENCE_BACKING', rules)
        self.assertIn('COVERAGE', rules)
        self.assertEqual(verdict['executed'], [])
        self.assertEqual(verdict['status'], 'FAIL')

    def test_host_census_fails_closed_on_empty_or_dead_samples(self):
        # R3: an unobservable census is not an empty host set.
        verdict = self.session._retirement_census_verdict({'rows': [], 'declared': [99999]})
        self.assertEqual(verdict['status'], 'FAIL')
        self.assertIn('absent from the census rows', verdict['problems'][0])
        verdict = self.session._retirement_census_verdict(
            {'rows': [{'pid': 99999, 'argvText': 'node pilotdeck.js server'}],
             'declared': [99999], 'declaredAlive': False})
        self.assertEqual(verdict['status'], 'FAIL')
        verdict = self.session._retirement_census_verdict(
            {'rows': [{'pid': 7, 'argvText': 'node pilotdeck.js server'}], 'declared': [7],
             'declaredAlive': True})
        self.assertEqual(verdict['status'], 'PASS')
        verdict = self.session._retirement_census_verdict(
            {'rows': [{'pid': 7, 'argvText': 'node pilotdeck.js server'}], 'declared': [8]})
        self.assertEqual(verdict['status'], 'FAIL')

    def test_network_verdict_fails_closed_on_empty_unparsed_or_failed_samples(self):
        # R3: an empty, unparsed or failed lsof sample cannot prove loopback.
        for connections, expected_status in (
                ({'rows': []}, 'BLOCKED'),
                ({'rows': [{'peer': None, 'raw': 'unparsed lsof line'}]}, 'BLOCKED'),
                ({'rows': [], 'commandStatus': 'failed', 'reason': 'lsof exited 1'}, 'BLOCKED'),
                ({'rows': [{'peer': '203.0.113.7:443', 'raw': 'x'}]}, 'FAIL'),
                ({'rows': [{'peer': '127.0.0.1:49153', 'raw': 'x'}]}, 'PASS')):
            verdict = self.session._retirement_network_verdict(connections)
            self.assertEqual(verdict['status'], expected_status, connections)
            self.assertEqual(verdict['rule'], 'NETWORK_FALLBACK')

    def test_lsof_failure_is_not_an_empty_connection_set(self):
        dead = subprocess.Popen(['true'])
        dead.wait()
        connections = self.session._retirement_connections(dead.pid)
        self.assertEqual(connections['commandStatus'], 'failed')
        self.assertNotEqual(self.session._retirement_network_verdict(connections)['status'], 'PASS')

    def test_listener_exposure_is_judged_not_only_recorded(self):
        # R9: a non-loopback or inconsistent listener binding is a FAIL that
        # participates in the host audit aggregation.
        verdict = self.session._retirement_listener_verdict(
            {'pid': 7, 'addresses': ['127.0.0.1:28762'], 'loopbackOnly': True}, 28762)
        self.assertEqual(verdict['status'], 'PASS')
        for listeners in ({'pid': 7, 'addresses': ['*:28762'], 'loopbackOnly': False},
                          {'pid': 7, 'addresses': [], 'loopbackOnly': False},
                          {'pid': 7, 'addresses': ['127.0.0.1:28762'], 'loopbackOnly': False},
                          {'pid': 7, 'addresses': ['127.0.0.1:28799'], 'loopbackOnly': True}):
            verdict = self.session._retirement_listener_verdict(listeners, 28762)
            self.assertEqual(verdict['status'], 'FAIL', listeners)
            self.assertEqual(verdict['rule'], 'LISTENER_EXPOSURE')

    def _fake_baseline(self, root, break_route=False, drop_listener=False):
        """Synthetic baseline whose parse reproduces the real 13/4 entry set."""
        gateway = root / 'src/gateway/server/GatewayServer.ts'
        gateway.parent.mkdir(parents=True, exist_ok=True)
        health = '/healthy' if break_route else '/health'
        gateway.write_text('\n'.join([
            f'if (url.pathname === "{health}") {{}}',
            'if (url.pathname === "/auth/local-token") {}',
            'if (url.pathname === "/feishu/webhook" && options.feishuWebhook) {}',
            'if (url.pathname.startsWith("/api/web/")) {}',
            'if (url.pathname === "/ws") {}']) + '\n')
        router = root / 'src/adapters/web/httpRouter.ts'
        router.parent.mkdir(parents=True, exist_ok=True)
        router.write_text('\n'.join([
            'if (url.pathname === "/api/web/projects" && request.method === "GET") {}',
            'if (subPath === "/files/tree") {}',
            'if (subPath === "/files/read") {}',
            'if (subPath === "/files/write") {}']) + '\n')
        host = root / 'src/composition/publicHostHttpTransport.ts'
        host.parent.mkdir(parents=True, exist_ok=True)
        host.write_text("if (path !== '/api/module-host/describe' && "
                        "path !== '/api/module-host/call') {}\n")
        approval = root / 'src/composition/publicApprovalHttpTransport.ts'
        approval.write_text("if (path !== '/api/module-host/approvals/status' && "
                            "path !== '/api/module-host/approvals/resume') {}\n")
        channel = root / 'src/adapters/channel'
        names = ['api-server/ApiServerChannel.ts', 'sms/SmsChannel.ts', 'webhook/WebhookChannel.ts']
        if not drop_listener:
            names.append('wecom-callback/WeComCallbackChannel.ts')
        for name in names:
            path = channel / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('const server = createServer(handler);\n')
        return root

    def test_http_route_parse_has_a_completeness_floor(self):
        # R6: a parse that stops finding a registered route is BLOCKED, not a
        # silently shrunken denominator that still passes.
        with tempfile.TemporaryDirectory() as tmp:
            baseline = self._fake_baseline(Path(tmp))
            routes = self.session._retirement_http_routes(baseline)
            self.assertEqual(sorted((r['route'], r['kind']) for r in routes),
                            sorted(self.session.RETIREMENT_REQUIRED_ROUTES))
        with tempfile.TemporaryDirectory() as tmp:
            baseline = self._fake_baseline(Path(tmp), break_route=True)
            with self.assertRaises(self.session.EvidenceError) as raised:
                self.session._retirement_http_routes(baseline)
            self.assertEqual(raised.exception.rule, 'ENTRY_SOURCE')

    def test_channel_listeners_keep_subdir_provenance_and_a_floor(self):
        # A5: provenance is the baseline-relative path, not path.name (which
        # dropped the adapter subdirectory); R6: the four registered
        # listeners are a floor.
        with tempfile.TemporaryDirectory() as tmp:
            baseline = self._fake_baseline(Path(tmp))
            entries = self.session._retirement_channel_listeners(baseline)
            self.assertEqual(sorted(e['file'].rsplit(':', 1)[0] for e in entries),
                            sorted(self.session.RETIREMENT_REQUIRED_LISTENERS))
        with tempfile.TemporaryDirectory() as tmp:
            baseline = self._fake_baseline(Path(tmp), drop_listener=True)
            with self.assertRaises(self.session.EvidenceError) as raised:
                self.session._retirement_channel_listeners(baseline)
            self.assertEqual(raised.exception.rule, 'ENTRY_SOURCE')

    def test_entry_inventory_gates_credential_gated_entries_and_counts_only_answered_methods(self):
        # R7: a credential-gated entry is GATED (visible, face stays
        # non-PASS) when the gate evidence holds, and NO_HIT when it does not;
        # A2: a never-answered (timeout) method does not satisfy an entry.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = self._fake_baseline(root)
            home = root / 'home'
            home.mkdir()
            (home / 'pilotdeck.yaml').write_text('schemaVersion: 1\n')
            row = {'label': 'round-test', 'http': [{'path': '/health', 'status': 200},
                                                   {'path': '/feishu/webhook', 'status': 404}],
                   'frames': self._frames(),
                   'outcomes': [{'method': 'cron_tick', 'outcome': 'timeout', 'shape': 'request'}],
                   'runtime': {'pilotHome': str(home)}}
            table = dict(self.session.RETIREMENT_GATED_ENTRIES)
            try:
                self.session.RETIREMENT_GATED_ENTRIES = {
                    '/feishu/webhook': {**table['/feishu/webhook'],
                                        'source': 'src/gateway/server/GatewayServer.ts:3'}}
                gated = self.session._retirement_entry_inventory(baseline, row)
            finally:
                self.session.RETIREMENT_GATED_ENTRIES = table
            self.assertEqual(gated['gated'], ['/feishu/webhook'])
            self.assertNotIn('/feishu/webhook', gated['unhit'])
            self.assertEqual(gated['status'], 'BLOCKED')
            self.assertIn('cron', gated['unhit'])  # timeout is not executed (A2)
            # The round config grows the credential key: the gate no longer
            # holds, so the 404 is an ordinary NO_HIT.
            (home / 'pilotdeck.yaml').write_text('feishu:\n  appId: x\n')
            ungated = self.session._retirement_entry_inventory(baseline, row)
            self.assertEqual(ungated['gated'], [])
            self.assertIn('/feishu/webhook', ungated['unhit'])
            self.assertEqual(ungated['status'], 'BLOCKED')


class RetirementLiveFaces(unittest.TestCase):
    """Behavior controls for the new, independently failing evidence faces."""

    def setUp(self):
        self.d = _load('review_live_session_domain', common.SCRIPT / 'session_domain.py')
        self.native = _load('review_live_retirement_native', common.SCRIPT / 'retirement_native.py')
        self.entries = _load('review_live_retirement_entries', common.SCRIPT / 'retirement_entries.py')
        self.cold = _load('review_live_retirement_cold', common.SCRIPT / 'retirement_cold.py')

    def test_source_inventory_cannot_hide_tracked_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            def git(*args):
                return subprocess.check_output(['git', '-C', tmp, *args], stderr=subprocess.DEVNULL)
            git('init', '-q')
            item = root / 'input'; item.write_bytes(b'ordinary source\n')
            (root / 'link').symlink_to('input')
            git('add', '--', 'input', 'link')
            git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test',
                '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture')
            rows = [{'path': 'input', 'sha256': common.digest(item), 'bytes': item.stat().st_size}]
            source = {'head': git('rev-parse', 'HEAD').decode().strip(), 'files': rows}
            self.native.verify_build_tree(self.d, root, source)
            with self.assertRaisesRegex(self.d.EvidenceError, 'omitted or added'):
                self.native.verify_build_tree(self.d, root, {**source, 'files': []})
            with self.assertRaisesRegex(self.d.EvidenceError, 'tracked file bytes'):
                self.native.verify_build_tree(self.d, root, {**source, 'files': [{**rows[0], 'sha256': '0'*64}]})
            for setting, reset in [('--skip-worktree', '--no-skip-worktree'),
                                   ('--assume-unchanged', '--no-assume-unchanged')]:
                git('update-index', setting, 'input')
                item.write_bytes(b'changed source\n')
                self.assertFalse(git('status', '--porcelain', '--untracked-files=no'))
                with self.assertRaisesRegex(self.d.EvidenceError, 'hides or overrides'):
                    self.native.verify_build_tree(self.d, root, source)
                item.write_bytes(b'ordinary source\n')
                git('update-index', reset, 'input')

    def test_ts_scenario_failure_and_missing_bytes_are_not_covered(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            rows = {}
            for side in ('ts', 'native'):
                row = {'label':'round-01','folder':str(root/side),'scenarios':[]}
                for scene in self.native.SESSION_SCENARIOS:
                    folder = root/side/'scenarios'/scene;folder.mkdir(parents=True)
                    files = {}
                    for name in ('frames.jsonl','result.json'):
                        file = folder/name;file.write_text('{}\n');files[name]=common.digest(file)
                    row['scenarios'].append({'case':scene,'folder':str(folder),'files':files,'driverExitCode':0})
                rows[side] = row
            with patch.object(self.native,'raw_session_input',return_value={}), \
                    patch.object(self.native,'session_observations',return_value={'observed':True}):
                _,_,executed,problems=self.native.session_faces(self.d,[rows['ts']],[rows['native']])
                self.assertFalse(problems)
                self.assertEqual(executed['ts']['permission'],['round-01'])
                rows['ts']['scenarios'][0]['driverExitCode']=1
                (root/'ts/scenarios/reconnect/frames.jsonl').unlink()
                _,_,executed,problems=self.native.session_faces(self.d,[rows['ts']],[rows['native']])
                self.assertEqual(executed['ts']['permission'],[])
                self.assertEqual(executed['ts']['reconnect'],[])
                self.assertEqual(executed['native']['permission'],['round-01'])
                self.assertEqual({(p['side'],p['case'],p['rule']) for p in problems},
                                 {('ts','permission','SESSION_SCENARIO_EVIDENCE'),('ts','reconnect','SESSION_SCENARIO_EVIDENCE')})

    def test_every_entry_needs_a_source_even_if_the_union_is_complete(self):
        path=common.SCRIPT/'fixtures/retirement-entry-inventory.json'
        inventory=self.d.read(path)
        methods=[r['id'].partition(':')[2] for r in inventory['entries'] if r['id'].startswith('gateway.method:')]
        with patch.object(self.entries,'sha',side_effect=lambda p: inventory['sources'].get(str(p),'fixture')):
            rows,_=self.entries.expected_rows(self.d,Path('.'),methods)
            self.assertEqual(len(rows),150)
            row=next(r for r in inventory['entries'] if r['id'].startswith('gateway.method:'))
            row['sources']=[]
            with patch.object(self.d,'read',return_value=inventory), \
                 self.assertRaisesRegex(self.d.EvidenceError,'every entry requires'):
                self.entries.expected_rows(self.d,Path('.'),methods)

    def test_always_on_needs_final_reaped_bytes_not_a_live_prefix(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);folder=root/'home/always-on/projects/project';folder.mkdir(parents=True)
            hist=folder/'run-history.jsonl';events=folder/'events.jsonl'
            hist.write_text('{"result":"no_plan"}\n');events.write_text('{"event":"discovery_started"}\n')
            raw={'providerRequests':[{'path':'/v1/chat/completions','body':{'stream':True}}],
                 'background':{'snapshotAfterReap':True,'runHistory':hist.read_text(),'events':events.read_text(),
                               'sha256':{'runHistory':common.digest(hist),'events':common.digest(events)}}}
            self.assertTrue(self.entries.raw_background(self.d,root,'alwayson',raw))
            with events.open('a') as stream:stream.write('{"event":"no_plan"}\n')
            self.assertFalse(self.entries.raw_background(self.d,root,'alwayson',raw))
            raw['background']['events']=events.read_text();raw['background']['sha256']['events']=common.digest(events)
            self.assertTrue(self.entries.raw_background(self.d,root,'alwayson',raw))
            raw['background']['snapshotAfterReap']=False
            self.assertFalse(self.entries.raw_background(self.d,root,'alwayson',raw))

    def test_outcome_nonzero_and_unreadable_report_are_distinct(self):
        report = {'command':'check','status':'PASS','violations':[],'violationsCount':0}
        self.assertEqual(self.native.classify_outcome(0,report),('PASS',None))
        self.assertEqual(self.native.classify_outcome(0,{}),('FAIL','OUTCOME_EXECUTION_OR_UNREADABLE_REPORT'))
        self.assertEqual(self.native.classify_outcome(1,None),('FAIL','OUTCOME_RULE_VIOLATION'))
        self.assertEqual(self.native.classify_outcome(2,None),('BLOCKED','OUTCOME_INPUT_REJECTED'))
        for code,report in ((0,None),(-15,{}),(False,{}),(74,{})):
            self.assertEqual(self.native.classify_outcome(code,report),('FAIL','OUTCOME_EXECUTION_OR_UNREADABLE_REPORT'))

    def test_cold_cron_never_normalizes_a_changed_task(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);prior,after,project=(root/name for name in ('prior','after','project'))
            task='home/cron/projects/key/tasks.json'; marker='home/cron/store-migration-v1.json'
            for location in (prior,after):
                path=location/task;path.parent.mkdir(parents=True)
                path.with_name('.cwd').write_text(str(project)+'\n')
            before=b'{"tasks":[{"taskId":"same","message":"preserve"}]}'
            (prior/task).write_bytes(before);(after/task).write_bytes(before+b'\n')
            marker_body={'version':1,'completedAt':'2026-11-10T23:02:17.100Z'}
            (after/marker).write_text(json.dumps(marker_body,indent=2)+'\n')
            kwargs=([task,marker],{task:{}},{marker:{'jsonValue':marker_body}},prior,after,project,lambda p:p)
            self.assertEqual(self.cold.exact_cron_startup_delta(self.d,*kwargs)['trailingLfOnly'],[task])
            (after/task).write_bytes(before.replace(b'preserve',b'mutated')+b'\n')
            with self.assertRaisesRegex(self.d.EvidenceError,'more than a single trailing LF'):
                self.cold.exact_cron_startup_delta(self.d,*kwargs)


if __name__ == '__main__':
    unittest.main()
