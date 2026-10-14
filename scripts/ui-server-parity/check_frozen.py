#!/usr/bin/env python3
"""Verify all B2 hashes, including this checker; a valid draft remains DRAFT/2."""
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
import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile
from apparatus import scan, ApparatusUnsafe, SPEC_FILES, EXP008_FILES

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / 'specs/ui-server-parity'
REQUIRED = set(SPEC_FILES)


def apparatus_files(root):
    files = {Path(p) for p in scan(root / 'scripts/ui-server-parity')}
    scan(root / 'scripts/exp008')
    return files


def required_files(root, folder):
    return ({folder / name for name in REQUIRED} | apparatus_files(root)
            | {root / 'scripts/exp008' / name for name in EXP008_FILES})


def check(folder=SPEC, *, root=ROOT):
    root, folder = Path(root).resolve(), Path(folder).resolve()
    try:
        required = {p.resolve() for p in required_files(root, folder)}
    except ApparatusUnsafe as error:
        return {'verdict': 'FAIL', 'approved': False, 'errors': [str(error)],
                'hashes_consistent': False}
    manifest = folder / 'FROZEN.md'
    if not manifest.is_file():
        return {'verdict': 'DRAFT', 'approved': False, 'reason': 'FROZEN.md absent; draft awaits human review'}
    raw = manifest.read_text()
    # Only the heading controls draft mode; body references do not disable enforcement.
    draft = '待定标' in raw.splitlines()[0] if raw.splitlines() else False
    errors = []
    approvals = re.findall(r'^[ \t]*Approved-By:([^\n]*)$', raw, re.M)
    bases = re.findall(r'^[ \t]*Approval-Basis:([^\n]*)$', raw, re.M)

    def concrete(value):
        return bool(value.strip()) and not re.search(r'<|>|pending|null|待批准|待人批准|待定标', value, re.I)

    approved = len(approvals) == 1 and concrete(approvals[0])
    if approved and re.search(r'AI|代批|待人复核', approvals[0], re.I):
        approved = approvals[0].strip() == 'AI 代批，待人复核（协调者）'
    # Both approval modes require one concrete basis: calibration report + two reviews.
    basis_paths = re.findall(r'(?:[^\s`+;,，；]+/)+[^\s`+;,，；]+\.(?:json|md|log|txt)', bases[0]) if len(bases) == 1 else []
    approved = approved and len(bases) == 1 and concrete(bases[0]) and len(set(basis_paths)) >= 3
    if not draft and not approved:
        errors.append('missing/invalid Approved-By or Approval-Basis (calibration report + two reviews)')
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
        for name in EXP008_FILES:
            path = root / 'scripts/exp008' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name)
        lines = [hashlib.sha256(p.read_bytes()).hexdigest() + '  ' +
                 str(Path('../../') / p.relative_to(root)) for p in sorted(required_files(root, folder))]
        manifest = folder / 'FROZEN.md'
        approval = 'Approved-By: 测试人'
        basis = 'Approval-Basis: 定标 _tmp/batch/summary.json；评审 _tmp/review/claude.md + _tmp/review/glm.md'
        good = '# B2\n' + approval + '\n' + basis + '\n' + '\n'.join(lines) + '\n'
        cases = []

        def expect(name, raw, verdict):
            manifest.write_text(raw)
            actual = check(folder, root=root)['verdict']
            cases.append({'name': name, 'expected': verdict, 'actual': actual, 'passed': actual == verdict})

        expect('approved', good, 'PASS')
        expect('draft', good.replace('# B2', '# B2（待定标）'), 'DRAFT')
        expect('ai-coordinator-approval', good.replace(approval, 'Approved-By: AI 代批，待人复核（协调者）'), 'PASS')
        expect('ai-approval-without-basis', good.replace(approval, 'Approved-By: AI 代批，待人复核（协调者）').replace(basis, ''), 'FAIL')
        for value in ('', '  ', '<待人批准>', 'pending', 'null', '待批准', 'AI 代批，待人复核'):
            expect('invalid-approval-' + repr(value), good.replace(approval, 'Approved-By: ' + value), 'FAIL')
        expect('old-approval-field', good.replace('Approved-By:', 'Human-Approved-By:'), 'FAIL')
        expect('duplicate-approval', good + 'Approved-By: 第二人\n', 'FAIL')
        expect('indented-duplicate-approval', good + '  Approved-By: 第二人\n', 'FAIL')
        expect('missing-basis', good.replace(basis, ''), 'FAIL')
        expect('duplicate-basis', good + basis + '\n', 'FAIL')
        expect('indented-duplicate-basis', good + '  ' + basis + '\n', 'FAIL')
        expect('incomplete-basis', good.replace(basis, 'Approval-Basis: _tmp/batch/summary.json'), 'FAIL')
        expect('placeholder-basis', good.replace(basis, 'Approval-Basis: <待定标及两家评审>'), 'FAIL')
        expect('missing-entry', good.replace(lines[0] + '\n', ''), 'FAIL')
        for name in EXP008_FILES:
            line = next(line for line in lines if line.endswith('../../scripts/exp008/' + name))
            expect('missing-exp008-' + name, good.replace(line + '\n', ''), 'FAIL')
        expect('duplicate', good + lines[0] + '\n', 'FAIL')
        expect('escape', good + '0' * 64 + '  ../../../outside\n', 'FAIL')
        device = root / 'scripts/ui-server-parity/check_frozen.py'
        device.write_text('tampered')
        expect('changed-checker', good, 'FAIL')
        expect('changed-draft', good.replace('# B2', '# B2（待定标）'), 'FAIL')
        device.write_text('check_frozen.py')
        device_dir = device.parent
        outside = root / '_tmp/jsonpkg'
        outside.mkdir(parents=True)
        (outside / '__init__.py').write_text('raise RuntimeError("injection")')
        for name, directory in (('symlink-directory', True), ('symlink-file', False)):
            link = device_dir / 'injected'
            link.symlink_to(outside if directory else outside / '__init__.py', target_is_directory=directory)
            expect(name, good, 'FAIL')
            expect(name + '-draft', good.replace('# B2', '# B2（待定标）'), 'FAIL')
            link.unlink()
        for name in ('common.pyc', '__pycache__'):
            path = device_dir / name
            path.mkdir() if name == '__pycache__' else path.write_bytes(b'forged pyc')
            expect(name, good, 'FAIL')
            path.rmdir() if path.is_dir() else path.unlink()
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
