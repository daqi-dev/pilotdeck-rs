import unittest
from check_frozen import self_test, required_files, ROOT, SPEC
from common import apparatus_hashes


class FrozenControls(unittest.TestCase):
    def test_capture_binds_all_registered_files_except_approval_manifest(self):
        hashes = apparatus_hashes()
        self.assertNotIn('specs/ui-server-parity/FROZEN.md', hashes)
        self.assertTrue({str(p.relative_to(ROOT)) for p in required_files(ROOT, SPEC)} <= set(hashes))

    def test_positive_draft_and_negative_manifest_controls(self):
        report = self_test()
        self.assertEqual(report['verdict'], 'PASS', report)
