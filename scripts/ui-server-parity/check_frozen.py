#!/usr/bin/env python3
"""Draft until an explicitly human-approved SHA256 manifest exists."""
import hashlib
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / 'specs/ui-server-parity'
REQUIRED = {'acceptance.md', 'normalization.md', 'accepted-diffs.md', 'multimodal.md'}


def check(folder=SPEC):
    manifest = folder / 'FROZEN.md'
    if not manifest.is_file():
        return {'verdict': 'DRAFT', 'approved': False, 'reason': 'FROZEN.md absent; draft awaits human review'}
    raw = manifest.read_text()
    errors = []
    if not re.search(r'^Human-Approved-By:\s*\S.+$', raw, re.M) or re.search(r'^Human-Approved-By:.*(?:null|AI 代批|待人|pending)', raw, re.M | re.I):
        errors.append('missing explicit human approval')
    entries = re.findall(r'^([0-9a-f]{64})\s+([^\s]+)\s*$', raw, re.M)
    names = [name for _, name in entries]
    if len(names) != len(set(names)) or not REQUIRED.issubset(names):
        errors.append('missing required or duplicate SHA256 entries')
    for expected, name in entries:
        path = (folder / name).resolve()
        if not path.is_relative_to(folder.resolve()) or not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            errors.append('invalid path/hash: ' + name)
    return {'verdict': 'FAIL' if errors else 'PASS', 'approved': not errors, 'errors': errors}


def main():
    result = check()
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 2 if result['verdict'] == 'DRAFT' else int(result['verdict'] != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())
