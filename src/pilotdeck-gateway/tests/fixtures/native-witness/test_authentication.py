#!/usr/bin/env python3
# anchor-selfcheck: unsupported authentication unit tests, no source mutation table
"""#1140 r8 evidence authentication controls; synthetic bytes and owned stubs only.

The cleanup matrix exercises the real temporary lease and capture handlers.
No gateway, provider socket, strace process, or original machine binary runs.
"""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import copy
from contextlib import ExitStack
import importlib.util
import json
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[5]


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


w = load_module('authentication_witness', ROOT / 'scripts/native-witness/witness.py')
c = load_module('authentication_collector', ROOT / 'scripts/native-witness/collect.py')
compare = load_module('authentication_compare', ROOT / 'scripts/gateway-parity/compare.py')
native = w.native
TRACER = 'native-witness-tracer:123'
PROVIDER = 'native-witness-provider'


def hx(value):
    return '"' + ''.join(f'\\x{byte:02x}' for byte in str(value).encode()) + '"'


def exec_line(tid, path):
    return f'{tid} execve({hx(path)}, [{hx(path)}], 0x0) = 0\n'


def wire_row(direction='receive', *, opcode=1, fin=True, rsv=0, masked=False):
    value = {'type': 'hello_ok', 'protocolVersion': '1.2',
             'interactionBinding': {'connectionId': 'authentication', 'generation': 1}}
    payload = json.dumps(value, separators=(',', ':')).encode()
    size = len(payload)
    code = size if size < 126 else 126
    head = bytes([(128 if fin else 0) | rsv | opcode, (128 if masked else 0) | code])
    if code == 126:
        head += size.to_bytes(2, 'big')
    if masked:
        mask = b'abcd'
        wire = head + mask + bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    else:
        wire = head + payload
    frame = value if direction == 'receive' and opcode == 1 else payload.decode()
    return {'connection': 1, 'direction': direction, 'frame': frame, 'wireHex': wire.hex(),
            'wire': {'opcode': opcode, 'fin': fin, 'rsv': rsv, 'masked': masked,
                     'lengthCode': code, 'payloadLength': size}}


class AuthenticationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='pdrs-1140-authentication-')
        self.addCleanup(self.temporary.cleanup)
        self.folder = Path(self.temporary.name).resolve()
        self.host, self.sidecar = self.folder / 'host', self.folder / 'sidecar'
        self.host.write_bytes(b'\x7fELFsynthetic-host')
        self.sidecar.write_bytes(b'\x7fELFsynthetic-sidecar')
        self.contract = {'launcherPid': 99, 'project': str(self.folder), 'binaries': {
            role: {'path': str(path), 'argv': [str(path)], 'sha256': native.sha(path)}
            for role, path in (('host', self.host), ('sidecar', self.sidecar))}}
        self.prefix = exec_line(100, self.host) + '100 clone(child_stack=NULL, flags=CLONE_THREAD) = 101\n'
        self.suffix = '101 +++ exited with 0 +++\n100 +++ exited with 0 +++\n'
        self.raw = self.prefix + self.suffix

    def assert_trace_rejected(self, raw, label):
        rejected = False
        try:
            native.parse_trace(raw, self.contract)
        except native.Incomplete:
            rejected = True
        except BaseException as error:
            self.fail('#1140 r8 unexpected trace exception: ' + repr(error))
        self.assertTrue(rejected, '#1140 r8 ' + label)

    def test_unknown_task_failed_exec(self):
        raw = self.prefix + '999 execve(' + hx('/missing') + ', [], 0x0) = -1 ENOENT (Missing)\n' + self.suffix
        self.assert_trace_rejected(raw, 'unknown task failed exec rejected')

    def test_ended_task_failed_exec(self):
        raw = self.prefix + '101 +++ exited with 0 +++\n101 execve(' + hx('/missing')
        raw += ', [], 0x0) = -1 ENOENT (Missing)\n100 +++ exited with 0 +++\n'
        self.assert_trace_rejected(raw, 'ended task failed exec rejected')

    def test_tracked_failed_exec_is_recorded(self):
        raw = self.prefix + '101 execve(' + hx('/missing') + ', [], 0x0) = -1 ENOENT (Missing)\n' + self.suffix
        result, error = None, None
        try:
            result = native.parse_trace(raw, self.contract)
        except BaseException as caught:
            error = caught
        self.assertTrue(error is None, '#1140 r8 tracked failed exec remains admissible: ' + repr(error))
        self.assertTrue(len(result['execWitness']['attempts']) == 2, '#1140 r8 tracked failed exec attempt retained')

    def test_pid_reuse_rejected(self):
        # The old ended set would otherwise incorrectly certify the new task.
        raw = self.prefix + '101 +++ exited with 0 +++\n'
        raw += '100 clone(child_stack=NULL, flags=CLONE_THREAD) = 101\n100 +++ exited with 0 +++\n'
        self.assert_trace_rejected(raw, 'PID reuse rejected')

    def test_nonleader_exec_rejected(self):
        self.assert_trace_rejected(self.prefix + exec_line(101, self.sidecar) + self.suffix,
                                   'nonleader successful exec rejected')

    def test_successful_execveat_rejected(self):
        raw = self.prefix + '100 execveat(AT_FDCWD, ' + hx(self.host) + ', [' + hx(self.host)
        raw += '], 0x0, 0) = 0\n' + self.suffix
        self.assert_trace_rejected(raw, 'successful execveat rejected')

    def assert_profile_rejected(self, row, label):
        # Ordinary wire integrity must pass first: bad profile is self-consistent.
        integrity_error = None
        try:
            compare.wire_frame(row)
        except BaseException as error:
            integrity_error = error
        self.assertTrue(integrity_error is None, '#1140 r8 ' + label + ' fixture is wire-consistent: ' + repr(integrity_error))
        rejected = False
        try:
            native.ws_profile(row['direction'], row['wire'])
        except native.Incomplete:
            rejected = True
        self.assertTrue(rejected, '#1140 r8 ' + label + ' profile rejected')

    def test_scm_rights_rejected(self):
        call = ('100 sendmsg(3, {msg_name=NULL, msg_namelen=0, msg_iov=[], msg_iovlen=0, '
                'msg_control=[{cmsg_len=20, cmsg_level=SOL_SOCKET, cmsg_type=SCM_RIGHTS, cmsg_data=[4]}], '
                'msg_controllen=24, msg_flags=0}, 0) = 1\n')
        self.assert_trace_rejected(self.prefix + call + self.suffix, 'SCM_RIGHTS egress rejected')

    def test_ws_fragment_rejected(self):
        self.assert_profile_rejected(wire_row(fin=False), 'fragmented hello')

    def test_ws_rsv_rejected(self):
        self.assert_profile_rejected(wire_row(rsv=64), 'RSV hello')

    def test_ws_binary_rejected(self):
        self.assert_profile_rejected(wire_row(opcode=2), 'binary frame')

    def test_ws_server_mask_rejected(self):
        self.assert_profile_rejected(wire_row(masked=True), 'masked server hello')

    def test_ws_client_unmasked_rejected(self):
        self.assert_profile_rejected(wire_row(direction='send'), 'unmasked client frame')

    def test_ws_unknown_direction_rejected(self):
        self.assert_profile_rejected(wire_row(direction='sideways'), 'unknown direction')

    def test_ws_complete_profile_accepted(self):
        for row in (wire_row(), wire_row(direction='send', masked=True)):
            with self.subTest(direction=row['direction']):
                error = None
                try:
                    compare.wire_frame(row)
                    native.ws_profile(row['direction'], row['wire'])
                except BaseException as caught:
                    error = caught
                self.assertTrue(error is None, '#1140 r8 complete controlled WS frame accepted: ' + repr(error))

    def test_wire_client_uses_controlled_profile(self):
        for label, row in (('FIN', wire_row(fin=False)), ('RSV', wire_row(rsv=64)),
                           ('mask', wire_row(masked=True)), ('opcode', wire_row(opcode=2))):
            with self.subTest(profile=label):
                client = object.__new__(c.WireClient)
                client.rows = []
                raw = bytearray.fromhex(row['wireHex'])
                def read(count):
                    value = bytes(raw[:count])
                    del raw[:count]
                    return value
                client.read = read
                rejected = False
                try:
                    client.receive()
                except native.Incomplete:
                    rejected = True
                except BaseException as error:
                    self.fail('#1140 r8 client rejects controlled profile with diagnostic: ' + repr(error))
                self.assertTrue(rejected, '#1140 r8 client uses controlled WS profile ' + label)
                self.assertTrue(not client.rows, '#1140 r8 rejected client frame is not recorded')

    def capture_failure(self, *, wait_error=None, kill_error=None, provider='absent', publish_error=False,
                        pending_cancel=False):
        """No waits execute: every Popen method is a deterministic in-memory stub."""
        output = self.folder / ('capture-' + str(len(list(self.folder.glob('capture-*')))))
        lock = output.with_name(output.name + '-lease')
        ownership = {PROVIDER} if provider != 'absent' else set()
        original = RuntimeError('business failed before tracer completion')
        tracer = mock.Mock(pid=123)
        tracer.kill.side_effect = kill_error
        tracer.wait.side_effect = wait_error
        tracer.wait.return_value = -9
        observed, registered, failure = None, [], None
        real_save = w.save
        def save(path, value):
            if Path(path).name == 'failure.json' and publish_error:
                raise OSError('deterministic failure publication')
            real_save(path, value)
        def spawn(*_args, **_kwargs):
            if pending_cancel:
                signal.getsignal(signal.SIGTERM)(signal.SIGTERM, None)
            return tracer
        def exercise(*_args):
            registered.append(TRACER in ownership)
            raise original
        with ExitStack() as stack:
            for patch in (mock.patch.object(sys, 'platform', 'linux'),
                          mock.patch.object(w.shutil, 'which', return_value='/stub-strace'),
                          mock.patch.object(w.subprocess, 'run', return_value=mock.Mock(stdout='--kill-on-exit')),
                          mock.patch.object(w.subprocess, 'Popen', side_effect=spawn),
                          mock.patch.object(w, 'save', side_effect=save),
                          mock.patch.object(c.common, 'LIVE_GROUPS', ownership)):
                stack.enter_context(patch)
            with c.common.parity_lock(lock=lock, timeout=1):
                try:
                    w.capture(self.contract, output, {}, exercise=exercise, ownership=ownership)
                except BaseException as error:
                    observed = error
                if provider == 'stopped':
                    ownership.discard(PROVIDER)
        failure_path = output / 'failure.json'
        if failure_path.is_file():
            failure = json.loads(failure_path.read_text())
        lease = json.loads((lock / 'owner.json').read_text()) if lock.is_dir() else None
        return {'original': original, 'error': observed, 'registered': registered, 'failure': failure,
                'lease': lease, 'ownership': ownership, 'tracer': tracer,
                'receipt': (output / 'receipt.json').exists()}

    def test_tracer_registered_before_exercise(self):
        result = self.capture_failure()
        self.assertTrue(result['registered'] == [True], '#1140 r8 tracer registered before exercise')
        self.assertTrue(result['error'] is result['original'], '#1140 r8 capture preserves original business error')

    def test_kill_error_still_waits(self):
        result = self.capture_failure(kill_error=OSError('kill denied'))
        self.assertTrue(result['tracer'].wait.call_count == 1, '#1140 r8 kill failure still waits for tracer')
        self.assertTrue(result['ownership'] == set() and result['lease'] is None,
                        '#1140 r8 successful wait clears tracer despite failed kill')
        self.assertTrue(result['error'] is result['original'], '#1140 r8 kill failure preserves original error')
        self.assertTrue(result['failure'] is not None and any('kill:' in item for item in result['failure']['cleanupErrors']),
                        '#1140 r8 kill failure retained in failure record')
        self.assertFalse(result['receipt'], '#1140 r8 failed capture never publishes receipt')

    def test_unconfirmed_tracer_retains_lease_matrix(self):
        for name, factory in (('timeout', lambda: subprocess.TimeoutExpired('stub-strace', 5)),
                              ('exception', lambda: OSError('wait failed'))):
            for provider in ('absent', 'stopped', 'pending'):
                for kill_error in (None, OSError('kill failed')):
                    with self.subTest(wait=name, provider=provider, kill=kill_error is not None):
                        result = self.capture_failure(wait_error=factory(), kill_error=kill_error, provider=provider)
                        expected = {TRACER} | ({PROVIDER} if provider == 'pending' else set())
                        self.assertTrue(result['ownership'] == expected, '#1140 r8 unconfirmed tracer retains ownership')
                        self.assertTrue(result['lease'] is not None and result['lease'].get('cleanupPending') == sorted(expected),
                                        '#1140 r8 unconfirmed tracer retains lease')
                        self.assertTrue(result['error'] is result['original'], '#1140 r8 wait failure preserves original error')
                        self.assertTrue(result['failure'] is not None and TRACER in result['failure']['cleanupPending'],
                                        '#1140 r8 unconfirmed tracer publishes cleanupPending')
                        self.assertTrue(any('wait:' in item for item in result['failure']['cleanupErrors']),
                                        '#1140 r8 wait failure retained in failure record')
                        self.assertFalse(result['receipt'], '#1140 r8 unconfirmed tracer publishes no receipt')

    def test_confirmed_tracer_provider_ownership_matrix(self):
        for provider in ('absent', 'stopped', 'pending'):
            with self.subTest(provider=provider):
                result = self.capture_failure(provider=provider)
                expected = {PROVIDER} if provider == 'pending' else set()
                self.assertTrue(result['ownership'] == expected, '#1140 r8 confirmed tracer removes only its ownership')
                self.assertTrue((result['lease'] is not None) == bool(expected), '#1140 r8 confirmed cleanup releases only empty lease')
                self.assertTrue(result['failure'] is not None and TRACER not in result['failure']['cleanupPending'],
                                '#1140 r8 confirmed tracer absent from cleanupPending')
                self.assertFalse(result['receipt'], '#1140 r8 failed exercise publishes no receipt')

    def test_failure_publication_preserves_original_error(self):
        result = self.capture_failure(wait_error=OSError('wait failed'), publish_error=True)
        self.assertTrue(result['error'] is result['original'], '#1140 r8 failure publication preserves original error')
        notes = getattr(result['error'], '__notes__', [])
        self.assertTrue(any('failure publication' in note for note in notes), '#1140 r8 failure publication error remains inspectable')
        self.assertTrue(result['ownership'] == {TRACER} and result['lease'] is not None,
                        '#1140 r8 failed failure publication retains unconfirmed ownership')
        self.assertFalse(result['receipt'], '#1140 r8 publication failure never publishes receipt')

    def test_pending_spawn_cancellation_retains_unconfirmed_tracer(self):
        result = self.capture_failure(wait_error=OSError('wait failed'), pending_cancel=True)
        self.assertTrue(isinstance(result['error'], KeyboardInterrupt), '#1140 r8 spawn cancellation preserves cancellation')
        self.assertTrue(result['ownership'] == {TRACER} and result['lease'] is not None,
                        '#1140 r8 spawn cancellation owns tracer before delivery')
        self.assertTrue(result['tracer'].kill.call_count == result['tracer'].wait.call_count == 1,
                        '#1140 r8 spawn cancellation runs bounded cleanup')
        self.assertFalse(result['receipt'], '#1140 r8 spawn cancellation publishes no receipt')

    def test_successful_capture_releases_tracer(self):
        output, ownership = self.folder / 'successful', set()
        tracer = mock.Mock(pid=123)
        tracer.wait.return_value = 0
        contract = copy.deepcopy(self.contract)
        def exercise(_tracer, folder, _deadline):
            (folder / 'trace.log').write_text(self.raw)
        error = None
        with mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(w.shutil, 'which', return_value='/stub-strace'), \
                mock.patch.object(w.subprocess, 'run', return_value=mock.Mock(stdout='--kill-on-exit')), \
                mock.patch.object(w.subprocess, 'Popen', return_value=tracer):
            try:
                w.capture(contract, output, {}, exercise=exercise, ownership=ownership)
            except BaseException as caught:
                error = caught
        self.assertTrue(error is None, '#1140 r8 successful synthetic capture remains accepted: ' + repr(error))
        self.assertTrue(not ownership, '#1140 r8 successful capture releases tracer ownership')
        self.assertTrue((output / 'receipt.json').is_file() and not (output / 'failure.json').exists(),
                        '#1140 r8 successful capture publishes only receipt')

    def test_receipt_producers_follow_schema(self):
        marker = '#1140 r10 capture and session producers follow shared receipt schema'
        output = self.folder / 'receipt-schema'
        tracer = mock.Mock(pid=123)
        tracer.wait.return_value = 0
        contract = copy.deepcopy(self.contract)
        def exercise(_tracer, folder, _deadline):
            (folder / 'trace.log').write_text(self.raw)
            (folder / 'extra.json').write_text('{}')
        try:
            with mock.patch.dict(native.NATIVE_RECEIPT_KEYS, {'capture/extra.json': 'extraCaptureSha256'}), \
                    mock.patch.object(sys, 'platform', 'linux'), \
                    mock.patch.object(w.shutil, 'which', return_value='/stub-strace'), \
                    mock.patch.object(w.subprocess, 'run', return_value=mock.Mock(stdout='--kill-on-exit')), \
                    mock.patch.object(w.subprocess, 'Popen', return_value=tracer):
                w.capture(contract, output, {}, exercise=exercise, ownership=set())
            receipt = json.loads((output / 'receipt.json').read_text())
            self.assertTrue(receipt.get('extraCaptureSha256') == native.sha(output / 'extra.json'), marker)
            # Rename a schema key to prove the session producer derives names at runtime.
            container = self.folder / 'session-receipt-schema'
            (container / 'capture').mkdir(parents=True)
            w.save(container / 'capture/receipt.json', receipt)
            with mock.patch.dict(c.w.native.NATIVE_RECEIPT_KEYS,
                                 {'session-domain/driver.json': 'renamedDriverSha256'}):
                c.publish_session_domain(container, {}, [], [], 'loopback', {'binding': {}})
            receipt = json.loads((container / 'capture/receipt.json').read_text())
            self.assertTrue(receipt.get('renamedDriverSha256') == native.sha(container / 'session-domain/driver.json'), marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))

    def provider_failure(self, publication_failure):
        output = self.folder / 'provider-failure'
        original = RuntimeError('business before provider cleanup')
        server, worker, ownership = mock.Mock(), mock.Mock(), set()
        worker.is_alive.return_value = False
        server.shutdown.side_effect = OSError('provider shutdown failed')
        real_save = c.w.save
        def capture(_contract, capture_output, *_args, **_kwargs):
            capture_output.mkdir()
            raise original
        def save(path, value):
            if publication_failure and Path(path).name == 'failure.json':
                raise OSError('provider failure record write failed')
            real_save(path, value)
        observed = None
        with mock.patch.object(c, 'BoundedHTTPServer', return_value=server), \
                mock.patch.object(c.threading, 'Thread', return_value=worker), \
                mock.patch.object(c.w, 'capture', side_effect=capture), \
                mock.patch.object(c.w, 'save', side_effect=save), \
                mock.patch.object(c.common, 'LIVE_GROUPS', ownership):
            try:
                c._native_run(self.host, self.sidecar, output, 28999)
            except BaseException as error:
                observed = error
        failure_path = output / 'capture/failure.json'
        failure = json.loads(failure_path.read_text()) if failure_path.is_file() else None
        return original, observed, ownership, failure

    def test_incomplete_provider_cleanup_refreshes_receipt(self):
        marker = '#1140 r9 provider cleanup finishes before lease release and refreshes pending'
        for pending_tracer in (False, True):
            with self.subTest(pending_tracer=pending_tracer):
                output = self.folder / ('incomplete-' + str(pending_tracer))
                lock = self.folder / ('lease-' + str(pending_tracer))
                original = c.w.Incomplete('#1140 test incomplete capture')
                server, worker = mock.Mock(), mock.Mock()
                worker.is_alive.return_value = False
                ownership = {TRACER} if pending_tracer else set()
                seen = []
                def shutdown():
                    seen.append(lock.exists() and PROVIDER in ownership)
                server.shutdown.side_effect = shutdown
                def capture(_contract, folder, *_args, **_kwargs):
                    folder.mkdir()
                    c.w.save(folder / 'failure.json', {'status': 'INCOMPLETE', 'error': repr(original),
                             'cleanupErrors': [], 'cleanupPending': sorted(ownership)})
                    raise original
                error = None
                try:
                    with mock.patch.object(c, 'BoundedHTTPServer', return_value=server), \
                            mock.patch.object(c.threading, 'Thread', return_value=worker), \
                            mock.patch.object(c.w, 'capture', side_effect=capture), \
                            mock.patch.object(c.common, 'LIVE_GROUPS', ownership), \
                            c.common.parity_lock(lock=lock, timeout=1):
                        c._native_run(self.host, self.sidecar, output, 28999)
                except BaseException as caught:
                    error = caught
                failure = json.loads((output / 'capture/failure.json').read_text())
                expected = [TRACER] if pending_tracer else []
                self.assertTrue(error is original and seen == [True] and server.server_close.called
                                and worker.join.called and sorted(ownership) == expected
                                and failure['cleanupPending'] == expected
                                and lock.exists() == pending_tracer, marker)

    def test_provider_cleanup_preserves_business_error(self):
        original, error, ownership, failure = self.provider_failure(False)
        self.assertTrue(error is original, '#1140 r8 provider cleanup preserves original business error')
        self.assertTrue(ownership == {PROVIDER}, '#1140 r8 failed provider cleanup retains provider ownership')
        self.assertTrue(failure is not None and failure['cleanupPending'] == [PROVIDER],
                        '#1140 r8 provider cleanup failure publishes retained ownership')

    def test_provider_publication_preserves_business_error(self):
        original, error, ownership, _failure = self.provider_failure(True)
        self.assertTrue(error is original, '#1140 r8 provider publication preserves original business error')
        self.assertTrue(any('provider failure publication' in note for note in getattr(error, '__notes__', [])),
                        '#1140 r8 provider publication failure remains inspectable')
        self.assertTrue(ownership == {PROVIDER}, '#1140 r8 provider publication failure retains provider ownership')

    def test_original_binary_path_required(self):
        relocated = self.folder / 'relocated-host'
        relocated.write_bytes(self.host.read_bytes())
        self.host.unlink()
        rejected, error = False, None
        try:
            native.verify_binaries(self.contract)
        except native.BinaryUnavailable as caught:
            rejected, error = True, caught
        except BaseException as caught:
            self.fail('#1140 r8 absent original binary reports controlled availability: ' + repr(caught))
        self.assertTrue(rejected, '#1140 r8 original binary path required despite relocated copy')
        self.assertTrue(str(self.host) in str(error), '#1140 r8 binary unavailable identifies original absolute path')
        self.host.write_bytes(relocated.read_bytes())
        error = None
        try:
            native.verify_binaries(self.contract)
        except BaseException as caught:
            error = caught
        self.assertTrue(error is None, '#1140 r8 restoring original binary path permits replay: ' + repr(error))

    def test_same_path_wrong_binary_rejected(self):
        self.host.write_bytes(b'\x7fELFdifferent-host')
        rejected = False
        try:
            native.verify_binaries(self.contract)
        except native.BinaryUnavailable:
            self.fail('#1140 r8 changed binary is integrity failure not unavailable')
        except native.Incomplete:
            rejected = True
        self.assertTrue(rejected, '#1140 r8 same path changed executable bytes rejected')


if __name__ == '__main__':
    unittest.main(verbosity=2)
