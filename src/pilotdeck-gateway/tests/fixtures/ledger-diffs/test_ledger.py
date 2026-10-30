#!/usr/bin/env python3
# anchor-selfcheck: unsupported ledger data controls with guarded historical-byte mutation, no checker source mutations
"""#1088 data controls; git protocol mocks are unit inputs, never a shadow repo.

The real checker is loaded at its real path, including its isolation/bootstrap.
Only check_append_only's git responses are mocked in temporary data directories.
Real CLI smoke and guarded one-byte legacy controls exercise the real checkout.
Run with exclusive ownership of this tree: concurrent checker readers must not
observe deliberate negative data. CI runs these commands sequentially.
"""
import ast
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
KIND = sys.argv.pop(1)
if KIND not in ('parity', 'ui-server-parity'):
    raise SystemExit('#1088 expected parity or ui-server-parity')
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

CHECKER = ROOT / f'scripts/{KIND}/check_frozen.py'
SPEC = Path(f'specs/{KIND}')
spec = importlib.util.spec_from_file_location('ledger_checker', CHECKER)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
ENTRY = 'C-LEDGER-TEST-001'
FIELDS = ('类别', '基线依据', 'Rust 行为', '范围', '机器判据', '批准记录')


def entry(name=ENTRY, original=None):
    front = f'---\nid: {name}\n'
    if original is not None:
        front += f'original: {original}\n'
    return front + '---\n\n' + '\n\n'.join(f'## {field}\n\ninput {field}' for field in FIELDS) + '\n'


