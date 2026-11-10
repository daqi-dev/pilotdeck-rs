#!/usr/bin/env python3
# anchor-selfcheck: unsupported lease lifecycle tests using runtime hooks, no source mutations
"""#1221 lease lifecycle controls; scoring and checkout guards stay separate."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import ast
import importlib.util
import io
import os
from pathlib import Path
import signal
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
path = ROOT / 'scripts/gateway-parity/session_domain.py'
spec = importlib.util.spec_from_file_location('lease_session', path)
s = importlib.util.module_from_spec(spec)
exec(compile(path.read_bytes(), str(path), 'exec'), s.__dict__)
c = s.common


class LeaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='pdrs-1221-')
        self.addCleanup(temporary.cleanup)
        self.folder = Path(temporary.name)
        self.lock = self.folder / 'lock'

    def test_guarded_sources_include_common(self):
        marker = '#a5b parity mutation guards include common.py'
        path = Path(__file__).with_name('negative.py')
        try:
            spec = importlib.util.spec_from_file_location('lease_negative', path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            self.assertTrue(ROOT / 'scripts/gateway-parity/common.py' in module.guarded_sources, marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_guarded_sources_exclude_captured_fixture(self):
        marker = '#1662 guarded sources exclude capture.py whose disk bytes are golden-verified'
        path = Path(__file__).with_name('negative.py')
        try:
            spec = importlib.util.spec_from_file_location('lease_negative_exclusion', path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            self.assertTrue(module.capture not in module.guarded_sources, marker)
            capture_case = next(row for row in module.cases
                                if row[0] == 'capture-term-raises-twice')
            self.assertTrue(capture_case[1] == module.capture, marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_capture_case_mutates_memory_only(self):
        marker = '#1662 the capture case mutates memory only, never the disk fixture'
        path = Path(__file__).with_name('negative.py')
        fixture = ROOT / 'src/pilotdeck-cli/tests/fixtures/sdk-signals/capture.py'
        try:
            spec = importlib.util.spec_from_file_location('lease_capture_routing', path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            commands, writes = [], []
            real_write_text = Path.write_text

            def recording_write_text(self, data, *args, **keywords):
                if self.resolve() == fixture:
                    # Record, do not write: the fixture bytes stay pristine even
                    # when the routing under test is broken.
                    writes.append('write_text')
                    return None
                return real_write_text(self, data, *args, **keywords)

            def fake_run_owned(command, **_keywords):
                commands.append(list(command))
                return subprocess.CompletedProcess(command, 0, '', 'Ran 1 test in 0.001s\n\nOK\n')

            class fake_guard:
                def __init__(self, *_args, **_keywords):
                    pass

                def __enter__(self):
                    return {}

                def __exit__(self, *_exc):
                    return None

            previous_target = os.environ.get('CARGO_TARGET_DIR')
            if previous_target is None:
                os.environ['CARGO_TARGET_DIR'] = str(ROOT / '_build/cargo-target')
            try:
                with tempfile.TemporaryDirectory(prefix='pdrs-1662-lease-') as folder:
                    with patch.object(module, 'mutating_sources', fake_guard), \
                            patch.object(module, 'run_owned', fake_run_owned), \
                            patch.object(Path, 'write_text', recording_write_text), \
                            patch('sys.stdout', new_callable=io.StringIO), \
                            patch.object(sys, 'argv', [str(path), str(Path(folder) / 'out'),
                                                       '--case', 'capture-term-raises-twice']):
                        module.main()
            finally:
                if previous_target is None:
                    del os.environ['CARGO_TARGET_DIR']
            self.assertTrue(writes == [], marker)
            self.assertTrue(any(command[3:5] == ['--memory-run', 'capture-term-raises-twice']
                                for command in commands), marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_documented_release_definition(self):
        marker = '#1221 allStopped documents lease proof separately from scoring'
        try:
            text = Path(__file__).with_name('README.md').read_text()
            self.assertTrue('#1221 更正 #209 E1 的 `allStopped` 定义' in text, marker)
            self.assertTrue('`allStopped` 是租约释放证明，不是判分通过' in text, marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def test_expected_case_count(self):
        marker = '#a5b lease negative case inventory is complete'
        path = Path(__file__).with_name('negative.py')
        try:
            spec = importlib.util.spec_from_file_location('lease_inventory', path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            module.validate_case_count()
            for arguments in ([], ['--check-anchors'], ['--memory-run', 'unused'],
                              ['unused-output'], ['unused-output', '--memory', '--case', 'unused']):
                with self.subTest(arguments=arguments), \
                        patch.object(module, 'cases', module.cases[:-1]), \
                        patch.object(sys, 'argv', [str(path), *arguments]):
                    with self.assertRaises(SystemExit, msg=marker) as rejected:
                        module.main()
                    self.assertTrue(str(rejected.exception.code).startswith(marker), marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_cli_signal_handlers_registered(self):
        marker = '#a5b CLI signal handlers are installed by production code'
        path = ROOT / 'scripts/cli-parity/run_round.py'
        previous = {}

        def sentinel(signum, frame):
            self.fail(marker + ': sentinel must not be called')

        try:
            spec = importlib.util.spec_from_file_location('cli_handler_registration', path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            for signum in (signal.SIGTERM, signal.SIGALRM):
                previous[signum] = signal.signal(signum, sentinel)
            module.install_handlers()
            self.assertTrue(signal.getsignal(signal.SIGTERM) is module.interrupted, marker)
            self.assertTrue(signal.getsignal(signal.SIGALRM) is sentinel, marker)
            old_alarm = module.install_handlers(lease=True)
            self.assertTrue(old_alarm is sentinel, marker)
            self.assertTrue(signal.getsignal(signal.SIGALRM) is module.lease_expired, marker)
            self.assertTrue(signal.getsignal(signal.SIGTERM) is module.interrupted, marker)
        except (Exception, SystemExit, KeyboardInterrupt) as error:
            self.fail(marker + ': ' + repr(error))
        finally:
            for signum, handler in previous.items():
                signal.signal(signum, handler)

    def test_cli_cancel_unwind(self):
        marker = '#1194 CLI preserves first cancellation and accepts later signals'
        path = ROOT / 'scripts/cli-parity/run_round.py'
        spec = importlib.util.spec_from_file_location('cli_cancel_round', path)
        module = importlib.util.module_from_spec(spec)
        try:
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            for signum, handler in ((signal.SIGTERM, module.interrupted),
                                    (signal.SIGALRM, module.lease_expired)):
                with self.subTest(signal=signum):
                    previous = signal.signal(signum, handler)
                    try:
                        first = None
                        cleanup_tail = False
                        try:
                            try:
                                signal.raise_signal(signum)
                                self.fail(marker + ': initial signal ignored')
                            except KeyboardInterrupt as error:
                                first = error
                                raise
                            finally:
                                signal.raise_signal(signum)
                                cleanup_tail = True
                        except KeyboardInterrupt as caught:
                            self.assertTrue(caught is first and cleanup_tail, marker)
                            # A handled cancellation is still active until except exits.
                            signal.raise_signal(signum)
                        with self.assertRaises(KeyboardInterrupt, msg=marker) as later:
                            signal.raise_signal(signum)
                        self.assertTrue(later.exception is not first, marker)
                    finally:
                        signal.signal(signum, previous)
        except (Exception, KeyboardInterrupt) as error:
            self.fail(marker + ': ' + repr(error))

    def test_capture_cancel_unwind(self):
        marker = '#1194 capture rehearsal preserves first cancellation and accepts later signals'
        path = ROOT / 'src/pilotdeck-cli/tests/fixtures/sdk-signals/capture.py'
        try:
            tree = ast.parse(path.read_bytes().decode())
            functions = [node for node in tree.body
                         if isinstance(node, ast.FunctionDef) and node.name == 'interrupted']
            self.assertTrue(len(functions) == 1, marker)
            namespace = {'sys': sys}
            exec(compile(ast.Module(body=functions, type_ignores=[]), str(path), 'exec'), namespace)
            handler = namespace['interrupted']
            for signum in (signal.SIGTERM, signal.SIGINT):
                with self.subTest(signal=signum):
                    previous = signal.signal(signum, handler)
                    try:
                        first = None
                        cleanup_tail = False
                        try:
                            try:
                                signal.raise_signal(signum)
                                self.fail(marker + ': initial signal ignored')
                            except KeyboardInterrupt as error:
                                first = error
                                raise
                            finally:
                                signal.raise_signal(signum)
                                cleanup_tail = True
                        except KeyboardInterrupt as caught:
                            self.assertTrue(caught is first and cleanup_tail, marker)
                            # A handled cancellation is still active until except exits.
                            signal.raise_signal(signum)
                        with self.assertRaises(KeyboardInterrupt, msg=marker) as later:
                            signal.raise_signal(signum)
                        self.assertTrue(later.exception is not first, marker)
                    finally:
                        signal.signal(signum, previous)
        except (Exception, KeyboardInterrupt) as error:
            self.fail(marker + ': ' + repr(error))

    def test_memory_rehearsal_holds_tree_lock(self):
        marker = '#1398 lease memory rehearsal holds the source tree lock'
        path = Path(__file__).with_name('negative.py')
        try:
            spec = importlib.util.spec_from_file_location('lease_memory_lock', path)
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
                os.environ['CARGO_TARGET_DIR'] = str(ROOT / '_build/cargo-target')
            try:
                with tempfile.TemporaryDirectory(prefix='pdrs-1398-lease-') as folder:
                    with patch.object(module, 'tree_lock', recording_tree_lock), \
                            patch.object(module, 'run_owned', fake_run_owned), \
                            patch('sys.stdout', new_callable=io.StringIO), \
                            patch.object(sys, 'argv', [str(path), str(Path(folder) / 'out'), '--memory',
                                                       '--case', 'memory-rehearsal-without-lock']):
                        module.main()
            finally:
                if previous_target is None:
                    del os.environ['CARGO_TARGET_DIR']
            self.assertTrue(seen == [((module.root,), {'timeout': 900})]
                            and order[:1] == ['lock'] and 'child' in order, marker)
        except (Exception, SystemExit) as error:
            self.fail(marker + ': ' + repr(error))

    def test_blocked_fixture_releases_lease(self):
        marker = '#1221 BLOCKED empty fixture releases lease for successor'
        # The real child/cleanup/lease run on this host. Only the scoring platform
        # is selected deterministically: unsupported platforms remain BLOCKED.
        with patch.object(c, 'LIVE_GROUPS', set()), patch.object(s.sys, 'platform', 'freebsd'):
            try:
                with c.parity_lock(lock=self.lock):
                    audit = s.run_fixture(self.folder, self.folder,
                                          [sys.executable, '-I', '-c', 'pass'])
                    self.assertTrue(audit['processTreeCleanup'] == s.CLEANUP_BLOCKED, marker)
                    self.assertTrue(s.cleanup_status(audit) != 'PASS', marker)
                    self.assertTrue(audit['allStopped'] and not audit['ownedGroupsRemaining'], marker)
                self.assertFalse(self.lock.exists(), marker)
                with c.parity_lock(timeout=.1, lock=self.lock):
                    self.assertTrue(c.owner(self.lock)['pid'] == s.os.getpid(), marker)
            except Exception as error:
                self.fail(marker + ': ' + repr(error))

    def test_failed_and_cancelled_fixture_release(self):
        marker = '#1221 non-PASS cleanup releases only a settled empty group'
        for event in ('failure', 'cancel', 'deadline'):
            with self.subTest(event=event), patch.object(c, 'LIVE_GROUPS', set()), \
                    patch.object(s.sys, 'platform', 'freebsd'):
                output = self.folder / event
                output.mkdir()
                lock = output / 'lock'
                original_wait = s.wait_exit_unreaped
                first = True

                def wait(pid, timeout):
                    nonlocal first
                    if first and event != 'failure':
                        first = False
                        if event == 'cancel':
                            raise KeyboardInterrupt('#1221 cancellation')
                        raise TimeoutError('#1221 launcher deadline')
                    return original_wait(pid, timeout)

                caught = None
                try:
                    with c.parity_lock(lock=lock), patch.object(s, 'wait_exit_unreaped', wait):
                        s.run_fixture(output, output, [sys.executable, '-I', '-c',
                                      'raise SystemExit(7)' if event == 'failure'
                                      else 'import time; time.sleep(5)'])
                except (KeyboardInterrupt, TimeoutError) as error:
                    caught = error
                except Exception as error:
                    self.fail(marker + ': ' + repr(error))
                self.assertTrue((caught is None) == (event == 'failure'), marker)
                self.assertFalse(c.LIVE_GROUPS or lock.exists(), marker)
                audit = c.json.loads((output / 'launch.json').read_text())
                self.assertTrue(audit['allStopped'], marker)
                self.assertTrue(audit['processTreeCleanup'] == s.CLEANUP_BLOCKED, marker)

    def test_lease_probe_is_fail_closed(self):
        marker = '#1221 lease proof requires held reaped leader and final ESRCH'
        for event in ('absent', 'live', 'denied', 'error', 'unreaped', 'identity-lost',
                      'probe-not-reaped', 'cleanup-error', 'cleanup-raised', 'unregistered',
                      'wrong-pgid', 'truthy-unreaped', 'truthy-reaped'):
            with self.subTest(event=event):
                child = SimpleNamespace(pid=424242, returncode=None if event == 'unreaped' else 0)
                audit = {'processTreeCleanup': s.CLEANUP_BLOCKED,
                         'processGroupProbe': {'pgid': 1 if event == 'wrong-pgid' else child.pid,
                                               'leaderUnreaped': event != 'identity-lost',
                                               'leaderReaped': event != 'probe-not-reaped'}}
                if event == 'truthy-unreaped':
                    audit['processGroupProbe']['leaderUnreaped'] = 1
                if event == 'truthy-reaped':
                    audit['processGroupProbe']['leaderReaped'] = 1
                if event == 'cleanup-error':
                    audit['cleanupBlocked'] = 'denied'
                if event == 'cleanup-raised':
                    audit['cleanupRaised'] = 'interrupted'
                before = audit['processTreeCleanup']
                error = (None if event == 'live' else PermissionError('denied') if event == 'denied'
                         else OSError('probe failed') if event == 'error' else ProcessLookupError())
                with patch.object(c, 'LIVE_GROUPS', set() if event == 'unregistered' else {child.pid}), \
                        patch.object(s.os, 'killpg', side_effect=error) as probe, \
                        patch.object(s.time, 'monotonic', side_effect=(0, 3)):
                    stopped = s.fixture_lease_stopped(child, audit)
                self.assertTrue(stopped == (event == 'absent'), marker)
                self.assertTrue(audit['processTreeCleanup'] == before, marker)
                self.assertTrue(all(call.args == (child.pid, 0) for call in probe.call_args_list), marker)
                if event in ('unreaped', 'identity-lost', 'probe-not-reaped', 'cleanup-error',
                             'cleanup-raised', 'unregistered', 'wrong-pgid',
                             'truthy-unreaped', 'truthy-reaped'):
                    self.assertTrue(probe.call_count == 0, marker)

    def test_lease_probe_waits_boundedly(self):
        marker = '#1221 lease probe waits for ESRCH within bounded deadline'
        child = SimpleNamespace(pid=424242, returncode=0)
        audit = {'processTreeCleanup': s.CLEANUP_BLOCKED,
                 'processGroupProbe': {'pgid': child.pid, 'leaderUnreaped': True, 'leaderReaped': True}}
        with patch.object(c, 'LIVE_GROUPS', {child.pid}), \
                patch.object(s.os, 'killpg', side_effect=[None, ProcessLookupError()]) as probe, \
                patch.object(s.time, 'monotonic', side_effect=(0, 1)), \
                patch.object(s.time, 'sleep') as sleep:
            self.assertTrue(s.fixture_lease_stopped(child, audit), marker)
            self.assertTrue(probe.call_count == 2 and sleep.call_count == 1, marker)


if __name__ == '__main__':
    signal.signal(signal.SIGINT, signal.default_int_handler)
    unittest.main()
