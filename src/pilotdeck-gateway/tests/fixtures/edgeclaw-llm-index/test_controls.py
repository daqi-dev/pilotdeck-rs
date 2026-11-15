# anchor-selfcheck: unsupported runner unit tests using an in-memory emptied table, no product mutation definitions
"""#3178 fix-r5 (DS sibling#2) controls: an emptied case table must refuse.

The delivered runner is module-level: after its tables it validates the
anchors and then answers ``--check-anchors`` with a green ``0 unique
anchors; no mutation`` (rc=0) — the vacuum green r4 closed for
edgeclaw-llm-recall. The refusal now precedes both. The twin executes the
delivered script once with its case table and the sibling ``messages``
dict emptied by an in-memory source edit; no cargo run, no slot wait and
no product source write happens here, and the read_text patch proves the
refusal fires before any anchor read.

Run with:

    python3 -B edgeclaw-llm-index/test_controls.py
"""
import contextlib
import io
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

here = Path(__file__).resolve().parent
if str(here.parents[0]) not in sys.path:
    sys.path.insert(0, str(here.parents[0]))


class EmptyTableControls(unittest.TestCase):

    def test_an_emptied_case_table_refuses_the_check_anchors_green_exit(self):
        script = here / 'negative.py'
        text = script.read_text()
        for before, after in (
                ('cases = [', 'cases = []\n_twin_emptied_cases = ['),
                ('messages = {', 'messages = {}\n_twin_emptied_messages = {')):
            count = text.count(before)
            self.assertEqual(count, 1,
                             f'{script}: anchor {before!r} occurs {count} times')
            text = text.replace(before, after, 1)
        output, errors = io.StringIO(), io.StringIO()
        argv = sys.argv
        sys.argv = [str(script), '--check-anchors']
        try:
            with patch.object(Path, 'read_text', autospec=True,
                              side_effect=AssertionError(
                                  '#3178 the refusal must precede the anchor '
                                  'read')), \
                 contextlib.redirect_stdout(output), \
                 contextlib.redirect_stderr(errors):
                with self.assertRaises(SystemExit) as stopped:
                    exec(compile(text, str(script), 'exec'),
                         {'__name__': 'llm_index_empty_table_twin',
                          '__file__': str(script)})
        finally:
            sys.argv = argv
        message = ('#3178 an emptied case table must refuse before the '
                   '--check-anchors green exit (the old preflight validated '
                   'zero anchors and exited 0)')
        self.assertEqual(stopped.exception.code, 2,
                         message + ' (the refusal must exit 2)')
        self.assertIn('EMPTY CASE TABLE', errors.getvalue(),
                      message + ' (the refusal reason must reach stderr)')
        self.assertNotIn('unique anchors', output.getvalue(),
                         message + ' (the green preflight line must never print)')


if __name__ == '__main__':
    unittest.main()
