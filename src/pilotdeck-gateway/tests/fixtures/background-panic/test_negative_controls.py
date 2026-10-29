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


class JudgeTests(unittest.TestCase):
    TEST = 'snapshot_write_a::zero_before_sync'
    FULL = 'runtime::background::panic_tests::' + TEST
    ANCHOR = '#818 subagent'

    def log(self, marker, thread=None):
        return (f"test {self.FULL} ... FAILED\n"
                f"thread '{thread or self.FULL}' panicked at src/x.rs:1:1:\n"
                f'{marker}\n'
                'test result: FAILED. 0 passed; 1 failed\n')

    def test_named_panic_block_kills(self):
        self.assertTrue(negative.judge(101, self.log(self.ANCHOR), self.TEST, self.ANCHOR))

    def test_sibling_panic_with_the_same_marker_is_rejected(self):
        # #818/#930 defect channel: the marker is in another test's block.
        self.assertFalse(negative.judge(
            101, self.log(self.ANCHOR, 'sibling::test'), self.TEST, self.ANCHOR))

    def test_stdout_marker_is_rejected(self):
        raw = (f'test {self.FULL} ... FAILED\n{self.ANCHOR}\n'
               'test result: FAILED. 0 passed; 1 failed\n')
        self.assertFalse(negative.judge(101, raw, self.TEST, self.ANCHOR))


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
