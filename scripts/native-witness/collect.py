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


def native_run(gateway, sidecar, output, port=28999, session_domain=False):
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
        result = _native_run(gateway, sidecar, output, port, session_domain=session_domain)
    print(json.dumps({'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN',
                      'sessionDomain': bool(session_domain), 'output': str(Path(output).resolve())}))
    return result


def validate_smoke(contract, evidence, requests, home):
    w.need(any(p['role'] == 'sidecar' for p in evidence['processes']), 'real sidecar exec missing')
    w.need(bool(evidence['networkWitness']['calls']) and bool(requests), 'real provider egress missing')
    w.need(all(c['target'] in contract['externalEndpoints'] for c in evidence['networkWitness']['calls']), 'unexpected native egress')
    w.need(bool(list(home.rglob('*.jsonl'))), 'durable native output missing')


# #1140 session-domain collection budget: 72 cases through one gateway, each
# with a settle pause and (for the timeout-first variants) a delayed provider
# response, must fit one capture window far below the tracer's own limits.
SESSION_LIMIT_SECONDS = 900
SESSION_TIMEOUT_MS = 400
SESSION_ORDERING_TIMEOUT_MS = 30000
SESSION_PROVIDER_DELAY = 1.5
SESSION_SETTLE_SECONDS = 0.5
SESSION_ORDERING_VARIANTS = ('known-final-first', 'unknown-first', 'after-complete',
                              'known-final-before-timeout-pending')


def session_manifest():
    """The frozen 72-case E1 manifest the native capture must declare in full."""
    path = Path(__file__).resolve().parents[2] / 'scripts/gateway-parity/fixtures/session-calibration.json'
    with open(path, encoding='utf-8') as stream:
        cases = json.load(stream)['cases']
    w.need(bool(cases) and len(set(cases)) == len(cases), 'session manifest is empty or has duplicates')
    return cases


def case_plan(case):
    """Timeout/provider plan mirroring the preregistered variant topology.

    Ordering variants let the turn finish before the deadline; every other
    variant is timeout-first, so the provider answers after the gateway's
    timeout has fired. The native host cannot reproduce the TS harness's
    controlled slow/failed durable writes; observed mismatches with the
    registered rust expectations are honest FAILs, never wiring failures.
    """
    variant = case.split('/')[1]
    if variant in SESSION_ORDERING_VARIANTS:
        return {'timeoutMs': SESSION_ORDERING_TIMEOUT_MS, 'delay': 0.0}
    return {'timeoutMs': SESSION_TIMEOUT_MS, 'delay': SESSION_PROVIDER_DELAY}


def read_observation(home, project, session, run_id, received):
    """The three observed surfaces of one native turn (the TS observation shape).

    frames are the raw gateway events the driver received; durableStatus
    entries are the transcript's agent_status_message rows; registry events and
    record come from runs.json under the turn's run key. An absent record is
    recorded as an empty record: an observed absence scores as a per-case
    mismatch, never as a structural failure.
    """
    frames = [frame['event'] for frame in received if isinstance(frame.get('event'), dict)]
    matches = sorted((home / 'projects').glob(f'*/chats/{session}.jsonl'))
    w.need(len(matches) == 1, 'expected exactly one transcript for session ' + session)
    entries = [json.loads(line) for line in matches[0].read_text().splitlines() if line.strip()]
    durable = [entry for entry in entries if entry.get('type') == 'agent_status_message']
    registry = {'applicable': True, 'events': [], 'record': {}}
    document = home / 'runs.json'
    if document.is_file():
        runs = json.loads(document.read_text())
        key = f'{project}\0{session}\0{run_id}'
        registry['events'] = runs.get('events', {}).get(key, [])
        registry['record'] = next((record for record in runs.get('records', [])
                                   if record.get('runId') == run_id and record.get('sessionKey') == session), {})
    return {'frames': frames,
            'durableStatus': {'entries': durable},
            'registry': registry}


def publish_session_domain(output, observer, rows, requests, endpoint):
    """Atomically publish the session-domain artifacts, then extend the receipt.

    Every artifact lands through a staging directory and os.replace; the
    receipt is rewritten last, so a partial publish can never score: the
    #1140 frozen loader fails closed on the first missing or altered hash.
    """
    folder, staging = output / 'session-domain', output / 'session-domain.partial'
    staging.mkdir(exist_ok=False)
    w.save(staging / 'observer.json', observer)
    w.save(staging / 'cases.json', {'cases': rows})
    w.save(staging / 'provider.json', {'endpoint': endpoint, 'requests': requests})
    folder.mkdir(exist_ok=False)
    for name in ('observer.json', 'cases.json', 'provider.json'):
        os.replace(staging / name, folder / name)
    staging.rmdir()
    receipt = json.loads((output / 'capture/receipt.json').read_text())
    receipt.update(observerSha256=w.sha(folder / 'observer.json'),
                   casesSha256=w.sha(folder / 'cases.json'),
                   providerSha256=w.sha(folder / 'provider.json'))
    temporary = output / 'capture/receipt.partial.json'
    w.save(temporary, receipt)
    os.replace(temporary, output / 'capture/receipt.json')


