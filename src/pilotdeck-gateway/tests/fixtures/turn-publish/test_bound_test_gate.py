#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own bound-test gate; no product source mutation table
"""#2692 fix-r9 (review sibling#3): the --check declares_test gate's red side.

fix-r6 added the gate (a bound test renamed in publish_tests.rs used to keep
the roster green with the name living only in this string table), but only the
host's out-of-tree negative round ever showed it red. The first case runs the
real `--check` and pins its bound-test count; the second simulates the rename
(the shared `declares_test` never matches a `_renamed` spelling) and demands
the matching refusal. Run from anywhere:

    python3 -I -B src/pilotdeck-gateway/tests/fixtures/turn-publish/test_bound_test_gate.py
"""
import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path

FIXTURES = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(FIXTURES))
import negative_suite  # noqa: E402

SCRIPT = Path(__file__).resolve().with_name('negative.py')


def run_check():
    """Exec negative.py under argv ['negative.py', '--check'].

    The `--check` branch runs at module level and terminates the module with
    SystemExit, so the exit code, both streams and the half-built module (its
    mutants table) are the result. No cargo runs; the tree is only read.
    """
    spec = importlib.util.spec_from_file_location('turn_publish_negative_check', SCRIPT)
    module = importlib.util.module_from_spec(spec)
    out, err = io.StringIO(), io.StringIO()
    saved_argv = sys.argv
    sys.argv = ['negative.py', '--check']
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            spec.loader.exec_module(module)
            raise AssertionError('#2692 fix-r9: --check must terminate the module')
    except SystemExit as stop:
        return stop.code, out.getvalue(), err.getvalue(), module
    finally:
        sys.argv = saved_argv


class BoundTestGate(unittest.TestCase):
    def test_check_passes_and_reports_bound_tests(self):
        code, out, err, module = run_check()
        self.assertTrue(code == 0,
                       f'#2692 fix-r9: --check must pass on the pristine tree: '
                       f'rc={code} err={err!r}')
        self.assertIn(f'{len(module.mutants)} bound tests declared', out,
                      '#2692 fix-r9: the bound-test declaration count must be reported')

    def test_check_rejects_a_renamed_bound_test(self):
        # The roster keeps the old spelling while the fn is renamed in
        # publish_tests.rs, so the real declares_test matching runs and must
        # come back empty. negative.py binds the name from negative_suite at
        # import time, so the shared attribute is swapped around the exec.
        real = negative_suite.declares_test

        def renamed(source, short, macro=None):
            return real(source, short + '_renamed', macro)

        saved = negative_suite.declares_test
        negative_suite.declares_test = renamed
        try:
            code, out, err, _module = run_check()
        finally:
            negative_suite.declares_test = saved
        self.assertTrue(code == 2,
                       f'#2692 fix-r9: a renamed bound test must refuse --check: '
                       f'rc={code} out={out!r}')
        self.assertIn('bound test not declared in publish_tests.rs', err,
                      '#2692 fix-r9: the refusal names the drifted binding')


if __name__ == '__main__':
    unittest.main()
