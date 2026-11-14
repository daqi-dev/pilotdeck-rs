# anchor-selfcheck: unsupported unit tests of the verdict tail, no product source mutations
#!/usr/bin/env python3
"""#3178 item 7 (#2973 sibling) + fix-r1 (boundary #3): the parity-lock verdict
tail must refuse a zero-row run without publishing a summary.

The old tail ``0 if counts['KILLED'] == len(results) else 1`` is vacuously
true on an empty row set — a lost mutation table or a misparsed selection
finishes with ``0 == 0`` and exits 0, the all-green code. The family contract
(fixtures/README.md「零行永不返回 0」) is ``negative_selection.verdict_exit``:
empty is a failure. #3178 fix-r1 additionally pins the boundary #3 shape: an
empty selection is a never-started run — rc=2 and **no** ``summary.json`` at
all, because a summary that exists reads like a completed run to anyone who
only checks for the artifact. The twin drives the delivered ``main()``
in-process to an empty finish — ``MUTATIONS`` emptied, the host-wide parity
lease, the source guard and the output-directory admission faked (a real
``mutating_sources`` lease is the coordinator's post-commit run, not a unit
twin) — and asserts the exit is 2 and the evidence directory holds no summary.
No cargo, no lease, no product source writes.

Run with:

    python3 -B -m unittest -v parity-lock-release/test_negative_controls.py
"""
import importlib.util
import sys
import tempfile
import unittest
from contextlib import nullcontext
from pathlib import Path
from unittest.mock import patch


def load_runner():
    source = Path(__file__).with_name('negative_controls.py')
    spec = importlib.util.spec_from_file_location(
        'parity_lock_release_negative', source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ZeroRowVerdictControls(unittest.TestCase):
    """A finished run with zero rows must exit 2 unpublished, never green."""

    def test_empty_run_exits_two_without_a_summary(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory(prefix='pdrs-parity-lock-zero-') as temp:
            out = Path(temp) / 'run'

            def fake_output_directory(path):
                Path(path).mkdir(parents=True, exist_ok=True)
                return Path(path)

            class Guard:
                def __enter__(self):
                    return {}

                def __exit__(self, *exc):
                    return False

            with patch.object(runner, 'MUTATIONS', []), \
                    patch.object(runner, 'parity_lease',
                                 lambda **kwargs: nullcontext()), \
                    patch.object(runner, 'mutating_sources',
                                 lambda root, paths, **kwargs: Guard()), \
                    patch.object(runner, 'new_output_directory',
                                 fake_output_directory), \
                    patch.object(sys, 'argv',
                                 ['negative_controls.py', '--output', str(out)]):
                code = runner.main()
            published = sorted(path.name for path in out.iterdir()) \
                if out.exists() else []
        message = ('#3178 a zero-row run must exit 2 and publish no summary '
                   '(the old tail read 0 == 0 as green and an empty '
                   'summary.json read like a completed run)')
        self.assertEqual(code, 2, message)
        self.assertEqual(published, [],
                         message + ' (the empty run must leave the evidence '
                         'directory empty)')


if __name__ == '__main__':
    unittest.main()
