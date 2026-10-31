#!/usr/bin/env python3
# anchor-selfcheck: unsupported witness parser and runtime controls, no source mutation table
"""#1140 synthetic parser controls; --capture separately tests real Linux output.

Only the exec/network fields are under test. The full frozen audit's unrelated
surface/binding fields are explicitly SYNTHETIC_CONTROL_ONLY, never A7 evidence.
"""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import copy
from contextlib import contextmanager
import errno
import io
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'scripts/native-witness/witness.py'
spec = importlib.util.spec_from_file_location('witness', SOURCE)
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)
spec = importlib.util.spec_from_file_location('witness_collector', ROOT / 'scripts/native-witness/collect.py')
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)


def hx(text):
    return '"' + ''.join(f'\\x{byte:02x}' for byte in text.encode()) + '"'


def exec_line(pid, path):
    return f'{pid} execve({hx(str(path))}, [{hx(str(path))}], 0x1 /* 0 vars */) = 0\n'


def compatibility_envelope(folder, contract, evidence):
    """Labelled audit consumer control; deliberately not a native scoring report."""
    evidence, contract = copy.deepcopy(evidence), copy.deepcopy(contract)
    artifact = Path(folder) / 'synthetic-control.txt'
    artifact.write_text('SYNTHETIC_CONTROL_ONLY; not native surfaces or A7 acceptance\n')
    binding = {'connectionId': 'SYNTHETIC_CONTROL_ONLY', 'generation': 1}
    frame = {'type': 'hello_ok', 'protocolVersion': '1.2', 'interactionBinding': binding}
    payload = json.dumps(frame, separators=(',', ':')).encode()
    length = len(payload)
    header = bytes([129, length]) if length < 126 else b'\x81\x7e' + length.to_bytes(2, 'big')
    rows = [{'connection': 1, 'direction': 'receive', 'frame': frame, 'wireHex': (header + payload).hex(),
             'wire': {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': False,
                      'lengthCode': min(length, 126), 'payloadLength': length}}]
    contract.update(cases=['SYNTHETIC_CONTROL_ONLY'], capabilities={}, connections=1,
                    binding=binding, observerRows=rows)
    evidence.update(expectedCases=contract['cases'], executedCases=contract['cases'], capabilities={},
                    binding=binding, observer={'connections': 1, 'rows': rows},
                    surfaces={name: {'status': 'RECORDED', 'artifacts': [
                        {'path': str(artifact), 'sha256': w.sha(artifact)}]} for name in ('output', 'disk', 'http')})
    return contract, evidence


class WitnessTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='pdrs-1140-')
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)
        self.host = self.folder / 'host'
        self.child = self.folder / 'child'
        self.host.write_bytes(b'synthetic host bytes (not executed)')
        self.child.write_bytes(b'synthetic child bytes (not executed)')
        self.contract = {'host': 'rust', 'launcherPid': 99, 'processRoles': ['host', 'sidecar'],
                         'binaries': {role: {'path': str(path), 'argv': [str(path)], 'sha256': w.sha(path)}
                                      for role, path in [('host', self.host), ('sidecar', self.child)]},
                         'externalEndpoints': ['127.0.0.1:29000', '[::1]:29001']}
        self.raw = (exec_line(100, self.host)
                    + '100 clone(child_stack=NULL, flags=CLONE_VM|CLONE_THREAD|CLONE_PARENT_SETTID|CLONE_CHILD_CLEARTID) = 101\n'
                    + '101 connect(3, {sa_family=AF_INET, sin_port=htons(29000), sin_addr=inet_addr(' + hx('127.0.0.1') + ')}, 16) = -1 EINPROGRESS (Operation now in progress)\n'
                    + '101 clone(child_stack=NULL, flags=CLONE_VM|CLONE_VFORK|SIGCHLD <unfinished ...>\n'
                    + exec_line(102, self.child)
                    + '101 <... clone resumed>) = 102\n'
                    + '102 connect(3, {sa_family=AF_INET6, sin6_port=htons(29001), sin6_flowinfo=htonl(0), inet_pton(AF_INET6, ' + hx('::1') + ', &sin6_addr), sin6_scope_id=0}, 28) = 0\n'
                    + '102 exit_group(7) = ?\n102 +++ exited with 7 +++\n'
                    + '101 exit(0) = ?\n101 +++ exited with 0 +++\n'
                    + '100 exit_group(0) = ?\n100 +++ exited with 0 +++\n')

    def test_parity_lock_busy(self):
        lock = self.folder / 'parity.lock'
        real_lock = c.parity_lock
        with real_lock(lock=lock), mock.patch.object(c, 'parity_lock', side_effect=lambda **_: real_lock(timeout=0.01, lock=lock)), \
                mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(c.socket, 'socket') as bind, \
                mock.patch.object(c, '_native_run') as run:
            original = (lock / 'owner.json').read_bytes()
            with self.assertRaises(c.Blocked, msg='#1140 busy parity lease blocks before bind'):
                c.native_run('', '', self.folder / 'blocked')
            self.assertTrue((lock / 'owner.json').read_bytes() == original, '#1140 predecessor lease unchanged')
            self.assertTrue(bind.call_count == run.call_count == 0, '#1140 busy lease never touches ports or process')

    def test_parity_lock_lifetime(self):
        lock = self.folder / 'parity.lock'
        real_lock = c.parity_lock
        for cancelled in (False, True):
            def run(*_):
                self.assertTrue(lock.is_dir(), '#1140 lease covers collection and cleanup')
                if cancelled:
                    raise KeyboardInterrupt('deterministic owned cancellation')
                return 'finished'
            with mock.patch.object(c, 'parity_lock', side_effect=lambda **_: real_lock(lock=lock)), \
                    mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(c.socket, 'socket') as bind, \
                    mock.patch.object(c, '_native_run', side_effect=run):
                bind.return_value.__enter__.return_value.bind.side_effect = lambda *_: self.assertTrue(lock.is_dir(), '#1140 lease precedes fixed port bind')
                if cancelled:
                    with self.assertRaises(KeyboardInterrupt, msg='#1140 owned cancellation propagates through lease'):
                        c.native_run('', '', self.folder / 'unused')
                else:
                    self.assertTrue(c.native_run('', '', self.folder / 'unused') == 'finished', '#1140 successful collection returns')
            self.assertFalse(lock.exists(), '#1140 own lease released after collection or cancellation')

    def test_parity_lock_cancel(self):
        @contextmanager
        def cancelled(**_):
            raise KeyboardInterrupt('deterministic waiting cancellation')
            yield
        with mock.patch.object(c, 'parity_lock', cancelled), mock.patch.object(sys, 'platform', 'linux'), \
                mock.patch.object(c.socket, 'socket') as bind, mock.patch.object(c, '_native_run') as run:
            with self.assertRaises(KeyboardInterrupt, msg='#1140 waiting cancellation never launches'):
                c.native_run('', '', self.folder / 'unused')
            self.assertTrue(bind.call_count == run.call_count == 0, '#1140 cancelled waiter never touches ports')

    def test_ports_blocked(self):
        from contextlib import nullcontext
        for busy in (28999, 29000):
            attempted = []
            def bind(address):
                attempted.append(address[1])
                if address[1] == busy:
                    raise OSError(errno.EADDRINUSE, 'deterministic occupied port')
            with mock.patch.object(c, 'parity_lock', return_value=nullcontext()), \
                    mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(c.socket, 'socket') as sockets, \
                    mock.patch.object(c, '_native_run') as run:
                sockets.return_value.__enter__.return_value.bind.side_effect = bind
                error = None
                try:
                    c.native_run('', '', self.folder / 'blocked')
                except Exception as caught:
                    error = caught
                self.assertTrue(isinstance(error, c.Blocked), '#1140 occupied fixed port reports BLOCKED')
                self.assertTrue(attempted == list(range(28999, busy + 1)) and run.call_count == 0, '#1140 occupied port has no fallback or launch')
        # A non-participant can bind between preflight and actual provider bind.
        with mock.patch.object(c, 'parity_lock', return_value=nullcontext()), \
                mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(c.socket, 'socket'), \
                mock.patch.object(c, '_native_run', side_effect=OSError(errno.EADDRINUSE, 'provider bind race')):
            with self.assertRaises(c.Blocked, msg='#1140 actual provider bind race is BLOCKED'):
                c.native_run('', '', self.folder / 'blocked')

    def test_gateway_bind_race(self):
        from contextlib import nullcontext
        tracer = mock.Mock()
        tracer.poll.return_value = 1
        def run(*_):
            c.wait_ready(tracer, self.folder, 28999, 0)
        with mock.patch.object(c, 'parity_lock', return_value=nullcontext()), \
                mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(c.socket, 'socket') as sockets, \
                mock.patch.object(c, '_native_run', side_effect=run):
            sockets.return_value.__enter__.return_value.bind.side_effect = [None, None, OSError(errno.EADDRINUSE, 'gateway bind race')]
            error = None
            try:
                c.native_run('', '', self.folder / 'unused')
            except Exception as caught:
                error = caught
            self.assertTrue(isinstance(error, c.Blocked), '#1140 gateway bind race reports BLOCKED')
        with mock.patch.object(c.socket, 'socket'):
            with self.assertRaises(c.w.Incomplete, msg='#1140 other startup failure remains incomplete'):
                c.wait_ready(tracer, self.folder, 28999, 0)

    def test_blocked_cli_status(self):
        from contextlib import redirect_stderr
        with mock.patch.object(sys, 'argv', ['collect.py', '--gateway', 'host', '--sidecar', 'child', '--output', 'unused']), \
                mock.patch.object(c, 'native_run', side_effect=c.Blocked('#1140 busy')), redirect_stderr(io.StringIO()) as stderr:
            self.assertTrue(c.main() == 2, '#1140 BLOCKED CLI has exit two')
        self.assertTrue(json.loads(stderr.getvalue())['status'] == 'BLOCKED', '#1140 BLOCKED CLI emits explicit status')

    def capture_fixture(self, folder, validate, *, result=0):
        tracer = mock.Mock(pid=99)
        tracer.poll.return_value = result
        tracer.wait.return_value = result
        contract = dict(self.contract, project=str(self.folder))
        contract.pop('launcherPid')
        def exercise(_tracer, output, _deadline):
            (output / 'trace.log').write_text(self.raw)
        with mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(w.shutil, 'which', return_value='/strace'), \
                mock.patch.object(w.subprocess, 'run', return_value=mock.Mock(stdout='--kill-on-exit')), \
                mock.patch.object(w.subprocess, 'Popen', return_value=tracer):
            return w.capture(contract, folder, {}, exercise=exercise, validate=validate)

    def test_smoke_before_receipt(self):
        home = self.folder / 'smoke-home'
        home.mkdir()
        durable = home / 'turn.jsonl'
        requests = [{'path': '/v1/chat/completions'}]
        for fault in ('none', 'sidecar', 'calls', 'requests', 'target', 'durable'):
            durable.write_text('{}\n')
            folder = self.folder / ('smoke-' + fault)
            seen = []
            def validate(contract, evidence):
                self.assertFalse((folder / 'receipt.json').exists(), '#1140 smoke runs before receipt publication')
                if fault == 'sidecar':
                    evidence['processes'] = [p for p in evidence['processes'] if p['role'] != 'sidecar']
                if fault == 'calls':
                    evidence['networkWitness']['calls'] = []
                if fault == 'target':
                    evidence['networkWitness']['calls'][0]['target'] = '127.0.0.1:29009'
                if fault == 'durable':
                    durable.unlink()
                c.validate_smoke(contract, evidence, [] if fault == 'requests' else requests, home)
                seen.append(True)
            error = None
            try:
                self.capture_fixture(folder, validate)
            except w.Incomplete as caught:
                error = caught
            # Collector owns a separate module identity for Incomplete; convert
            # it through the capture's generic exception path without masking it.
            except c.w.Incomplete as caught:
                error = caught
            if fault == 'none':
                self.assertTrue(error is None and seen == [True], '#1140 valid smoke runs validation')
                self.assertTrue(w.load_capture(folder)[1]['launcherPid'] == 99, '#1140 valid smoke receipt reloads')
            else:
                self.assertTrue(error is not None, '#1140 smoke rejects ' + fault)
                self.assertFalse((folder / 'receipt.json').exists(), '#1140 failed smoke publishes no receipt')
                self.assertTrue((folder / 'failure.json').is_file(), '#1140 failed smoke records failure')
                with self.assertRaises(w.Incomplete, msg='#1140 failed smoke cannot reload'):
                    w.load_capture(folder)

    def test_native_validator_wired(self):
        # Exercise the real collector closure, with no socket or child process.
        def capture(contract, output, environment, *, exercise, validate=None):
            self.assertTrue(callable(validate), '#1140 collector wires prepublication validator')
            validate(contract, {'synthetic': True})
            self.assertTrue(server.shutdown.call_count == 1, '#1140 provider stops before receipt')
            return 'validated'
        server, worker = mock.Mock(), mock.Mock()
        worker.is_alive.return_value = False
        with mock.patch.object(c, 'BoundedHTTPServer', return_value=server), \
                mock.patch.object(c.threading, 'Thread', return_value=worker), \
                mock.patch.object(c.w, 'identity', return_value={}), mock.patch.object(c.w, 'capture', side_effect=capture), \
                mock.patch.object(c, 'validate_smoke') as validate:
            result = c._native_run(self.host, self.child, self.folder / 'native', 28999)
        self.assertTrue(result == 'validated' and validate.call_count == 1, '#1140 collector invokes smoke validation')
        self.assertTrue(server.shutdown.call_count == server.server_close.call_count == worker.join.call_count == 1, '#1140 provider cleanup is idempotent')

    def test_tracer_nonzero(self):
        for result in (-signal.SIGTERM, 128 + signal.SIGTERM, 1):
            folder = self.folder / ('nonzero-' + str(result))
            with self.assertRaises(w.Incomplete, msg='#1140 nonzero tracer cannot publish success'):
                self.capture_fixture(folder, None, result=result)
            self.assertFalse((folder / 'receipt.json').exists(), '#1140 nonzero tracer has no receipt')

    def test_provider_read_deadline(self):
        import socket
        spec = importlib.util.spec_from_file_location('collector_test', ROOT / 'scripts/native-witness/collect.py')
        collector = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(collector)
        server = collector.BoundedHTTPServer.__new__(collector.BoundedHTTPServer)
        left, right = socket.socketpair()
        self.addCleanup(left.close)
        self.addCleanup(right.close)
        with mock.patch.object(collector.http.server.HTTPServer, 'get_request', return_value=(left, ('local', 0))):
            connection, _ = server.get_request()
        self.assertTrue(connection.gettimeout() == 5, '#1140 provider headers have a read deadline')

    def test_exec_argv_hex_roundtrip(self):
        args = [str(self.host), '--native', 'comma, space', '', '中文', 'line\nquote"']
        contract = copy.deepcopy(self.contract)
        contract['binaries']['host']['argv'] = args
        line = '100 execve(' + hx(str(self.host)) + ', [' + ', '.join(map(hx, args)) + '], 0x1 /* 0 vars */) = 0\n'
        raw = self.raw.replace(exec_line(100, self.host), line)
        result = w.parse_trace(raw, contract)
        self.assertTrue(result['processes'][0]['argv'] == args, '#1140 full launch argv preserves arbitrary UTF8')
        with self.assertRaises(w.Incomplete, msg='#1140 truncated argv cannot be certified'):
            w.parse_trace(raw.replace(hx('--native'), hx('--native') + '...'), contract)

    def test_process_and_network(self):
        value = w.parse_trace(self.raw, self.contract)
        self.assertTrue([p['pid'] for p in value['processes']] == [100, 102], '#1140 short-lived child retained')
        self.assertTrue(value['processes'][1]['ppid'] == 100, '#1140 thread child parent is TGID')
        self.assertTrue(value['processes'][1]['exitCode'] == 7, '#1140 child exit code retained')
        self.assertTrue(len(value['networkWitness']['calls']) == 2, '#1140 every connect retained')
        self.assertTrue(value['networkWitness']['calls'][0]['ownerPid'] == 100, '#1140 thread egress owner is TGID')
        self.assertTrue(value['networkWitness']['calls'][1]['target'] == '[::1]:29001', '#1140 IPv6 endpoint retained')

    def test_replay_exec(self):
        value = w.parse_trace(self.raw, self.contract)
        value['execWitness']['events'] = [e for e in value['execWitness']['events']
                                          if not (e['event'] == 'exec' and e['pid'] == 102)]
        checked = w.checked_evidence(self.raw, self.contract, value)
        self.assertFalse(checked['execWitness']['complete'], '#1140 replay-exec detects omitted short child')

    def test_replay_network(self):
        value = w.parse_trace(self.raw, self.contract)
        value['networkWitness']['calls'].pop()
        checked = w.checked_evidence(self.raw, self.contract, value)
        self.assertFalse(checked['networkWitness']['complete'], '#1140 replay-network detects omitted connect')

    def test_command(self):
        cmd = w.command('/usr/bin/strace', self.folder, [str(self.host)])
        self.assertTrue('-q' in cmd and '-qq' not in cmd, '#1140 retain strace exit records')
        self.assertTrue('--kill-on-exit' in cmd and '-f' in cmd, '#1140 retain traced descendants on cancellation')
        self.assertTrue('execveat' in w.TRACE and 'sendmsg' in w.TRACE and 'io_uring_setup' in w.TRACE, '#1140 unsupported escape syscalls observed')

    def test_incomplete(self):
        for label, raw in [
            ('missing terminal', self.raw.replace('101 +++ exited with 0 +++\n', '')),
            ('unfinished', self.raw.replace('101 <... clone resumed>) = 102\n', '')),
            ('unknown line', self.raw + 'unexpected diagnostics\n'),
            ('clone escape', self.raw.replace('CLONE_VFORK', 'CLONE_UNTRACED')),
            ('clone parent', self.raw.replace('CLONE_VFORK', 'CLONE_PARENT')),
            ('unknown argv', self.raw.replace(hx(str(self.child)), hx('/unknown'))),
            ('repeat exec', self.raw.replace('102 exit_group', exec_line(102, self.child) + '102 exit_group')),
            ('no exec child', self.raw.replace(exec_line(102, self.child), '')),
            ('addressed UDP', self.raw.replace('102 exit_group', '102 sendto(3, "\\x61", 1, 0, {sa_family=AF_INET}, 16) = 1\n102 exit_group')),
            ('uring', self.raw.replace('102 exit_group', '102 io_uring_setup(16, {}) = 4\n102 exit_group')),
            ('local helper', self.raw.replace('sa_family=AF_INET, sin_port=htons(29000), sin_addr=inet_addr(' + hx('127.0.0.1') + ')', 'sa_family=AF_UNIX, sun_path=' + hx('/tmp/helper'))),
        ]:
            with self.subTest(label=label):
                with self.assertRaises(w.Incomplete, msg='#1140 incomplete trace rejected: ' + label):
                    w.parse_trace(raw, self.contract)

    def test_failed_exec_and_connected_write(self):
        raw = self.raw.replace('102 exit_group', '102 execve(' + hx('/missing') + ', [], 0x0) = -1 ENOENT (No such file)\n102 sendto(3, "\\x61", 1, MSG_NOSIGNAL, NULL, 0) = 1\n102 exit_group')
        value = w.parse_trace(raw, self.contract)
        self.assertTrue(len(value['execWitness']['attempts']) == 3, '#1140 failed exec attempts retained')
        self.assertTrue(len(value['networkWitness']['calls']) == 2, '#1140 connected write adds no destination')
        self.child.write_bytes(b'changed')
        with self.assertRaises(w.Incomplete, msg='#1140 changed executable rejected'):
            w.parse_trace(raw, self.contract)

    def test_frozen_audit_and_controls(self):
        contract, value = compatibility_envelope(self.folder, self.contract, w.parse_trace(self.raw, self.contract))
        w.audit(self.raw, contract, value)
        self.assertTrue(value['execWitness']['complete'], '#1140 positive complete witness')
        for kind in ('exec', 'network', 'ps', 'both-child-ledgers'):
            bad = copy.deepcopy(value)
            if kind == 'exec':
                bad['execWitness']['events'] = [e for e in bad['execWitness']['events']
                                                 if not (e['event'] == 'exec' and e['pid'] == 102)]
            elif kind == 'network':
                bad['networkWitness']['calls'].pop()
            elif kind == 'ps':
                bad['execWitness']['source'] = 'ps'
            else:
                bad['processes'] = [p for p in bad['processes'] if p['pid'] != 102]
                bad['execWitness']['events'] = [p for p in bad['execWitness']['events'] if p['pid'] != 102]
            with self.subTest(kind=kind):
                with self.assertRaises(ValueError, msg='#1140 original audit rejects ' + kind):
                    w.audit(self.raw, contract, bad)
        # Freeze the known limitation explicitly: raw replay, not the frozen
        # function alone, closes omissions in the caller-supplied calls list.
        bad = copy.deepcopy(value)
        bad['networkWitness']['calls'] = []
        w.frozen_auditor().audit_native(bad, contract)

    def test_platform_and_identity(self):
        with mock.patch.object(sys, 'platform', 'darwin'):
            with self.assertRaises(w.Incomplete, msg='#1140 macOS unsupported before launch'):
                w.capture({}, self.folder / 'forbidden', {})
        self.assertFalse((self.folder / 'forbidden').exists(), '#1140 rejected platform creates no capture')
        with self.assertRaises(w.Incomplete, msg='#1140 shebang/interpreter rejected'):
            w.identity(self.host, [str(self.host)])

    def test_timeout_kills_owned_tracer(self):
        tracer = mock.Mock()
        tracer.pid = 123
        tracer.poll.return_value = None
        tracer.wait.side_effect = [subprocess.TimeoutExpired('strace', 1), -9]
        contract = copy.deepcopy(self.contract)
        contract['project'] = str(self.folder)
        with mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(w.shutil, 'which', return_value='/strace'), \
                mock.patch.object(w.subprocess, 'run', return_value=mock.Mock(stdout='--kill-on-exit')), \
                mock.patch.object(w.subprocess, 'Popen', return_value=tracer):
            with self.assertRaises(subprocess.TimeoutExpired, msg='#1140 never-finishing predecessor times out'):
                w.capture(contract, self.folder / 'timeout', {}, timeout=1)
        self.assertTrue(tracer.kill.call_count == 1, '#1140 timeout kills only owned tracer')
        self.assertFalse((self.folder / 'timeout/receipt.json').exists(), '#1140 timeout publishes no complete receipt')

    def test_cancel_kills_owned_tracer(self):
        tracer = mock.Mock(pid=123)
        tracer.poll.return_value = None
        tracer.wait.return_value = -9
        contract = copy.deepcopy(self.contract)
        contract['project'] = str(self.folder)
        def cancel(*_):
            raise KeyboardInterrupt('deterministic cancellation')
        with mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(w.shutil, 'which', return_value='/strace'), \
                mock.patch.object(w.subprocess, 'run', return_value=mock.Mock(stdout='--kill-on-exit')), \
                mock.patch.object(w.subprocess, 'Popen', return_value=tracer):
            with self.assertRaises(KeyboardInterrupt, msg='#1140 caller cancellation propagates'):
                w.capture(contract, self.folder / 'cancel', {}, exercise=cancel)
        self.assertTrue(tracer.kill.call_count == 1, '#1140 cancellation kills owned tracer')


    def test_capture_receipt_and_tampering(self):
        contract = copy.deepcopy(self.contract)
        value = w.parse_trace(self.raw, contract)
        w.save(self.folder / 'launch.json', contract)
        w.save(self.folder / 'prelaunch.json', {k: v for k, v in contract.items() if k != 'launcherPid'})
        w.save(self.folder / 'evidence.json', value)
        (self.folder / 'trace.log').write_text(self.raw)
        names = [('trace.log', 'traceSha256'), ('launch.json', 'launchSha256'),
                 ('prelaunch.json', 'prelaunchSha256'), ('evidence.json', 'evidenceSha256')]
        receipt = {'status': 'WITNESS_CAPTURED', 'tracerExitCode': 0,
                   **{key: w.sha(self.folder / name) for name, key in names}}
        w.save(self.folder / 'receipt.json', receipt)
        self.assertTrue(w.load_capture(self.folder) == (self.raw, contract, value), '#1140 receipt accepts original bytes')
        for name, _ in names:
            path = self.folder / name
            data = path.read_bytes()
            path.write_bytes(data + b' ')
            with self.subTest(name=name):
                with self.assertRaises(w.Incomplete, msg='#1140 receipt rejects changed ' + name):
                    w.load_capture(self.folder)
            path.write_bytes(data)
        w.save(self.folder / 'failure.json', {'status': 'INCOMPLETE'})
        with self.assertRaises(w.Incomplete, msg='#1140 failure cannot coexist with a valid receipt'):
            w.load_capture(self.folder)
        (self.folder / 'failure.json').unlink()
        changed = copy.deepcopy(contract)
        changed['host'] = 'ts'
        w.save(self.folder / 'launch.json', changed)
        receipt['launchSha256'] = w.sha(self.folder / 'launch.json')
        w.save(self.folder / 'receipt.json', receipt)
        with self.assertRaises(w.Incomplete, msg='#1140 prelaunch contract is independent of launch hash'):
            w.load_capture(self.folder)

    def test_negative_classifier_and_stale_anchor(self):
        import contextlib
        import io
        from types import SimpleNamespace
        negative_path = Path(__file__).with_name('negative.py')
        spec = importlib.util.spec_from_file_location('witness_negative_test', negative_path)
        negative = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(negative)
        good = SimpleNamespace(returncode=0, stdout='', stderr='Ran 1 test in 0.1s\nOK\n')
        bad = SimpleNamespace(returncode=1, stdout='', stderr='FAIL: test_replay_exec (__main__.WitnessTests.test_replay_exec)\nAssertionError: False is not true : #1140 target\nRan 1 test in 0.1s\nFAILED (failures=1)\n')
        self.assertTrue(negative.classify(good, 'test_replay_exec', '#1140 target') == 'SURVIVED', '#1140 classifier positive remains survivor')
        self.assertTrue(negative.classify(bad, 'test_replay_exec', '#1140 target') == 'KILLED', '#1140 classifier requires named failure')
        legacy = SimpleNamespace(returncode=1, stdout='', stderr=bad.stderr.replace('(__main__.WitnessTests.test_replay_exec)', '(__main__.WitnessTests)'))
        self.assertTrue(negative.classify(legacy, 'test_replay_exec', '#1140 target') == 'KILLED', '#1140 Python 3.11 unittest identity accepted')
        for changed in [bad.stderr.replace('AssertionError', 'SyntaxError'), bad.stderr.replace('#1140 target', '#1140 wrong'), bad.stderr.replace('test_replay_exec', 'test_other')]:
            result = SimpleNamespace(returncode=1, stdout='', stderr=changed)
            self.assertTrue(negative.classify(result, 'test_replay_exec', '#1140 target') == 'INVALID', '#1140 unrelated failures are invalid')
        with mock.patch.object(Path, 'read_text', return_value='stale source'), contextlib.redirect_stderr(io.StringIO()) as stderr:
            with self.assertRaises(SystemExit, msg='#1140 stale anchors refuse execution') as error:
                negative.preflight()
        self.assertTrue(error.exception.code == 2 and 'STALE ANCHOR' in stderr.getvalue(), '#1140 stale anchors error rather than survive')

    def test_sigterm_during_spawn(self):
        tracer = mock.Mock(pid=123)
        tracer.poll.return_value = None
        tracer.wait.return_value = -9
        contract = copy.deepcopy(self.contract)
        contract['project'] = str(self.folder)
        previous = signal.getsignal(signal.SIGTERM)
        def interrupted_spawn(*args, **kwargs):
            os.kill(os.getpid(), signal.SIGTERM)
            return tracer
        with mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(w.shutil, 'which', return_value='/strace'), \
                mock.patch.object(w.subprocess, 'run', return_value=mock.Mock(stdout='--kill-on-exit')), \
                mock.patch.object(w.subprocess, 'Popen', side_effect=interrupted_spawn):
            with self.assertRaises(KeyboardInterrupt, msg='#1140 SIGTERM during spawn remains cancellable'):
                w.capture(contract, self.folder / 'spawn-cancel', {})
        self.assertTrue(tracer.kill.call_count == 1, '#1140 SIGTERM waits for owned tracer assignment')
        self.assertTrue(signal.getsignal(signal.SIGTERM) == previous, '#1140 signal handler restored')
        self.assertFalse((self.folder / 'spawn-cancel/receipt.json').exists(), '#1140 spawn cancellation never complete')


