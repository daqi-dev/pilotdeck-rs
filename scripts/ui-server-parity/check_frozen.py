#!/usr/bin/env python3
"""Verify all B2 hashes, including this checker; a valid draft remains DRAFT/2."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / 'specs/ui-server-parity'
REQUIRED = {'acceptance.md', 'normalization.md', 'accepted-diffs.md', 'multimodal.md'}
PROXY = ('Cargo.toml', 'Cargo.lock', 'src/main.rs')


def apparatus_files(root):
    return {p for p in (root / 'scripts/ui-server-parity').rglob('*')
            if p.is_file() and '__pycache__' not in p.parts and p.suffix != '.pyc'}


def required_files(root, folder):
    return ({folder / name for name in REQUIRED} | apparatus_files(root)
            | {root / 'scripts/exp008/proxy' / name for name in PROXY})


def check(folder=SPEC, *, root=ROOT):
    root, folder = Path(root).resolve(), Path(folder).resolve()
    manifest = folder / 'FROZEN.md'
    if not manifest.is_file():
        return {'verdict': 'DRAFT', 'approved': False, 'reason': 'FROZEN.md absent; draft awaits human review'}
    raw = manifest.read_text()
    # Only the heading controls draft mode; body references do not disable enforcement.
    draft = '待定标' in raw.splitlines()[0] if raw.splitlines() else False
    errors = []
    approvals = re.findall(r'^Human-Approved-By:([^\n]*)$', raw, re.M)
    approved = len(approvals) == 1 and bool(approvals[0].strip())
    if re.search(r'^Human-Approved-By:.*(?:null|AI 代批|待|pending|<|>)', raw, re.M | re.I):
        approved = False
    if not draft and not approved:
        errors.append('missing explicit human approval')
    entries = re.findall(r'^([0-9a-f]{64})  ([^\s]+)\s*$', raw, re.M)
    names = [name for _, name in entries]
    if len(names) != len(set(names)):
        errors.append('duplicate SHA256 entries')
    registered = set()
    for expected, name in entries:
        path = (folder / name).resolve()
        registered.add(path)
        if (Path(name).is_absolute() or not path.is_relative_to(root) or not path.is_file()
                or hashlib.sha256(path.read_bytes()).hexdigest() != expected):
            errors.append('invalid path/hash: ' + name)
    required = {p.resolve() for p in required_files(root, folder)}
    errors.extend('unregistered/missing file: ' + str(p.relative_to(root))
                  for p in sorted(required - registered) if p.is_relative_to(root))
    errors.extend('unexpected registered file: ' + str(p) for p in sorted(registered - required))
    # Resolve aliases as well as duplicate names, and forbid symlink escapes.
    if len(registered) != len(entries):
        errors.append('duplicate resolved SHA256 paths')
    if any(not p.is_relative_to(root) for p in required):
        errors.append('apparatus symlink escapes repository')
    return {'verdict': 'FAIL' if errors else 'DRAFT' if draft else 'PASS',
            'approved': approved and not draft and not errors, 'errors': errors,
            'entries': len(entries), 'hashes_consistent': not errors}


def self_test():
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        folder = root / 'specs/ui-server-parity'
        folder.mkdir(parents=True)
        for name in REQUIRED:
            (folder / name).write_text(name)
        for name in ('check_frozen.py', 'controls/nested.py', 'calibrate.sh'):
            path = root / 'scripts/ui-server-parity' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name)
        for name in PROXY:
            path = root / 'scripts/exp008/proxy' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name)
        lines = [hashlib.sha256(p.read_bytes()).hexdigest() + '  ' +
                 str(Path('../../') / p.relative_to(root)) for p in sorted(required_files(root, folder))]
        manifest = folder / 'FROZEN.md'
        good = '# B2\nHuman-Approved-By: 测试人\n' + '\n'.join(lines) + '\n'
        cases = []

        def expect(name, raw, verdict):
            manifest.write_text(raw)
            actual = check(folder, root=root)['verdict']
            cases.append({'name': name, 'expected': verdict, 'actual': actual, 'passed': actual == verdict})

        expect('approved', good, 'PASS')
        expect('draft', good.replace('# B2', '# B2（待定标）'), 'DRAFT')
        expect('missing-approval', good.replace('Human-Approved-By: 测试人', 'Human-Approved-By: <待人批准>'), 'FAIL')
        expect('blank-approval', good.replace('Human-Approved-By: 测试人', 'Human-Approved-By:  '), 'FAIL')
        expect('duplicate-approval', good + 'Human-Approved-By: 第二人\n', 'FAIL')
        expect('missing-entry', good.replace(lines[0] + '\n', ''), 'FAIL')
        expect('duplicate', good + lines[0] + '\n', 'FAIL')
        expect('escape', good + '0' * 64 + '  ../../../outside\n', 'FAIL')
        device = root / 'scripts/ui-server-parity/check_frozen.py'
        device.write_text('tampered')
        expect('changed-checker', good, 'FAIL')
        expect('changed-draft', good.replace('# B2', '# B2（待定标）'), 'FAIL')
        device.write_text('check_frozen.py')
        (root / 'scripts/ui-server-parity/new-device.txt').write_text('unregistered')
        expect('unregistered-device', good, 'FAIL')
        return {'verdict': 'PASS' if all(c['passed'] for c in cases) else 'FAIL', 'controls': cases}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    result = self_test() if args.self_test else check()
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 2 if result['verdict'] == 'DRAFT' else int(result['verdict'] != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())
