#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own bound-test gate; no product source mutation table
"""#2692 fix-r9 (review sibling#3): the --check bound-test gate's red side.

fix-r6 added the gate (a bound test renamed in the file its module path names
used to keep the roster green with the name living only in this string table),
but only the host's out-of-tree negative round ever showed it red. The first
case runs the real `--check` and pins its bound-test count; the second renames
the short name the shared binder returns and demands the matching refusal.
Run from anywhere:

    python3 -I -B src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/test_bound_test_gate.py
"""
import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve()
SPEC = importlib.util.spec_from_file_location(
    'blankkey_negative_controls', HERE.with_name('negative_controls.py'))
nc = importlib.util.module_from_spec(SPEC)
# The module name is never `__main__`, so the `if __name__` guard keeps main()
# inert here; loading only defines the table and the functions.
SPEC.loader.exec_module(nc)


def run_check():
    """Call nc.main() under argv ['negative_controls.py', '--check'].

    Returns (exit code, stdout, stderr); the refusal envelope exits 2 with the
    reason on stderr. No cargo runs; the tree is only read.
    """
    out, err = io.StringIO(), io.StringIO()
    saved_argv = sys.argv
    sys.argv = ['negative_controls.py', '--check']
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = nc.main()
    finally:
        sys.argv = saved_argv
    return code, out.getvalue(), err.getvalue()


class BoundTestGate(unittest.TestCase):
    def test_check_passes_and_reports_bound_tests(self):
        code, out, err = run_check()
        self.assertTrue(code == 0,
                       f'#2692 fix-r9: --check must pass on the pristine tree: '
                       f'rc={code} err={err!r}')
        self.assertIn(f'{len(nc.mutations)} bound tests declared', out,
                      '#2692 fix-r9: the bound-test declaration count must be reported')

    def test_check_rejects_a_renamed_bound_test(self):
        # The roster keeps the old spelling while the fn is renamed in the
        # file its module path names, so the real declares_test matching runs
        # and must come back empty.
        real = nc.bind_module_file

        def renamed(test, modules):
            path, short = real(test, modules)
            return path, short + '_renamed'

        saved_argv, saved_bind = sys.argv, nc.bind_module_file
        nc.bind_module_file = renamed
        out, err = io.StringIO(), io.StringIO()
        try:
            sys.argv = ['negative_controls.py', '--check']
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                with self.assertRaises(SystemExit,
                                       msg='#2692 fix-r9: a renamed bound test must '
                                           'refuse --check') as caught:
                    nc.main()
        finally:
            nc.bind_module_file = saved_bind
            sys.argv = saved_argv
        self.assertTrue(caught.exception.code == 2,
                       f'#2692 fix-r9: the refusal must be a clean rc 2: '
                       f'code={caught.exception.code!r} out={out.getvalue()!r}')
        self.assertIn('bound test not declared', err.getvalue(),
                      '#2692 fix-r9: the refusal names the drifted binding')


if __name__ == '__main__':
    unittest.main()
