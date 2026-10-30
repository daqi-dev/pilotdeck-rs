#!/usr/bin/env python3
# anchor-selfcheck: unsupported data-only registration unit tests; no checkout source mutations
"""#1089 registration unit tests on synthetic data, not an apparatus CLI.

Only the named pure registration functions are compiled from the real source.
No bootstrap, HEAD verifier, real checker CLI, or source admission is mocked or
replaced. Git responses are explicit unit inputs for temporary data directories;
this is not a checkout and cannot run apparatuses. Real CLI checks are separate.
"""
import ast
import hashlib
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
KIND = sys.argv.pop(1)
if KIND not in ('parity', 'gateway-parity', 'ui-server-parity'):
    raise SystemExit('#1089 unknown apparatus')
SOURCE = ROOT / f'scripts/{KIND}/check_frozen.py'
SELECTED = {'manifest_row', 'manifest_path', 'render_row', 'manifest_parts',
            'manifest_entries', 'regular_file', 'generated_manifest', 'write_manifest',
            'check_manifest', 'read_events', 'check_registration', 'git',
            'EVENT_FIELDS', 'EVENT_NAME', 'CHANGE', 'APPARATUS_KIND'}
checker = types.ModuleType('registration_unit')
checker.__dict__.update(Path=Path, re=re, subprocess=subprocess, hashlib=hashlib,
                        tempfile=tempfile, ROOT=ROOT)
source = ast.parse(SOURCE.read_bytes(), filename=str(SOURCE))
nodes = [node for node in source.body if (
    isinstance(node, ast.FunctionDef) and node.name in SELECTED) or (
    isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name)
    and node.targets[0].id in SELECTED)]
exec(compile(ast.Module(body=nodes, type_ignores=[]), str(SOURCE), 'exec'), checker.__dict__)


