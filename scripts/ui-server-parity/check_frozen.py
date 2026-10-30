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
_boot_write = __name__ == '__main__' and '--write' in _boot_sys.argv[1:]
# Regeneration cannot use stale manifest pins. It admits only committed source,
# including the executing code object, before loading the HEAD-verified scanner.
# A dirty checker is rejected, never treated as a registration exemption.
if _boot_write:
    _boot_head = _boot_subprocess.check_output(
        ['git', '-C', _boot_root, 'rev-parse', '--verify', 'HEAD']).decode().strip()
    _boot_checker = _boot_subprocess.check_output(
        ['git', '-C', _boot_root, 'show', f'{_boot_head}:scripts/ui-server-parity/check_frozen.py'])
    if _boot_sys._getframe().f_code != compile(
            _boot_checker, __file__, 'exec', dont_inherit=True, optimize=_boot_sys.flags.optimize):
        raise SystemExit('#1089 --write executing checker differs from HEAD')
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_write:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', f'{_boot_head}:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('--write apparatus source differs from HEAD')
    elif _boot_os.path.isfile(_boot_manifest):
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
if _boot_write:
    # Only this source-only module is needed by the writer. Do not install the
    # normal FROZEN loader while its hash list is intentionally out of date.
    import types as _boot_types
    _boot_module = _boot_types.ModuleType('apparatus')
    _boot_module.__dict__.update(_boot_namespace)
    _boot_sys.modules['apparatus'] = _boot_module
else:
    _boot_namespace['preflight'](_boot_dir)
import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile
import subprocess
import sys
from apparatus import scan, ApparatusUnsafe, SPEC_FILES, EXP008_FILES, toolchain_files, approval_valid

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / 'specs/ui-server-parity'
REQUIRED = set(SPEC_FILES)


ENTRY_ID = r'[A-Z][A-Z0-9]*(?:\.[0-9]+)*-(?:[A-Z0-9]+-)+[0-9]{3}'
ENTRY_NAME = re.compile(rf'({ENTRY_ID})(-R[1-9][0-9]*)?\.md')
FIELDS = ('类别', '基线依据', 'Rust 行为', '范围', '机器判据', '批准记录')


def ledger_files(folder):
    """Historical bytes first, then entries in filename order; README is not data."""
    return [folder / 'accepted-diffs.md',
            *sorted(path for path in (folder / 'diffs').glob('*.md')
                    if path.name != 'README.md')]


def ledger_bytes(folder):
    """An empty diffs directory preserves the historical ledger byte for byte."""
    return b'\n\n'.join(path.read_bytes() for path in ledger_files(folder))


def check_diffs(folder, root):
    failures = []
    old = (folder / 'accepted-diffs.md').read_text(encoding='utf-8')
    ids = set(re.findall(rf'^#{{1,6}}\s+({ENTRY_ID}(?:-R[1-9][0-9]*)?)\b', old, re.M))
    parsed = []
    for path in sorted((folder / 'diffs').iterdir()) if (folder / 'diffs').exists() else []:
        if path.name == 'README.md' and path.is_file() and not path.is_symlink():
            continue
        match = ENTRY_NAME.fullmatch(path.name)
        if not match or not path.is_file() or path.is_symlink():
            failures.append(f'diffs/{path.name}: invalid entry filename or file type')
            continue
        name = path.stem
        if name in ids:
            failures.append(f'diffs/{path.name}: ID already exists in historical ledger')
        ids.add(name)
        try:
            content = path.read_text(encoding='utf-8')
        except (OSError, UnicodeError) as error:
            failures.append(f'diffs/{path.name}: {error}')
            continue
        # Deliberately small frontmatter grammar: flat, unquoted ID scalars only.
        # No YAML dependency or implicit boolean/null coercion.
        header = re.match(r'\A---\n(.*?)\n---(?:\n|\Z)', content, re.S)
        metadata = {}
        if header:
            for line in header[1].splitlines():
                field = re.fullmatch(r'(id|original):\s*(\S+)\s*', line)
                if not field or field[1] in metadata:
                    failures.append(f'diffs/{path.name}: invalid/duplicate frontmatter field')
                else:
                    metadata[field[1]] = field[2]
        if metadata.get('id') != name:
            failures.append(f'diffs/{path.name}: frontmatter id must match filename')
        sections = {}
        body = content[header.end():] if header else content
        headings = list(re.finditer(r'^## ([^\n]+)\n', body, re.M))
        for index, heading in enumerate(headings):
            key = '基线依据' if heading[1] == 'TS 依据' else heading[1]
            end = headings[index + 1].start() if index + 1 < len(headings) else len(body)
            if key in sections:
                failures.append(f'diffs/{path.name}: duplicate section {key}')
            sections[key] = body[heading.end():end].strip()
        for field in FIELDS:
            if not sections.get(field):
                failures.append(f'diffs/{path.name}: missing/empty field {field}')
        for pointer in re.findall(r'state/decisions-[\w.-]+\.md', sections.get('批准记录', '')):
            if not (root / pointer).is_file():
                failures.append(f'diffs/{path.name}: missing approval file {pointer}')
        parsed.append((path.name, match, metadata))
    for name, match, metadata in parsed:
        original = metadata.get('original')
        if match[2]:
            if original != match[1] or original not in ids:
                failures.append(f'diffs/{name}: original must name the existing base ID {match[1]}')
        elif original is not None:
            failures.append(f'diffs/{name}: original is only valid for corrections')
    return failures


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], stderr=subprocess.PIPE,
                                   timeout=30)