class Ledger(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-ledger-1088-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.folder = self.root / SPEC
        self.diffs = self.folder / 'diffs'
        self.diffs.mkdir(parents=True)
        self.legacy = self.folder / 'accepted-diffs.md'
        self.legacy.write_bytes((ROOT / SPEC / 'accepted-diffs.md').read_bytes())
        self.manifest = self.folder / 'FROZEN.md'
        self.manifest.write_bytes((ROOT / SPEC / 'FROZEN.md').read_bytes())
        self.before = self.legacy.read_bytes()
        self.before_manifest = self.manifest.read_bytes()
        self.base, self.head, self.tip = 'base-commit', 'head-commit', 'tip-commit'
        self.blobs = {}
        self.git_calls = []

    def add(self, name=ENTRY, original=None, text=None):
        path = self.diffs / f'{name}.md'
        path.write_text(entry(name, original) if text is None else text, encoding='utf-8')
        return path

    def valid(self):
        self.assertEqual(checker.check_diffs(self.folder, self.root), [], '#1088 valid ledger')

    def invalid(self, reason):
        self.assertIn(reason, '\n'.join(checker.check_diffs(self.folder, self.root)),
                      '#1088 invalid ledger must report ' + reason)

    def fake_git(self, root, *args):
        # Only explicit protocol fixtures; unexpected commands fail the unit.
        self.assertEqual(root, self.root, '#1088 git mock must not touch actual checkout')
        self.git_calls.append(args)
        if args == ('merge-base', 'HEAD', 'base-ref'):
            return self.base.encode()
        if args == ('rev-parse', 'HEAD'):
            return self.head.encode()
        if args == ('rev-parse', '--verify', 'base-ref^{commit}'):
            return self.tip.encode()
        if args == ('show', f'{self.base}:{SPEC}/accepted-diffs.md'):
            return self.before
        if args == ('show', f'{self.base}:{SPEC}/FROZEN.md'):
            return self.before_manifest
        if args == ('ls-tree', '-rz', self.base, '--', f'{SPEC}/diffs/'):
            return b''.join(b'100644 blob ' + str(i).encode() + b'\t' +
                            str(SPEC / 'diffs' / name).encode() + b'\0'
                            for i, name in enumerate(self.blobs))
        if args[:2] == ('cat-file', 'blob'):
            return list(self.blobs.values())[int(args[2])]
        self.fail('#1088 unexpected git protocol request: ' + repr(args))

    def append_errors(self, notes=None):
        with patch.object(checker, 'git', self.fake_git):
            return checker.check_append_only(self.root, 'base-ref', notes)

    def test_ledger_order(self):
        self.assertEqual(checker.ledger_bytes(self.folder), self.before, '#1088 empty union exact bytes')
        later = self.add('C-LEDGER-TEST-002')
        earlier = self.add()
        (self.diffs / 'README.md').write_text('not data')
        self.valid()
        self.assertEqual(checker.ledger_bytes(self.folder),
                         b'\n\n'.join([self.before, earlier.read_bytes(), later.read_bytes()]),
                         '#1088 legacy first, filename order, README excluded')

    def test_fields(self):
        self.add()
        self.valid()
        for field in FIELDS:
            for bad in (entry().replace(f'## {field}\n', '## omitted\n'),
                        entry().replace(f'input {field}', '')):
                with self.subTest(field=field, bad=bad):
                    self.add(text=bad)
                    self.invalid(f'missing/empty field {field}')
        self.add(text=entry() + '\n## 类别\n\nsecond\n')
        self.invalid('duplicate section 类别')
        self.add(text=entry().replace('## 基线依据', '## TS 依据'))
        self.valid()

    def test_frontmatter(self):
        for bad in (entry().replace(ENTRY, 'C-WRONG-001'), entry().split('---\n', 2)[-1],
                    entry().replace('---\nid:', '---\nid: duplicate\nid:'),
                    entry().replace('---\nid:', '---\nunknown: field\nid:')):
            self.add(text=bad)
            self.invalid('frontmatter')

    def test_names_types_and_encoding(self):
        for name in ('c-TEST-001.md', 'C-TEST-1.md', 'C-TEST-001-R0.md',
                     'C-TEST-001-R01.md', 'C-TEST-001.txt'):
            with self.subTest(name=name):
                path = self.diffs / name
                path.write_text(entry())
                self.invalid('invalid entry filename or file type')
                path.unlink()
        path = self.add()
        path.write_bytes(b'\xff')
        self.invalid('utf-8')
        path.unlink()
        path.symlink_to(self.legacy)
        self.invalid('invalid entry filename or file type')
        path.unlink()
        path.mkdir()
        self.invalid('invalid entry filename or file type')

    def test_corrections(self):
        self.add()
        self.add(ENTRY + '-R1', ENTRY)
        self.valid()  # -R sorts before the base .md.
        for original in (None, 'C-MISSING-001'):
            self.add(ENTRY + '-R1', original)
            self.invalid('original must name the existing base ID')
        (self.diffs / (ENTRY + '.md')).unlink()
        self.add(ENTRY + '-R1', ENTRY)
        self.invalid('original must name the existing base ID')
        self.legacy.write_bytes(self.before + f'\n## {ENTRY}\n\nhistorical input\n'.encode())
        self.valid()
        self.add()
        self.invalid('ID already exists in historical ledger')
        self.legacy.write_bytes(self.before)
        self.add(original=ENTRY)
        self.invalid('original is only valid for corrections')

    def test_numeric_history(self):
        if KIND != 'parity':
            self.skipTest('#1088 numeric historical IDs exist only in parity')
        self.add('1-R1', '1')
        self.valid()
        for name, original in (('1-R1', '2'), ('7-R1', '7'), ('01-R1', '01'),
                               ('1-R0', '1'), ('1-R01', '1'), ('1', None)):
            with self.subTest(name=name, original=original):
                for path in self.diffs.iterdir():
                    path.unlink()
                self.add(name, original)
                self.assertTrue(checker.check_diffs(self.folder, self.root),
                                '#1088 numeric corrections require an existing exact base ID')

    def test_shared_grammar(self):
        # Independent frozen apparatuses keep local code; flag accidental drift.
        selected = {'ENTRY_ID', 'ENTRY_NAME', 'FIELDS', 'ledger_files', 'ledger_bytes',
                    'check_diffs', 'git'}
        def grammar(kind):
            source = (ROOT / f'scripts/{kind}/check_frozen.py').read_text()
            if kind == 'parity':
                # The only C-specific grammar: corrections of historical table IDs.
                source = source.replace("    ids.update(re.findall(r'^\\| ([1-9][0-9]*) \\|', old, re.M))\n", '')
                source = source.replace('ENTRY_NAME.fullmatch(path.name) or LEGACY_CORRECTION_NAME.fullmatch(path.name)',
                                        'ENTRY_NAME.fullmatch(path.name)')
            nodes = []
            for node in ast.parse(source).body:
                name = node.name if isinstance(node, ast.FunctionDef) else (
                    node.targets[0].id if isinstance(node, ast.Assign) and
                    isinstance(node.targets[0], ast.Name) else None)
                if name in selected:
                    nodes.append(ast.dump(node, include_attributes=False))
            return nodes
        self.assertEqual(grammar(KIND), grammar('gateway-parity'),
                         '#1088 shared ledger grammar drift must be explicit')

    def test_approval_pointer(self):
        pointer = 'state/decisions-20261030-test.md'
        self.add(text=entry() + f'\n`{pointer}:12`\n')
        self.invalid('missing approval file')
        (self.root / 'state').mkdir()
        (self.root / pointer).write_text('test input, not an approval')
        self.valid()

    def test_append_only_bytes(self):
        self.assertEqual(self.append_errors(), [], '#1088 unchanged baseline')
        for data in (b'!' + self.before[1:], self.before + b'\nappend\n',
                     self.before.replace(b'\n', b'\r\n', 1), self.before[:-1]):
            self.legacy.write_bytes(data)
            self.assertIn('historical ledger differs', '\n'.join(self.append_errors()),
                          '#1088 old ledger byte drift rejected')
        self.legacy.unlink()
        self.assertIn('accepted-diffs.md:1:', '\n'.join(self.append_errors()),
                      '#1088 deleted legacy rejected at first line')

    def test_hash_row(self):
        if KIND == 'parity':
            row = checker.LEGACY_FULL_ROW
        else:
            row = next(row for row in self.before_manifest.splitlines(keepends=True)
                       if row.rstrip().endswith(b'  ../../specs/ui-server-parity/accepted-diffs.md'))
        for changed in (b'', row + row, row.replace(b'\n', b'\r\n'), b' ' + row):
            self.manifest.write_bytes(self.before_manifest.replace(row, changed))
            self.assertIn('hash line changed/absent/duplicate', '\n'.join(self.append_errors()),
                          '#1088 legacy hash row byte drift rejected')
        self.manifest.write_bytes(self.before_manifest + b'\nother approved registration\n')
        self.assertEqual(self.append_errors(), [], '#1088 other registration allowed')

    def test_header_migration(self):
        if KIND != 'parity':
            self.skipTest('#1088 only parity has a historical header hash')
        self.before_manifest = self.before_manifest.replace(checker.LEGACY_FULL_ROW, checker.LEGACY_HEADER_ROW)
        self.assertEqual(self.append_errors(), [], '#1088 exact approved migration allowed')
        self.legacy.write_bytes(b'!' + self.before[1:])
        self.assertIn('historical ledger differs', '\n'.join(self.append_errors()),
                      '#1088 migration does not allow legacy edits')
        self.legacy.write_bytes(self.before)
        self.manifest.write_bytes(self.manifest.read_bytes().replace(checker.LEGACY_FULL_ROW,
                                                                    checker.LEGACY_FULL_ROW.replace(b'12ca', b'ffff', 1)))
        self.assertIn('hash line changed', '\n'.join(self.append_errors()),
                      '#1088 migration hash must be the exact approved row')
        self.manifest.write_bytes(self.before_manifest)
        self.before_manifest = (ROOT / SPEC / 'FROZEN.md').read_bytes()
        self.assertIn('hash line changed', '\n'.join(self.append_errors()),
                      '#1088 migration cannot be reversed')

    def test_existing_files(self):
        for name in (ENTRY + '.md', 'README.md'):
            path = self.diffs / name
            path.write_text(entry() if name != 'README.md' else 'frozen rules')
            original = path.read_bytes()
            self.blobs = {name: original}
            self.assertEqual(self.append_errors(), [], '#1088 unchanged existing file allowed')
            path.write_bytes(original + b'!')
            self.assertIn('append-only', '\n'.join(self.append_errors()), '#1088 changed entry rejected')
            path.write_bytes(original)
            path.chmod(0o755)
            self.assertIn('append-only', '\n'.join(self.append_errors()), '#1088 executable entry rejected')
            path.unlink()
            self.assertIn('append-only', '\n'.join(self.append_errors()), '#1088 deletion/rename rejected')
            path.symlink_to(self.legacy)
            self.assertIn('append-only', '\n'.join(self.append_errors()), '#1088 symlink rejected')
            path.unlink()

    def test_base_edges(self):
        self.base = self.head
        self.assertIn('base 与 HEAD 同源', '\n'.join(self.append_errors()), '#1088 descendant base refused')
        self.tip = self.head
        notes = []
        self.assertEqual(self.append_errors(notes), [], '#1088 exact HEAD allowed')
        self.assertTrue(notes, '#1088 exact HEAD explains worktree byte verification')
        self.legacy.write_bytes(self.before + b'!')
        self.assertIn('historical ledger differs', '\n'.join(self.append_errors()), '#1088 exact HEAD still checks bytes')
        with patch.object(checker, 'git', side_effect=subprocess.CalledProcessError(128, ['git'])):
            self.assertIn('cannot verify append-only', '\n'.join(checker.check_append_only(self.root, 'bad')),
                          '#1088 unavailable base fails closed')

    def test_full_file_hash(self):
        if KIND == 'parity':
            self.manifest.write_bytes(checker.LEGACY_FULL_ROW)
            # #1089 full-inventory checks have separate fixtures; this case binds only the ledger digest.
            self.assertTrue(checker.check(self.root)['checked']['accepted-diffs.md']['ok'], '#1088 full-file hash positive')
            self.legacy.write_bytes(self.before + b'!')
            self.assertFalse(checker.check(self.root)['checked']['accepted-diffs.md']['ok'],
                             '#1088 full ledger hash must fail')
        else:
            # Data-only inventory fixture; the real module bootstrap already ran.
            # No checkout, fake git, or loaded-source guard is replaced.
            digest = hashlib.sha256(self.before).hexdigest()
            self.manifest.write_text('# B2（待定标）\n' + digest + '  ../../specs/ui-server-parity/accepted-diffs.md\n')
            with patch.object(checker, 'required_files', return_value={self.legacy}):
                self.assertTrue(checker.check(self.folder, root=self.root)['hashes_consistent'],
                                '#1088 full-file hash positive')
                self.legacy.write_bytes(self.before + b'!')
                self.assertFalse(checker.check(self.folder, root=self.root)['hashes_consistent'],
                                 '#1088 full ledger hash must fail')

    def test_real_cli(self):
        for args in ([], ['--base', 'HEAD']):
            result = run_owned([sys.executable, '-I', str(CHECKER), *args], cwd=ROOT, timeout=60)
            self.assertEqual(result.returncode, 0, '#1088 real CLI baseline: ' + result.stdout + result.stderr)
        result = run_owned([sys.executable, '-I', str(CHECKER), '--print-ledger'], cwd=ROOT, timeout=60)
        self.assertEqual(result.returncode, 0, '#1088 print-ledger baseline: ' + result.stdout + result.stderr)
        self.assertEqual(result.stdout.encode(), checker.ledger_bytes(ROOT / SPEC), '#1088 real CLI union bytes')
        for base in ('refs/does-not-exist-1088', ''):
            result = run_owned([sys.executable, '-I', str(CHECKER), '--base', base], cwd=ROOT, timeout=60)
            self.assertNotEqual(result.returncode, 0, '#1088 real invalid base rejected')
            self.assertIn('cannot verify append-only', result.stdout, '#1088 real invalid base reason')

    def test_real_missing_field_negative(self):
        # A new input record, not a replacement source or fake repository.
        path = ROOT / SPEC / 'diffs' / 'C-LEDGER-NEGATIVE-998.md'
        data = entry('C-LEDGER-NEGATIVE-998').replace('## 机器判据\n', '## omitted\n').encode()
        with mutating_sources(ROOT, [SPEC / 'accepted-diffs.md']):
            with path.open('xb') as stream:
                stream.write(data)
            try:
                result = run_owned([sys.executable, '-I', str(CHECKER)], cwd=ROOT, timeout=60)
                print(f'#1088 {KIND} missing-field exit={result.returncode}\n{result.stdout}{result.stderr}')
                self.assertEqual(result.returncode, 1, '#1088 missing field must fail CLI')
                self.assertIn('missing/empty field 机器判据', result.stdout,
                              '#1088 missing field failure is bound to its field')
            finally:
                if path.read_bytes() != data:
                    raise RuntimeError('#1088 input record changed; preserving it for owner')
                path.unlink()
        restored = run_owned([sys.executable, '-I', str(CHECKER)], cwd=ROOT, timeout=60)
        self.assertEqual(restored.returncode, 0, '#1088 removed invalid input restores PASS')

    def test_real_legacy_byte_negative(self):
        relative = SPEC / 'accepted-diffs.md'
        with mutating_sources(ROOT, [relative]) as originals:
            (ROOT / relative).write_bytes(b'!' + originals[relative][1:])
            result = run_owned([sys.executable, '-I', str(CHECKER)], cwd=ROOT, timeout=60)
            print(f'#1088 {KIND} one-byte legacy exit={result.returncode}\n{result.stdout}{result.stderr}')
            self.assertNotEqual(result.returncode, 0, '#1088 one-byte legacy mutation must fail')
            self.assertIn('accepted-diffs.md', result.stdout + result.stderr, '#1088 fail is bound to legacy hash')
        restored = run_owned([sys.executable, '-I', str(CHECKER)], cwd=ROOT, timeout=60)
        self.assertEqual(restored.returncode, 0, '#1088 restored legacy must pass: ' + restored.stdout + restored.stderr)


if __name__ == '__main__':
    unittest.main()
