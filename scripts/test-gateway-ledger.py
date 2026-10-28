#!/usr/bin/env python3
"""Real-ledger regressions in bounded, disposable git repositories (no Cargo)."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = Path('specs/gateway-parity')
CHECKER = Path('scripts/gateway-parity/check_frozen.py')
ENTRY = 'A7.2-LEDGER-TEST-001'
FIELDS = ('类别', '基线依据', 'Rust 行为', '范围', '机器判据', '批准记录')


def entry_text(name=ENTRY, original=None):
    # Synthetic validation input, never a TS behavior golden or accepted diff.
    header = f'---\nid: {name}\n'
    if original is not None:
        header += f'original: {original}\n'
    return header + '---\n\n' + '\n\n'.join(
        f'## {field}\n\nsynthetic validation input ({field})' for field in FIELDS) + '\n'


class Ledger(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-ledger-581-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.folder = self.root / SPEC
        self.diffs = self.folder / 'diffs'
        self.diffs.mkdir(parents=True)
        registered = ['acceptance.md', 'normalization.md', 'accepted-diffs.md',
                      '../../scripts/gateway-parity/_guard.py',
                      '../../scripts/gateway-parity/check_frozen.py',
                      '../../scripts/gateway-parity/common.py']
        lines = []
        for name in registered:
            data = (ROOT / SPEC / name).read_bytes()
            target = self.folder / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            lines.append(f'{hashlib.sha256(data).hexdigest()}  {name}\n')
        (self.folder / 'FROZEN.md').write_text(''.join(lines))
        self.git('init', '-b', 'main')
        self.commit('base with real historical ledger')
        self.base = self.git('rev-parse', 'HEAD').stdout.decode().strip()

    def git(self, *args):
        return subprocess.run(['git', '-C', str(self.root), *args], check=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)

    def commit(self, message):
        self.git('add', '.')
        self.git('-c', 'user.name=Ledger Test', '-c', 'user.email=ledger@example.invalid',
                 '-c', 'core.hooksPath=/dev/null', 'commit', '-m', message)

    def add(self, name=ENTRY, original=None):
        self.diffs.mkdir(parents=True, exist_ok=True)
        path = self.diffs / f'{name}.md'
        path.write_text(entry_text(name, original))
        return path

    def check(self, *args, ok=True, reason=None):
        result = subprocess.run([sys.executable, '-I', str(self.root / CHECKER), *args],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
        self.assertEqual(result.returncode, 0 if ok else 1,
                         result.stdout.decode(errors='replace') + result.stderr.decode())
        if reason:
            self.assertIn(reason, result.stdout.decode())
        return result.stdout

    def freeze_entry(self):
        path = self.add()
        self.commit('freeze existing diff')
        return path, self.git('rev-parse', 'HEAD').stdout.decode().strip()

    def test_empty_union_preserves_real_bytes_and_check_output(self):
        self.assertEqual(self.check('--print-ledger'),
                         (ROOT / SPEC / 'accepted-diffs.md').read_bytes())
        report = json.loads(self.check('--base', self.base))
        self.assertEqual(json.loads(self.check()), {k: v for k, v in report.items() if k != 'notes'})
        self.assertIn('notes', report)
        self.assertIn('base 解析出的提交等于 HEAD', report['notes'][0])

    def test_new_files_sorted_and_readme_excluded_without_shared_edits(self):
        later = self.add('A7.2-LEDGER-TEST-002')
        earlier = self.add()
        (self.diffs / 'README.md').write_text('documentation, not ledger data')
        self.check('--base', self.base)
        self.assertEqual(self.check('--print-ledger'), b'\n\n'.join([
            (self.folder / 'accepted-diffs.md').read_bytes(),
            earlier.read_bytes(), later.read_bytes()]))
        self.assertEqual(self.git('diff', '--name-only').stdout, b'')

    def test_correction_of_real_historical_title(self):
        self.add('A4-FILE-TOCTOU-001-R1', 'A4-FILE-TOCTOU-001')
        self.check('--base', self.base)

    def test_correction_of_new_file_even_when_sorted_before_original(self):
        self.add()
        self.add(ENTRY + '-R1', ENTRY)
        self.check('--base', self.base)

    def test_missing_each_of_six_fields(self):
        for field in FIELDS:
            with self.subTest(field=field):
                self.add().write_text(entry_text().replace(f'## {field}\n', '## omitted\n'))
                self.check(ok=False, reason=f'missing/empty field {field}')

    def test_empty_and_duplicate_sections(self):
        self.add().write_text(entry_text().replace('synthetic validation input (类别)', ''))
        self.check(ok=False, reason='missing/empty field 类别')
        self.add().write_text(entry_text() + '\n## 类别\n\nextra\n')
        self.check(ok=False, reason='duplicate section 类别')

    def test_ts_basis_alias(self):
        self.add().write_text(entry_text().replace('## 基线依据', '## TS 依据'))
        self.check()

    def test_bad_names(self):
        for name in ('a7-TEST-001', 'A7-TEST-001-R0', 'A7-TEST-001-R01',
                     'A7-TEST-1', 'A7_TEST_001', 'A7-TEST-001.txt'):
            with self.subTest(name=name):
                path = self.diffs / (name if name.endswith('.txt') else name + '.md')
                path.write_text(entry_text(name))
                self.check(ok=False, reason='invalid entry filename')
                path.unlink()

    def test_bad_or_duplicate_frontmatter_id(self):
        for text in (entry_text().replace(f'id: {ENTRY}', 'id: A7-OTHER-001'),
                     entry_text().replace('---\nid:', '---\nid: duplicate\nid:'),
                     entry_text().split('---\n', 2)[-1]):
            self.add().write_text(text)
            self.check(ok=False, reason='frontmatter')

    def test_correction_missing_wrong_or_nonexistent_original(self):
        for original in (None, 'A7-MISSING-001', ENTRY):
            with self.subTest(original=original):
                self.add(ENTRY + '-R1', original)
                self.check(ok=False, reason='original must name the existing base ID')

    def test_wrong_original_even_when_target_exists(self):
        self.add()
        self.add(ENTRY + '-R1', 'A4-FILE-TOCTOU-001')
        self.check(ok=False, reason='original must name the existing base ID')

    def test_original_forbidden_on_noncorrection(self):
        self.add(original='A4-FILE-TOCTOU-001')
        self.check(ok=False, reason='original is only valid for corrections')

    def test_duplicate_historical_id(self):
        self.add('A4-FILE-TOCTOU-001')
        self.check(ok=False, reason='ID already exists in historical ledger')

    def test_approval_decision_pointer(self):
        path = self.add()
        pointer = 'state/decisions-20261023.md'
        path.write_text(entry_text() + f'\nDecision: `{pointer}:25`\n')
        self.check(ok=False, reason='missing approval file ' + pointer)
        (self.root / 'state').mkdir()
        (self.root / pointer).write_text('synthetic decision file, not an approval')
        self.check()

    def test_edit_existing_file(self):
        path, base = self.freeze_entry()
        data = path.read_bytes()
        path.write_bytes(data.replace(b'synthetic', b'Synthetic', 1))
        self.check('--base', base, ok=False, reason='append-only')
        self.check()  # Static-only mode intentionally has no historical comparison.

    def test_delete_existing_file(self):
        path, base = self.freeze_entry()
        path.unlink()
        self.check('--base', base, ok=False, reason='append-only')

    def test_rename_existing_file(self):
        path, base = self.freeze_entry()
        path.unlink()
        self.add('A7.2-LEDGER-TEST-002')
        self.check('--base', base, ok=False, reason='append-only')

    def test_readme_is_also_append_only(self):
        path = self.diffs / 'README.md'
        path.write_text('base rules')
        self.commit('freeze README')
        path.write_text('altered rules')
        self.check('--base', 'HEAD', ok=False, reason='append-only')

    def test_merge_base_instead_of_base_tip(self):
        self.git('switch', '-c', 'other')
        self.add('A7.2-LEDGER-OTHER-001')
        self.commit('independent entry on advanced base')
        self.git('switch', 'main')
        self.add()
        self.commit('independent entry on current branch')
        self.check('--base', 'other')

    def test_descendant_base_fails_closed(self):
        self.git('switch', '-c', 'advanced-base')
        self.add()
        self.commit('base has advanced beyond HEAD')
        self.git('switch', 'main')
        self.assertEqual(self.git('merge-base', 'HEAD', 'advanced-base').stdout.strip(),
                         self.git('rev-parse', 'HEAD').stdout.strip())
        self.check('--base', 'advanced-base', ok=False,
                   reason='base 与 HEAD 同源，只追加对照无效')

    def test_exact_head_base_passes_with_note(self):
        # An annotated tag must resolve to its commit, not its tag object.
        self.git('-c', 'user.name=Ledger Test', '-c', 'user.email=ledger@example.invalid',
                 'tag', '-a', 'same-head', '-m', 'resolve tag to commit')
        for base in ('HEAD', self.base, 'main', 'same-head'):
            with self.subTest(base=base):
                report = json.loads(self.check('--base', base))
                self.assertEqual(report['status'], 'PASS')
                self.assertIn('notes', report)
                self.assertIn('base 解析出的提交等于 HEAD', report['notes'][0])
                self.assertEqual(self.check('--base', base, '--print-ledger'),
                                 (self.folder / 'accepted-diffs.md').read_bytes())

    def test_exact_head_base_still_checks_worktree(self):
        path = self.folder / 'accepted-diffs.md'
        path.write_bytes(path.read_bytes() + b'\nunauthorized append\n')
        self.register_legacy_hash()
        self.check()
        self.check('--base', 'HEAD', ok=False, reason='historical ledger differs from base')

    def test_utf8_input_independent_of_locale(self):
        manifest = self.folder / 'FROZEN.md'
        manifest.write_bytes('# 冻结登记\n'.encode('utf-8') + manifest.read_bytes())
        env = {**os.environ, 'LC_ALL': 'C', 'PYTHONUTF8': '0', 'PYTHONCOERCECLOCALE': '0',
               'PYTHONIOENCODING': 'utf-8'}
        # The checker now refuses to run without -I (A5.4 pkg2 fix-r7): under
        # isolated mode the hostile PYTHON* variables are ignored by design, so
        # what remains to prove is (a) a non-isolated run is refused rather than
        # silently trusting that environment, and (b) the isolated run still
        # reads the UTF-8 manifest under a C locale.
        refused = subprocess.run([sys.executable, str(self.root / CHECKER), '--base', self.base],
                                 capture_output=True, timeout=30, env=env)
        self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
        self.assertIn(b'-I', refused.stdout + refused.stderr)
        result = subprocess.run([sys.executable, '-I', str(self.root / CHECKER), '--base', self.base],
                                capture_output=True, timeout=30, env=env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout)['status'], 'PASS')

    def test_invalid_base_fails_closed(self):
        self.check('--base', 'nonexistent-ref', ok=False, reason='cannot verify append-only')

    def test_static_mode_does_not_require_git_history(self):
        (self.root / '.git').rename(self.root / 'saved-git')
        self.check()
        self.assertEqual(self.check('--print-ledger'),
                         (ROOT / SPEC / 'accepted-diffs.md').read_bytes(),
                         '#166 static ledger bytes must remain available without HEAD')
        self.check('--base', self.base, ok=False, reason='cannot verify append-only')

    def test_static_mode_without_history_still_checks_frozen_bytes(self):
        (self.root / '.git').rename(self.root / 'saved-git')
        path = self.folder / 'accepted-diffs.md'
        path.write_bytes(path.read_bytes() + b'\nchanged\n')
        self.check(ok=False, reason='accepted-diffs.md')

    def test_static_mode_in_checkout_still_rejects_loaded_source_changes(self):
        path = self.root / 'scripts/gateway-parity/common.py'
        path.write_bytes(path.read_bytes() + b'\n# changed after commit\n')
        result = subprocess.run([sys.executable, '-I', str(self.root / CHECKER)],
                                capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 2,
                         '#166 checkout static mode must reject loaded source changes: '
                         + repr(result.stdout + result.stderr))
        self.assertIn(b'#166 loaded apparatus differs from HEAD', result.stdout,
                      '#166 checkout static mode must report the provenance refusal')

    def test_static_mode_with_broken_git_or_unborn_head_fails_closed(self):
        (self.root / '.git').rename(self.root / 'saved-git')
        marker = self.root / '.git'
        for kind in ('unborn', 'invalid-file', 'dangling-symlink'):
            with self.subTest(kind=kind):
                if kind == 'unborn':
                    self.git('init', '-b', 'main')
                elif kind == 'invalid-file':
                    marker.write_text('gitdir: missing-git-dir\n')
                else:
                    marker.symlink_to(self.root / 'missing-git-dir')
                result = subprocess.run([sys.executable, '-I', str(self.root / CHECKER)],
                                        capture_output=True, timeout=30)
                self.assertEqual(result.returncode, 2,
                                 '#166 damaged checkout metadata must fail closed: '
                                 + repr(result.stdout + result.stderr))
                self.assertIn(b'"status": "BLOCKED"', result.stdout,
                              '#166 damaged checkout metadata must report BLOCKED')
                marker.rename(self.root / ('saved-' + kind))

    def test_legacy_hash_still_rejects_one_byte_change(self):
        path = self.folder / 'accepted-diffs.md'
        data = path.read_bytes()
        path.write_bytes(b'!' + data[1:])
        self.check(ok=False, reason='accepted-diffs.md')

    def register_legacy_hash(self):
        path = self.folder / 'accepted-diffs.md'
        manifest = self.folder / 'FROZEN.md'
        rows = manifest.read_text().splitlines(keepends=True)
        manifest.write_text(''.join(
            f'{hashlib.sha256(path.read_bytes()).hexdigest()}  accepted-diffs.md\n'
            if row.split()[-1] == 'accepted-diffs.md' else row for row in rows))

    def test_legacy_append_and_reregister_cannot_bypass_base(self):
        path = self.folder / 'accepted-diffs.md'
        original = path.read_bytes()
        path.write_bytes(original + b'\nunauthorized legacy append\n')
        self.register_legacy_hash()
        self.check()  # Reproduce the old bypass: self-consistent static hashes.
        line = original.count(b'\n') + 1
        output = self.check('--base', self.base, ok=False,
                            reason=f'accepted-diffs.md:{line}:')
        self.assertIn(b'accepted-diffs.md hash line changed', output)

    def test_legacy_first_differing_line_and_byte_changes(self):
        path = self.folder / 'accepted-diffs.md'
        original = path.read_bytes()
        offset = original.index(b'\n') + 1
        for changed, line in ((original[:offset] + b'!' + original[offset + 1:], 2),
                              (original.replace(b'\n', b'\r\n', 1), 1),
                              (original[:-1], original.count(b'\n'))):
            with self.subTest(line=line):
                path.write_bytes(changed)
                self.register_legacy_hash()
                self.check('--base', self.base, ok=False,
                           reason=f'accepted-diffs.md:{line}:')

    def test_legacy_deletion_fails_at_first_line(self):
        (self.folder / 'accepted-diffs.md').unlink()
        self.check('--base', self.base, ok=False, reason='accepted-diffs.md:1:')

    def test_legacy_hash_row_is_byte_frozen_even_with_same_digest(self):
        path = self.folder / 'FROZEN.md'
        path.write_bytes(path.read_bytes().replace(b'  accepted-diffs.md\n',
                                                   b'\taccepted-diffs.md\n'))
        self.check()  # The digest and historical bytes still match.
        self.check('--base', self.base, ok=False, reason='accepted-diffs.md hash line changed')

    def test_unchanged_legacy_allows_other_frozen_registration(self):
        checker = self.root / CHECKER
        checker.write_bytes(checker.read_bytes() + b'\n# approved registration test\n')
        manifest = self.folder / 'FROZEN.md'
        rows = manifest.read_text().splitlines(keepends=True)
        name = '../../scripts/gateway-parity/check_frozen.py'
        manifest.write_text('new registration note\n' + ''.join(
            f'{hashlib.sha256(checker.read_bytes()).hexdigest()}  {name}\n'
            if row.split()[-1] == name else row for row in rows))
        self.check('--base', self.base)

    def test_only_new_diff_preserves_legacy_and_hash_row(self):
        manifest = (self.folder / 'FROZEN.md').read_bytes()
        legacy = (self.folder / 'accepted-diffs.md').read_bytes()
        self.add()
        self.check('--base', self.base)
        self.assertEqual((self.folder / 'FROZEN.md').read_bytes(), manifest)
        self.assertEqual((self.folder / 'accepted-diffs.md').read_bytes(), legacy)

    def test_file_types_and_mode_changes(self):
        path, base = self.freeze_entry()
        path.chmod(0o755)
        self.check('--base', base, ok=False, reason='append-only')
        path.unlink()
        path.symlink_to(self.folder / 'accepted-diffs.md')
        self.check(ok=False, reason='invalid entry filename or file type')
        self.check('--base', base, ok=False, reason='append-only')

    def test_parallel_additions_merge_without_conflicts(self):
        self.git('switch', '-c', 'left')
        self.add()
        self.commit('left adds one entry')
        self.git('switch', '-c', 'right', self.base)
        self.add('A7.2-LEDGER-TEST-002')
        self.commit('right adds a distinct entry')
        result = self.git('-c', 'user.name=Ledger Test',
                          '-c', 'user.email=ledger@example.invalid',
                          '-c', 'core.hooksPath=/dev/null', 'merge', '--no-edit', 'left')
        self.assertNotIn(b'CONFLICT', result.stdout)
        print('\nParallel branch merge transcript:\n' + result.stdout.decode(), flush=True)
        self.check('--base', self.base)
        self.assertEqual(len(list(self.diffs.glob('*.md'))), 2)
        self.assertEqual(self.git('diff', '--name-only', self.base).stdout.decode().splitlines(),
                         [str(SPEC / 'diffs' / f'{ENTRY}.md'),
                          str(SPEC / 'diffs' / 'A7.2-LEDGER-TEST-002.md')])


if __name__ == '__main__':
    unittest.main()