def real_capture(folder):
    raw, contract, value = w.load_capture(folder)
    with tempfile.TemporaryDirectory(prefix='pdrs-1140-real-audit-') as temporary:
        contract, evidence = compatibility_envelope(temporary, contract, value)
        w.audit(raw, contract, evidence)
        verdicts = []
        for name in ('omit-short-child', 'omit-egress', 'ps-sampling'):
            bad = copy.deepcopy(evidence)
            try:
                if name == 'omit-short-child':
                    children = [p for p in bad['processes'] if p['role'] != 'host']
                    w.need(bool(children), 'real fixture must contain a child')
                    pid = children[0]['pid']
                    bad['execWitness']['events'] = [e for e in bad['execWitness']['events'] if e['pid'] != pid]
                elif name == 'omit-egress':
                    w.need(bool(bad['networkWitness']['calls']), 'real fixture must contain egress')
                    bad['networkWitness']['calls'].pop()
                else:
                    bad['execWitness']['source'] = 'ps'
                try:
                    w.audit(raw, contract, bad)
                    verdicts.append({'name': name, 'status': 'SURVIVED'})
                except ValueError as error:
                    # Only the frozen consumer's expected rejection is a kill.
                    expected = 'NETWORK_WITNESS' if name == 'omit-egress' else 'PROCESS_WITNESS'
                    verdicts.append({'name': name, 'status': 'KILLED' if getattr(error, 'rule', None) == expected else 'INVALID', 'reason': str(error)})
            except Exception as error:
                verdicts.append({'name': name, 'status': 'INVALID', 'reason': str(error)})
        print(json.dumps({'scope': 'REAL_WITNESS_WITH_SYNTHETIC_AUDIT_ENVELOPE', 'scoring': 'NOT_RUN', 'controls': verdicts}, indent=2))
        return 0 if all(row['status'] == 'KILLED' for row in verdicts) else 1


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == '--capture':
        raise SystemExit(real_capture(sys.argv[2]))
    unittest.main(verbosity=2)
