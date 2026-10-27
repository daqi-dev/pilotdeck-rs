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


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


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
            self.assertIs(kwargs['preexec_fn'], common.unblock_child_signals, '#209 child mask restored')
            os.kill(os.getpid(), common.signal.SIGINT)
            return child

        with patch.object(common, 'LIVE_GROUPS', set()), patch.object(runner.subprocess, 'Popen', popen):
            with self.assertRaises(KeyboardInterrupt, msg='#209 pending interrupt must be delivered'):
                runner.start_owned(entries, log, {}, ['fixture'])
            self.assertEqual(common.LIVE_GROUPS, {child.pid}, '#209 register before signal delivery')
            self.assertEqual([row[0] for row in entries], [child], '#209 cleanup list published before signal')
        with patch.object(common.signal, 'pthread_sigmask') as mask:
            common.unblock_child_signals()
            mask.assert_called_once_with(common.signal.SIG_UNBLOCK, common.DEFERRED_SIGNALS)

    def test_spawn_failure_closes_log_without_registering(self):
        runner = _load('run_round', common.SCRIPT / 'run_round.py')
        log, entries = unittest.mock.Mock(), []
        with patch.object(common, 'LIVE_GROUPS', set()), \
                patch.object(runner.subprocess, 'Popen', side_effect=OSError('#209 spawn failed')):
            with self.assertRaisesRegex(OSError, '#209 spawn failed'):
                runner.start_owned(entries, log, {}, ['fixture'])
            self.assertFalse(common.LIVE_GROUPS, '#209 failed spawn has no owned group')
            self.assertFalse(entries, '#209 failed spawn has no process entry')
        log.close.assert_called_once()

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
            log.close.assert_called_once()

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


class ApparatusInputs(unittest.TestCase):
    """#52 / #85: unregistered files, installed deps and cargo configs are inputs."""

    def test_deadline_not_run_has_dedicated_exit_code(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-166-deadline-') as folder:
            output = Path(folder) / 'deadline'
            proc = subprocess.run([sys.executable, '-I', str(common.SCRIPT / 'session_domain.py'),
                                   '--suite', 'deadline', '--output', str(output)],
                                  text=True, capture_output=True, timeout=30)
            self.assertEqual(proc.returncode, 3, '#166 NOT_RUN must exit 3: ' + proc.stdout + proc.stderr)
            report = json.loads((output / 'report.json').read_text())
            for key in ('status', 'executionStatus', 'acceptanceStatus'):
                self.assertEqual(report[key], 'NOT_RUN', f'#166 {key} must disclose no execution')
            self.assertEqual(report['structureStatus'], 'PASS', '#166 structure validation remains separate')
            self.assertEqual(len(report['cells']), 4, '#166 all four unexecuted cells remain')
            for cell in report['cells']:
                self.assertEqual(cell['executedCases'], [], '#166 no execution is fabricated')
            missing = subprocess.run([sys.executable, '-I', str(common.SCRIPT / 'session_domain.py'),
                                      '--suite', 'calibrate', '--baseline', str(Path(folder) / 'missing'),
                                      '--output', str(Path(folder) / 'blocked')],
                                     text=True, capture_output=True, timeout=30)
            self.assertEqual(missing.returncode, 2, '#166 BLOCKED remains distinct from NOT_RUN')
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
        for name in common.A2_SCRIPTS:
            if name == 'common.py' or not name.endswith('.py'):
                continue
            text = (folder / name).read_text(encoding='utf-8')
            if 'common' not in text and '_load(' not in text:
                continue
            start = text.find(BOOTSTRAP_START)
            self.assertNotEqual(start, -1, f'{name}: missing apparatus bootstrap')
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
        for name in common.A2_SCRIPTS:
            if not name.endswith('.py') or name == 'common.py':
                continue
            text = (folder / name).read_text(encoding='utf-8')
            self.assertNotRegex(text, r'sys[.]path[.]insert', name)
            self.assertNotRegex(text, APPARATUS_IMPORT, name)

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
