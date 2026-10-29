# anchor-selfcheck: unsupported shared bounded pipe transport, no source mutation definitions
"""Bounded raw pipe I/O: readiness never delegates to blocking TextIO."""
import os
import select
import time


class DeadlineExceeded(RuntimeError):
    pass


def ready(fd, deadline, label, writing=False, stop=None):
    while True:
        if stop is not None and stop.is_set():
            raise InterruptedError(f"#969 {label}: owned pipe stopped")
        left = deadline - time.monotonic()
        if left <= 0:
            raise DeadlineExceeded(f"#969 {label}: pipe deadline expired")
        readers, writers, _ = select.select([] if writing else [fd], [fd] if writing else [], [],
                                            min(left, .1) if stop is not None else left)
        if writers if writing else readers:
            return
        if stop is None:
            raise DeadlineExceeded(f"#969 {label}: pipe deadline expired")


class Lines:
    def __init__(self, stream):
        self.fd = stream.fileno()
        os.set_blocking(self.fd, False)
        self.buffer = bytearray()

    def readline(self, deadline, label, stop=None):
        while b'\n' not in self.buffer:
            ready(self.fd, deadline, label, stop=stop)
            try:
                chunk = os.read(self.fd, 65536)
            except BlockingIOError:
                continue
            if not chunk:
                if self.buffer:
                    raise RuntimeError(f"#969 {label}: incomplete JSON line at EOF")
                return ''
            self.buffer.extend(chunk)
        line, _, tail = self.buffer.partition(b'\n')
        self.buffer = bytearray(tail)
        return line.decode('utf-8') + '\n'


def write(stream, encoded, deadline, label):
    fd = stream.fileno()
    os.set_blocking(fd, False)
    remaining = memoryview(encoded.encode('utf-8'))
    while remaining:
        ready(fd, deadline, label, writing=True)
        try:
            count = os.write(fd, remaining)
        except BlockingIOError:
            continue
        remaining = remaining[count:]
