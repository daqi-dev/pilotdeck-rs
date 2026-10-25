#!/usr/bin/env python3
"""Verify registered FROZEN hashes, reporting unregistered parity files.

The accepted-diffs header is frozen, while entries are append-only. For the
initial empty table registration, verify the exact prefix through the blank
line following its separator. Never rewrite FROZEN.md.

Bytecode is never registered: a source-adjacent .pyc/.pyo (or any __pycache__
directory, empty included) is executable input that no FROZEN row binds, so it
is reported unregistered and fails this check (#694).
"""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it, exit before loading any apparatus module.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-parity-pyc-')
sys.dont_write_bytecode = True

import hashlib
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / 'specs/parity'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def bytecode_paths(root):
    """Bytecode under the apparatus, reported at the first __pycache__ segment.

    A __pycache__ directory itself is reported and the scan does not need to
    descend into it; a .pyc/.pyo outside one is reported by its own path.
    """
    script = root / 'scripts/parity'
    found = set()
    for path in script.rglob('*'):
        rel = path.relative_to(root)
        parts = [part.casefold() for part in rel.parts]
        if '__pycache__' in parts:
            index = parts.index('__pycache__')
            found.add(Path(*rel.parts[:index + 1]).as_posix())
        elif path.suffix.casefold() in ('.pyc', '.pyo'):
            found.add(rel.as_posix())
    return sorted(found)


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
    bytecode = bytecode_paths(root)
    files = [p for p in spec.iterdir() if p.is_file() and p.name != 'FROZEN.md']
    files += [p for p in (root / 'scripts/parity').rglob('*')
              if p.is_file()
              and '__pycache__' not in [part.casefold() for part in p.parts]
              and p.suffix.casefold() not in ('.pyc', '.pyo')]
    skipped = sorted({str(p.relative_to(root)) for p in files if p.resolve() not in registered} | set(bytecode))
    errors.extend(f'{name}: unregistered bytecode' for name in bytecode)
    if not checked:
        errors.append('no frozen hashes found')
    return {'ok': not errors, 'fully_registered': not skipped, 'checked': checked,
            'unregistered_skipped': skipped, 'notice': '未登记文件暂跳过；装置内任何 __pycache__ / .pyc / .pyo 即 FAIL。' if skipped else None, 'errors': errors}


def main():
    result = check()
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result['ok'] else 1


if __name__ == '__main__':
    sys.exit(main())
