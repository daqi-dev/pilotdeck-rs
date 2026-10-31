#!/usr/bin/env python3
"""#983 macOS RSS probe: actual CLI, streaming local mock, no large capture copies.

Usage: PDRS_BASELINE=... python3 measure.py NEW_OUTPUT rust|ts 32|64|128
CARGO_TARGET_DIR is required. Rust is built and snapshotted under the tree lock.
"""
import base64
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import struct
import subprocess
import sys
import threading
import time


ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / 'src/pilotdeck-gateway/tests/fixtures'))
from negative_process import build_snapshot


def rust_binary(out):
    return build_snapshot(ROOT, out / 'binary', package='pilotdeck-cli',
                          target='pilotdeck', kind='bin', release=False)


def exact(sock, size):
    data = bytearray()
    while len(data) < size:
        chunk = sock.recv(size - len(data))
        if not chunk:
            raise EOFError('peer closed')
        data.extend(chunk)
    return bytes(data)


def receive(sock):
    first, second = exact(sock, 2)
    size = second & 127
    if size == 126:
        size = struct.unpack('!H', exact(sock, 2))[0]
    elif size == 127:
        size = struct.unpack('!Q', exact(sock, 8))[0]
    if size > 65536:
        raise ValueError('#983 unexpected large client frame')
    mask = exact(sock, 4) if second & 128 else b'\0' * 4
    data = exact(sock, size)
    return first & 15, bytes(b ^ mask[i % 4] for i, b in enumerate(data))


def header(size, opcode=0x81):
    if size < 126:
        return bytes([opcode, size])
    if size < 65536:
        return bytes([opcode, 126]) + struct.pack('!H', size)
    return bytes([opcode, 127]) + struct.pack('!Q', size)


