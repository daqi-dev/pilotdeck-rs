#!/usr/bin/env python3
# anchor-selfcheck: unsupported witness parser and runtime controls, no source mutation table
"""#1140 synthetic parser controls; --capture separately tests real Linux output.

Only the exec/network fields are under test; the envelope below fills the
remaining audit fields offline. The contract never carries a binding or
observer rows: the independent observer records the binding at hello and it is
bound to the contract only at audit time (#1140), so a pre-registered binding
cannot audit, in tests or in real captures.
"""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import base64
import copy
from contextlib import contextmanager, redirect_stdout, redirect_stderr
import errno
import hashlib
import io
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import threading
import time
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


def signal_lease_child(folder, phase, signum):
    """Real signals in a disposable process; never touch the global lease/ports."""
    folder = Path(folder)
    lock = folder / 'parity.lock'
    original_lock, original_save = c.parity_lock, c.common.save
    original_remove = c.common.shutil.rmtree
    previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
    predecessor = {'pid': os.getpid(), 'timestamp': time.time(), 'token': 'predecessor', 'cleanupPending': True}
    if phase == 'waiting':
        lock.mkdir()
        original_save(lock / 'owner.json', predecessor)
    server, worker = mock.Mock(), mock.Mock()
    worker.is_alive.return_value = phase == 'cleanup-failed'
    events = []
    fired = False

    def hit(name):
        nonlocal fired
        events.append(name)
        if name == phase and not fired:
            fired = True
            os.kill(os.getpid(), signum)
            if name.startswith('cleanup-'):
                os.kill(os.getpid(), signum)

    def publish(path, value):
        original_save(path, value)
        hit('acquire')

    def remove(path, *args, **kwargs):
        hit('release')
        return original_remove(path, *args, **kwargs)

    def bind(address):
        hit('preflight-gateway' if address[1] == 28999 else 'preflight-provider')

    def start():
        hit('provider-start')
        events.append('started')

    def identity(*_):
        hit('identity')
        if phase.startswith('cleanup-'):
            raise ValueError('deterministic pre-capture failure')
        return {}

    def capture(*_, **__):
        # Exercise the real nested capture handler with an owned mock tracer.
        hit('capture-entry')
        tracer = mock.Mock(pid=123)
        tracer.poll.return_value = None
        tracer.wait.return_value = -signal.SIGKILL
        def spawn(*args, **kwargs):
            hit('capture-spawn')
            return tracer
        def exercise(*args):
            hit('capture-running')
            raise ValueError('deterministic capture failure')
        with mock.patch.object(c.w.shutil, 'which', return_value='/strace'), \
                mock.patch.object(c.w.subprocess, 'run', return_value=mock.Mock(stdout='--kill-on-exit')), \
                mock.patch.object(c.w.subprocess, 'Popen', side_effect=spawn):
            try:
                binary = {'path': str(SOURCE), 'sha256': c.w.sha(SOURCE), 'argv': [str(SOURCE)]}
                real_capture({'project': str(folder), 'binaries': {'host': binary}}, folder / 'capture', {}, exercise=exercise)
            finally:
                if tracer.kill.called:
                    events.append('tracer-killed')

    real_capture = c.w.capture
    server.shutdown.side_effect = lambda: hit('cleanup-shutdown')
    server.server_close.side_effect = lambda: hit('cleanup-close')
    worker.start.side_effect = start
    worker.join.side_effect = lambda **_: hit('cleanup-failed' if phase == 'cleanup-failed' else 'cleanup-join')
    caught = None
    with mock.patch.object(c, 'parity_lock', side_effect=lambda **_: original_lock(lock=lock)), \
            mock.patch.object(c.common, 'save', side_effect=publish), \
            mock.patch.object(c.common.shutil, 'rmtree', side_effect=remove), \
            mock.patch.object(c.common.time, 'sleep', side_effect=lambda _: hit('waiting')), \
            mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(c.socket, 'socket') as sockets, \
            mock.patch.object(c, 'BoundedHTTPServer', return_value=server), \
            mock.patch.object(c.threading, 'Thread', return_value=worker), \
            mock.patch.object(c.w, 'identity', side_effect=identity), \
            mock.patch.object(c.w, 'capture', side_effect=capture):
        sockets.return_value.__enter__.return_value.bind.side_effect = bind
        try:
            c.native_run('', '', folder / 'native')
        except BaseException as error:
            caught = type(error).__name__
    owner = json.loads((lock / 'owner.json').read_text()) if lock.exists() else None
    print(json.dumps({'phase': phase, 'fired': fired, 'error': caught, 'events': events,
                      'lease': owner, 'closed': server.server_close.call_count,
                      'predecessorUnchanged': owner == predecessor,
                      'joined': worker.join.call_count,
                      'restored': all(signal.getsignal(sig) == handler for sig, handler in previous.items())}))


def hx(text):
    return '"' + ''.join(f'\\x{byte:02x}' for byte in text.encode()) + '"'


def exec_line(pid, path):
    return f'{pid} execve({hx(str(path))}, [{hx(str(path))}], 0x1 /* 0 vars */) = 0\n'


