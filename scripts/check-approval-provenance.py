#!/usr/bin/env python3
"""CI provenance guard for small B2 approval originals; outside the frozen ruler.

Trusts the reviewed manifest. It detects changed/missing originals, not a malicious
actor editing both the manifest and the originals. It does not approve identities.
"""
import sys
if not (sys.flags.isolated and sys.flags.ignore_environment and sys.flags.no_user_site
        and sys.pycache_prefix is None):
    raise SystemExit('Python isolation required: use python3 -I')
if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir,))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

sys.dont_write_bytecode = True

import hashlib
import json
from pathlib import Path
import re

EXPECTED = frozenset(
    'specs/ui-server-parity/approval/' + name
    for name in ('calibration-summary.json', 'review-glm.md', 'review-deepseek.md', 'driver.log')
)
PREFIX = 'Approval-Source-SHA256:'
# Superseded or process originals kept for audit; each must stay hash-bound (fail-closed).
ARCHIVE = frozenset(
    'specs/ui-server-parity/approval/' + name
    for name in ('2026-10-14/calibration-summary.json', '2026-10-14/review-glm.md',
                 '2026-10-14/review-claude.md', '2026-10-14/driver.log', 'review-deepseek-r1.md')
)
ARCHIVE_PREFIX = 'Approval-Archive-SHA256:'


def regular(path, root):
    return (path.is_file() and not path.is_symlink()
            and all(not parent.is_symlink() for parent in path.parents
                    if parent == root or root in parent.parents))


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def check(root):
    root = Path(root).absolute()
    errors, sources, archived = [], {}, {}
    manifest = root / 'specs/ui-server-parity/FROZEN.md'
    try:
        if not regular(manifest, root):
            raise ValueError('manifest must be a regular file without symlink ancestors')
        for number, line in enumerate(manifest.read_text(encoding='utf-8').splitlines(), 1):
            for prefix, allowed, table, kind in ((PREFIX, EXPECTED, sources, 'approval source'),
                                                 (ARCHIVE_PREFIX, ARCHIVE, archived, 'archived approval original')):
                if not line.startswith(prefix):
                    continue
                match = re.fullmatch(re.escape(prefix) + r' ([a-f0-9]{64})  (.+)', line)
                if not match:
                    errors.append(f'invalid {kind} registration at line {number}')
                    continue
                digest, path = match.groups()
                if path not in allowed:
                    errors.append(f'unknown {kind} at line {number}')
                elif path in table:
                    errors.append(f'duplicate {kind}: {path}')
                else:
                    table[path] = digest
        if set(sources) != EXPECTED:
            errors.append('approval source set must contain all four registered originals')
        if set(archived) != ARCHIVE:
            errors.append('archived approval set must contain all five registered originals')
        def references(field):
            rows = [line.split(':', 1)[1].strip() for line in manifest.read_text(encoding='utf-8').splitlines()
                    if line.startswith(field + ':')]
            if len(rows) != 1:
                errors.append(f'{field} must occur exactly once')
                return []
            return [part.strip() for part in rows[0].split(';')]

        basis = references('Approval-Basis')
        reviews = references('Independent-Reviews')
        calibration = references('Calibration-Evidence')
        approval_sources = EXPECTED - {'specs/ui-server-parity/approval/driver.log'}
        review_sources = approval_sources - {'specs/ui-server-parity/approval/calibration-summary.json'}
        if len(basis) != 3 or set(basis) != approval_sources:
            errors.append('Approval-Basis must reference the three hashed approval originals')
        if len(reviews) != 2 or set(reviews) != review_sources:
            errors.append('Independent-Reviews must reference the two hashed review originals')
        required_evidence = {'specs/ui-server-parity/approval/calibration-summary.json',
                             'specs/ui-server-parity/approval/driver.log'}
        if not required_evidence <= set(calibration) or len(calibration) != len(set(calibration)):
            errors.append('Calibration-Evidence must reference current summary/driver without duplicates')
        for name, expected in [*sources.items(), *archived.items()]:
            path = root / name
            if not regular(path, root):
                errors.append(f'missing/nonregular approval original: {name}')
            elif sha256_file(path) != expected:
                errors.append(f'approval original SHA256 mismatch: {name}')
    except (OSError, UnicodeError, ValueError) as error:
        errors.append(str(error))
    return {'verdict': 'FAIL' if errors else 'PASS', 'checkedSources': len(sources),
            'checkedArchive': len(archived), 'errors': errors}


if __name__ == '__main__':
    result = check(Path(__file__).resolve().parents[1])
    print(json.dumps(result, ensure_ascii=False, indent=2))
    raise SystemExit(result['verdict'] != 'PASS')
