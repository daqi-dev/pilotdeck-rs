# anchor-selfcheck: unsupported unit tests of the verdict tail, no product source mutations
#!/usr/bin/env python3
"""#3178 item 7 (#2973 sibling): the parity-lock verdict tail must refuse a
zero-row run.

The old tail ``0 if counts['KILLED'] == len(results) else 1`` is vacuously
true on an empty row set — a lost mutation table or a misparsed selection
finishes with ``0 == 0`` and exits 0, the all-green code. The family contract
(fixtures/README.md「零行永不返回 0」) is ``negative_selection.verdict_exit``:
empty is a failure. The twin drives the delivered ``main()`` in-process to an
empty finish — ``MUTATIONS`` emptied, the host-wide parity lease, the source
guard and the output-directory admission faked (a real ``mutating_sources``
lease is the coordinator's post-commit run, not a unit twin) — and asserts
the exit is 1 while the empty summary still reaches the disk. No cargo, no
lease, no product source writes.

Run with:

    python3 -B -m unittest -v parity-lock-release/test_negative_controls.py
"""
import importlib.util
import json
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
    """A finished run with zero rows must exit 1, never the all-green 0."""

    def test_empty_run_exits_one(self):
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
            summary = json.loads((out / 'summary.json').read_text(encoding='utf-8'))
        message = '#3178 a zero-row run must exit 1 (the old tail read 0 == 0 as green)'
        self.assertEqual(code, 1, message)
        self.assertEqual(summary, [],
                         message + ' (the empty run still publishes its empty summary)')


if __name__ == '__main__':
    unittest.main()
