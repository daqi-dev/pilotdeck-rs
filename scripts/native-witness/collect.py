#!/usr/bin/env python3
"""Bounded real native gateway probe, loopback mock only; no A7 score publication."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import argparse
import base64
from contextlib import ExitStack, contextmanager
import errno
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import socket
import threading
import time

spec = importlib.util.spec_from_file_location('native_witness', Path(__file__).with_name('witness.py'))
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)
# Explicit source load preserves the frozen auditor's isolated sys.path and
# records executed bytes for its existing provenance guard.
common_path = Path(__file__).resolve().parents[1] / 'gateway-parity/common.py'
spec = importlib.util.spec_from_file_location('native_witness_common', common_path)
common = importlib.util.module_from_spec(spec)
common._apparatus_source = common_path.read_bytes()
exec(compile(common._apparatus_source, str(common_path), 'exec'), common.__dict__)
sys.modules[spec.name] = common
parity_lock = common.parity_lock


class Blocked(w.Incomplete):
    """A shared lease or fixed port is unavailable; no fallback endpoint."""


@contextmanager
def lease_signals():
    """Install before acquiring the lease; restore only after its cleanup."""
    def cancel(signum, _frame):
        raise KeyboardInterrupt(f'#1140 collection cancelled by signal {signum}')
    previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        for sig in previous:
            signal.signal(sig, cancel)
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


@contextmanager
def fixed_port_bind():
    """Only a witness listener bind can establish fixed-port contention."""
    try:
        yield
    except OSError as error:
        if error.errno == errno.EADDRINUSE:
            raise Blocked('#1140 BLOCKED: fixed witness port occupied') from error
        raise


def probe_bind(port, *, reuse_address):
    # Tokio's gateway bind does not reuse addresses; HTTPServer does.
    # Probe with the respective policy, including during startup rechecks.
    with socket.socket() as probe:
        if reuse_address:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        with fixed_port_bind():
            probe.bind(('127.0.0.1', port))


class BoundedHTTPServer(http.server.HTTPServer):
    shutdown_timeout = 6

    def server_bind(self):
        with fixed_port_bind():
            super().server_bind()

    def get_request(self):
        connection, address = super().get_request()
        # Covers request line and headers, before BaseHTTPRequestHandler runs.
        connection.settimeout(5)
        return connection, address

    def shutdown(self):
        # HTTPServer.shutdown itself waits without a deadline. A slow request
        # must leave cleanupPending instead of trapping cancellation forever.
        stopper = threading.Thread(target=super().shutdown, daemon=True)
        stopper.start()
        stopper.join(timeout=self.shutdown_timeout)
        w.need(not stopper.is_alive(), 'provider shutdown deadline')


class WireClient:
    def __init__(self, port, deadline):
        self.deadline = deadline
        self.socket = socket.create_connection(('127.0.0.1', port), timeout=self.remaining())
        self.buffer = b''
        self.rows = []
        key = base64.b64encode(os.urandom(16)).decode()
        request = (f'GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n'
                   f'Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n')
        self.socket.sendall(request.encode())
        header = b''
        while not header.endswith(b'\r\n\r\n'):
            header += self.read(1)
            w.need(len(header) < 16384, 'oversized HTTP upgrade')
        accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
        w.need(header.startswith(b'HTTP/1.1 101 ') and accept.lower() in header.lower(), 'websocket upgrade rejected')
        self.upgrade = header.decode()

    def remaining(self):
        left = self.deadline - time.monotonic()
        w.need(left > 0, 'native driver deadline')
        return left

    def read(self, count):
        while len(self.buffer) < count:
            self.socket.settimeout(self.remaining())
            data = self.socket.recv(max(4096, count - len(self.buffer)))
            w.need(data, 'websocket closed before final')
            self.buffer += data
        value, self.buffer = self.buffer[:count], self.buffer[count:]
        return value

    def send(self, value):
        payload = json.dumps(value, separators=(',', ':')).encode()
        size = len(payload)
        mask = os.urandom(4)
        if size < 126:
            header = bytes([0x81, 0x80 | size])
        elif size <= 65535:
            header = b'\x81\xfe' + size.to_bytes(2, 'big')
        else:
            header = b'\x81\xff' + size.to_bytes(8, 'big')
        frame = header + mask + bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        self.socket.settimeout(self.remaining())
        self.socket.sendall(frame)
        self.rows.append({'direction': 'send', 'wireHex': frame.hex(), 'frame': value, 'connection': 1})

    def receive(self):
        head = self.read(2)
        w.need(head[0] == 0x81 and head[1] & 0x80 == 0, 'unexpected/fragmented WS frame')
        length = head[1] & 127
        extra = self.read(2 if length == 126 else 8) if length >= 126 else b''
        length = int.from_bytes(extra, 'big') if extra else length
        w.need(length <= 4 * 1024 * 1024, 'oversized WS frame')
        payload = self.read(length)
        value = json.loads(payload)
        self.rows.append({'direction': 'receive', 'wireHex': (head + extra + payload).hex(),
                          'frame': value, 'connection': 1})
        return value


def native_run(gateway, sidecar, output, port=28999):
    w.need(sys.platform == 'linux', 'Linux only; macOS eslogger + root deferred')
    w.need(28999 <= port <= 29008, 'gateway port must be in 28999 series')
    with lease_signals(), ExitStack() as lease:
        try:
            lease.enter_context(parity_lock(timeout=1))
        except TimeoutError as error:
            raise Blocked('#1140 BLOCKED: parity lease unavailable') from error
        # Check both fixed endpoints before launching any owned process.
        # The shared lease spans the bind handoff and provider cleanup.
        for candidate, reuse_address in ((port, False), (port + 1, True)):
            probe_bind(candidate, reuse_address=reuse_address)
        result = _native_run(gateway, sidecar, output, port)
    print(json.dumps({'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN', 'output': str(Path(output).resolve())}))
    return result


def validate_smoke(contract, evidence, requests, home):
    w.need(any(p['role'] == 'sidecar' for p in evidence['processes']), 'real sidecar exec missing')
    w.need(bool(evidence['networkWitness']['calls']) and bool(requests), 'real provider egress missing')
    w.need(all(c['target'] in contract['externalEndpoints'] for c in evidence['networkWitness']['calls']), 'unexpected native egress')
    w.need(bool(list(home.rglob('*.jsonl'))), 'durable native output missing')


def wait_ready(tracer, folder, port, deadline):
    while True:
        if tracer.poll() is not None:
            # A non-participant may claim the gateway port after preflight.
            # Probe the same endpoint; only its bind maps EADDRINUSE to BLOCKED.
            probe_bind(port, reuse_address=False)
            w.need(False, 'native gateway exited before readiness')
        stderr = (folder / 'stderr.log').read_text()
        if f'native listening 127.0.0.1:{port}\n' in stderr:
            return
        w.need(time.monotonic() < deadline, 'readiness deadline')
        time.sleep(0.01)


def _native_run(gateway, sidecar, output, port):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    home, project = output / 'home', output / 'project'
    home.mkdir()
    project.mkdir()
    requests = []

    class Provider(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            length = int(self.headers.get('Content-Length', '0'))
            w.need(0 < length <= 1024 * 1024, 'provider request size')
            body = json.loads(self.rfile.read(length))
            requests.append({'path': self.path, 'body': body})
            if body.get('stream'):
                data = ''.join('data: ' + json.dumps({'id': 'witness', 'choices': [{'index': 0, 'delta': delta,
                                        'finish_reason': finish}]}) + '\n\n'
                               for delta, finish in [({'role': 'assistant', 'content': 'native witness text'}, None), ({}, 'stop')]) + 'data: [DONE]\n\n'
                mime = 'text/event-stream'
            else:
                data = json.dumps({'id': 'title', 'choices': [{'message': {'role': 'assistant',
                        'content': '{"title":"Witness"}'}, 'finish_reason': 'stop'}]})
                mime = 'application/json'
            encoded = data.encode()
            self.send_response(200)
            self.send_header('Content-Type', mime)
            self.send_header('Content-Length', str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

    server = worker = None
    started = stopped = False
    def stop_provider():
        nonlocal stopped
        # The provider thread inherits the blocked mask below; no other thread
        # can deliver a Python cancellation while the main thread cleans it up.
        with common.deferred_signals():
            if not stopped and server is not None:
                if started:
                    server.shutdown()
                server.server_close()
                if started:
                    worker.join(timeout=6)
                    w.need(not worker.is_alive(), 'provider did not stop')
                stopped = True
                common.LIVE_GROUPS.discard('native-witness-provider')
    try:
        # Publish ownership before cancellation can escape. Only this short
        # window is masked; capture/Popen inherits the original main-thread mask.
        with common.deferred_signals():
            server = BoundedHTTPServer(('127.0.0.1', port + 1), Provider)
            common.LIVE_GROUPS.add('native-witness-provider')
            worker = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.05}, daemon=True)
            worker.start()
            started = True
        (home / 'pilotdeck.yaml').write_text(
            'schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n'
            f'      protocol: openai\n      url: http://127.0.0.1:{port + 1}/v1\n'
            '      apiKey: local-fixture\n      models:\n        mock: {}\n')
        (home / 'token').write_text('native-witness-fixture\n')
        gateway, sidecar = str(Path(gateway).resolve(strict=True)), str(Path(sidecar).resolve(strict=True))
        argv = [gateway, '--native', '--home', str(home), '--project', str(project), '--listen',
                f'127.0.0.1:{port}', '--token-file', str(home / 'token'), '--sidecar', sidecar, '--drain-ms', '1500']
        contract = {'host': 'rust', 'project': str(project), 'processRoles': ['host', 'sidecar'],
                    'binaries': {'host': w.identity(gateway, argv), 'sidecar': w.identity(sidecar, [sidecar])},
                    'externalEndpoints': [f'127.0.0.1:{port + 1}'], 'cases': ['native-witness-smoke'],
                    'capabilities': {'provider': 'loopback-mock', 'tools': []}}

        def exercise(tracer, folder, deadline):
            wait_ready(tracer, folder, port, deadline)
            client = WireClient(port, deadline)
            try:
                client.send({'type': 'hello', 'protocolVersion': '1.2', 'clientName': 'ui',
                             'clientVersion': 'native-witness', 'token': 'native-witness-fixture'})
                hello = client.receive()
                w.need(hello.get('type') == 'hello_ok', 'native hello failed')
                client.send({'type': 'request', 'id': 'witness', 'method': 'submit_turn', 'params': {
                    'projectKey': str(project), 'sessionKey': 'witness-session', 'runId': 'witness-run',
                    'channelKey': 'probe', 'message': 'hello native witness', 'allowedTools': []}})
                frames = []
                while True:
                    frame = client.receive()
                    frames.append(frame)
                    w.need(frame.get('event', {}).get('type') != 'error', 'native submit reported error')
                    if frame.get('id') == 'witness' and frame.get('final'):
                        w.need(frame.get('event', {}).get('finishReason') == 'completed', 'native turn incomplete')
                        break
                w.need(any(f.get('event', {}).get('text') == 'native witness text' for f in frames), 'provider text absent')
                w.save(output / 'driver.json', {'binding': hello['interactionBinding'], 'rows': client.rows,
                                               'upgrade': client.upgrade, 'requests': requests})
            finally:
                client.socket.close()
            root_exec = re.search(r'^(\d+)\s+execve\(', w.read_trace(folder / 'trace.log'), re.M)
            w.need(root_exec is not None, 'host exec is absent from trace')
            w.stop_host(tracer, int(root_exec[1]))

        def validate(launch, evidence):
            validate_smoke(launch, evidence, requests, home)
            # Complete owned provider cleanup before capture publishes success.
            stop_provider()

        return w.capture(contract, output / 'capture', {'HOME': str(home), 'PILOT_HOME': str(home),
                         'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'}, exercise=exercise, validate=validate)
    finally:
        stop_provider()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gateway', required=True)
    parser.add_argument('--sidecar', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--port', type=int, default=28999)
    args = parser.parse_args()
    try:
        native_run(args.gateway, args.sidecar, args.output, args.port)
    except Blocked as error:
        print(json.dumps({'status': 'BLOCKED', 'scoring': 'NOT_RUN', 'error': str(error)}))
        return 2
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
