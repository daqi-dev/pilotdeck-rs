#!/usr/bin/env python3
"""Mutation controls for the independent CI provenance guard (no ruler edits)."""
import sys
if not (sys.flags.isolated and sys.flags.ignore_environment and sys.flags.no_user_site
        and sys.pycache_prefix is None):
    raise SystemExit('Python isolation required: use python3 -I')
sys.dont_write_bytecode = True

import hashlib
from pathlib import Path
import runpy
import tempfile
import unittest

check = runpy.run_path(str(Path(__file__).with_name('check-approval-provenance.py')))['check']
NAMES = ('calibration-summary.json', 'review-glm.md', 'review-claude.md', 'driver.log')


class ProvenanceControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.approval = self.root / 'specs/ui-server-parity/approval'
        self.approval.mkdir(parents=True)
        self.manifest = self.approval.parent / 'FROZEN.md'
        lines = []
        for name in NAMES:
            content = ('original:' + name).encode()
            (self.approval / name).write_bytes(content)
            lines.append(f'Approval-Source-SHA256: {hashlib.sha256(content).hexdigest()}  '
                         f'specs/ui-server-parity/approval/{name}')
        paths = ['specs/ui-server-parity/approval/' + name for name in NAMES]
        lines += ['Approval-Basis: ' + '; '.join(paths[:3]),
                  'Independent-Reviews: ' + '; '.join(paths[1:3]),
                  'Calibration-Evidence: ' + paths[0] + '; ' + paths[3]]
        self.original = '\n'.join(lines) + '\n'
        self.manifest.write_text(self.original)

    def rejected(self):
        self.assertEqual(check(self.root)['verdict'], 'FAIL')

    def test_four_originals_pass(self):
        self.assertEqual(check(self.root), {'verdict': 'PASS', 'checkedSources': 4, 'errors': []})

    def test_each_changed_original_fails(self):
        for name in NAMES:
            with self.subTest(name=name):
                path = self.approval / name
                content = path.read_bytes()
                path.write_bytes(content + b' changed')
                self.rejected()
                path.write_bytes(content)

    def test_each_missing_original_fails(self):
        for name in NAMES:
            with self.subTest(name=name):
                path = self.approval / name
                content = path.read_bytes()
                path.unlink()
                self.rejected()
                path.write_bytes(content)

    def test_empty_and_duplicate_registrations_fail(self):
        self.manifest.write_text('')
        self.rejected()
        self.manifest.write_text(self.original + self.original.splitlines()[0] + '\n')
        self.rejected()

    def test_unknown_path_and_malformed_digest_fail(self):
        self.manifest.write_text(self.original + 'Approval-Source-SHA256: ' + 'a' * 64 + '  ../outside\n')
        self.rejected()
        self.manifest.write_text(self.original.replace('Approval-Source-SHA256: ', 'Approval-Source-SHA256: z', 1))
        self.rejected()

    def test_basis_and_review_references_cannot_bypass_hashed_sources(self):
        lines = self.original.splitlines()
        for field in ['Approval-Basis', 'Independent-Reviews']:
            with self.subTest(field=field):
                self.manifest.write_text('\n'.join(
                    (field + ': arbitrary-existing-file') if line.startswith(field + ':') else line
                    for line in lines) + '\n')
                self.rejected()

    def test_calibration_driver_cannot_point_back_to_tmp(self):
        self.manifest.write_text(self.original.replace(
            'Calibration-Evidence: specs/ui-server-parity/approval/calibration-summary.json; '
            'specs/ui-server-parity/approval/driver.log',
            'Calibration-Evidence: specs/ui-server-parity/approval/calibration-summary.json; _tmp/driver.log'))
        self.rejected()

    def test_symlinked_approval_directory_fails(self):
        moved = self.root / 'original-directory'
        self.approval.rename(moved)
        self.approval.symlink_to(moved, target_is_directory=True)
        self.rejected()

    def test_symlinked_original_or_manifest_fails(self):
        source = self.approval / NAMES[0]
        outside = self.root / 'outside'
        outside.write_bytes(source.read_bytes())
        source.unlink()
        source.symlink_to(outside)
        self.rejected()
        source.unlink()
        source.write_bytes(outside.read_bytes())
        self.manifest.unlink()
        outside.write_text(self.original)
        self.manifest.symlink_to(outside)
        self.rejected()


if __name__ == '__main__':
    unittest.main(verbosity=2)
