#!/usr/bin/env python3
"""Bounded real native gateway probe, loopback mock only; no A7 score publication."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import argparse
import base64
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import re
import socket
import threading
import time

spec = importlib.util.spec_from_file_location('native_witness', Path(__file__).with_name('witness.py'))
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)


class BoundedHTTPServer(http.server.HTTPServer):
    def get_request(self):
        connection, address = super().get_request()
        # Covers request line and headers, before BaseHTTPRequestHandler runs.
        connection.settimeout(5)
        return connection, address


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
        header = bytes([0x81, 0x80 | size]) if size < 126 else b'\x81\xfe' + size.to_bytes(2, 'big')
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

    server = BoundedHTTPServer(('127.0.0.1', port + 1), Provider)
    worker = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.05}, daemon=True)
    worker.start()
    try:
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
            while True:
                w.need(tracer.poll() is None, 'native gateway exited before readiness')
                stderr = (folder / 'stderr.log').read_text()
                if f'native listening 127.0.0.1:{port}\n' in stderr:
                    break
                w.need(time.monotonic() < deadline, 'readiness deadline')
                time.sleep(0.01)
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
            root_exec = re.search(r'^(\d+)\s+execve\(', (folder / 'trace.log').read_text(), re.M)
            w.need(root_exec is not None, 'host exec is absent from trace')
            w.stop_host(tracer, int(root_exec[1]))

        launch, evidence = w.capture(contract, output / 'capture', {'HOME': str(home), 'PILOT_HOME': str(home),
                                      'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'}, exercise=exercise)
        w.need(any(p['role'] == 'sidecar' for p in evidence['processes']), 'real sidecar exec missing')
        w.need(bool(evidence['networkWitness']['calls']) and bool(requests), 'real provider egress missing')
        w.need(all(c['target'] in contract['externalEndpoints'] for c in evidence['networkWitness']['calls']), 'unexpected native egress')
        w.need(bool(list(home.rglob('*.jsonl'))), 'durable native output missing')
        print(json.dumps({'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN', 'output': str(output)}))
        return launch, evidence
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=6)
        w.need(not worker.is_alive(), 'provider did not stop')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gateway', required=True)
    parser.add_argument('--sidecar', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--port', type=int, default=28999)
    args = parser.parse_args()
    native_run(args.gateway, args.sidecar, args.output, args.port)


if __name__ == '__main__':
    main()