def main():
    if sys.argv[1] == '--exec':
        path = Path(sys.argv[2])
        pid = os.fork()
        if pid == 0:
            temporary = path.with_suffix('.tmp')
            temporary.write_text(str(os.getpid()))
            temporary.replace(path)
            os.execvpe(sys.argv[3], sys.argv[3:], os.environ)
        _, status, usage = os.wait4(pid, 0)
        temporary = path.with_suffix('.usage.tmp')
        code = os.waitstatus_to_exitcode(status)
        temporary.write_text(json.dumps({'exit': code, 'rssWait4Bytes': usage.ru_maxrss}))
        temporary.replace(path.with_suffix('.usage.json'))
        return code if code >= 0 else 128 - code
    out = Path(sys.argv[1]).resolve()
    implementation, mib = sys.argv[2], int(sys.argv[3])
    assert implementation in ('rust', 'ts') and mib in (32, 64, 128), '#983 probe arguments'
    out.mkdir(parents=True, exist_ok=False)
    baseline = Path(os.environ['PDRS_BASELINE']).resolve()
    node = os.environ.get('NODE22', '/opt/homebrew/bin/node')
    version = subprocess.check_output([node, '--version'], text=True)
    assert version == 'v22.22.0\n', '#983 actual Node v22.22.0 required'
    # Prove kernel rejection before starting any CLI or mock. A pre-existing
    # user service on 18789 is never contacted by an unconfined probe.
    profile = out / 'network.sb'
    profile.write_text('(version 1)\n(allow default)\n(deny network*)\n'
                       '(allow network-outbound (remote ip "localhost:28999"))\n')
    isolation = subprocess.run([
        '/usr/bin/sandbox-exec', '-f', str(profile), sys.executable, '-c',
        'import socket,errno; s=socket.socket(); s.settimeout(1); '
        'code=s.connect_ex(("127.0.0.1",18789)); print(code); '
        'raise SystemExit(0 if code in (errno.EPERM,errno.EACCES) else 1)'],
        capture_output=True, timeout=10)
    (out / 'isolation.stdout').write_bytes(isolation.stdout)
    (out / 'isolation.stderr').write_bytes(isolation.stderr)
    if isolation.returncode:
        raise RuntimeError('#983 kernel isolation unavailable; no unconfined fallback')
    binary = rust_binary(out) if implementation == 'rust' else None
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(('127.0.0.1', 28999))
    listener.listen(1)
    listener.settimeout(120)
    env = {'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C', 'TZ': 'UTC',
           'NO_COLOR': '1', 'TERM': 'dumb', 'ANALYTICS_ENABLED': '0',
           'DOCKER_CONTAINER': '1', 'PILOTDECK_GATEWAY_PORT': '28999',
           'PILOTDECK_GATEWAY_HOST': '127.0.0.1',
           'PILOTDECK_SDK_GATEWAY_URL': 'ws://127.0.0.1:28999/ws',
           'PILOTDECK_GATEWAY_URL': 'ws://127.0.0.1:28999/ws',
           'PILOTDECK_SDK_AUTH_TOKEN': 'd1-fixture-not-a-secret'}
    for key in ('HOME', 'PILOT_HOME', 'TMPDIR', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME'):
        directory = out / key.lower()
        directory.mkdir()
        env[key] = str(directory)
    project = out / 'project'
    project.mkdir()
    Path(env['PILOT_HOME'], 'pilotdeck.yaml').write_text('schemaVersion: 1\n')
    command = ([binary] if implementation == 'rust'
               else [node, '--import', str(baseline / 'node_modules/tsx/dist/loader.mjs'),
                     str(baseline / 'src/cli/pilotdeck.ts')]) + ['sessions', 'list']
    events = []

    def serve():
        try:
            with listener.accept()[0] as sock:
                sock.settimeout(120)
                upgrade = bytearray()
                while not upgrade.endswith(b'\r\n\r\n'):
                    upgrade.extend(exact(sock, 1))
                    assert len(upgrade) < 16384, '#983 bounded upgrade'
                key = re.search(rb'(?im)^sec-websocket-key: *([^\r]+)', upgrade).group(1)
                accept = base64.b64encode(hashlib.sha1(key + b'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest())
                sock.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ' + accept + b'\r\n\r\n')
                hello = json.loads(receive(sock)[1])
                events.append({'hello': hello})
                raw = b'{"type":"hello_ok","protocolVersion":"1.2","serverVersion":"test","serverInfo":{}}'
                sock.sendall(header(len(raw)) + raw)
                request = json.loads(receive(sock)[1])
                assert request['method'] == 'list_sessions', '#983 real CLI list request'
                events.append({'request': request})
                prefix = ('{"type":"response","id":' + json.dumps(request['id']) + ',"ok":true,"result":{"sessions":[').encode()
                size = mib * 1024 * 1024
                count = (size - len(prefix) - 3 + 1) // 2
                padding = size - len(prefix) - (2 * count - 1) - 3
                events.append({'frameBytes': size, 'elements': count, 'padding': padding})
                sock.sendall(header(size) + prefix)
                pairs = count - 1
                chunk = b'0,' * (512 * 1024)
                while pairs:
                    n = min(pairs, len(chunk) // 2)
                    sock.sendall(chunk[:2 * n])
                    pairs -= n
                sock.sendall(b'0]}}' + b' ' * padding)
                events.append({'sent': size})
                while True:
                    opcode, data = receive(sock)
                    if opcode == 8:
                        events.append({'close': data.hex()})
                        sock.sendall(header(len(data), 0x88) + data)
                        break
        except Exception as error:
            events.append({'serverEnd': repr(error)})
        finally:
            listener.close()

    server = threading.Thread(target=serve, daemon=True)
    server.start()
    pid_file = out / 'client.pid'
    wrapped = ['/usr/bin/time', '-l', sys.executable, str(Path(__file__).resolve()),
               '--exec', str(pid_file), '/usr/bin/sandbox-exec', '-f', str(profile), *command]
    (out / 'command.json').write_text(json.dumps({'argv': wrapped, 'env': env, 'node': version}, indent=2) + '\n')
    digest = hashlib.sha256()
    length = 0
    with (out / 'stderr.txt').open('xb') as stderr:
        child = subprocess.Popen(wrapped, env=env, cwd=project, stdout=subprocess.PIPE, stderr=stderr, start_new_session=True)

        def drain():
            nonlocal length
            for chunk in iter(lambda: child.stdout.read(65536), b''):
                length += len(chunk)
                digest.update(chunk)

        reader = threading.Thread(target=drain)
        reader.start()
        peak = 0
        stopped = None
        started = time.monotonic()
        try:
            libproc = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
            while child.poll() is None:
                if time.monotonic() - started > 120:
                    stopped = '120 s deadline'
                client_pid = None
                if pid_file.exists():
                    client_pid = int(pid_file.read_text())
                    usage = ctypes.create_string_buffer(1024)
                    rc = libproc.proc_pid_rusage(client_pid, 0, ctypes.byref(usage))
                    if rc == 0:
                        peak = max(peak, ctypes.c_uint64.from_buffer(usage, 64).value)
                    elif ctypes.get_errno() != errno.ESRCH:
                        stopped = f'RSS monitor failed: errno={ctypes.get_errno()}'
                    if peak > 6 * 1024**3:
                        stopped = 'RSS >6 GiB'
                if stopped:
                    if client_pid is None:
                        os.killpg(child.pid, signal.SIGKILL)
                    else:
                        try:
                            os.kill(client_pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                    break
                time.sleep(0.02)
            child.wait(timeout=10)
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=10)
            reader.join(timeout=10)
            assert not reader.is_alive(), '#983 stdout reader reaped'
    server.join(timeout=2)
    raw = (out / 'stderr.txt').read_text()
    rss = re.search(r'(\d+)\s+maximum resident set size', raw)
    usage = json.loads(pid_file.with_suffix('.usage.json').read_text())
    result = {'implementation': implementation, 'mib': mib, 'exit': usage['exit'],
              'rssWait4Bytes': usage['rssWait4Bytes'], 'timeExit': child.returncode,
              'rssTimeBytes': int(rss[1]) if rss else None, 'rssSampledBytes': peak,
              'stopped': stopped, 'stdoutBytes': length, 'stdoutSha256': digest.hexdigest(),
              'serverJoined': not server.is_alive(), 'events': events}
    (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result), flush=True)
    return int(stopped is not None or not any('sent' in e for e in events))


if __name__ == '__main__':
    sys.exit(main())