class Manifest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-frozen-1089-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.folder = self.root / f'specs/{KIND}'
        self.events = self.folder / 'frozen'
        self.events.mkdir(parents=True)
        self.names = ('a.txt', 'z.txt') if KIND != 'ui-server-parity' else (
            '../../specs/ui-server-parity/a.txt', '../../specs/ui-server-parity/z.txt')
        self.inventory = {name: checker.manifest_path(name) for name in self.names}
        self.files = {name: self.root / relative for name, relative in self.inventory.items()}
        for name, path in self.files.items():
            path.write_text(name)
        self.blobs = {relative: (self.root / relative).read_bytes() for relative in self.inventory.values()}
        self.manifest = self.folder / 'FROZEN.md'
        self.raw = b'# historical\r\n\n' + b''.join(self.row(name) for name in self.names) + b'\nold prose\r\n'
        self.manifest.write_bytes(self.raw)
        self.event_blobs = {}
        self.addCleanup(patch.stopall)
        patch.object(checker, 'manifest_inventory', return_value=self.inventory, create=True).start()
        patch.object(checker, 'git', side_effect=self.git).start()

    def row(self, name):
        return checker.render_row(name, hashlib.sha256(self.files[name].read_bytes()).hexdigest(),
                                  b' | historical date | historical note |\n' if KIND == 'parity' else b'')

    def git(self, root, *args):
        self.assertEqual(root, self.root, '#1089 unit git must stay in synthetic data')
        if args == ('merge-base', 'HEAD', 'base-ref'):
            return b'base-commit'
        if args == ('show', f'base-commit:specs/{KIND}/FROZEN.md'):
            return self.raw
        if args[0] == 'show' and args[1].startswith('base-commit:'):
            return self.blobs[args[1].split(':', 1)[1]]
        if args == ('ls-tree', '-rz', 'base-commit', '--', f'specs/{KIND}/frozen/'):
            return b''.join(f'100644 blob {index}\tspecs/{KIND}/frozen/{name}\0'.encode()
                            for index, name in enumerate(self.event_blobs))
        if args[:2] == ('cat-file', 'blob'):
            return list(self.event_blobs.values())[int(args[2])]
        self.fail('#1089 unexpected git protocol: ' + repr(args))

    def errors(self):
        return '\n'.join(checker.check_registration(self.root, 'base-ref'))

    def change(self, name=None):
        name = name or self.names[0]
        relative = self.inventory[name]
        old = hashlib.sha256(self.blobs[relative]).hexdigest()
        self.files[name].write_bytes(b'new bytes for ' + name.encode())
        new = hashlib.sha256(self.files[name].read_bytes()).hexdigest()
        checker.write_manifest(self.root)
        return relative, old, new

    def event(self, changes, name='20261030-1089.md'):
        text = ('# Registration unit input\nCalibration-Evidence: evidence.json\n'
                'Independent-Reviews: DeepSeek: review-ds.md; GLM: review-glm.md\n'
                'Approved-By: AI 代批，待人复核（协调者）\n'
                'Approval-Basis: unit input, not an actual approval\n')
        text += ''.join(f'Change: {old} {new} {path}\n' for path, old, new in changes)
        path = self.events / name
        path.write_text(text)
        return path

    def test_write_preserves_history_and_is_deterministic(self):
        self.manifest.write_bytes(b'# historical\r\n\n' + b''.join(self.row(n) for n in reversed(self.names)) + b'\nold prose\r\n')
        checker.write_manifest(self.root)
        self.assertEqual(self.manifest.read_bytes(), self.raw, '#1089 sorted list keeps all other bytes')
        checker.write_manifest(self.root)
        self.assertEqual(self.manifest.read_bytes(), self.raw, '#1089 write is idempotent')
        self.assertEqual(checker.check_manifest(self.root), [], '#1089 canonical list passes')

    def test_hash_drift_and_manual_list(self):
        self.assertEqual(checker.check_manifest(self.root), [], '#1089 canonical positive')
        self.files[self.names[0]].write_bytes(b'changed source')
        self.assertIn('differs from generated', '\n'.join(checker.check_manifest(self.root)), '#1089 source drift rejected')
        checker.write_manifest(self.root)
        self.assertEqual(checker.check_manifest(self.root), [], '#1089 regenerated drift consistent')
        raw = self.manifest.read_bytes()
        pin = hashlib.sha256(self.files[self.names[0]].read_bytes()).hexdigest().encode()
        self.manifest.write_bytes(raw.replace(pin, b'0' * 64))
        self.assertIn('differs from generated', '\n'.join(checker.check_manifest(self.root)), '#1089 manual hash rejected')

    def test_missing_duplicate_and_reordered_rows(self):
        row = self.row(self.names[0])
        for raw in (self.raw.replace(row, b''), self.raw.replace(row, row + row),
                    self.raw.replace(b''.join(self.row(n) for n in self.names), b''.join(self.row(n) for n in reversed(self.names)))):
            self.manifest.write_bytes(raw)
            self.assertTrue(checker.check_manifest(self.root), '#1089 missing duplicate or reordered row rejected')

    def test_conflicts(self):
        row = self.row(self.names[0])
        conflict = b'<<<<<<< ours\n' + row + b'=======\n' + row + b'>>>>>>> theirs\n'
        self.manifest.write_bytes(self.raw.replace(row, conflict))
        self.assertTrue(checker.check_manifest(self.root), '#1089 unresolved conflict rejected')
        checker.write_manifest(self.root)
        self.assertEqual(self.manifest.read_bytes(), self.raw, '#1089 list conflict regenerated')
        self.manifest.write_bytes(b'<<<<<<< outside\nprose\n=======\ntext\n>>>>>>> other\n' + self.raw)
        with self.assertRaisesRegex(ValueError, 'conflict outside', msg='#1089 prose conflict must not be overwritten'):
            checker.write_manifest(self.root)

    def test_regular_files(self):
        target = self.files[self.names[0]]
        target.unlink()
        target.symlink_to(self.files[self.names[1]])
        self.assertIn('nonregular', '\n'.join(checker.check_manifest(self.root)), '#1089 symlink source rejected')
        target.unlink()
        target.mkdir()
        self.assertIn('nonregular', '\n'.join(checker.check_manifest(self.root)), '#1089 directory source rejected')
        target.rmdir()
        self.assertIn('nonregular', '\n'.join(checker.check_manifest(self.root)), '#1089 missing source rejected')

    def test_event_required_and_bound_to_hashes(self):
        self.assertEqual(self.errors(), '', '#1089 unchanged baseline needs no event')
        change = self.change()
        self.assertIn('lacks exact new event chain', self.errors(), '#1089 changed file without event rejected')
        event = self.event([change])
        self.assertEqual(self.errors(), '', '#1089 exact new event passes')
        for wrong in ((change[0], '0' * 64, change[2]), (change[0], change[1], '0' * 64),
                      (self.inventory[self.names[1]], change[1], change[2])):
            self.event([wrong])
            self.assertIn('lacks exact new event chain', self.errors(), '#1089 wrong old new or path rejected')
        self.event([change])
        self.event_blobs[event.name] = event.read_bytes()
        self.assertIn('lacks exact new event chain', self.errors(), '#1089 existing event cannot cover new change')

    def test_event_chain(self):
        path, old, new = self.change()
        middle = '1' * 64
        self.event([(path, old, middle)])
        self.event([(path, middle, new)], '20261030-1089-r2.md')
        self.assertEqual(self.errors(), '', '#1089 sequential event chain passes')
        self.event([(path, old, new)], '20261030-1089-extra.md')
        self.assertIn('lacks exact new event chain', self.errors(), '#1089 branch or duplicate event rejected')

    def test_history_and_existing_events(self):
        change = self.change()
        event = self.event([change])
        self.assertEqual(self.errors(), '', '#1089 initial event positive')
        self.manifest.write_bytes(self.manifest.read_bytes() + b'new prose\n')
        self.assertIn('historical prose changed', self.errors(), '#1089 historical prose drift rejected')
        self.manifest.write_bytes(checker.generated_manifest(self.root, self.raw))
        self.event_blobs[event.name] = event.read_bytes()
        for operation in ('edit', 'executable', 'delete', 'symlink'):
            if event.exists() or event.is_symlink():
                event.unlink()
            if operation == 'symlink':
                event.symlink_to(self.manifest)
            elif operation != 'delete':
                event.write_bytes(self.event_blobs[event.name] + (b'edited' if operation == 'edit' else b''))
                if operation == 'executable':
                    event.chmod(0o755)
            self.assertIn('append-only', self.errors(), '#1089 existing event edit mode deletion alias rejected')

    def test_event_fields(self):
        event = self.event([self.change()])
        good = event.read_text()
        for field in checker.EVENT_FIELDS:
            for bad in (re.sub(r'^' + field + r':.*\n', '', good, flags=re.M),
                        re.sub(r'^' + field + r':.*$', field + ':', good, flags=re.M),
                        good + field + ': duplicate\n'):
                event.write_text(bad)
                self.assertIn(field, self.errors(), '#1089 event required field rejected')
        event.write_text(good.replace('DeepSeek: review-ds.md; GLM: review-glm.md', 'one review'))
        self.assertIn('two distinct', self.errors(), '#1089 two review references required')

    def test_event_names_and_changes(self):
        event = self.event([self.change()])
        good = event.read_bytes()
        for bad in (b'Change: no hashes', b'Change: ' + b'0'*64 + b' ' + b'1'*64 + b' ../escape',
                    b'Change: ' + b'0'*64 + b' ' + b'0'*64 + b' scripts/file'):
            event.write_bytes(re.sub(rb'Change:[^\n]*', bad, good))
            self.assertIn('Change', self.errors(), '#1089 malformed change row rejected')
        event.write_bytes(good)
        event.rename(self.events / 'bad-name.md')
        self.assertIn('invalid event filename', self.errors(), '#1089 malformed event filename rejected')

    def test_row_metadata(self):
        if KIND != 'parity':
            self.skipTest('#1089 only parity has historical table columns')
        self.manifest.write_bytes(self.raw.replace(b'historical note', b'rewritten approval', 1))
        self.assertIn('historical row metadata changed', self.errors(), '#1089 table prose drift rejected')

    def test_duplicate_parser(self):
        row = self.row(self.names[0])
        with self.assertRaisesRegex(ValueError, 'duplicate', msg='#1089 duplicate parser rejected'):
            checker.manifest_entries(self.raw.replace(row, row + row))

    def test_writer_head_binding(self):
        self.assertEqual(checker.generated_manifest(self.root, self.raw, head='base-commit'), self.raw,
                         '#1089 committed writer bytes admitted')
        self.files[self.names[0]].write_bytes(b'dirty writer input')
        with self.assertRaisesRegex(ValueError, 'differs from HEAD', msg='#1089 dirty writer source rejected'):
            checker.generated_manifest(self.root, self.raw, head='base-commit')
        self.assertEqual(self.manifest.read_bytes(), self.raw, '#1089 rejected writer leaves manifest untouched')

    def test_addition_and_deletion(self):
        name = 'm.txt' if KIND != 'ui-server-parity' else '../../specs/ui-server-parity/m.txt'
        relative = checker.manifest_path(name)
        path = self.root / relative
        path.write_bytes(b'new apparatus input')
        self.inventory[name] = relative
        checker.write_manifest(self.root)
        new = hashlib.sha256(path.read_bytes()).hexdigest()
        self.assertIn('lacks exact new event chain', self.errors(), '#1089 unregistered addition rejected')
        self.event([(relative, '-', new)])
        self.assertEqual(self.errors(), '', '#1089 registered addition passes')
        removed = self.names[0]
        relative = self.inventory.pop(removed)
        old = hashlib.sha256(self.blobs[relative]).hexdigest()
        self.files[removed].unlink()
        checker.write_manifest(self.root)
        self.assertIn('lacks exact new event chain', self.errors(), '#1089 unregistered deletion rejected')
        self.event([(relative, old, '-')], '20261030-delete.md')
        self.assertEqual(self.errors(), '', '#1089 registered deletion passes')

    def test_sibling_helpers_and_cli_abbreviations(self):
        selected = {'manifest_parts', 'manifest_entries', 'regular_file', 'generated_manifest',
                    'write_manifest', 'check_manifest', 'read_events', 'check_registration'}
        variants = []
        for kind in ('parity', 'gateway-parity', 'ui-server-parity'):
            tree = ast.parse((ROOT / f'scripts/{kind}/check_frozen.py').read_bytes())
            variants.append({n.name: ast.dump(n, include_attributes=False) for n in tree.body
                             if isinstance(n, ast.FunctionDef) and n.name in selected})
            parsers = [n for n in ast.walk(tree) if isinstance(n, ast.Call)
                       and isinstance(n.func, ast.Attribute) and n.func.attr == 'ArgumentParser']
            main = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == 'main')
            writes = [n for n in ast.walk(main) if isinstance(n, ast.Call)
                      and isinstance(n.func, ast.Name) and n.func.id == 'write_manifest']
            self.assertEqual(len(writes), 1, '#1089 one writer dispatch per apparatus')
            self.assertTrue(any(k.arg == 'head' and isinstance(k.value, ast.Name)
                                for k in writes[0].keywords), '#1089 production writer requires HEAD binding')
            self.assertEqual(len(parsers), 1, '#1089 one CLI parser per apparatus')
            self.assertTrue(any(k.arg == 'allow_abbrev' and isinstance(k.value, ast.Constant)
                                and k.value.value is False for k in parsers[0].keywords),
                            '#1089 CLI abbreviations must not diverge from exact bootstrap dispatch')
        self.assertEqual(variants[0], variants[1], '#1089 parity/gateway shared helper drift')
        self.assertEqual(variants[1], variants[2], '#1089 gateway/UI shared helper drift')

    def test_base_failures(self):
        with patch.object(checker, 'git', side_effect=subprocess.CalledProcessError(128, ['git'])):
            self.assertIn('cannot verify registration base', self.errors(), '#1089 unavailable base rejected')
        name = self.names[0]
        digest = hashlib.sha256(self.files[name].read_bytes()).hexdigest().encode()
        self.raw = self.raw.replace(digest, b'0'*64)
        self.assertIn('base manifest does not match', self.errors(), '#1089 corrupt baseline pin rejected')


if __name__ == '__main__':
    unittest.main()
