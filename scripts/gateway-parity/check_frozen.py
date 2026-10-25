#!/usr/bin/env python3
"""Check coordinator-approved hash manifest; absence means DRAFT, never PASS."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile

# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# Load apparatus modules by explicit path, never sys.path: a same-named package
# or extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing common.py is what this block defends.  -B only
# stops cache writes, so redirect the bytecode prefix too (fix-r5).
import importlib.util, sys, tempfile
from pathlib import Path

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

A2_SCRIPTS = common.A2_SCRIPTS
ROOT = common.ROOT
SCRIPT = common.SCRIPT
digest = common.digest
unknown_apparatus_files = common.unknown_apparatus_files


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
        legacy = 'specs/gateway-parity/accepted-diffs.md'
        before = git(root, 'show', f'{merge_base}:{legacy}')
        path = root / legacy
        after = path.read_bytes() if path.is_file() and not path.is_symlink() else b''
        if before != after or path.is_symlink():
            offset = next((i for i, (a, b) in enumerate(zip(before, after)) if a != b),
                          min(len(before), len(after)))
            line = before[:offset].count(b'\n') + 1
            failures.append(f'{legacy}:{line}: historical ledger differs from base '
                            '(byte-frozen; first differing line)')
        manifest = 'specs/gateway-parity/FROZEN.md'
        before_manifest = git(root, 'show', f'{merge_base}:{manifest}')
        after_manifest = (root / manifest).read_bytes()
        # Compare the entire raw hash row (including its line ending), not just
        # the digest. Other rows may change for an approved ruler registration.
        hash_row = rb'[0-9a-f]{64}\s+accepted-diffs\.md\s*'
        before_rows = [row for row in before_manifest.splitlines(keepends=True)
                       if re.fullmatch(hash_row, row)]
        after_rows = [row for row in after_manifest.splitlines(keepends=True)
                      if re.fullmatch(hash_row, row)]
        if len(before_rows) != 1 or before_rows != after_rows:
            failures.append(f'{manifest}: accepted-diffs.md hash line changed/absent/duplicate '
                            '(byte-frozen)')
        entries = git(root, 'ls-tree', '-rz', merge_base, '--',
                      'specs/gateway-parity/diffs/').split(b'\0')
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


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', help='freeze historical ledger/hash row and existing diffs '
                        'against merge-base(HEAD, ref)')
    parser.add_argument('--print-ledger', action='store_true', help='emit validated combined ledger bytes')
    args = parser.parse_args(argv)
    folder = ROOT / 'specs/gateway-parity'
    manifest = folder / 'FROZEN.md'
    if not manifest.exists():
        print(json.dumps({'status': 'DRAFT', 'approved': False, 'reason': 'FROZEN.md absent; awaiting coordinator review'}, ensure_ascii=False))
        return 2
    entries = re.findall(r'^([0-9a-f]{64})\s+([\w./-]+)\s*$', manifest.read_text(encoding='utf-8'), re.M)
    failures = []
    required = {'acceptance.md', 'normalization.md', 'accepted-diffs.md'}
    if not required.issubset({name for _, name in entries}) or len(entries) != len({name for _, name in entries}):
        failures.append('missing required / duplicate hash entries')
    # An unregistered executable file can run inside an otherwise frozen
    # apparatus, so absence from the manifest is an error, never a skip.
    try:
        failures.extend(f'unregistered apparatus file: {name}'
                        for name in unknown_apparatus_files())
    except OSError as error:
        failures.append(f'cannot scan apparatus: {error}')
    for expected, name in entries:
        path = (folder / name).resolve()
        if not path.is_relative_to(ROOT) or not path.is_file() or digest(path) != expected:
            failures.append(name)
    # Membership in A2_SCRIPTS is not registration: a whitelisted-but-unhashed
    # apparatus file would run while its bytes are unverified. Only files that
    # actually exist are required, so trimmed-down test trees stay valid.
    registered = {name for _, name in entries}
    unregistered = sorted(f'../../scripts/gateway-parity/{name}' for name in A2_SCRIPTS
                          if (SCRIPT / name).is_file()
                          and f'../../scripts/gateway-parity/{name}' not in registered)
    if unregistered:
        failures.append('A2 apparatus missing FROZEN hash: ' + ', '.join(unregistered))
    try:
        failures.extend(check_diffs(folder, ROOT))
    except (OSError, UnicodeError) as error:
        failures.append(f'cannot read ledger: {error}')
    notes = []
    if args.base:
        failures.extend(check_append_only(ROOT, args.base, notes))
    if args.print_ledger and not failures:
        sys.stdout.buffer.write(ledger_bytes(folder))
        return 0
    report = {'status': 'FAIL' if failures else 'PASS', 'failures': failures, 'entries': len(entries)}
    if notes:
        report['notes'] = notes
    print(json.dumps(report, ensure_ascii=False))
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(main())
