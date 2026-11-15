# anchor-selfcheck: unsupported unit tests of the verdict tail, no product source mutations
#!/usr/bin/env python3
"""#3178 item 7 (#2973 sibling) + fix-r1 (boundary #3): the session-domain
verdict tail must refuse a zero-row run without publishing a summary.

The old tail ``0 if counts['KILLED'] == len(results) else 1`` is vacuously
true on an empty row set — a lost mutation table or a misparsed selection
finishes with ``0 == 0`` and exits 0, the all-green code. The family contract
(fixtures/README.md「零行永不返回 0」) is ``negative_selection.verdict_exit``:
empty is a failure. #3178 fix-r1 additionally pins the boundary #3 shape: an
empty selection is a never-started run — rc=2 and **no** ``summary.json`` at
all, because a summary that exists reads like a completed run to anyone who
only checks for the artifact. The twin drives the delivered ``main()``
in-process to an empty finish — ``MUTATIONS`` emptied, the source guard and
the output-directory admission faked (a real ``mutating_sources`` lease is the
coordinator's post-commit run, not a unit twin) — and asserts the exit is 2
and the evidence directory holds no summary. No cargo, no lease, no product
source writes.

Run with:

    python3 -B -m unittest -v session-domain-lock-release/test_negative_controls.py
"""
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


def load_runner():
    source = Path(__file__).with_name('negative_controls.py')
    spec = importlib.util.spec_from_file_location(
        'session_domain_lock_release_negative', source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ZeroRowVerdictControls(unittest.TestCase):
    """A finished run with zero rows must exit 2 unpublished, never green."""

    def test_empty_run_exits_two_without_a_summary(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory(prefix='pdrs-session-lock-zero-') as temp:
            out = Path(temp) / 'run'

            class Guard:
                def __enter__(self):
                    return {}

                def __exit__(self, *exc):
                    return False

            with patch.object(runner, 'MUTATIONS', []), \
                    patch.object(runner, 'mutating_sources',
                                 lambda root, paths, **kwargs: Guard()), \
                    patch.object(sys, 'argv',
                                 ['negative_controls.py', '--output', str(out)]):
                code = runner.main()
            # #3178 fix-r2 (DS concurrency#1): the real admission ran above
            # (the old fake_output_directory twin shape kept the .claim O_EXCL
            # path out of the twin), so the refusal's withdrawal is pinned on
            # the real claim: nothing may remain, and a re-run of the same
            # output path must be admitted again instead of refused on the
            # leftover never-ran directory (#1060/#953).
            leftover = sorted(path.name for path in out.iterdir()) \
                if out.exists() else []
            try:
                runner.new_output_directory(out)
                refused, reclaimed = None, (out / '.claim').is_file()
            except SystemExit as error:
                refused, reclaimed = str(error), False
        message = ('#3178 a zero-row run must exit 2 and publish no summary '
                   '(the old tail read 0 == 0 as green and an empty '
                   'summary.json read like a completed run)')
        self.assertEqual(code, 2, message)
        self.assertEqual(leftover, [],
                         message + ' (the never-started run must withdraw its '
                         'claim: the evidence directory must not exist or be '
                         'empty; a leftover .claim refused the same path on '
                         're-run)')
        self.assertIsNone(refused,
                           message + f' (a re-run of the same output path must '
                           f'be admitted, not refused: {refused})')
        self.assertTrue(reclaimed,
                        message + ' (the re-admitted evidence directory must '
                        'hold a fresh claim)')


if __name__ == '__main__':
    unittest.main()
