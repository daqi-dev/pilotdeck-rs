#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the coexistence kill judge, no product source mutations
"""#1087 unit tests for the node --test TAP-block kill criterion.

The coexistence runner judges a mutant KILLED only when the dedicated marker
lands inside a failing subtest's own TAP diagnostic block. The shape below is
the real `node --test` output of the runner's Node (v22.22.0), captured by
_tmp/20261111-issue-1087-1061/probe-node-tap.mjs: a failing subtest's block is
the indented ``---``/``...`` region after its ``not ok N - <name>`` line, while
a sibling's console.log lands on unindented top-level ``# comment`` lines.
Run with:

    python3 -B -m unittest -v fe-coexist/test_negative_controls.py
"""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import negative_controls  # noqa: E402


TAP = """TAP version 13
# printed marker from a passing sibling
# Subtest: dirty-tracked
not ok 1 - dirty-tracked
  ---
  duration_ms: 0.591667
  type: 'test'
  failureType: 'testCodeFailure'
  error: |-
    baseline tracked changes present

    1 !== 2

  code: 'ERR_ASSERTION'
  name: 'AssertionError'
  expected: 2
  actual: 1
  stack: |-
    TestContext.<anonymous> (file:///probe.mjs:5:10)
  ...
# Subtest: passing sibling that prints the marker
ok 2 - passing sibling that prints the marker
  ---
  duration_ms: 0.260875
  type: 'test'
  ...
# Subtest: failing sibling with a different marker
not ok 3 - failing sibling with a different marker
  ---
  duration_ms: 0.117375
  type: 'test'
  error: |-
    some other marker entirely

    3 !== 4

  stack: |-
    TestContext.<anonymous> (file:///probe.mjs:13:10)
  ...
1..3
# tests 3
# pass 1
# fail 2
"""


def kill(log, marker):
    return any(marker in block for block in negative_controls.failing_subtest_blocks(log))


class TAPBlockKillCriterion(unittest.TestCase):
    def test_marker_in_the_failing_block_kills(self):
        self.assertTrue(kill(TAP, 'baseline tracked changes present'),
                        '#1087 the marker inside a failing subtest\'s own '
                        'diagnostic block is a kill')

    def test_sibling_stdout_marker_is_not_a_kill(self):
        # The negative case the #1087 ticket demands: a sibling subtest prints
        # the marker to stdout (the top-level `# printed marker from a passing
        # sibling` comment), and the bound marker never appears in any failing
        # subtest's diagnostic block. That must not be judged KILLED.
        self.assertFalse(kill(TAP, 'printed marker from a passing sibling'),
                         '#1087 a marker printed to stdout by a sibling '
                         'subtest is not a kill')
        self.assertFalse(kill(TAP, 'passing sibling that prints the marker'),
                         '#1087 a passing subtest\'s name printed on a '
                         'top-level TAP line is not a kill')

    def test_other_failures_marker_is_not_this_rows_kill(self):
        self.assertTrue(kill(TAP, 'some other marker entirely'),
                        '#1087 a different failing subtest\'s own marker is a kill')
        self.assertFalse(kill(TAP, 'a marker that appears nowhere'),
                         '#1087 an absent marker never kills')

    def test_failing_blocks_exclude_passing_diagnostics(self):
        blocks = negative_controls.failing_subtest_blocks(TAP)
        self.assertEqual(len(blocks), 2,
                         '#1087 only the two failing subtests contribute blocks')
        self.assertNotIn('0.260875', '\n'.join(blocks),
                         '#1087 the passing subtest\'s diagnostic block never counts')


if __name__ == '__main__':
    unittest.main()
