#!/usr/bin/env python3
"""Measure the capture bracket without starting services or changing clocks."""
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