def compatibility_envelope(folder, contract, evidence, *, connection_id='audit-envelope', rows=None):
    """Offline audit envelope: real witness bytes, envelope-only remaining fields.

    The contract carries no binding and no observer rows; w.audit binds the
    hello the independent observer recorded (#1140). This is the audit
    consumer path itself, no longer a synthetic-control shell.  rows, when
    given, are a real recorder's observer rows: the binding comes from the
    hello they recorded, so the audit sees the collector's true row shape
    (#1140 fix-r1) instead of the fabricated envelope hello.
    """
    evidence, contract = copy.deepcopy(evidence), copy.deepcopy(contract)
    artifact = Path(folder) / 'audit-envelope.txt'
    artifact.write_text('#1140 offline audit envelope; exec/network fields are real witness bytes\n')
    if rows is None:
        binding = {'connectionId': connection_id, 'generation': 1}
        frame = {'type': 'hello_ok', 'protocolVersion': '1.2', 'interactionBinding': binding}
        payload = json.dumps(frame, separators=(',', ':')).encode()
        length = len(payload)
        if length < 126:
            header = bytes([129, length])
        elif length <= 65535:
            header = b'\x81\x7e' + length.to_bytes(2, 'big')
        else:
            header = b'\x81\x7f' + length.to_bytes(8, 'big')
        rows = [{'connection': 1, 'direction': 'receive', 'frame': frame, 'wireHex': (header + payload).hex(),
                 'wire': {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': False,
                          'lengthCode': header[1], 'payloadLength': length}}]
    else:
        # The hello a real recorder captured is the only admissible binding.
        binding = w.observer_binding({'observer': {'rows': copy.deepcopy(rows)}})
    contract.update(cases=['AUDIT_ENVELOPE'], capabilities={}, connections=1)
    evidence.update(expectedCases=contract['cases'], executedCases=contract['cases'], capabilities={},
                    binding=binding, observer={'connections': 1, 'rows': copy.deepcopy(rows)},
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

    def test_large_client_frames(self):
        client = c.WireClient.__new__(c.WireClient)
        client.socket, client.rows = mock.Mock(), []
        client.deadline = time.monotonic() + 30
        for size in (125, 126, 65535, 65536):
            value = 'x' * (size - 2)  # JSON quotes account for the other bytes.
            error = None
            try:
                client.send(value)
            except Exception as caught:
                error = caught
            self.assertTrue(error is None, '#1169 client sends 64-bit frame lengths: ' + str(error))
            frame = client.socket.sendall.call_args.args[0]
            code, extra = (size, 0) if size < 126 else ((126, 2) if size <= 65535 else (127, 8))
            self.assertTrue(frame[:2] == bytes([0x81, 0x80 | code]), '#1169 client frame header at length boundary')
            if extra:
                self.assertTrue(int.from_bytes(frame[2:2 + extra], 'big') == size, '#1169 client extended length is exact')
            mask, payload = frame[2 + extra:6 + extra], frame[6 + extra:]
            self.assertTrue(len(payload) == size, '#1169 client payload length agrees with header')
            decoded = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
            self.assertTrue(decoded == json.dumps(value).encode(), '#1169 client payload mask roundtrips')
            self.assertTrue(client.rows[-1]['wireHex'] == frame.hex(), '#1169 driver records sent bytes')

    def test_large_compatibility_frame(self):
        error, rows = None, None
        try:
            _, evidence = compatibility_envelope(self.folder, self.contract, {}, connection_id='x' * 65536)
            rows = evidence['observer']['rows']
        except Exception as caught:
            error = caught
        self.assertTrue(error is None and bool(rows), '#1169 envelope supports 64-bit frame lengths: ' + str(error))
        row = rows[0]
        frame = bytes.fromhex(row['wireHex'])
        payload = json.dumps(row['frame'], separators=(',', ':')).encode()
        self.assertTrue(frame[:2] == b'\x81\x7f' and int.from_bytes(frame[2:10], 'big') == len(payload), '#1169 envelope extended length is exact')
        self.assertTrue(frame[10:] == payload and row['wire']['lengthCode'] == 127, '#1169 envelope wire metadata matches bytes')

    def test_wire_row_schema(self):
        # The frozen audit reads wire metadata and the payload text from every
        # sent row (#1140 fix-r1): a row without either cannot be audited.
        client = c.WireClient.__new__(c.WireClient)
        client.socket, client.rows = mock.Mock(), []
        client.deadline = time.monotonic() + 30
        value = 'x' * 200  # forces the 16-bit extended length
        client.send(value)
        row = client.rows[-1]
        self.assertTrue(set(row) == {'direction', 'wireHex', 'wire', 'frame', 'connection'},
                        '#1140 sent recorder rows carry the audit contract fields')
        self.assertTrue(row['direction'] == 'send' and isinstance(row['frame'], str),
                        '#1140 a sent row records the payload text, not a decoded object')
        self.assertTrue(row['frame'] == json.dumps(value, separators=(',', ':')),
                        '#1140 a sent row frame is the exact payload text')
        size = len(row['frame'].encode())
        self.assertTrue(row['wire'] == {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': True,
                                        'lengthCode': 126, 'payloadLength': size},
                        '#1140 sent-row wire metadata matches the frame bytes')
        frozen = w.frozen_auditor()
        decoded = []
        frozen.Decoder(decoded.append, 'send').feed(
            b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n' + bytes.fromhex(row['wireHex']))
        self.assertTrue(len(decoded) == 1 and decoded[0].get('raw') == row['frame'],
                        '#1140 the frozen decoder cross-check reads a sent row as its payload text')
        error = None
        try:
            frozen.wire_frame(copy.deepcopy(row))
        except Exception as caught:
            error = caught
        self.assertTrue(error is None, '#1140 the frozen wire checker accepts a real sent row: ' + str(error))

    def test_receive_wire_row_schema(self):
        for payload in (b'{"ok":1}', b'{"pad":"' + b'x' * 200 + b'"}'):
            code = len(payload) if len(payload) < 126 else 126
            head = bytes([0x81, code]) if len(payload) < 126 else b'\x81\x7e' + len(payload).to_bytes(2, 'big')
            client = c.WireClient.__new__(c.WireClient)
            client.socket, client.rows = mock.Mock(), []
            client.socket.recv.side_effect = [head, payload]
            client.buffer, client.deadline = b'', time.monotonic() + 30
            value = client.receive()
            row = client.rows[-1]
            self.assertTrue(set(row) == {'direction', 'wireHex', 'wire', 'frame', 'connection'},
                            '#1140 received recorder rows carry the audit contract fields')
            self.assertTrue(row['direction'] == 'receive' and row['frame'] == value == json.loads(payload),
                            '#1140 a received row records the decoded frame')
            self.assertTrue(row['wire'] == {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': False,
                                            'lengthCode': code, 'payloadLength': len(payload)},
                            '#1140 received-row wire metadata matches the frame bytes')
            self.assertTrue(row['wireHex'] == (head + payload).hex(), '#1140 received-row bytes are the raw frame')
            frozen = w.frozen_auditor()
            decoded = []
            frozen.Decoder(decoded.append, 'receive').feed(
                b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n' + bytes.fromhex(row['wireHex']))
            self.assertTrue(len(decoded) == 1 and decoded[0].get('frame') == row['frame'],
                            '#1140 the frozen decoder cross-check reads a received row as its decoded frame')
            error = None
            try:
                frozen.wire_frame(copy.deepcopy(row))
            except Exception as caught:
                error = caught
            self.assertTrue(error is None, '#1140 the frozen wire checker accepts a real received row: ' + str(error))

    def test_recorder_rows_audit(self):
        # A real loopback WebSocket exchange: every observer row the frozen
        # audit consumes is produced by the real WireClient against a real
        # socket, so the audit no longer only ever sees fabricated hellos
        # (#1140 fix-r1 P2-3).
        listener = socket.socket()
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
        listener.listen(1)
        listener.settimeout(10)

        def read_exact(connection, count):
            data = b''
            while len(data) < count:
                chunk = connection.recv(count - len(data))
                if not chunk:
                    raise AssertionError('#1140 recorder audit server socket closed early')
                data += chunk
            return data

        def drain_frame(connection):
            head = read_exact(connection, 2)
            length = head[1] & 127
            if length >= 126:
                length = int.from_bytes(read_exact(connection, 2 if length == 126 else 8), 'big')
            read_exact(connection, 4 + length)

        def server():
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(10)
                request = b''
                while not request.endswith(b'\r\n\r\n'):
                    request += read_exact(connection, 1)
                key = request.decode().split('Sec-WebSocket-Key: ', 1)[1].split('\r\n', 1)[0]
                accept = base64.b64encode(hashlib.sha1(
                    (key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
                connection.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n'
                                  b'Sec-WebSocket-Accept: ' + accept + b'\r\n\r\n')
                drain_frame(connection)
                frames = [{'type': 'hello_ok', 'protocolVersion': '1.2',
                           'interactionBinding': {'connectionId': 'recorder-audit', 'generation': 1}},
                          {'id': 'witness', 'final': True,
                           'event': {'finishReason': 'completed', 'text': 'native witness text'}}]
                for index, frame in enumerate(frames):
                    payload = json.dumps(frame, separators=(',', ':')).encode()
                    header = (bytes([0x81, len(payload)]) if len(payload) < 126
                              else b'\x81\x7e' + len(payload).to_bytes(2, 'big'))
                    connection.sendall(header + payload)
                    if index == 0:
                        drain_frame(connection)

        thread = threading.Thread(target=server)
        thread.start()
        client = c.WireClient(port, time.monotonic() + 10)
        try:
            client.send({'type': 'hello', 'protocolVersion': '1.2', 'clientName': 'ui',
                         'clientVersion': 'native-witness', 'token': 'native-witness-fixture'})
            hello = client.receive()
            client.send({'type': 'request', 'id': 'witness', 'method': 'submit_turn', 'params': {
                'projectKey': 'proj', 'sessionKey': 'recorder-audit', 'runId': 'run-01',
                'channelKey': 'session-domain', 'message': '[native-witness:recorder] controlled wait',
                'allowedTools': [], 'timeoutMs': 400}})
            reply = client.receive()
        finally:
            thread.join(10)
            client.socket.close()
        listener.close()
        self.assertTrue(hello.get('type') == 'hello_ok' and reply.get('final'), '#1140 recorder exchange completed')
        rows = client.rows
        self.assertTrue(len(rows) == 4 and [row['direction'] for row in rows] == ['send', 'receive', 'send', 'receive'],
                        '#1140 the recorder observed the full exchange')
        contract, evidence = compatibility_envelope(self.folder, self.contract,
                                                     w.parse_trace(self.raw, self.contract), rows=rows)
        w.audit(self.raw, contract, evidence)
        for kind in ('send-frame-object', 'no-wire'):
            bad = copy.deepcopy(evidence)
            for row in bad['observer']['rows']:
                if kind == 'send-frame-object' and row['direction'] == 'send':
                    row['frame'] = json.loads(row['frame'])
                elif kind == 'no-wire':
                    row.pop('wire')
            with self.subTest(kind=kind):
                error = None
                try:
                    w.audit(self.raw, contract, bad)
                except ValueError as caught:
                    error = caught
                self.assertTrue(error is not None, '#1140 the frozen audit rejects a recorder row with ' + kind)
                self.assertTrue(getattr(error, 'rule', None) == 'OBSERVER_WIRE'
                                or 'wire bytes/decoded frame disagree' in str(error),
                                '#1140 ' + kind + ' fails on the wire contract, not elsewhere')

    def test_connect_eaddrinuse_not_blocked(self):
        from contextlib import nullcontext
        fault = OSError(errno.EADDRINUSE, 'ephemeral source ports exhausted')
        def connect(*_, **__):
            return c.WireClient(28999, time.monotonic() + 5)
        error = None
        with mock.patch.object(c, 'parity_lock', return_value=nullcontext()), \
                mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(c.socket, 'socket'), \
                mock.patch.object(c.socket, 'create_connection', side_effect=fault) as opened, \
                mock.patch.object(c, '_native_run', side_effect=connect):
            try:
                c.native_run('', '', self.folder / 'not-blocked')
            except Exception as caught:
                error = caught
        self.assertTrue(error is fault, '#1169 connect EADDRINUSE is not fixed-port BLOCKED')
        self.assertTrue(opened.call_count == 1, '#1169 EADDRINUSE test reaches real WireClient connect path')

    def test_bind_error_scope(self):
        def original_error(operation, expected, marker):
            caught = None
            try:
                operation()
            except Exception as error:
                caught = error
            self.assertTrue(caught is expected, marker)
        fault = OSError(errno.EADDRINUSE, 'not a listener bind')
        with mock.patch.object(c.socket, 'socket', side_effect=fault):
            original_error(lambda: c.probe_bind(28999, reuse_address=False), fault,
                           '#1169 socket creation error is not BLOCKED')
        with mock.patch.object(c.socket, 'socket') as sockets:
            sockets.return_value.__enter__.return_value.setsockopt.side_effect = fault
            original_error(lambda: c.probe_bind(29000, reuse_address=True), fault,
                           '#1169 setsockopt error is not BLOCKED')
        for error_number in (errno.EACCES, errno.EADDRNOTAVAIL):
            fault = OSError(error_number, 'other bind error')
            with mock.patch.object(c.socket, 'socket') as sockets:
                sockets.return_value.__enter__.return_value.bind.side_effect = fault
                original_error(lambda: c.probe_bind(28999, reuse_address=False), fault,
                               '#1169 non-conflict bind error propagates')

    def test_non_utf8_string_is_incomplete(self):
        for value in ('"\\xff"', '"\\xc3\\x28"'):
            raw = self.raw.replace(hx(str(self.host)), value)
            error = None
            try:
                w.parse_trace(raw, self.contract)
            except Exception as caught:
                error = caught
            self.assertTrue(isinstance(error, w.Incomplete), '#1169 invalid UTF8 becomes Incomplete')
            self.assertTrue(isinstance(error.__cause__, UnicodeDecodeError), '#1169 invalid UTF8 keeps decode cause')
        self.assertTrue(w.text_string(hx('comma, space')) == 'comma, space', '#1169 hex encoding protects argv delimiters')

    def test_sendmmsg_explicitly_unsupported(self):
        message = '{msg_hdr={msg_name=NULL, msg_namelen=0, msg_iov=[{iov_base="\\x61", iov_len=1}], msg_iovlen=1, msg_controllen=0, msg_flags=0}, msg_len=1}'
        for batch in (message, message + ', ' + message,
                      message + ', ' + message.replace('msg_name=NULL', 'msg_name={sa_family=AF_INET}'),
                      message + ', ' + message.replace('msg_controllen=0', 'msg_controllen=24')):
            raw = self.raw.replace('102 exit_group', f'102 sendmmsg(3, [{batch}], {batch.count("msg_hdr=")}, MSG_NOSIGNAL) = 1\n102 exit_group')
            with self.assertRaisesRegex(w.Incomplete, 'sendmmsg batches are unsupported', msg='#1169 sendmmsg explicitly refuses every batch'):
                w.parse_trace(raw, self.contract)
        for call in ('sendto(3, "\\x61", 1, MSG_NOSIGNAL, NULL, 0)',
                     'sendmsg(3, {msg_name=NULL, msg_namelen=0, msg_iov=[], msg_iovlen=0, msg_controllen=0, msg_flags=0}, 0)'):
            raw = self.raw.replace('102 exit_group', f'102 {call} = 1\n102 exit_group')
            self.assertTrue(len(w.parse_trace(raw, self.contract)['networkWitness']['calls']) == 2, '#1169 supported connected writes retain existing calls')

    def test_trace_size_precheck(self):
        self.assertTrue(w.TRACE_LIMIT == 64 * 1024 * 1024, '#1169 trace production cap is 64 MiB')
        path = self.folder / 'oversize.trace'
        with path.open('wb') as stream:
            stream.truncate(w.TRACE_LIMIT + 1)
        stream = mock.MagicMock()
        stream.__enter__.return_value = stream
        stream.read.return_value = b''
        with mock.patch.object(Path, 'open', return_value=stream), \
                mock.patch.object(w.os, 'fstat', return_value=path.stat()):
            with self.assertRaises(w.Incomplete, msg='#1169 oversized trace rejected before read'):
                w.read_trace(path)
        self.assertTrue(stream.read.call_count == 0, '#1169 oversized trace never allocates input')

    def test_trace_size_growth_and_read_bound(self):
        path = self.folder / 'growing.trace'
        path.write_bytes(b'x' * 33)
        with mock.patch.object(w.native, 'TRACE_LIMIT', 32), \
                mock.patch.object(w.os, 'fstat', return_value=mock.Mock(st_size=0)):
            with self.assertRaises(w.Incomplete, msg='#1169 trace growth past cap is rejected'):
                w.read_trace(path)
        stream = mock.MagicMock()
        stream.__enter__.return_value = stream
        stream.read.return_value = b'ok'
        with mock.patch.object(w.native, 'TRACE_LIMIT', 32), mock.patch.object(Path, 'open', return_value=stream), \
                mock.patch.object(w.os, 'fstat', return_value=mock.Mock(st_size=2)):
            self.assertTrue(w.read_trace(path) == 'ok', '#1169 bounded read returns original trace')
        self.assertTrue(stream.read.call_args_list == [mock.call(33)], '#1169 trace read requests at most cap plus one')
        for size in (0, 31, 32):
            path.write_bytes(b'x' * size)
            with mock.patch.object(w.native, 'TRACE_LIMIT', 32):
                value, error = None, None
                try:
                    value = w.read_trace(path)
                except Exception as caught:
                    error = caught
                self.assertTrue(error is None and value == 'x' * size, '#1169 trace cap is inclusive in bytes')
        path.write_bytes(b'\xff')
        error = None
        try:
            w.read_trace(path)
        except Exception as caught:
            error = caught
        self.assertTrue(isinstance(error, w.Incomplete), '#1169 trace file decode is controlled')

    def test_capture_trace_limit_wired(self):
        folder = self.folder / 'oversize-capture'
        with mock.patch.object(w.native, 'TRACE_LIMIT', len(self.raw.encode()) - 1):
            with self.assertRaises(w.Incomplete, msg='#1169 capture enforces trace limit before receipt'):
                self.capture_fixture(folder, None)
        self.assertFalse((folder / 'receipt.json').exists(), '#1169 oversize capture publishes no receipt')
        self.assertTrue((folder / 'failure.json').is_file(), '#1169 oversize capture records failure')

    def test_load_trace_limit_wired(self):
        folder = self.folder / 'valid-capture'
        self.capture_fixture(folder, None)
        self.assertTrue(w.load_capture(folder)[0] == self.raw, '#1169 normal capture reloads')
        with mock.patch.object(w.native, 'TRACE_LIMIT', len(self.raw.encode()) - 1):
            with self.assertRaises(w.Incomplete, msg='#1169 reload enforces trace limit'):
                w.load_capture(folder)

    def test_collector_trace_limit_wired(self):
        folder = self.folder / 'driver'
        folder.mkdir()
        (folder / 'trace.log').write_text('100 execve("\\x61", [], 0x0) = 0\n')
        server, worker, client = mock.Mock(), mock.Mock(), mock.Mock()
        worker.is_alive.return_value = False
        client.receive.side_effect = [{'type': 'hello_ok', 'interactionBinding': {}},
                                     {'id': 'witness', 'final': True, 'event': {'finishReason': 'completed', 'text': 'native witness text'}}]
        client.rows, client.upgrade = [], 'synthetic upgrade'
        def capture(_contract, _output, _environment, *, exercise, validate, **_):
            exercise(mock.Mock(), folder, time.monotonic() + 5)
            # A real capture returns (launch, evidence); the mutant path must
            # run to the named assertion, not die on a None unpack (#1140
            # cap-fix).
            return mock.Mock(), mock.Mock()
        with mock.patch.object(c, 'BoundedHTTPServer', return_value=server), \
                mock.patch.object(c.threading, 'Thread', return_value=worker), \
                mock.patch.object(c.w, 'identity', return_value={}), \
                mock.patch.object(c.w, 'capture', side_effect=capture), \
                mock.patch.object(c, 'wait_ready'), mock.patch.object(c, 'WireClient', return_value=client), \
                mock.patch.object(c.w.native, 'TRACE_LIMIT', 1), mock.patch.object(c.w, 'stop_host') as stop:
            with self.assertRaises(c.w.Incomplete, msg='#1169 driver enforces trace limit before host lookup'):
                c._native_run(self.host, self.child, self.folder / 'native-limit', 28999)
        self.assertTrue(stop.call_count == 0, '#1169 oversized trace never selects a host pid')
        self.assertTrue(client.socket.close.call_count == 1, '#1169 driver still closes owned client')

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

    def test_signal_lease_phases(self):
        phases = ('waiting', 'acquire', 'preflight-gateway', 'preflight-provider', 'provider-start',
                  'identity', 'capture-entry', 'capture-spawn', 'capture-running',
                  'cleanup-shutdown', 'cleanup-close', 'cleanup-join', 'release')
        for sig in (signal.SIGTERM, signal.SIGINT):
            for phase in phases:
                folder = self.folder / (phase + '-' + str(sig))
                folder.mkdir()
                result = subprocess.run([sys.executable, '-I', str(Path(__file__).resolve()),
                                         '--signal-lease-child', str(folder), phase, str(sig)],
                                        capture_output=True, text=True, timeout=15)
                self.assertTrue(result.returncode == 0, '#1140 signal lease child survives cancellation: ' + phase + result.stderr)
                self.assertTrue(bool(result.stdout.strip()), '#1140 signal lease child publishes result: ' + phase)
                value = json.loads(result.stdout)
                self.assertTrue(value['fired'] and value['error'] == ('ValueError' if phase.startswith('cleanup-') else 'KeyboardInterrupt'), '#1140 signal reaches lease cleanup: ' + phase)
                if phase == 'waiting':
                    self.assertTrue(value['predecessorUnchanged'], '#1140 cancelled waiter leaves predecessor untouched')
                else:
                    self.assertTrue(value['lease'] is None, '#1140 signal releases cleaned lease: ' + phase)
                self.assertTrue(value['restored'], '#1140 lease signal handlers restored: ' + phase)
                if phase not in ('waiting', 'acquire', 'preflight-gateway', 'preflight-provider'):
                    self.assertTrue(value['closed'] == value['joined'] == 1, '#1140 signals cannot interrupt provider cleanup: ' + phase)
                if phase in ('capture-spawn', 'capture-running'):
                    self.assertTrue('tracer-killed' in value['events'], '#1140 signal capture kills owned tracer: ' + phase)

    def test_provider_cleanup_pending(self):
        result = subprocess.run([sys.executable, '-I', str(Path(__file__).resolve()),
                                 '--signal-lease-child', str(self.folder), 'cleanup-failed', str(signal.SIGTERM)],
                                capture_output=True, text=True, timeout=15)
        self.assertTrue(result.returncode == 0, '#1140 pending cleanup child completes: ' + result.stderr)
        self.assertTrue(bool(result.stdout.strip()), '#1140 pending cleanup child publishes result')
        value = json.loads(result.stdout)
        self.assertTrue(value['lease'] is not None and bool(value['lease'].get('cleanupPending')), '#1140 failed provider cleanup retains lease')
        self.assertTrue(value['fired'] and value['error'] == 'ValueError' and value['closed'] == value['joined'] == 1, '#1140 cancelled provider cleanup remains incomplete')
        self.assertTrue(value['restored'], '#1140 failed cleanup restores signal handlers')

    def test_provider_start_failure(self):
        server, worker = mock.Mock(), mock.Mock()
        worker.start.side_effect = RuntimeError('deterministic thread start failure')
        with mock.patch.object(c, 'BoundedHTTPServer', return_value=server), \
                mock.patch.object(c.threading, 'Thread', return_value=worker):
            with self.assertRaises(RuntimeError, msg='#1140 provider start failure propagates'):
                c._native_run('', '', self.folder / 'start-failure', 28999)
        self.assertTrue(server.server_close.call_count == 1, '#1140 unstarted provider socket is closed')
        self.assertTrue(server.shutdown.call_count == worker.join.call_count == 0, '#1140 unstarted provider never waits for serving thread')
        self.assertTrue(not c.common.LIVE_GROUPS, '#1140 failed start leaves no owned provider')

    def test_preflight_listener_policies(self):
        from contextlib import nullcontext
        probes = [mock.MagicMock(), mock.MagicMock()]
        for probe in probes:
            probe.__enter__.return_value = probe
            probe.__exit__.return_value = False
        with mock.patch.object(c, 'parity_lock', return_value=nullcontext()), \
                mock.patch.object(sys, 'platform', 'linux'), \
                mock.patch.object(c.socket, 'socket', side_effect=probes), \
                mock.patch.object(c, '_native_run', return_value='done'), redirect_stdout(io.StringIO()):
            def gateway_bind(address):
                self.assertTrue(probes[0].setsockopt.call_count == 0, '#1140 gateway preflight never enables reuse')
                self.assertTrue(address == ('127.0.0.1', 28999), '#1140 gateway preflight uses gateway port')
            def provider_bind(address):
                self.assertTrue(probes[1].setsockopt.call_args_list == [mock.call(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)], '#1140 provider preflight enables reuse before bind')
                self.assertTrue(address == ('127.0.0.1', 29000), '#1140 provider preflight uses provider port')
            probes[0].bind.side_effect = gateway_bind
            probes[1].bind.side_effect = provider_bind
            c.native_run('', '', self.folder / 'unused')
        self.assertTrue(all(probe.bind.call_count == 1 for probe in probes), '#1140 policy preflight checks both ports')

    def test_wait_ready_gateway_policy(self):
        tracer = mock.Mock()
        tracer.poll.return_value = 1
        with mock.patch.object(c.socket, 'socket') as sockets:
            probe = sockets.return_value.__enter__.return_value
            def bind(address):
                self.assertTrue(probe.setsockopt.call_count == 0, '#1140 readiness gateway never enables reuse')
                self.assertTrue(address == ('127.0.0.1', 28999), '#1140 readiness checks gateway port')
            probe.bind.side_effect = bind
            with self.assertRaises(c.w.Incomplete, msg='#1140 readiness reports original failure on free port'):
                c.wait_ready(tracer, self.folder, 28999, 0)
        self.assertTrue(probe.bind.call_count == 1, '#1140 readiness probes exited gateway once')

    def test_real_probe_time_wait_and_listener(self):
        # Exercised on Darwin as well as the Linux collector host; no BSD
        # reuse assumption. The listener models HTTPServer, not the gateway.
        # Server actively closes first: recv EOF orders FIN before client close.
        with socket.socket() as listener:
            listener.settimeout(2)
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
            listener.listen()
            for reuse in (False, True):
                with self.assertRaises(c.Blocked, msg='#1140 both probe policies reject real listener') as busy:
                    c.probe_bind(port, reuse_address=reuse)
                self.assertTrue(busy.exception.__cause__.errno == errno.EADDRINUSE, '#1140 real listener is address in use')
            with socket.create_connection(('127.0.0.1', port), timeout=2) as client:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(2)
                    connection.shutdown(socket.SHUT_WR)
                    self.assertTrue(client.recv(1) == b'', '#1140 server FIN precedes client close')
                    client.shutdown(socket.SHUT_WR)
                    self.assertTrue(connection.recv(1) == b'', '#1140 client FIN completes TIME_WAIT setup')
        with self.assertRaises(c.Blocked, msg='#1140 gateway probe rejects real TIME_WAIT') as waiting:
            c.probe_bind(port, reuse_address=False)
        self.assertTrue(waiting.exception.__cause__.errno == errno.EADDRINUSE, '#1140 TIME_WAIT occupies bare bind')
        error = None
        try:
            c.probe_bind(port, reuse_address=True)
        except (OSError, c.Blocked) as caught:
            error = caught
        self.assertTrue(error is None, '#1140 reusable probe accepts real TIME_WAIT')

    def test_parity_lock_lifetime(self):
        lock = self.folder / 'parity.lock'
        real_lock = c.parity_lock
        for cancelled in (False, True):
            def run(*_, **__):
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
    def test_provider_bind_conflict(self):
        from contextlib import nullcontext
        # A non-participant can bind between preflight and actual provider bind.
        def provider_bind(*_, **__):
            server = c.BoundedHTTPServer.__new__(c.BoundedHTTPServer)
            server.server_bind()
        with mock.patch.object(c, 'parity_lock', return_value=nullcontext()), \
                mock.patch.object(sys, 'platform', 'linux'), mock.patch.object(c.socket, 'socket'), \
                mock.patch.object(c.http.server.HTTPServer, 'server_bind', side_effect=OSError(errno.EADDRINUSE, 'provider bind race')), \
                mock.patch.object(c, '_native_run', side_effect=provider_bind):
            error = None
            try:
                c.native_run('', '', self.folder / 'blocked')
            except Exception as caught:
                error = caught
            self.assertTrue(isinstance(error, c.Blocked), '#1169 actual provider bind race is BLOCKED')

    def test_gateway_bind_race(self):
        from contextlib import nullcontext
        tracer = mock.Mock()
        tracer.poll.return_value = 1
        def run(*_, **__):
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
        with mock.patch.object(sys, 'argv', ['collect.py', '--gateway', 'host', '--sidecar', 'child', '--output', 'unused']), \
                mock.patch.object(c, 'native_run', side_effect=c.Blocked('#1140 busy')), \
                redirect_stdout(io.StringIO()) as stdout, redirect_stderr(io.StringIO()) as stderr:
            self.assertTrue(c.main() == 2, '#1140 BLOCKED CLI has exit two')
        self.assertTrue(bool(stdout.getvalue().strip()), '#1140 BLOCKED CLI status is on stdout')
        self.assertTrue(json.loads(stdout.getvalue())['status'] == 'BLOCKED', '#1140 BLOCKED CLI emits explicit status')
        self.assertTrue(stderr.getvalue() == '', '#1140 BLOCKED CLI keeps diagnostics separate')

    def test_cli_usage_is_not_blocked(self):
        for args in ([], ['--gateway', 'host', '--sidecar', 'child', '--output', 'unused', '--port', 'abc']):
            with mock.patch.object(sys, 'argv', ['collect.py', *args]), redirect_stdout(io.StringIO()) as stdout, \
                    redirect_stderr(io.StringIO()) as stderr, mock.patch.object(c, 'native_run') as run:
                with self.assertRaises(SystemExit, msg='#1140 bad CLI arguments terminate') as error:
                    c.main()
            self.assertTrue(error.exception.code == 2 and 'usage:' in stderr.getvalue(), '#1140 argparse retains usage error')
            self.assertTrue(stdout.getvalue() == '' and run.call_count == 0, '#1140 usage error cannot claim BLOCKED')

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
        def capture(contract, output, environment, *, exercise, validate=None, **_):
            self.assertTrue(callable(validate), '#1140 collector wires prepublication validator')
            validate(contract, {'synthetic': True})
            self.assertTrue(server.shutdown.call_count == 1, '#1140 provider stops before receipt')
            return 'launch', 'validated'
        server, worker = mock.Mock(), mock.Mock()
        worker.is_alive.return_value = False
        with mock.patch.object(c, 'BoundedHTTPServer', return_value=server), \
                mock.patch.object(c.threading, 'Thread', return_value=worker), \
                mock.patch.object(c.w, 'identity', return_value={}), mock.patch.object(c.w, 'capture', side_effect=capture), \
                mock.patch.object(c, 'validate_smoke') as validate:
            result = c._native_run(self.host, self.child, self.folder / 'native', 28999)
        self.assertTrue(result == ('launch', 'validated') and validate.call_count == 1, '#1140 collector invokes smoke validation')
        self.assertTrue(server.shutdown.call_count == server.server_close.call_count == worker.join.call_count == 1, '#1140 provider cleanup is idempotent')

    def test_session_contract_declares_connections(self):
        # The frozen audit checks observer connections against the contract;
        # the session-domain contract must declare the planned count (#1140
        # fix-r1 P1) instead of leaving the key absent.
        seen, published = [], []

        def capture(contract, output, environment, *, exercise, validate=None, **_):
            seen.append(copy.deepcopy(contract))
            return 'launch', 'validated'

        def publish(output, observer, rows, requests, endpoint, driver, sources):
            published.append((copy.deepcopy(observer), list(rows), list(requests), endpoint, copy.deepcopy(driver)))

        server, worker = mock.Mock(), mock.Mock()
        worker.is_alive.return_value = False
        with mock.patch.object(c, 'BoundedHTTPServer', return_value=server), \
                mock.patch.object(c.threading, 'Thread', return_value=worker), \
                mock.patch.object(c.w, 'identity', return_value={}), mock.patch.object(c.w, 'capture', side_effect=capture), \
                mock.patch.object(c, 'publish_session_domain', side_effect=publish):
            c._native_run(self.host, self.child, self.folder / 'native-session', 28999, session_domain=True)
        self.assertTrue(len(seen) == 1, '#1140 the session-domain run launches exactly one capture')
        contract = seen[0]
        self.assertTrue(contract.get('connections') == 1, '#1140 session-domain contract declares the planned connection count')
        self.assertTrue(contract['cases'] == c.session_manifest(), '#1140 session-domain contract declares the full 72-case manifest')
        self.assertTrue(contract['externalEndpoints'] == ['127.0.0.1:29000'], '#1140 session-domain contract declares the provider endpoint')
        self.assertTrue(published and published[0][0] == {'connections': 1, 'rows': []},
                        '#1140 session-domain publish declares the observer connection count')
        # The exercise closure is mocked away here, so no hello ever arrives:
        # the driver record is passed through as the empty placeholder dict
        # (#1140 fix-r3); a real hello fills it before publish.
        self.assertTrue(published and published[0][4] == {},
                        '#1140 session-domain publish carries the driver hello record slot')

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

    def test_provider_shutdown_deadline(self):
        server = c.BoundedHTTPServer.__new__(c.BoundedHTTPServer)
        server.shutdown_timeout = 0.05
        release, finished = c.threading.Event(), c.threading.Event()
        def never_finishes():
            try:
                release.wait(5)
            finally:
                finished.set()
        try:
            with mock.patch.object(c.http.server.HTTPServer, 'shutdown', side_effect=never_finishes):
                with self.assertRaises(c.w.Incomplete, msg='#1140 provider shutdown cannot wait forever'):
                    server.shutdown()
        finally:
            release.set()
            self.assertTrue(finished.wait(2), '#1140 shutdown test releases owned waiter')
        server.shutdown_timeout = 2
        with mock.patch.object(c.http.server.HTTPServer, 'shutdown') as stopped:
            server.shutdown()
        self.assertTrue(stopped.call_count == 1, '#1140 bounded shutdown accepts stopped provider')

    def test_provider_thread_signal_mask(self):
        # Real provider and shutdown threads, ephemeral loopback socket only.
        observed = []
        ready = c.threading.Event()
        def serve(server, **_):
            observed.append(signal.pthread_sigmask(signal.SIG_BLOCK, []))
            ready.set()
        def capture(*_, **__):
            self.assertTrue(ready.wait(2), '#1140 provider thread publishes signal mask')
            observed.append(signal.pthread_sigmask(signal.SIG_BLOCK, []))
            return 'launch', 'evidence'
        before = signal.pthread_sigmask(signal.SIG_BLOCK, [])
        with mock.patch.object(c.BoundedHTTPServer, 'serve_forever', serve), \
                mock.patch.object(c.http.server.HTTPServer, 'shutdown'), \
                mock.patch.object(c.w, 'identity', return_value={}), mock.patch.object(c.w, 'capture', side_effect=capture):
            c._native_run('', '', self.folder / 'mask', -1)
        self.assertTrue(observed[0] >= {signal.SIGINT, signal.SIGTERM}, '#1140 provider thread defers cancellation delivery')
        self.assertTrue(observed[1] == before and signal.pthread_sigmask(signal.SIG_BLOCK, []) == before, '#1140 capture inherits original signal mask')

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

    def test_transcript_witness_parsed(self):
        # #1140 fix-r4 P1-1: the host's session-transcript opens are recorded
        # with afterExecs (the number of execs already recorded at that row),
        # giving opens and execs one shared order -- the only per-session field
        # the audit can attribute sidecars by.  Non-transcript opens are not
        # recorded.  #1140 fix-r5 P2-1: openat filtering is scoped to the
        # host-owned, transcript-shaped, successful, absolute-path opens that
        # carry window semantics; every other openat (dirfd-relative library
        # internals, failed opens, child-owned transcript opens) is skipped
        # instead of voiding the capture, and only a host-owned transcript
        # path the witness cannot attribute still fails closed.
        def openat(key, fd='5', owner=100):
            return (f'{owner} openat(AT_FDCWD, ' + hx(f'/home/projects/mock/chats/{key}.jsonl')
                    + ', O_WRONLY|O_CREAT|O_APPEND, 0644) = ' + fd + '\n')
        raw = (exec_line(100, self.host)
               + '100 openat(AT_FDCWD, ' + hx('/home/token') + ', O_RDONLY|O_CLOEXEC) = 3\n'
               + openat('first.session')
               + '100 clone(child_stack=NULL, flags=CLONE_VM|CLONE_VFORK|SIGCHLD) = 102\n'
               + exec_line(102, self.child)
               + openat('second.session')
               + '100 clone(child_stack=NULL, flags=CLONE_VM|CLONE_VFORK|SIGCHLD) = 103\n'
               + exec_line(103, self.child)
               + openat('first.session', '6')
               + '100 exit_group(0) = ?\n100 +++ exited with 0 +++\n'
               + '102 exit_group(0) = ?\n102 +++ exited with 0 +++\n'
               + '103 exit_group(0) = ?\n103 +++ exited with 0 +++\n')
        value = w.parse_trace(raw, self.contract)
        expected = {'complete': True, 'source': 'strace-openat',
                    'opens': [{'pid': 100, 'afterExecs': 1,
                               'path': '/home/projects/mock/chats/first.session.jsonl'},
                              {'pid': 100, 'afterExecs': 2,
                               'path': '/home/projects/mock/chats/second.session.jsonl'},
                              {'pid': 100, 'afterExecs': 3,
                               'path': '/home/projects/mock/chats/first.session.jsonl'}]}
        for index, entry in enumerate(expected['opens']):
            entry.update(flags='O_WRONLY|O_CREAT|O_APPEND, 0644', result='6' if index == 2 else '5', dirfd='AT_FDCWD')
        self.assertTrue(value['transcriptWitness'] == expected,
                        '#1140 transcript opens are recorded in exec order')
        replay = w.checked_evidence(raw, self.contract, value)
        self.assertTrue(replay['transcriptWitness']['complete'] is True,
                        '#1140 replayed transcript witness stays complete')
        bad = copy.deepcopy(value)
        bad['transcriptWitness']['opens'][0]['path'] = '/home/projects/mock/chats/other.jsonl'
        self.assertFalse(w.checked_evidence(raw, self.contract, bad)['transcriptWitness']['complete'],
                         '#1140 replay detects a rewritten transcript open')
        skipped = None
        try:
            skipped = w.parse_trace(raw.replace('100 openat(AT_FDCWD, ' + hx('/home/token'),
                                                '100 openat(3, ' + hx('token')), self.contract)
        except w.Incomplete:
            pass
        self.assertTrue(skipped is not None and skipped['transcriptWitness'] == expected,
                        '#1140 fix-r5 P2-1 a dirfd-relative non-transcript openat is skipped, not fatal')
        failed = w.parse_trace(raw.replace(openat('second.session'),
                                           openat('second.session', '-1 ENOENT (No such file or directory)')),
                               self.contract)
        self.assertTrue([o['path'] for o in failed['transcriptWitness']['opens']]
                        == [o['path'] for o in expected['opens']][:1] + [expected['opens'][2]['path']],
                        '#1140 fix-r5 P2-1 a failed transcript open is not recorded as the first open')
        child = w.parse_trace(raw.replace(exec_line(102, self.child),
                                          exec_line(102, self.child) + openat('other.session', owner=102)),
                              self.contract)
        self.assertTrue(child['transcriptWitness'] == expected,
                        '#1140 fix-r5 P2-1 a child-owned transcript open is skipped, not recorded')
        with self.assertRaises(w.Incomplete, msg='#1140 fix-r5 P2-1 a relative transcript path fails closed'):
            w.parse_trace(raw.replace(openat('second.session'),
                                      '100 openat(AT_FDCWD, ' + hx('mock/chats/second.session.jsonl')
                                      + ', O_WRONLY|O_CREAT|O_APPEND, 0644) = 5\n'), self.contract)

    def test_cb8_interleaved_sendto(self):
        # Verbatim cb8 trace fragment: mock WS text only, no HTTP prompts/paths.
        fragment = Path(__file__).with_name('cb8-sendto-interleaved.trace').read_text()
        raw = (exec_line(2378057, self.host)
               + '2378057 clone(flags=CLONE_VM|CLONE_THREAD) = 2378081\n'
               + fragment
               + '2378057 exit_group(0) = ?\n2378057 +++ exited with 0 +++\n')
        value, error = None, None
        try:
            value = w.parse_trace(raw, self.contract)
        except w.Incomplete as caught:
            error = caught
        self.assertTrue(error is None, '#1140 cb8 interleaved sendto parses complete call: ' + str(error))
        self.assertTrue(value['networkWitness']['calls'] == [], '#1140 cb8 connected send adds no destination')
        self.assertTrue(value['processes'][0]['exitCode'] == 0, '#1140 cb8 host exit remains witnessed')
        rows = w.records(fragment)
        sends = [body for pid, body in rows if pid == 2378057 and body.startswith('sendto(')]
        self.assertTrue(len(sends) == 1, '#1140 cb8 sendto entry and return form one record')
        self.assertTrue(sends[0].startswith('sendto(14, ') and sends[0].endswith(', 352, MSG_NOSIGNAL, NULL, 0 )           = 352'), '#1140 cb8 merged fd arguments and result retained')
        # Normalizing the separator must not certify an addressed send.
        with self.assertRaises(w.Incomplete, msg='#1140 cb8 addressed send remains rejected'):
            w.parse_trace(raw.replace('NULL, 0 <unfinished', '{sa_family=AF_INET}, 16 <unfinished'), self.contract)

    def test_interleaved_connect(self):
        connection = ('101 connect(3, {sa_family=AF_INET, sin_port=htons(29000), sin_addr=inet_addr('
                      + hx('127.0.0.1') + ')}, 16) = -1 EINPROGRESS (Operation now in progress)\n')
        split = ('101 connect(3, {sa_family=AF_INET, sin_port=htons(29000), <unfinished ...>\n'
                 + '100 sendto(14, "\\x61", 1, MSG_NOSIGNAL, NULL, 0) = 1\n'
                 + '101 <... connect resumed>sin_addr=inet_addr(' + hx('127.0.0.1')
                 + ')}, 16) = -1 EINPROGRESS (Operation now in progress)\n')
        value, error = None, None
        try:
            value = w.parse_trace(self.raw.replace(connection, split), self.contract)
        except w.Incomplete as caught:
            error = caught
        self.assertTrue(error is None, '#1140 resumed connect preserves destination: ' + str(error))
        self.assertTrue(value['networkWitness']['calls'][0] == {
            'ownerPid': 100, 'tid': 101, 'target': '127.0.0.1:29000', 'address': '127.0.0.1',
            'port': 29000, 'syscall': 'connect', 'result': '-1 EINPROGRESS (Operation now in progress)'}, '#1140 resumed connect records one complete destination and result')
        self.assertTrue(len(value['networkWitness']['calls']) == 2, '#1140 interleaving neither duplicates nor drops connects')

    def test_trace_pairing_guards(self):
        entry = '100 sendto(14, "\\x61", 1, MSG_NOSIGNAL, NULL, 0 <unfinished ...>\n'
        resumed = '100 <... sendto resumed>) = 1\n'
        invalid_traces = [
            ('orphan', resumed),
            ('wrong pid', entry + resumed.replace('100 ', '101 ')),
            ('wrong syscall', entry + resumed.replace('sendto', 'sendmsg')),
            ('duplicate', entry + entry + resumed),
            ('unterminated', entry),
        ]
        for label, raw in invalid_traces:
            with self.assertRaises(w.Incomplete, msg='#1140 trace pairing rejects ' + label):
                w.records('99 exit(0) = ?\n' + raw)

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
        # The contract is bound the same way w.audit binds it (#1140).
        bad = copy.deepcopy(value)
        bad['networkWitness']['calls'] = []
        w.frozen_auditor().audit_native(bad, w.bind_contract(contract, bad))

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

    def _assert_cancel_unwind(self, first, second, marker):
        reached, caught = [], None
        try:
            try:
                os.kill(os.getpid(), first)
            finally:
                os.kill(os.getpid(), second)
                reached.append('cleanup-completed')
        except KeyboardInterrupt as error:
            caught = error
        self.assertTrue(reached == ['cleanup-completed'] and caught is not None
                        and f'signal {first}' in str(caught), marker)
        # Suppression lasts only as long as that exception is active.
        with self.assertRaises(KeyboardInterrupt, msg=marker):
            os.kill(os.getpid(), first)
        with self.assertRaises(KeyboardInterrupt, msg=marker):
            try:
                raise ValueError('unrelated failure')
            finally:
                os.kill(os.getpid(), second)

    def test_collection_cancel_unwind(self):
        marker = '#1194 collection preserves first cancellation and cleanup'
        previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
        with c.lease_signals():
            for first, second in ((signal.SIGINT, signal.SIGTERM), (signal.SIGTERM, signal.SIGINT)):
                self._assert_cancel_unwind(first, second, marker)
        self.assertTrue(all(signal.getsignal(sig) == handler for sig, handler in previous.items()), marker)

    def test_shutdown_cancel_unwind(self):
        marker = '#1194 shutdown preserves first cancellation and cleanup'
        previous = signal.getsignal(signal.SIGTERM)
        try:
            c.common.install_shutdown()
            self._assert_cancel_unwind(signal.SIGTERM, signal.SIGTERM, marker)
        finally:
            signal.signal(signal.SIGTERM, previous)

    def test_capture_cancel_unwind(self):
        marker = '#1194 capture preserves first cancellation and cleanup'
        previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
        for first, second in ((signal.SIGINT, signal.SIGTERM), (signal.SIGTERM, signal.SIGINT)):
            tracer = mock.Mock(pid=123)
            tracer.wait.return_value = -9
            contract = copy.deepcopy(self.contract)
            contract['project'] = str(self.folder)
            reached, ownership, caught = [], set(), None

            def exercise(*_):
                try:
                    os.kill(os.getpid(), first)
                finally:
                    os.kill(os.getpid(), second)
                    reached.append('cleanup-completed')

            output = self.folder / f'unwind-{first}'
            with mock.patch.object(sys, 'platform', 'linux'), \
                    mock.patch.object(w.shutil, 'which', return_value='/strace'), \
                    mock.patch.object(w.subprocess, 'run', return_value=mock.Mock(stdout='--kill-on-exit')), \
                    mock.patch.object(w.subprocess, 'Popen', return_value=tracer):
                try:
                    w.capture(contract, output, {}, exercise=exercise, ownership=ownership)
                except KeyboardInterrupt as error:
                    caught = error
                except Exception as error:
                    self.fail(marker + ': ' + repr(error))
            self.assertTrue(reached == ['cleanup-completed'] and caught is not None
                            and f'signal {first}' in str(caught), marker)
            self.assertTrue(tracer.kill.call_count == 1 and tracer.wait.call_count == 1
                            and not ownership, marker)
            try:
                failure = json.loads((output / 'failure.json').read_text())
            except Exception as error:
                self.fail(marker + ': ' + repr(error))
            self.assertTrue(failure['error'] == repr(caught) and not failure['cleanupPending'], marker)
            self.assertTrue(all(signal.getsignal(sig) == handler for sig, handler in previous.items()), marker)

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

    def test_registry_applicable_derived(self):
        # Registry applicability is an observation from runs.json, not the
        # hardcoded assumption the frozen projection could never match (#1140
        # fix-r1 P2-4): a run key the host never answered is inapplicable.
        home = self.folder / 'home'
        (home / 'projects' / 'proj' / 'chats').mkdir(parents=True)
        (home / 'projects' / 'proj' / 'chats' / 'session-a.jsonl').write_text(
            json.dumps({'type': 'agent_status_message', 'event': 'turn-start'}) + '\n')
        (home / 'projects/proj/.cwd').write_text('proj')
        received = [{'id': 'case-01', 'final': True,
                     'event': {'type': 'agent_status_message', 'event': 'turn-start'}}]
        observation = c.read_observation(home, 'proj', 'session-a', 'run-01', received, 'case-01')
        self.assertTrue(observation['frames'] == [{'type': 'agent_status_message', 'event': 'turn-start'}],
                        '#1140 observed frames are the raw gateway events')
        self.assertTrue(observation['durableStatus']['entries'] == [{'type': 'agent_status_message', 'event': 'turn-start'}],
                        '#1140 durable entries are the transcript status rows')
        self.assertTrue(observation['registry']['applicable'] is False,
                        '#1140 without runs.json the registry surface is inapplicable')
        runs = {'events': {'proj\0session-a\0run-01': [{'seq': 1, 'event': {'type': 'run', 'event': 'start'}}]},
                'records': []}
        (home / 'runs.json').write_text(json.dumps(runs))
        observation = c.read_observation(home, 'proj', 'session-a', 'run-01', received, 'case-01')
        self.assertTrue(observation['registry']['applicable'] is True
                        and observation['registry']['events'] == runs['events']['proj\0session-a\0run-01'],
                        '#1140 a run key the registry answered is applicable')
        runs = {'events': {}, 'records': [{'projectKey': 'proj', 'runId': 'run-01', 'sessionKey': 'session-a',
                                           'state': 'completed', 'result': None}]}
        (home / 'runs.json').write_text(json.dumps(runs))
        observation = c.read_observation(home, 'proj', 'session-a', 'run-01', received, 'case-01')
        self.assertTrue(observation['registry']['applicable'] is True
                        and observation['registry']['record'] == runs['records'][0],
                        '#1140 a recorded run record makes the registry applicable')
        runs['records'][0]['runId'] = 'run-02'
        (home / 'runs.json').write_text(json.dumps(runs))
        observation = c.read_observation(home, 'proj', 'session-a', 'run-01', received, 'case-01')
        self.assertTrue(observation['registry']['applicable'] is False
                        and observation['registry']['record'] == {},
                        '#1140 an unanswered run key stays inapplicable with an empty record')

    def test_case_plan_and_ordering_variants(self):
        # The hardcoded ordering list must stay reconciled with the frozen
        # manifest (#1140 fix-r1 opus#7): a new manifest variant is never
        # silently treated as timeout-first.
        manifest = c.session_manifest()
        variants = {case.split('/')[1] for case in manifest}
        self.assertTrue(set(c.SESSION_ORDERING_VARIANTS) <= variants,
                        '#1140 every ordering variant is a manifest variant')
        for case in manifest:
            plan = c.case_plan(case)
            if case.split('/')[1] in c.SESSION_ORDERING_VARIANTS:
                self.assertTrue(plan == {'timeoutMs': c.SESSION_ORDERING_TIMEOUT_MS, 'delay': 0.0},
                                '#1140 ordering variants finish before the deadline: ' + case)
            else:
                self.assertTrue(plan == {'timeoutMs': c.SESSION_TIMEOUT_MS, 'delay': c.SESSION_PROVIDER_DELAY},
                                '#1140 non-ordering variants are timeout-first: ' + case)

    def test_bind_contract_rejects_preregistered(self):
        # The mirror of the frozen loader's rejection: a contract that already
        # carries a binding, observer rows or a binding source cannot audit
        # (#1140 fix-r1 opus#6).
        _, evidence = compatibility_envelope(self.folder, self.contract, {})
        for key in ('binding', 'observerRows', 'bindingSource'):
            contract = copy.deepcopy(self.contract)
            contract[key] = 'pre-registered'
            with self.assertRaises(w.Incomplete, msg='#1140 a pre-registered ' + key + ' cannot audit'):
                w.bind_contract(contract, evidence)
        bound = w.bind_contract(copy.deepcopy(self.contract), evidence)
        self.assertTrue(bound['bindingSource'] == 'observer-hello'
                        and bound['binding'] == evidence['binding']
                        and bound['observerRows'] == evidence['observer']['rows'],
                        '#1140 the observer hello binds the runtime record')

    def session_fixture(self):
        path = ROOT / 'scripts/gateway-parity/controls/session_domain.py'
        spec = importlib.util.spec_from_file_location('witness_session_controls', path)
        control = importlib.util.module_from_spec(spec)
        exec(compile(path.read_bytes(), str(path), 'exec'), control.__dict__)
        folder = control.native_capture_fixture(self.folder)
        capture = control.session_domain_module._load_native_artifacts(folder)
        return control, folder, capture

    def test_validate_session_covers_manifest(self):
        control, folder, capture = self.session_fixture()
        rows, manifest = capture['rows'], capture['expected']
        requests = control.read(folder / 'session-domain/provider.json')['requests']
        evidence, launch = capture['evidence'], capture['contract']
        c.validate_session(launch, evidence, rows, requests, manifest)
        c.validate_session(launch, evidence, rows, [], manifest)
        for fault in ('requests', 'rows', 'order', 'sidecar-count', 'misallocation',
                      'witness-missing', 'witness-shape', 'marker'):
            bad_rows, bad_requests = copy.deepcopy(rows), copy.deepcopy(requests)
            bad_evidence = copy.deepcopy(evidence)
            if fault == 'requests':
                bad_requests[0]['raw'] = json.dumps({'model': 'mock', 'stream': True, 'messages': []})
            elif fault == 'rows':
                bad_rows.pop()
            elif fault == 'order':
                bad_rows[0], bad_rows[1] = bad_rows[1], bad_rows[0]
            elif fault == 'sidecar-count':
                bad_evidence['processes'].pop()
            elif fault == 'misallocation':
                bad_evidence['transcriptWitness']['opens'][len(manifest) // 2]['afterExecs'] += 1
            elif fault == 'witness-missing':
                bad_evidence.pop('transcriptWitness')
            elif fault == 'witness-shape':
                bad_evidence['transcriptWitness'].pop('opens')
            elif fault == 'marker':
                bad_evidence['transcriptWitness']['opens'][0]['path'] = '/chats/' + c.session_key(manifest[0]) + '.jsonl'
            rejected = False
            try:
                c.validate_session(launch, bad_evidence, bad_rows, bad_requests, manifest)
            except c.w.Incomplete:
                rejected = True
            except Exception as error:
                self.fail('#1140 unexpected session validation exception: ' + repr(error))
            self.assertTrue(rejected, '#1140 session capture rejects ' + fault)

    def test_session_marker_cross_replay(self):
        control, folder, capture = self.session_fixture()
        manifest, launch, healthy = capture['expected'], capture['contract'], capture['evidence']
        raw = (folder / 'capture/trace.log').read_text()
        old = healthy['transcriptPaths'][0]
        # Only opens[0] changes: every other session and exec window survives.
        inputs = {'healthy': old, 'root-chats': '/chats/' + Path(old).name,
                  'nested-key': old[:-6] + '/b.jsonl', 'suffix': old + '.tmp',
                  'decoy-root': '/decoy/chats/' + Path(old).name,
                  'relative': 'home/chats/' + Path(old).name,
                  'unnormalized': str(Path(old).parent) + '/../chats/' + Path(old).name}
        requests = control.read(folder / 'session-domain/provider.json')['requests']
        for label, path in inputs.items():
            evidence = copy.deepcopy(healthy)
            try:
                replay = w.parse_trace(raw.replace(hx(old), hx(path)), launch)
                evidence.update(replay)
            except w.Incomplete:
                self.assertTrue(label == 'relative', '#1140 r8 only relative marker is rejected by parser itself')
                continue
            verdicts = []
            for check in (lambda: c.validate_session(launch, evidence, capture['rows'], requests, manifest),
                          lambda: control.session_domain_module.audit_native(evidence, launch)):
                accepted = True
                try:
                    check()
                except ValueError:
                    accepted = False
                verdicts.append(accepted)
            self.assertTrue(verdicts == [label == 'healthy'] * 2,
                            '#1140 r8 parser collector frozen marker verdict ' + label)
        # The old rsplit accepts this key, the sole frozen extractor rejects it.
        root = '/chats/' + Path(old).name
        self.assertTrue(root.rsplit('/chats/', 1)[-1][:-6] == c.session_key(manifest[0]),
                        '#1140 r8 root marker fixture distinguishes rsplit')
        self.assertTrue(w.native.transcript_key(root) is None,
                        '#1140 r8 shared marker rejects root chats')

    def test_publish_session_domain_receipt(self):
        # The published receipt pins every session-domain artifact, so the
        # frozen loader fails closed on the first altered file (#1140 fix-r1
        # opus#6).
        output = self.folder / 'session'
        (output / 'capture').mkdir(parents=True)
        w.save(output / 'capture/receipt.json', {'status': 'WITNESS_CAPTURED', 'tracerExitCode': 0})
        rows = [{'case': 'no-registry-fast/fast/model', 'observation': None}]
        requests = [{'case': 'no-registry-fast/fast/model', 'path': '/v1/chat/completions'}]
        observer = {'connections': 1, 'rows': []}
        driver = {'binding': {'connectionId': 'witness-session', 'generation': 1}}
        c.publish_session_domain(output, observer, rows, requests, '127.0.0.1:29000', driver)
        receipt = json.loads((output / 'capture/receipt.json').read_text())
        self.assertTrue(receipt['status'] == 'WITNESS_CAPTURED' and receipt['tracerExitCode'] == 0,
                        '#1140 the session receipt extends the capture receipt')
        for name, key in (('observer.json', 'observerSha256'), ('cases.json', 'casesSha256'),
                          ('provider.json', 'providerSha256'), ('driver.json', 'driverSha256')):
            self.assertTrue(receipt[key] == w.sha(output / 'session-domain' / name),
                            '#1140 the session receipt pins ' + name)
        self.assertTrue(json.loads((output / 'session-domain/driver.json').read_text()) == driver,
                        '#1140 publish records the driver hello binding verbatim')
        self.assertFalse((output / 'session-domain.partial').exists(), '#1140 publish leaves no staging directory')
        (output / 'session-domain/observer.json').write_text('{}')
        self.assertTrue(receipt['observerSha256'] != w.sha(output / 'session-domain/observer.json'),
                        '#1140 an altered artifact breaks its pinned hash')
        (output / 'session-domain/driver.json').write_text('{"binding": {}}')
        self.assertTrue(receipt['driverSha256'] != w.sha(output / 'session-domain/driver.json'),
                        '#1140 an altered driver record breaks its pinned hash')
        with self.assertRaises(FileExistsError, msg='#1140 publish refuses an existing session-domain directory'):
            c.publish_session_domain(output, observer, rows, requests, '127.0.0.1:29000', driver)
        # A driver record without a dict binding is refused before anything
        # is staged or published (#1140 fix-r3): the record is the independent
        # anchor, an empty placeholder must never reach the receipt.
        bad = self.folder / 'session-bad-driver'
        (bad / 'capture').mkdir(parents=True)
        w.save(bad / 'capture/receipt.json', {'status': 'WITNESS_CAPTURED', 'tracerExitCode': 0})
        with self.assertRaises(c.w.Incomplete, msg='#1140 publish rejects a driver record without a binding'):
            c.publish_session_domain(bad, observer, rows, requests, '127.0.0.1:29000', {'binding': None})


def real_capture(folder):
    raw, contract, value = w.load_capture(folder)
    # The audit envelope must see the recorder's true row shape (#1140 fix-r1):
    # session-domain captures carry the full observer ledger, smoke captures
    # carry the driver ledger, and only a capture without either falls back to
    # the fabricated envelope hello.
    parent = Path(folder).parent
    rows, source = None, 'envelope'
    for name, document in (('session-domain/observer.json', 'session-domain'),
                           ('driver.json', 'driver')):
        if (parent / name).is_file():
            rows, source = json.loads((parent / name).read_text())['rows'], document
            break
    with tempfile.TemporaryDirectory(prefix='pdrs-1140-real-audit-') as temporary:
        contract, evidence = compatibility_envelope(temporary, contract, value, rows=rows)
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
        print(json.dumps({'scope': 'REAL_WITNESS_AUDIT', 'scoring': 'NOT_RUN', 'rows': source,
                          'controls': verdicts}, indent=2))
        return 0 if all(row['status'] == 'KILLED' for row in verdicts) else 1


if __name__ == '__main__':
    if len(sys.argv) == 5 and sys.argv[1] == '--signal-lease-child':
        signal_lease_child(sys.argv[2], sys.argv[3], int(sys.argv[4]))
        raise SystemExit(0)
    if len(sys.argv) == 3 and sys.argv[1] == '--capture':
        raise SystemExit(real_capture(sys.argv[2]))
    unittest.main(verbosity=2)
