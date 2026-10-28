#!/usr/bin/env python3
"""Bounded offline regressions for lock lifecycle, corrupt evidence and observer."""
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

# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# Load apparatus modules by explicit path, never sys.path: a same-named package
# or extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing common.py is what this block defends.  -B only
# stops cache writes, so redirect the bytecode prefix too (fix-r5).
import importlib.util, sys, tempfile
from pathlib import Path

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before loading anything and print the command.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {Path(__file__).resolve()}')

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        if not isinstance(getattr(existing, '_apparatus_source', None), bytes):
            raise RuntimeError(f'#166 unverified apparatus module cache: {path}')
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        source = path.read_bytes()
        exec(compile(source, str(path), 'exec'), module.__dict__)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    module._apparatus_source = source
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module



def _verify_cli_imports():
    # This trust root uses only stdlib + Git, never a helper from disk common:
    # that module's verifier can itself be the active negative-control mutant.
    import subprocess
    from types import ModuleType
    root = _apparatus.parent.parent
    head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', '--verify', 'HEAD'],
                                   timeout=30).decode().strip()
    source = subprocess.check_output(['git', '-C', str(root), 'show',
                                      f'{head}:scripts/gateway-parity/common.py'], timeout=30)
    trusted = ModuleType('pristine_cli_verifier')
    trusted.__file__ = str(_apparatus / 'common.py')
    exec(compile(source, trusted.__file__, 'exec'), trusted.__dict__)
    trusted.verify_loaded_apparatus(head)


_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

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
        containers = [({'projectKey': 'p', 'fullPath': 'p', 'name': 'n', 'lastActivity': 1}, 'createdAt'),
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
                     'check_frozen.py', 'compare.py', 'build_proxy.py'):
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
        command = 'import json, subprocess\n' + body + '\n' + ast.get_source_segment(source, blocks[0])
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
                     'check_ruler.py', 'controls/run_all.py', 'controls/session_domain.py'):
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
                     'check_ruler.py', 'controls/run_all.py', 'controls/session_domain.py'):
            for status, code in (('PASS', 0), ('FAIL', 1), ('BLOCKED', 2), ('NOT_RUN', 3)):
                with self.subTest(entry=name, status=status):
                    body = ("def _verify_cli_imports(): pass\n"
                            "def main():\n"
                            f"    print(json.dumps({{'status': {status!r}}}))\n    return {code}\n")
                    result = self.session_dispatch_process(body, name)
                    self.assertEqual(result.returncode, code, message + ': ' + result.stderr)
                    self.assertEqual(result.stdout.strip(), json.dumps({'status': status}), message + ': JSON')
                    self.assertEqual(result.stderr, '', message + ': no traceback')

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
                for error_type in (ValueError, OSError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt):
                    if stage == 'work' and error_type not in (ValueError, OSError):
                        continue
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
                        self.assertEqual((summary.get('status'), code), ('BLOCKED', 1 if stage == 'work' else 2), message)
                        self.assertEqual(summary.get('failure', {}).get('rule'),
                                         None if stage == 'work' else 'APPARATUS_IMPORTS', message + ': rule')
                        self.assertEqual(errors.getvalue(), '', message + ': no traceback')
                        self.assertFalse(held, message + ': lease released')

    def test_ruler_summary_uses_shared_status_without_changing_pending_review(self):
        from contextlib import ExitStack, nullcontext, redirect_stdout
        from unittest.mock import call
        import io
        message = '#166 fix-r9b ruler summary must share aggregation and preserve PENDING_REVIEW/2 or FAIL/1'
        entry = _load('ruler_summary_entry', common.SCRIPT / 'check_ruler.py')
        gates = ('self_compare', 'positive_controls', 'prerequisite_controls',
                 'same_bad_rejected', 'proxy_parity')
        for failed_gate in (None, *gates):
            with self.subTest(failed_gate=failed_gate), ExitStack() as stack:
                path = self.folder / (failed_gate or 'all-green')
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
                shared = stack.enter_context(patch.object(entry.common, 'aggregate_status',
                                                          wraps=common.aggregate_status))
                stack.enter_context(patch.object(sys, 'argv', ['check_ruler', '--output', str(path)]))
                output = stack.enter_context(redirect_stdout(io.StringIO()))
                try:
                    code = entry.main()
                    report = json.loads(output.getvalue())
                    saved = json.loads((path / 'ruler.json').read_text())
                except (Exception, KeyboardInterrupt) as error:
                    self.fail('#166 fix-r9b main/report execution error; not a contract kill: ' + repr(error))
                expected = ('PENDING_REVIEW', 2) if failed_gate is None else ('FAIL', 1)
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

    def test_deadline_not_run_has_dedicated_exit_code(self):
        for children in ((), (None,), (None, None), ('PASS', None)):
            self.assertEqual(common.aggregate_status(*children), ('PASS', 0),
                             '#166 empty optional aggregation stays PASS/0')
        # State/exit mutations run under the negative runner's parity lease.
        # Exercise the actual main body in-process; CLI isolation is tested by
        # ImportIsolation, so a HEAD refusal cannot masquerade as a status kill.
        from contextlib import nullcontext, redirect_stdout
        import io
        entry = _load('session_domain', common.SCRIPT / 'session_domain.py')
        with tempfile.TemporaryDirectory(prefix='pdrs-166-deadline-') as folder:
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
        # Every A2 entry that touches common must carry the same inline block.
        # It cannot move into common.py (importing common is what it defends),
        # so the machine check is what stops a future entry from reverting to
        # ``from common import`` and being shadowed before the scan (fix-r6 P1).
        folder = Path(__file__).resolve().parent
        blocks = {}
        for name in common.A2_SCRIPTS + common.A3_SCRIPTS:
            if name == 'common.py' or not name.endswith('.py'):
                continue
            text = (folder / name).read_text(encoding='utf-8')
            if 'common' not in text and '_load(' not in text:
                continue
            start = text.find(BOOTSTRAP_START)
            self.assertNotEqual(start, -1, f'#701 {name}: missing apparatus bootstrap')
            end = text.find(BOOTSTRAP_END, start)
            self.assertNotEqual(end, -1, f'{name}: unterminated apparatus bootstrap')
            blocks[name] = text[start:end + len(BOOTSTRAP_END)]
            prefix = text[:start]
            self.assertNotRegex(prefix, r'sys[.]path[.]insert', name)
            self.assertNotRegex(prefix, APPARATUS_IMPORT, name)
        self.assertEqual(sorted(blocks), sorted([
            'build_proxy.py', 'check_frozen.py', 'check_ruler.py', 'compare.py',
            'run_round.py', 'session_domain.py', 'test_safety.py',
            'controls/run_all.py', 'controls/prerequisites.py',
            'controls/session_domain.py']))
        self.assertEqual(len(set(blocks.values())), 1,
                         'apparatus bootstrap differs between entries')
        block = next(iter(blocks.values()))
        self.assertIn('sys.flags.isolated', block)
        self.assertLess(block.index('sys.flags.isolated'),
                        block.index('sys.pycache_prefix'),
                        'isolated-mode guard must precede module loading')

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
        try:
            module = _load('probe_controls_prerequisites',
                           tree / 'controls/prerequisites.py')
            self.assertEqual(Path(module.common.__file__).resolve(),
                             (tree / 'common.py').resolve())
            self.assertFalse(marker.exists())
        finally:
            if saved is None:
                sys.modules.pop('common', None)
            else:
                sys.modules['common'] = saved
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


if __name__ == '__main__':
    unittest.main()
