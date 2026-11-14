# anchor-selfcheck: unsupported unit tests of the verdict tail, no product source mutations
#!/usr/bin/env python3
"""#3178 fix-r1 (sibling #2): the dirty-tree control's verdict tail must not go
vacuously green on a controlless run.

``dirty_tree_control_1555.py`` was the third structural twin the review named:
its old tail ``restored and not mode_lost and counts['KILLED'] == len(rows)``
read ``0 == 0`` as green when the two control calls were deleted — a run that
checked nothing would exit 0, the all-green code. The family contract
(fixtures/README.md「零行永不返回 0」) is
``negative_selection.verdict_exit``: an empty row set is a failure. The twin
drives the delivered ``main()`` in-process for the positive shape, then re-drives
a mutated copy whose two ``rows.append(...)`` control calls are deleted (the
realistic bad edit) and asserts the verdict tail still exits 1 — reverting the
tail to the old counts comparison makes the controlless copy exit 0 and this
twin red. No cargo, no lock, no product source writes — the real controls are
the coordinator's post-commit run.

Run with:

    python3 -B -m unittest -v user-dialog-http-store-1555/test_dirty_tree_control_1555.py
"""
import contextlib
import importlib.util
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

# The mutated copy is exec'd from a temp directory; pre-seed the shared helpers
# so its `from negative_process import ...` resolves from sys.modules instead
# of the (temp) directory it inserts into sys.path.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import negative_process  # noqa: E402
import negative_selection  # noqa: E402,F401  (same modules the runner imports)

CONTROL_CALLS = ('        rows.append(lock_check(out, env))\n'
                 '        rows.append(dirty_check(out, env, original))\n')


def _load(source_text):
    with tempfile.TemporaryDirectory(prefix='pdrs-dirty-tree-1555-') as temp:
        mutated = Path(temp) / 'dirty_tree_control_1555.py'
        mutated.write_text(source_text, encoding='utf-8')
        spec = importlib.util.spec_from_file_location(
            'dirty_tree_control_1555_negative', mutated)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
    return module


def _drive(runner):
    temp = tempfile.mkdtemp(prefix='pdrs-dirty-tree-1555-')
    out = Path(temp) / 'run'
    with patch.dict(os.environ,
                    {'CARGO_TARGET_DIR': str(Path(temp) / 'target')}), \
            patch.object(runner, '_head_bytes', lambda *args: b'original'), \
            patch.object(runner, 'restore_locked',
                         # The restoration predicates are the runner's own and
                         # stay honest; only the controls differ per drive.
                         lambda source, original: (True, False, '')), \
            patch.object(sys, 'argv',
                         ['dirty_tree_control_1555.py', str(out)]), \
            contextlib.redirect_stdout(io.StringIO()):
        code = runner.main()
    return code, out


class ControllessVerdictControls(unittest.TestCase):
    """A run with zero recorded controls must exit 1, never the green code."""

    def test_a_both_killed_run_still_exits_zero(self):
        runner = _load(Path(__file__).with_name('dirty_tree_control_1555.py')
                       .read_text(encoding='utf-8'))
        killed = dict(control='probe', status='KILLED')
        with patch.object(runner, 'lock_check', lambda out, env: dict(killed)), \
                patch.object(runner, 'dirty_check',
                             lambda out, env, original: dict(killed)):
            code, _ = _drive(runner)
        self.assertEqual(
            code, 0,
            '#3178 an honestly all-KILLED run must keep the green code (the '
            'verdict tail may only punish an empty or non-green row set)')

    def test_a_controlless_run_exits_one_not_zero(self):
        source = Path(__file__).with_name('dirty_tree_control_1555.py') \
            .read_text(encoding='utf-8')
        self.assertEqual(source.count(CONTROL_CALLS), 1,
                         '#3178 the two control calls must stay a unique anchor '
                         'for the controlless mutation')
        controlless = _load(source.replace(CONTROL_CALLS, '        pass\n'))
        code, out = _drive(controlless)
        message = ('#3178 a run whose control calls were deleted must exit 1 '
                   '(the old tail read counts[\'KILLED\'] == len(rows) as '
                   '0 == 0 and returned the all-green code)')
        self.assertEqual(code, 1, message)
        self.assertTrue((out / 'summary.json').is_file(),
                        message + ' (the diagnostic record is still published; '
                        'only the verdict changed)')


if __name__ == '__main__':
    unittest.main()
