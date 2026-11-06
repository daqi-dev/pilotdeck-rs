# anchor-selfcheck: unsupported expectation-replay server, no mutation table
#!/usr/bin/env python3
"""#1140 fix-r1 end-to-end mock gateway: real sockets, registered expectations.

A real listening WebSocket server that replays the registered rust expectation
of every manifest case: it answers hello, serves each submit_turn by posting
the turn message to the real loopback provider (so the driver's own ledger
records that egress), writes the transcript and runs.json surfaces the
collector reads, then returns the registered event frames followed by the
final response.  It is the measured stand-in for the Rust gateway inside the
offline end-to-end test; no part of it is a witness.  Only the OS layer is
mocked by the test (strace, pidfd, platform); this server and the collector
run for real.
"""
import base64
import hashlib
import json
from pathlib import Path
import re
import socket
import threading
import urllib.request

GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'


def raw(label):
    """The inverse of the frozen projection's label: type plus first detail."""
    kind, _, detail = label.partition(':')
    return {'type': kind, 'event': detail}


def load_expectations(root):
    """The registered rust expectations the replay scores against.

    #1363 (#10): mirror of the collector's structural checks for the three
    shapes that change the case->rust mapping itself (empty table, non-string
    case key, duplicate case keys). The collector's reader is the one on the
    scoring path and validates the full row shape; this replay dereferences
    ``frames``/``registry``/``durableStatus`` directly, so a missing field
    fails loudly inside the serving thread (``self.errors`` -> ``close()``
    False) rather than silently changing the mapping.  Messages keep the
    ``#1140`` prefix because the collector's ``w.need`` prepends the same one.
    """
    document = json.loads(
        (Path(root) / 'scripts/gateway-parity/fixtures/session-expectations.json').read_text(encoding='utf-8'))
    rows = document.get('cases') if isinstance(document, dict) else None
    if not isinstance(rows, list) or not rows:
        raise ValueError('#1140 session expectations is empty or malformed')
    keys = [row.get('case') if isinstance(row, dict) else None for row in rows]
    if not all(isinstance(key, str) for key in keys):
        raise ValueError('#1140 session expectations case key must be a string')
    if len(set(keys)) != len(keys):
        raise ValueError('#1140 session expectations has duplicate cases')
    return {row['case']: row['rust'] for row in rows}