def validate_session(launch, evidence, rows, requests, cases):
    """Pre-receipt invariants specific to a session-domain capture.

    The frozen audit already rejects a missing sidecar role or an undeclared
    egress target; here the driver ledger must show real provider traffic and
    the observed rows must cover the frozen manifest exactly. Each row's
    transcript was already required to exist while the turn was driven.
    """
    w.need(bool(requests), 'session capture saw no provider traffic')
    w.need([row['case'] for row in rows] == cases,
           'session-domain rows must cover the manifest exactly; no denominator shrink')


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


def _native_run(gateway, sidecar, output, port, session_domain=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    home, project = output / 'home', output / 'project'
    home.mkdir()
    project.mkdir()
    requests = []
    cases = session_manifest() if session_domain else None
    rows, observer_rows = [], []

    class Provider(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            length = int(self.headers.get('Content-Length', '0'))
            w.need(0 < length <= 1024 * 1024, 'provider request size')
            raw = self.rfile.read(length)
            body = json.loads(raw)
            # The driver's own request ledger: keyed by the case marker the
            # submitted message carries, recorded before any delay, so a
            # request the gateway abandoned after a timeout is still ledged.
            marker = re.search(r'\[native-witness:([^\]]+)\]'.encode(), raw)
            case = marker.group(1).decode() if marker else None
            requests.append({'case': case, 'path': self.path})
            if case is not None:
                time.sleep(case_plan(case)['delay'])
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
                    'externalEndpoints': [f'127.0.0.1:{port + 1}'],
                    'cases': cases if session_domain else ['native-witness-smoke'],
                    'capabilities': {'provider': 'loopback-mock', 'tools': []}}

        def exercise(tracer, folder, deadline):
            wait_ready(tracer, folder, port, deadline)
            client = WireClient(port, deadline)
            try:
                client.send({'type': 'hello', 'protocolVersion': '1.2', 'clientName': 'ui',
                             'clientVersion': 'native-witness', 'token': 'native-witness-fixture'})
                hello = client.receive()
                w.need(hello.get('type') == 'hello_ok', 'native hello failed')
                if session_domain:
                    # One connection, one hello, 72 sequential turns: every
                    # case gets its own session and run, so per-case surfaces
                    # never alias. Timeout-first variants answer after the
                    # gateway's timeout has fired; ordering variants complete.
                    for index, case in enumerate(cases, start=1):
                        session_key = case.replace('/', '.')
                        run_id = f'run-{index:02d}'
                        request_id = f'case-{index:02d}'
                        client.send({'type': 'request', 'id': request_id, 'method': 'submit_turn', 'params': {
                            'projectKey': str(project), 'sessionKey': session_key, 'runId': run_id,
                            'channelKey': 'session-domain', 'message': f'[native-witness:{case}] controlled wait',
                            'allowedTools': [], 'timeoutMs': case_plan(case)['timeoutMs']}})
                        received = []
                        while True:
                            frame = client.receive()
                            received.append(frame)
                            if frame.get('id') == request_id and frame.get('final'):
                                break
                        # Durable writes settle after the final frame; the
                        # three surfaces are observed, never raced.
                        time.sleep(SESSION_SETTLE_SECONDS)
                        rows.append({'case': case,
                                     'observation': read_observation(home, str(project), session_key,
                                                                     run_id, received)})
                else:
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
            observer_rows.extend(client.rows)

        def validate(launch, evidence):
            if session_domain:
                validate_session(launch, evidence, rows, requests, cases)
            else:
                validate_smoke(launch, evidence, requests, home)
            # Complete owned provider cleanup before capture publishes success.
            stop_provider()

        launch, evidence = w.capture(contract, output / 'capture',
                                      {'HOME': str(home), 'PILOT_HOME': str(home),
                                       'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'},
                                      timeout=SESSION_LIMIT_SECONDS if session_domain else w.LIMIT_SECONDS,
                                      limit=SESSION_LIMIT_SECONDS,
                                      exercise=exercise, validate=validate)
        if session_domain:
            publish_session_domain(output, {'connections': 1, 'rows': observer_rows},
                                   rows, requests, f'127.0.0.1:{port + 1}')
        return launch, evidence
    finally:
        stop_provider()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gateway', required=True)
    parser.add_argument('--sidecar', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--port', type=int, default=28999)
    parser.add_argument('--session-domain', action='store_true',
                        help='#1140 drive the frozen 72-case E1 manifest through one gateway '
                             'and publish observer rows plus per-case three-surface observations '
                             '(session-domain/) for the deadline scoring suite')
    args = parser.parse_args()
    try:
        native_run(args.gateway, args.sidecar, args.output, args.port, session_domain=args.session_domain)
    except Blocked as error:
        print(json.dumps({'status': 'BLOCKED', 'scoring': 'NOT_RUN', 'error': str(error)}))
        return 2
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
