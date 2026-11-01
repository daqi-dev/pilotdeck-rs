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
    """The registered rust expectations the replay scores against."""
    document = json.loads(
        (Path(root) / 'scripts/gateway-parity/fixtures/session-expectations.json').read_text())
    return {row['case']: row['rust'] for row in document['cases']}


class Gateway:
    """One real-socket gateway connection serving the 72-case manifest."""

    def __init__(self, port, home, provider, expectations):
        self.port, self.home = port, Path(home)
        self.provider, self.expectations = provider, expectations
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
        # runs.json is written by this thread only, always before the turn's
        # final frame: the collector observes it after the turn settles.
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
        for seq, label in enumerate(expectation['frames']):
            self._send_frame(connection, {'type': 'event', 'id': request.get('id'), 'seq': seq,
                                          'final': False, 'event': raw(label)})
        # The final response frame carries a prefix-type event: the frozen
        # projection filters those, like the real gateway's finish frame.
        self._send_frame(connection, {'type': 'event', 'id': request.get('id'),
                                      'seq': len(expectation['frames']), 'final': True,
                                      'event': {'type': 'turn_started', 'event': 'completed'}})
        return case

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