class Gateway:
    """One real-socket gateway connection serving the 72-case manifest."""

    def __init__(self, port, home, provider, expectations, *, after_final=None):
        self.port, self.home = port, Path(home)
        self.provider, self.expectations = provider, expectations
        self.after_final = after_final
        self.errors = []
        self.completed = threading.Event()
        self.served_cases = set()
        self.finished = threading.Event()
        self.stopping = threading.Event()
        self.connected = threading.Event()
        self.connection = None
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            self.listener.bind(('127.0.0.1', port))
            self.listener.listen(1)
            self.listener.settimeout(0.1)
        except BaseException:
            self.listener.close()
            raise
        # runs.json has one writer. The default replay writes before final;
        # after_final supplies a deterministic delayed-write test hook.
        self.runs = {'events': {}, 'records': []}
        self.thread = threading.Thread(target=self._serve, daemon=True)
        try:
            self.thread.start()
        except BaseException:
            self.listener.close()
            raise

    # ---- wire helpers ----------------------------------------------------
    def _read_exact(self, connection, count):
        data = b''
        while len(data) < count:
            chunk = connection.recv(count - len(data))
            if not chunk:
                raise ConnectionError('client closed mid-frame')
            data += chunk
        return data

    def _read_frame(self, connection):
        head = self._read_exact(connection, 2)
        length = head[1] & 127
        if length >= 126:
            length = int.from_bytes(self._read_exact(connection, 2 if length == 126 else 8), 'big')
        mask = self._read_exact(connection, 4)
        payload = self._read_exact(connection, length)
        payload = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        return json.loads(payload)

    def _send_frame(self, connection, value):
        payload = json.dumps(value, separators=(',', ':')).encode()
        header = (bytes([0x81, len(payload)]) if len(payload) < 126
                  else b'\x81\x7e' + len(payload).to_bytes(2, 'big'))
        connection.sendall(header + payload)

    # ---- serving ---------------------------------------------------------
    def _serve(self):
        try:
            while not self.stopping.is_set():
                try:
                    connection, _ = self.listener.accept()
                    break
                except socket.timeout:
                    continue
            else:
                return
            self.connection = connection
            with connection:
                self.connected.set()
                if self.stopping.is_set():
                    return
                connection.settimeout(180)
                request = b''
                while not request.endswith(b'\r\n\r\n'):
                    chunk = connection.recv(4096)
                    if not chunk:
                        raise ConnectionError('client closed before upgrade')
                    request += chunk
                key = request.decode().split('Sec-WebSocket-Key: ', 1)[1].split('\r\n', 1)[0]
                accept = base64.b64encode(
                    hashlib.sha1((key + GUID).encode()).digest())
                connection.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n'
                                   b'Sec-WebSocket-Accept: ' + accept + b'\r\n\r\n')
                hello = self._read_frame(connection)
                if hello.get('type') != 'hello':
                    raise ValueError('unexpected hello: ' + json.dumps(hello))
                self._send_frame(connection, {'type': 'hello_ok', 'protocolVersion': '1.2',
                                              'interactionBinding': {
                                                  'connectionId': 'e2e-connection', 'generation': 1}})
                while True:
                    try:
                        frame = self._read_frame(connection)
                    except ConnectionError:
                        break  # the driver closes the connection after the last turn
                    case = self._turn(connection, frame)
                    self.served_cases.add(case)
                    if self.served_cases == set(self.expectations):
                        self.completed.set()
        except Exception as error:
            if not (self.stopping.is_set() and isinstance(error, (OSError, ConnectionError))):
                self.errors.append(f'{type(error).__name__}: {error}')
        finally:
            if not self.completed.is_set():
                self.errors.append('manifest incomplete: served '
                                   + str(len(self.served_cases)) + '/' + str(len(self.expectations)))
            try:
                self.listener.close()
            except OSError:
                pass
            self.finished.set()

    def _turn(self, connection, request):
        params = request.get('params', {})
        message = params.get('message', '')
        marker = re.search(r'\[native-witness:([^\]]+)\]', message)
        if marker is None:
            raise ValueError('turn without a case marker: ' + json.dumps(request))
        case = marker.group(1)
        if case not in self.expectations:
            raise ValueError('unknown case: ' + case)
        expectation = self.expectations[case]
        # Real provider egress carrying the case marker: the driver's own
        # request ledger records it, exactly like the real gateway's model call.
        body = json.dumps({'model': 'mock',
                           'messages': [{'role': 'user', 'content': message}],
                           'stream': True}).encode()
        post = urllib.request.Request(self.provider + '/v1/chat/completions', data=body,
                                      headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(post, timeout=30) as response:
            response.read()
        # The durable and registry surfaces land before the final frame, so the
        # collector's post-turn read observes them without a race.
        session, run_id = params['sessionKey'], params['runId']
        project = params['projectKey']
        transcript = self.home / 'projects' / 'mock' / 'chats' / f'{session}.jsonl'
        transcript.parent.mkdir(parents=True, exist_ok=True)
        (transcript.parent.parent / '.cwd').write_text(project)
        if self.after_final is not None:
            # Deliberately publish a clean but incomplete state before final.
            transcript.write_text('')
            pending = {'events': {}, 'records': [{'projectKey': project,
                       'sessionKey': session, 'runId': run_id, 'state': 'running'}]}
            temporary = self.home / 'runs.json.partial'
            temporary.write_text(json.dumps(pending))
            temporary.replace(self.home / 'runs.json')
            self._events(connection, request, expectation)
            self.after_final()
        with transcript.open('a') as stream:
            for label in expectation['durableStatus']:
                stream.write(json.dumps({'type': 'agent_status_message', 'event': label}) + '\n')
        registry = expectation['registry']
        if registry.get('applicable'):
            key = f'{project}\0{session}\0{run_id}'
            self.runs['events'][key] = [
                {'seq': int(item.partition(':')[0]), 'event': raw(item.partition(':')[2])}
                for item in registry['events']]
            self.runs['records'].append(
                {'projectKey': project, 'runId': run_id, 'sessionKey': session, 'state': registry['state'],
                 'result': None if registry['result'] is None else raw(registry['result'])})
        document, temporary = self.home / 'runs.json', self.home / 'runs.json.partial'
        temporary.write_text(json.dumps(self.runs))
        temporary.replace(document)
        if self.after_final is None:
            self._events(connection, request, expectation)
        return case

    def _events(self, connection, request, expectation):
        frames = [raw(label) for label in expectation['frames']]
        for seq, event in enumerate(frames):
            self._send_frame(connection, {'type': 'event', 'id': request.get('id'), 'seq': seq,
                                          'final': False, 'event': event})
        # The gateway emits one final:false envelope per iterator yield and then
        # one final:true envelope carrying lastError ?? lastCompleted ?? the
        # empty-stream fallback (GatewayWsConnection.ts:157-169,
        # runtime/connection/mod.rs:386-408).  Deriving it here keeps the mock on
        # the real wire shape instead of inventing a finish frame.
        terminal = (next((event for event in reversed(frames) if event['type'] == 'error'), None)
                    or next((event for event in reversed(frames) if event['type'] == 'turn_completed'), None)
                    or {'type': 'error', 'code': 'result_unknown',
                        'message': 'Gateway stream ended without a terminal result.',
                        'recoverable': True})
        self._send_frame(connection, {'type': 'event', 'id': request.get('id'),
                                      'seq': len(frames), 'final': True, 'event': terminal})

    def close(self, timeout=30):
        self.stopping.set()
        owned_sockets = (self.listener, self.connection)
        for owned in owned_sockets:
            if owned is not None:
                try:
                    owned.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass  # A listener or an already closed peer may reject shutdown.
        # Keep the descriptor alive until recv wakes on shutdown. Closing it
        # first can leave a macOS timed recv polling the old descriptor until
        # its 180s idle deadline; shutdown alone reliably wakes the reader.
        self.thread.join(timeout)
        for owned in owned_sockets:
            if owned is not None:
                owned.close()
        return self.completed.is_set() and not self.thread.is_alive() and not self.errors
