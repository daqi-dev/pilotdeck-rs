#!/usr/bin/env python3
"""Measure the capture bracket without starting services or changing clocks."""
# Admit source-only apparatus before json or any local module can be imported.
import os as _boot_os
import sys as _boot_sys
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
with open(_boot_source, 'rb') as _boot_file:
    exec(compile(_boot_file.read(), _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import json
import math
import platform
import time


def benchmark(count=100_000):
    widths = []
    for _ in range(count):
        before = time.monotonic_ns()
        time.time_ns()
        after = time.monotonic_ns()
        widths.append(after - before)
    widths.sort()
    return {'samples': count, 'quantile': 'nearest-rank', 'python': platform.python_version(),
            'platform': platform.platform(), 'p99_99_ns': widths[math.ceil(count * .9999) - 1],
            'max_ns': widths[-1], 'over_1ms': sum(w > 1_000_000 for w in widths)}


if __name__ == '__main__':
    print(json.dumps(benchmark(), ensure_ascii=False, indent=2))
