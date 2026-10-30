#!/usr/bin/env python3
"""Verify registered FROZEN hashes, reporting unregistered parity files.

The historical accepted-diffs bytes are frozen. New entries and corrections
are separate append-only files under diffs/. Never rewrite FROZEN.md.

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

import argparse
import hashlib
import subprocess
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / 'specs/parity'


# #1088 sole approved header-to-full-file migration; both raw rows are pinned.
LEGACY_HEADER_ROW = b'| `accepted-diffs.md` | `653ccb1001d1b09a\xe2\x80\xa6` | 2026-10-14\xef\xbc\x88\xe4\xba\xba\xe8\xbf\x87\xe7\x9b\xae\xef\xbc\x89 | \xe8\xa1\xa8\xe5\xa4\xb4\xef\xbc\x9b\xe6\xad\xa4\xe5\x90\x8e\xe5\x8f\xaa\xe5\xa2\x9e\xe4\xb8\x8d\xe5\x88\xa0\xef\xbc\x8cAI \xe8\xbf\xbd\xe5\x8a\xa0\xe7\x9a\x84\xe6\x9d\xa1\xe7\x9b\xae\xe7\x8a\xb6\xe6\x80\x81\xe4\xb8\xba\xe3\x80\x8c\xe5\xbe\x85\xe6\x89\xb9\xe3\x80\x8d\xef\xbc\x8c\xe8\xbf\xbd\xe5\x8a\xa0\xe4\xb8\x8d\xe6\x94\xb9\xe8\xa1\xa8\xe5\xa4\xb4 |\n'
LEGACY_FULL_ROW = b'| `accepted-diffs.md` | `12ca6dbfc7f161a0bdc18e2a2c927152d0c45285f5a58398b9ee5e3dbc4873b2` | 2026-10-30 #1088\xef\xbc\x88**AI \xe4\xbb\xa3\xe6\x89\xb9\xef\xbc\x8c\xe5\xbe\x85\xe4\xba\xba\xe5\xa4\x8d\xe6\xa0\xb8**\xef\xbc\x89 | \xe5\x8e\x86\xe5\x8f\xb2\xe8\xb4\xa6\xe6\x9c\xac\xe5\x8e\x9f\xe5\xad\x97\xe8\x8a\x82\xe5\x86\xbb\xe7\xbb\x93\xef\xbc\x9b\xe6\x96\xb0\xe6\x9d\xa1\xe7\x9b\xae\xe5\x8f\xaa\xe6\x96\xb0\xe5\xa2\x9e diffs/<ID>.md |\n'

ENTRY_ID = r'[A-Z][A-Z0-9]*(?:\.[0-9]+)*-(?:[A-Z0-9]+-)+[0-9]{3}'
ENTRY_NAME = re.compile(rf'({ENTRY_ID})(-R[1-9][0-9]*)?\.md')
FIELDS = ('类别', '基线依据', 'Rust 行为', '范围', '机器判据', '批准记录')
# Historical C ledger rows use integers, retained as IDs only for corrections.
LEGACY_CORRECTION_NAME = re.compile(r'([1-9][0-9]*)(-R[1-9][0-9]*)\.md')


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
    ids.update(re.findall(r'^\| ([1-9][0-9]*) \|', old, re.M))
    parsed = []
    for path in sorted((folder / 'diffs').iterdir()) if (folder / 'diffs').exists() else []:
        if path.name == 'README.md' and path.is_file() and not path.is_symlink():
            continue
        match = ENTRY_NAME.fullmatch(path.name) or LEGACY_CORRECTION_NAME.fullmatch(path.name)
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
        legacy = 'specs/parity/accepted-diffs.md'
        before = git(root, 'show', f'{merge_base}:{legacy}')
        path = root / legacy
        after = path.read_bytes() if path.is_file() and not path.is_symlink() else b''
        if before != after or path.is_symlink():
            offset = next((i for i, (a, b) in enumerate(zip(before, after)) if a != b),
                          min(len(before), len(after)))
            line = before[:offset].count(b'\n') + 1
            failures.append(f'{legacy}:{line}: historical ledger differs from base '
                            '(byte-frozen; first differing line)')
        manifest = 'specs/parity/FROZEN.md'
        before_manifest = git(root, 'show', f'{merge_base}:{manifest}')
        after_manifest = (root / manifest).read_bytes()
        # Compare the entire raw hash row (including its line ending), not just
        # the digest. Other rows may change for an approved ruler registration.
        hash_row = rb'\| `accepted-diffs\.md` \|[^\r\n]*\r?\n?'
        before_rows = [row for row in before_manifest.splitlines(keepends=True)
                       if re.fullmatch(hash_row, row)]
        after_rows = [row for row in after_manifest.splitlines(keepends=True)
                      if re.fullmatch(hash_row, row)]
        migration = before_rows == [LEGACY_HEADER_ROW] and after_rows == [LEGACY_FULL_ROW]
        if len(before_rows) != 1 or (before_rows != after_rows and not migration):
            failures.append(f'{manifest}: accepted-diffs.md hash line changed/absent/duplicate '
                            '(byte-frozen)')
        entries = git(root, 'ls-tree', '-rz', merge_base, '--',
                      'specs/parity/diffs/').split(b'\0')
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


def check(root=ROOT, *, base=None):
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
    try:
        errors.extend(check_diffs(spec, root))
    except (OSError, UnicodeError) as error:
        errors.append(f'cannot read ledger: {error}')
    notes = []
    if base is not None:
        errors.extend(check_append_only(root, base, notes))
    return {'hashes_consistent': not errors, 'notes': notes, 'ok': not errors, 'fully_registered': not skipped, 'checked': checked,
            'unregistered_skipped': skipped, 'notice': '未登记文件暂跳过；装置内任何 __pycache__ / .pyc / .pyo 即 FAIL。' if skipped else None, 'errors': errors}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', help='check append-only against merge-base(HEAD, ref)')
    parser.add_argument('--print-ledger', action='store_true', help='emit validated combined ledger bytes')
    args = parser.parse_args(argv)
    result = check(base=args.base)
    if args.print_ledger and result['ok']:
        sys.stdout.buffer.write(ledger_bytes(SPEC))
        return 0
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result['ok'] else 1


if __name__ == '__main__':
    sys.exit(main())
