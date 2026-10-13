#!/usr/bin/env python3
"""Independent transparent TCP observation point. Never alters WS wire bytes.

HTTP bytes are forwarded unchanged. WS payloads are decoded after the upgrade
for A4 no-forwarding evidence; both direction and connection order are retained.
"""
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


async def main(args):
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


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, required=True)
    ap.add_argument('--upstream', type=int, required=True)
    ap.add_argument('--out', type=Path, required=True)
    args = ap.parse_args()
    if not all(28000 <= port <= 28999 for port in (args.port, args.upstream)):
        ap.error('28xxx ports only')
    asyncio.run(main(args))
