#!/usr/bin/env -S python3 -I
"""Verify all B2 hashes, including this checker; a valid draft remains DRAFT/2."""
# Isolated bootstrap: verify the source loader before executing local code.
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:
    print('{"verdict":"FAIL","errors":["os must be preloaded from CPython 3.11+ frozen modules"]}')
    raise SystemExit(1)
import os as _boot_os
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (fix-r3 P1, same defect as #701 fix-r2). Nonexistent sys.path entries skip.
if any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.isdir(root)
                and _boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))
        for entry in _boot_sys.path):
    print('{"verdict":"FAIL","errors":["apparatus directory on sys.path; run the entry file directly with -I"]}')
    raise SystemExit(1)
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
import hashlib as _boot_hashlib
import subprocess as _boot_subprocess
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_os.path.isfile(_boot_manifest):
        with open(_boot_manifest) as _boot_file:
            _boot_pins = [line.split()[0] for line in _boot_file
                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]
        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:
            raise ValueError('apparatus bootstrap hash mismatch')
    else:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile
import subprocess
from apparatus import scan, ApparatusUnsafe, SPEC_FILES, EXP008_FILES, toolchain_files, approval_valid

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / 'specs/ui-server-parity'
REQUIRED = set(SPEC_FILES)


def apparatus_files(root):
    files = {Path(p) for p in scan(root / 'scripts/ui-server-parity')}
    files.update(Path(p) for p in scan(root / 'scripts/exp008'))
    return files


def required_files(root, folder):
    return ({folder / name for name in REQUIRED} | apparatus_files(root)
            | {root / 'scripts/exp008' / name for name in EXP008_FILES}
            | set(toolchain_files(root)))


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

    approved = approval_valid(approvals[0] if len(approvals) == 1 else None,
                              bases[0] if len(bases) == 1 else None, root,
                              allow_ai_delegate=True)
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
        subprocess.run(['git', 'init', '-q', str(root)], check=True)
        subprocess.run(['git', '-C', str(root), 'add', 'scripts'], check=True)
        for name in ('_tmp/batch/summary.json', '_tmp/review/claude.md', '_tmp/review/glm.md'):
            evidence = root / name
            evidence.parent.mkdir(parents=True, exist_ok=True)
            evidence.write_text('test evidence')
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
        for value in ('', '  ', '<待人批准>', 'TODO', 'Claude', 'GPT-6', 'GLM-5', 'Gemini', '测试\u200b人', '测试\x00人', '测试\ufe0f人', 'pending', 'null', '待批准', 'AI 代批，待人复核'):
            expect('invalid-approval-' + repr(value), good.replace(approval, 'Approved-By: ' + value), 'FAIL')
        expect('old-approval-field', good.replace('Approved-By:', 'Human-Approved-By:'), 'FAIL')
        expect('duplicate-approval', good + 'Approved-By: 第二人\n', 'FAIL')
        expect('indented-duplicate-approval', good + '  Approved-By: 第二人\n', 'FAIL')
        expect('missing-basis', good.replace(basis, ''), 'FAIL')
        expect('duplicate-basis', good + basis + '\n', 'FAIL')
        expect('indented-duplicate-basis', good + '  ' + basis + '\n', 'FAIL')
        expect('incomplete-basis', good.replace(basis, 'Approval-Basis: _tmp/batch/summary.json'), 'FAIL')
        expect('alias-basis', good.replace('_tmp/review/glm.md', './_tmp/review/claude.md'), 'FAIL')
        expect('missing-basis-file', good.replace('_tmp/review/glm.md', '_tmp/review/absent.md'), 'FAIL')
        outside_basis = root.parent / (root.name + '-outside.md')
        outside_basis.write_text('outside')
        try:
            expect('outside-basis-file', good.replace('_tmp/review/glm.md', str(outside_basis)), 'FAIL')
            link = root / '_tmp/review/escape.md'
            link.symlink_to(outside_basis)
            expect('symlink-basis-escape', good.replace('_tmp/review/glm.md', '_tmp/review/escape.md'), 'FAIL')
        finally:
            outside_basis.unlink()
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
        humans = folder / 'human-approvers.txt'
        original_humans = humans.read_text()
        humans.write_text('zhengdaqi\nunauthorized-human\n')
        expect('changed-human-approvers', good, 'FAIL')
        expect('changed-human-approvers-draft', good.replace('# B2', '# B2（待定标）'), 'FAIL')
        humans.write_text(original_humans)
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
