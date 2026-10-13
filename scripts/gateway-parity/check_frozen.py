#!/usr/bin/env python3
"""Check coordinator-approved hash manifest; absence means DRAFT, never PASS."""
import json
from pathlib import Path
import re

from common import ROOT, digest


def main():
    folder = ROOT / 'specs/gateway-parity'
    manifest = folder / 'FROZEN.md'
    if not manifest.exists():
        print(json.dumps({'status': 'DRAFT', 'approved': False, 'reason': 'FROZEN.md absent; awaiting coordinator review'}, ensure_ascii=False))
        return 2
    entries = re.findall(r'^([0-9a-f]{64})\s+([\w./-]+)\s*$', manifest.read_text(), re.M)
    failures = []
    required = {'acceptance.md', 'normalization.md', 'accepted-diffs.md'}
    if not required.issubset({name for _, name in entries}) or len(entries) != len({name for _, name in entries}):
        failures.append('missing required / duplicate hash entries')
    for expected, name in entries:
        path = (folder / name).resolve()
        if not path.is_relative_to(ROOT) or not path.is_file() or digest(path) != expected:
            failures.append(name)
    print(json.dumps({'status': 'FAIL' if failures else 'PASS', 'failures': failures, 'entries': len(entries)}, ensure_ascii=False))
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(main())
