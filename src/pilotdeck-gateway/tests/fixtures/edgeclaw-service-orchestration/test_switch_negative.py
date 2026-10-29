#!/usr/bin/env python3
# anchor-selfcheck: unsupported tests of the orchestration switch control, no product source mutations
"""#1036 failure-path tests for the switches-table runner; no cargo runs.

The runner only delegates to the shared precompiled executor, so these tests
pin the audited declaration (the literal table against the real tree) and the
delegation contract; the executor's own failure paths are covered by
``_shared/test_mutant_switch.py`` (#996).
"""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import switch_negative as runner


class SwitchNegativeTests(unittest.TestCase):
    def test_declared_table_binds_the_real_switch_site(self):
        # The literal table is the checker-audited declaration: its single row
        # must point at the production switch site (exactly one occurrence)
        # and carry a nonempty test and assertion binding.
        self.assertEqual([row[0] for row in runner.switches], ['dream-tail-per-call-locks'])
        name, path, test, anchor = runner.switches[0]
        self.assertEqual(path, runner.SWITCH_SOURCE)
        self.assertEqual(path.read_text().count(f'::mutants::on("{name}")'), 1,
                         '#1036 the declared switch must exist exactly once in the target')
        self.assertEqual(test, 'dream_tail_race::dream_tail_cannot_interleave_across_threads')
        self.assertIn('#1036 tail race: the workspace root operation lock was free', anchor)

    def test_check_preflight_passes_on_the_real_tree(self):
        with patch.object(sys, 'argv', ['switch_negative.py', '--check']):
            self.assertEqual(runner.main(), 0, '#1036 --check must pass against the real tree')

    def test_check_rejects_extra_arguments(self):
        with tempfile.TemporaryDirectory() as temp:
            with patch.object(sys, 'argv', ['switch_negative.py', '--check', temp]):
                self.assertEqual(runner.main(), 2)

    def test_output_and_target_dir_are_required(self):
        # A caller that already exports CARGO_TARGET_DIR must not let the
        # output-only invocation reach the real executor either.
        for argv in (['switch_negative.py'], ['switch_negative.py', 'out']):
            with patch.object(sys, 'argv', argv), \
                    patch.dict(os.environ, {}, clear=False) as env:
                env.pop('CARGO_TARGET_DIR', None)
                self.assertEqual(runner.main(), 2)
        self.assertFalse(Path('out').exists(),
                         '#1036 a rejected invocation must not create the output')

    def test_selection_must_be_a_declared_switch(self):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / 'out'
            with patch.object(sys, 'argv', ['switch_negative.py', str(out), 'missing']), \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': temp}):
                self.assertEqual(runner.main(), 2)
            self.assertFalse(out.exists(), 'a rejected selection must not create the output')

    def test_run_receives_the_audited_rows_and_denominator(self):
        captured = {}

        def fake_run(root, output, chosen, env, *, guarded, script, requested, **_):
            captured.update(root=root, output=output, rows=chosen, env=env,
                            guarded=guarded, script=script, requested=requested)
            return 0

        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / 'out'
            stale = dict(os.environ, CARGO_TARGET_DIR=temp, PDRS_MUTANT='stale-selector')
            with patch.object(runner.mutant_switch, 'run', side_effect=fake_run), \
                    patch.object(sys, 'argv',
                                 ['switch_negative.py', str(out), 'dream-tail-per-call-locks']), \
                    patch.dict(os.environ, stale, clear=True):
                self.assertEqual(runner.main(), 0)
        declared = dict(zip(('name', 'path', 'test', 'anchor'), runner.switches[0]))
        self.assertEqual(captured['rows'], [declared],
                         '#1036 the executor must consume the audited table row')
        self.assertEqual(captured['root'], runner.ROOT)
        self.assertEqual(captured['output'], out.resolve())
        self.assertEqual(captured['script'], Path(runner.__file__).resolve(),
                         '#1036 the denominator check must reconcile against this script')
        self.assertEqual(captured['requested'], ['dream-tail-per-call-locks'])
        self.assertNotIn('PDRS_MUTANT', captured['env'],
                         '#1036 a stale selector must not reach the executor environment')
        self.assertIn(runner.SWITCH_SOURCE, captured['guarded'])
        self.assertIn(runner.TEST_SOURCE, captured['guarded'])
        self.assertTrue(all(Path(item).is_file() for item in captured['guarded']),
                        '#1036 every guarded input must exist')


if __name__ == '__main__':
    unittest.main()