def check_append_only(root, base, notes=None):
    """Freeze the legacy ledger/hash row and existing diffs at the merge-base."""
    failures = []
    try:
        merge_base = git(root, 'merge-base', 'HEAD', base).decode(encoding='utf-8').strip()
        head = git(root, 'rev-parse', 'HEAD').decode(encoding='utf-8').strip()
        base_commit = git(root, 'rev-parse', '--verify', f'{base}^{{commit}}').decode(encoding='utf-8').strip()
        if merge_base == head:
            if base_commit != head:
                return [f'base {base}: base 与 HEAD 同源，只追加对照无效 '
                        '(merge-base == HEAD; base commit != HEAD)']
            if notes is not None:
                notes.append('base 解析出的提交等于 HEAD（如空推送）；继续核对工作树冻结字节')
        legacy = 'specs/ui-server-parity/accepted-diffs.md'
        before = git(root, 'show', f'{merge_base}:{legacy}')
        path = root / legacy
        after = path.read_bytes() if path.is_file() and not path.is_symlink() else b''
        if before != after or path.is_symlink():
            offset = next((i for i, (a, b) in enumerate(zip(before, after)) if a != b),
                          min(len(before), len(after)))
            line = before[:offset].count(b'\n') + 1
            failures.append(f'{legacy}:{line}: historical ledger differs from base '
                            '(byte-frozen; first differing line)')
        manifest = 'specs/ui-server-parity/FROZEN.md'
        before_manifest = git(root, 'show', f'{merge_base}:{manifest}')
        after_manifest = (root / manifest).read_bytes()
        # Compare the entire raw hash row (including its line ending), not just
        # the digest. Other rows may change for an approved ruler registration.
        hash_row = rb'[0-9a-f]{64}  \.\./\.\./specs/ui-server-parity/accepted-diffs\.md\s*'
        before_rows = [row for row in before_manifest.splitlines(keepends=True)
                       if re.fullmatch(hash_row, row)]
        after_rows = [row for row in after_manifest.splitlines(keepends=True)
                      if re.fullmatch(hash_row, row)]
        if len(before_rows) != 1 or before_rows != after_rows:
            failures.append(f'{manifest}: accepted-diffs.md hash line changed/absent/duplicate '
                            '(byte-frozen)')
        entries = git(root, 'ls-tree', '-rz', merge_base, '--',
                      'specs/ui-server-parity/diffs/').split(b'\0')
        for entry in filter(None, entries):
            meta, raw_path = entry.split(b'\t', 1)
            mode, kind, oid = meta.split()
            name = raw_path.decode(encoding='utf-8')
            path = root / name
            if (mode != b'100644' or kind != b'blob' or path.is_symlink()
                    or not path.is_file() or path.stat().st_mode & 0o111
                    or path.read_bytes() != git(root, 'cat-file', 'blob', oid.decode(encoding='utf-8'))):
                failures.append(f'{name}: base file changed/deleted/renamed (append-only)')
    except (OSError, UnicodeError, ValueError, subprocess.SubprocessError) as error:
        failures.append(f'cannot verify append-only base {base}: {error}')
    return failures


