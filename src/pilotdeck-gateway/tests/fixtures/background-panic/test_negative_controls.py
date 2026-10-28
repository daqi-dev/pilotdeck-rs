# anchor-selfcheck: unsupported mutation helper unit tests on in-memory strings/JSON, no product source replacement table
"""Stale controls must stop before mutating sources or launching cargo."""
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    'background_panic_negative', Path(__file__).with_name('negative_controls.py'))
negative = importlib.util.module_from_spec(spec)
spec.loader.exec_module(negative)


class AnchorTests(unittest.TestCase):
    def test_source_anchor_missing_or_ambiguous(self):
        for content in ['renamed', 'anchor anchor']:
            with self.subTest(content=content), self.assertRaisesRegex(RuntimeError, 'stale source anchor'):
                negative.mutate('probe', content, 'anchor', 'changed')
        self.assertEqual(negative.mutate('probe', 'anchor', 'anchor', 'changed'), 'changed')

    def test_fixture_anchor_missing(self):
        content = json.dumps({'cases': [{'id': 'renamed'}]})
        with self.assertRaisesRegex(RuntimeError, 'stale fixture anchor'):
            negative.mutate('probe', content, None, ('write-a-', 'delete'))
        restored = json.dumps({'cases': [{'id': 'write-a-0-before-sync'}]})
        self.assertEqual(json.loads(negative.mutate('probe', restored, None,
                                                  ('write-a-', 'delete')))['cases'], [])

    def test_noop_mutation_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError, 'mutation made no change'):
            negative.mutate('probe', 'anchor', 'anchor', 'anchor')


if __name__ == '__main__':
    unittest.main()
