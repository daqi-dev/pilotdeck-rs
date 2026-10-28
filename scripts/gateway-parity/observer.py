#!/usr/bin/env python3
"""Independent transparent TCP observation point. Never alters WS wire bytes.

HTTP bytes are forwarded unchanged. WS payloads are decoded after the upgrade
for A4 no-forwarding evidence; both direction and connection order are retained.
"""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is already initialised by the interpreter, so it is the only safe import.
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import importlib.util
from pathlib import Path
import tempfile

# -B only stops cache writes, so redirect the bytecode prefix too (fix-r5).
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code

# Load the one guard by explicit path, never sys.path: a same-named package or
# extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing the guard is what this block defends.
_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
_guard_path = _apparatus / '_guard.py'
_guard = sys.modules.get('_guard')
if not (getattr(_guard, '__file__', None)
        and Path(_guard.__file__).resolve() == _guard_path
        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):
    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)
    _guard = importlib.util.module_from_spec(_spec)
    sys.modules['_guard'] = _guard
    _guard_source = _guard_path.read_bytes()
    try:
        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)
    except BaseException:
        sys.modules.pop('_guard', None)
        raise
    _guard._apparatus_source = _guard_source
if Path(_guard.__file__).resolve() != _guard_path:
    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')

_load = _guard.load
_has_git_checkout = _guard._has_git_checkout
_verify_cli_imports = _guard.verify_cli_imports
run_entry = _guard.run_entry
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

import argparse
import asyncio
import json
from pathlib import Path
import time


class Decoder:
    def __init__(self, emit, direction):
        self.buffer = b''
        self.headers = False
        self.ws = False
        self.emit = emit
        self.direction = direction

    def feed(self, data):
        self.buffer += data
        if not self.headers:
            end = self.buffer.find(b'\r\n\r\n')
            if end < 0:
                return
            header, self.buffer = self.buffer[:end], self.buffer[end + 4:]
            self.headers = True
            self.ws = b'upgrade: websocket' in header.lower()
        if not self.ws:
            self.buffer = b''
            return
        while len(self.buffer) >= 2:
            first, second = self.buffer[:2]
            size, offset = second & 127, 2
            extra = 2 if size == 126 else 8 if size == 127 else 0
            if extra:
                if len(self.buffer) < offset + extra:
                    return
                size = int.from_bytes(self.buffer[offset:offset + extra], 'big')
                offset += extra
            masked = bool(second & 128)
            mask = self.buffer[offset:offset + 4] if masked else b''
            offset += 4 if masked else 0
            if len(self.buffer) < offset + size:
                return
            payload = self.buffer[offset:offset + size]
            self.buffer = self.buffer[offset + size:]
            if masked:
                payload = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
            row = {'direction': self.direction, 'opcode': first & 15, 'payloadHex': payload.hex()}
            if first & 15 == 1:
                row['raw'] = payload.decode('utf8', errors='replace')
                try:
                    row['frame'] = json.loads(row['raw'])
                except ValueError:
                    pass
            self.emit(row)


async def serve(args):
    counter = 0
    sequence = 0
    file = args.out.open('w', buffering=1)

    async def connection(reader, writer):
        nonlocal counter, sequence
        counter += 1
        identity = counter

        def emit(row):
            nonlocal sequence
            sequence += 1
            file.write(json.dumps({'seq': sequence, 'connection': identity,
                                   'monotonicNs': time.monotonic_ns(), **row}) + '\n')

        upstream_writer = None
        try:
            upstream_reader, upstream_writer = await asyncio.open_connection('127.0.0.1', args.upstream)

            async def pipe(source, target, direction):
                decoder = Decoder(emit, direction)
                while data := await source.read(65536):
                    decoder.feed(data)
                    target.write(data)
                    await target.drain()
                try:
                    target.write_eof()
                except (OSError, AttributeError):
                    pass

            await asyncio.gather(pipe(reader, upstream_writer, 'send'),
                                 pipe(upstream_reader, writer, 'receive'))
        except (OSError, asyncio.CancelledError) as error:
            emit({'transportError': type(error).__name__})
        finally:
            writer.close()
            if upstream_writer:
                upstream_writer.close()

    server = await asyncio.start_server(connection, '127.0.0.1', args.port)
    async with server:
        await server.serve_forever()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, required=True)
    ap.add_argument('--upstream', type=int, required=True)
    ap.add_argument('--out', type=Path, required=True)
    args = ap.parse_args()
    if not all(28000 <= port <= 28999 for port in (args.port, args.upstream)):
        ap.error('28xxx ports only')
    asyncio.run(serve(args))
    return 0


if __name__ == '__main__':
    run_entry(main)