def apparatus_files(root):
    files = {Path(p) for p in scan(root / 'scripts/ui-server-parity')}
    files.update(Path(p) for p in scan(root / 'scripts/exp008'))
    return files


def required_files(root, folder):
    return ({folder / name for name in REQUIRED} | apparatus_files(root)
            | {root / 'scripts/exp008' / name for name in EXP008_FILES}
            | set(toolchain_files(root)))


def check(folder=SPEC, *, root=ROOT, base=None):
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
    try:
        errors.extend(check_diffs(folder, root))
    except (OSError, UnicodeError) as error:
        errors.append(f'cannot read ledger: {error}')
    errors.extend(check_manifest(root))
    errors.extend(check_registration(root, base))
    notes = []
    if base is not None:
        errors.extend(check_append_only(root, base, notes))
    return {'notes': notes, 'verdict': 'FAIL' if errors else 'DRAFT' if draft else 'PASS',
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


APPARATUS_KIND = 'ui-server-parity'

def manifest_row(line):
    match = re.fullmatch(rb'([0-9a-f]{64})  ([\w./-]+)\r?\n?', line)
    return (match[2].decode(), match[1].decode(), b'') if match else None


def manifest_path(name):
    if not name.startswith('../../'):
        raise ValueError('noncanonical manifest path: ' + name)
    name = name[6:]
    if Path(name).is_absolute() or '..' in Path(name).parts or Path(name).as_posix() != name:
        raise ValueError('noncanonical manifest path: ' + name)
    return name


def manifest_inventory(root):
    return {'../../' + path.relative_to(root).as_posix(): path.relative_to(root).as_posix()
            for path in required_files(root, root / 'specs/ui-server-parity')}


def render_row(name, digest, tail):
    return f'{digest}  {name}\n'.encode()


# Registration is deliberately local to each frozen apparatus (#1089).
EVENT_FIELDS = ('Calibration-Evidence', 'Independent-Reviews', 'Approved-By', 'Approval-Basis')
EVENT_NAME = re.compile(r'[0-9]{8}-[A-Za-z0-9]+(?:-[A-Za-z0-9]+)*\.md')
CHANGE = re.compile(r'Change: ([0-9a-f]{64}|-) ([0-9a-f]{64}|-) ([\w./-]+)')


def manifest_parts(raw):
    """Keep every non-list byte, including historical prose and table headings."""
    lines = raw.splitlines(keepends=True)
    indices = [i for i, line in enumerate(lines) if manifest_row(line) is not None]
    if not indices:
        raise ValueError('missing hash list')
    start, end = indices[0], indices[-1] + 1
    # Conflict markers immediately outside the first/last row belong to the list.
    while start and lines[start - 1].startswith((b'<<<<<<< ', b'||||||| ', b'=======', b'>>>>>>> ')):
        start -= 1
    while end < len(lines) and lines[end].startswith((b'<<<<<<< ', b'||||||| ', b'=======', b'>>>>>>> ')):
        end += 1
    for line in lines[start:end]:
        if manifest_row(line) is None and not line.startswith((b'<<<<<<< ', b'||||||| ', b'=======', b'>>>>>>> ')):
            raise ValueError('malformed or noncontiguous hash list')
    prefix, suffix = b''.join(lines[:start]), b''.join(lines[end:])
    if any(line.startswith((b'<<<<<<< ', b'||||||| ', b'=======', b'>>>>>>> '))
           for line in (prefix + suffix).splitlines()):
        raise ValueError('conflict outside hash list')
    return prefix, lines[start:end], suffix


def manifest_entries(raw):
    _, lines, _ = manifest_parts(raw)
    entries = {}
    for line in lines:
        row = manifest_row(line)
        if row is None:
            raise ValueError('unresolved hash list conflict')
        name, digest, _ = row
        if name in entries:
            raise ValueError('duplicate hash entry: ' + name)
        entries[name] = digest
    return entries


def regular_file(root, path):
    """No aliases, links, directories or escapes may be hashed as source files."""
    path = Path(path)
    relative = path.relative_to(root)
    if any((root / Path(*relative.parts[:i])).is_symlink()
           for i in range(1, len(relative.parts) + 1)) or not path.is_file():
        raise ValueError('nonregular frozen file: ' + str(relative))
    return path


def generated_manifest(root, raw, *, head=None):
    prefix, lines, suffix = manifest_parts(raw)
    metadata = {}
    for line in lines:
        row = manifest_row(line)
        if row is not None:
            name, _, tail = row
            if name in metadata and metadata[name] != tail:
                raise ValueError('conflicting historical row metadata: ' + name)
            metadata[name] = tail
    rows = []
    for name, relative in sorted(manifest_inventory(root).items()):
        path = regular_file(root, root / relative)
        data = path.read_bytes()
        if head is not None and data != git(root, 'show', f'{head}:{relative}'):
            raise ValueError('--write source differs from HEAD: ' + relative)
        digest = hashlib.sha256(data).hexdigest()
        rows.append(render_row(name, digest, metadata.get(name)))
    return prefix + b''.join(rows) + suffix


def write_manifest(root=ROOT, *, head=None):
    """Only publish a fully generated list; no event or approval is synthesized."""
    manifest = root / f'specs/{APPARATUS_KIND}/FROZEN.md'
    regular_file(root, manifest)
    raw = manifest.read_bytes()
    generated = generated_manifest(root, raw, head=head)
    if generated != raw:
        # Same-directory replace prevents readers observing a partial hash list.
        import os
        with tempfile.NamedTemporaryFile(dir=manifest.parent, prefix='.FROZEN-', delete=False) as stream:
            temporary = Path(stream.name)
            try:
                stream.write(generated)
                stream.flush()
                os.fchmod(stream.fileno(), manifest.stat().st_mode & 0o777)
            except BaseException:
                temporary.unlink()
                raise
        try:
            temporary.replace(manifest)
        finally:
            temporary.unlink(missing_ok=True)


def check_manifest(root):
    manifest = root / f'specs/{APPARATUS_KIND}/FROZEN.md'
    try:
        raw = regular_file(root, manifest).read_bytes()
        manifest_entries(raw)
        if generated_manifest(root, raw) != raw:
            return ['hash list differs from generated inventory; run check_frozen.py --write']
    except (OSError, ValueError, UnicodeError, subprocess.SubprocessError) as error:
        return ['cannot verify generated hash list: ' + str(error)]
    return []


def read_events(root):
    folder = root / f'specs/{APPARATUS_KIND}/frozen'
    events, failures = {}, []
    if folder.is_symlink():
        return {}, ['frozen directory is a symlink']
    for path in sorted(folder.iterdir()) if folder.exists() else []:
        try:
            regular_file(root, path)
            if path.stat().st_mode & 0o111:
                raise ValueError('executable event file')
            if path.name == 'README.md':
                continue
            if not EVENT_NAME.fullmatch(path.name):
                raise ValueError('invalid event filename')
            text = path.read_text(encoding='utf-8')
            for field in EVENT_FIELDS:
                values = re.findall(r'^' + field + r':([^\n]*)$', text, re.M)
                if len(values) != 1 or not values[0].strip():
                    raise ValueError('missing/duplicate/empty ' + field)
            reviews = re.findall(r'^Independent-Reviews:([^\n]*)$', text, re.M)[0]
            references = [re.fullmatch(r'([A-Za-z][A-Za-z0-9.-]*): (\S+)', part.strip())
                          for part in reviews.split(';')]
            if (len(references) != 2 or not all(references)
                    or len({ref[1].casefold() for ref in references}) != 2
                    or len({ref[2] for ref in references}) != 2):
                raise ValueError('Independent-Reviews requires two distinct families and references separated by ;')
            changes = []
            for line in text.splitlines():
                if not line.startswith('Change:'):
                    continue
                match = CHANGE.fullmatch(line)
                if not match:
                    raise ValueError('invalid Change row')
                old, new, name = match.groups()
                if (Path(name).is_absolute() or Path(name).as_posix() != name
                        or '..' in Path(name).parts or old == new):
                    raise ValueError('invalid Change path or equal hashes')
                changes.append((name, old, new))
            if not changes or len(changes) != len(set(changes)):
                raise ValueError('missing/duplicate Change rows')
            events[path.name] = changes
        except (OSError, UnicodeError, ValueError) as error:
            failures.append(f'frozen/{path.name}: {error}')
    return events, failures


def check_registration(root, base):
    events, failures = read_events(root)
    if base is None:
        return failures
    try:
        commit = git(root, 'merge-base', 'HEAD', base).decode().strip()
        spec = f'specs/{APPARATUS_KIND}'
        before = git(root, 'show', f'{commit}:{spec}/FROZEN.md')
        after = (root / spec / 'FROZEN.md').read_bytes()
        bp, _, bs = manifest_parts(before)
        ap, _, ass = manifest_parts(after)
        if (bp, bs) != (ap, ass):
            failures.append('FROZEN historical prose changed (byte-frozen)')
        before_tails = {row[0]: row[2] for line in manifest_parts(before)[1]
                        if (row := manifest_row(line)) is not None}
        after_tails = {row[0]: row[2] for line in manifest_parts(after)[1]
                       if (row := manifest_row(line)) is not None}
        if any(after_tails.get(name) != tail for name, tail in before_tails.items()
               if name in after_tails):
            failures.append('FROZEN historical row metadata changed (byte-frozen)')
        old_entries = manifest_entries(before)
        new_entries = manifest_entries(after)
        old_hashes = {}
        for name, pin in old_entries.items():
            relative = manifest_path(name)
            actual = hashlib.sha256(git(root, 'show', f'{commit}:{relative}')).hexdigest()
            if not actual.startswith(pin):
                raise ValueError('base manifest does not match base blob: ' + name)
            old_hashes[relative] = actual
        new_hashes = {manifest_path(name): hashlib.sha256(
            regular_file(root, root / manifest_path(name)).read_bytes()).hexdigest()
                      for name in new_entries}
        existing = set()
        for entry in filter(None, git(root, 'ls-tree', '-rz', commit, '--', spec + '/frozen/').split(b'\0')):
            meta, raw_path = entry.split(b'\t', 1)
            mode, kind, oid = meta.split()
            name = raw_path.decode()
            path = root / name
            existing.add(path.name)
            if (mode != b'100644' or kind != b'blob' or path.is_symlink() or not path.is_file()
                    or path.stat().st_mode & 0o111 or path.read_bytes() != git(root, 'cat-file', 'blob', oid.decode())):
                failures.append(name + ': base event changed/deleted/renamed (append-only)')
        edges = {}
        for event, changes in events.items():
            if event in existing:
                continue
            for name, old, new in changes:
                edges.setdefault(name, []).append((old, new))
        changed = {name for name in old_hashes.keys() | new_hashes.keys()
                   if old_hashes.get(name, '-') != new_hashes.get(name, '-')}
        for name in sorted(changed | edges.keys()):
            current, target = old_hashes.get(name, '-'), new_hashes.get(name, '-')
            remaining = list(edges.get(name, []))
            visited = set()
            while current != target and current not in visited:
                visited.add(current)
                matches = [edge for edge in remaining if edge[0] == current]
                if len(matches) != 1:
                    break
                edge = matches[0]
                remaining.remove(edge)
                current = edge[1]
            if name not in changed or current != target or remaining:
                failures.append(name + ': hash change lacks exact new event chain (or has stale/duplicate event)')
    except (OSError, UnicodeError, ValueError, subprocess.SubprocessError) as error:
        failures.append(f'cannot verify registration base {base}: {error}')
    return failures


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--base', help='check append-only against merge-base(HEAD, ref)')
    parser.add_argument('--write', action='store_true', help='regenerate only the hash list; does not approve registration')
    parser.add_argument('--print-ledger', action='store_true', help='emit validated combined ledger bytes')
    args = parser.parse_args(argv)
    if args.write and (args.print_ledger or args.base is not None or args.self_test):
        parser.error('--write cannot be combined with --base, --print-ledger or --self-test')
    if args.write:
        write_manifest(head=_boot_head)
        return 0
    if args.self_test and (args.base is not None or args.print_ledger):
        parser.error('--self-test cannot be combined with --base or --print-ledger')
    result = self_test() if args.self_test else check(base=args.base)
    if args.print_ledger and result['verdict'] == 'PASS':
        sys.stdout.buffer.write(ledger_bytes(SPEC))
        return 0
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 2 if result['verdict'] == 'DRAFT' else int(result['verdict'] != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())
