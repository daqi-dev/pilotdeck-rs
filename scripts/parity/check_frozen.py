#!/usr/bin/env python3
"""Verify registered FROZEN hashes, reporting unregistered parity files.

The accepted-diffs header is frozen, while entries are append-only. For the
initial empty table registration, verify the exact prefix through the blank
line following its separator. Never rewrite FROZEN.md.
"""
import hashlib
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / 'specs/parity'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def check(root=ROOT):
    spec = root / 'specs/parity'
    frozen = (spec / 'FROZEN.md').read_text()
    checked, errors, registered = {}, [], set()
    for line in frozen.splitlines():
        match = re.match(r'^\|\s*`([^`]+)`\s*\|\s*`([0-9a-f]{16,64})(?:…|\.\.\.)?`', line)
        if not match:
            continue
        name, expected = match.groups()
        path = root / name if name.startswith('scripts/') or name.startswith('specs/') else spec / name
        registered.add(path.resolve())
        try:
            data = path.read_bytes()
            digest = sha(data)
            mode = 'full-file'
            if name == 'accepted-diffs.md' and not digest.startswith(expected):
                separator = b'|---|---|---|---|---|---|\n\n'
                end = data.find(separator)
                if end != -1:
                    digest = sha(data[:end + len(separator)])
                    mode = 'frozen-header'
            ok = digest.startswith(expected)
            checked[name] = {'ok': ok, 'sha256': digest, 'expected_prefix': expected, 'mode': mode}
            if not ok:
                errors.append(f'{name}: hash mismatch')
        except OSError as error:
            errors.append(f'{name}: {error}')
    files = [p for p in spec.iterdir() if p.is_file() and p.name != 'FROZEN.md']
    files += [p for p in (root / 'scripts/parity').rglob('*') if p.is_file() and '__pycache__' not in p.parts and p.suffix != '.pyc']
    skipped = sorted(str(p.relative_to(root)) for p in files if p.resolve() not in registered)
    if not checked:
        errors.append('no frozen hashes found')
    return {'ok': not errors, 'fully_registered': not skipped, 'checked': checked,
            'unregistered_skipped': skipped, 'notice': '未登记文件暂跳过；须由协调者批准后登记。' if skipped else None, 'errors': errors}


def main():
    result = check()
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result['ok'] else 1


if __name__ == '__main__':
    sys.exit(main())
