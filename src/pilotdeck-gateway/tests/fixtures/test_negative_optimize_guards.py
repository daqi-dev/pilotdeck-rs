#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-test of the negative-control guards, spawns -O subprocesses only
"""#1666: negative-control guards must not disappear under ``python3 -O``.

``-O`` / ``PYTHONOPTIMIZE=1`` strips every ``assert`` statement. A control
script whose only guard is ``assert len(selected) == len(names)`` therefore
selected nothing, ran zero mutants, printed ``0/0 killed`` and exited 0 -- a
ruler that passes without measuring anything.

Every script below is invoked with a mutation name that does not exist. The
correct answer is exit code 2 ("never started") with the unknown name on
stderr, and the same answer whether or not assertions are stripped. The first
test is the harness's own negative control: it proves this interpreter really
does drop ``assert`` under ``-O``, so a green run here cannot be an artefact of
a Python that ignores the flag.

Run from this directory: ``python3 -m unittest test_negative_optimize_guards``.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

FIXTURES = Path(__file__).resolve().parent
UNKNOWN = 'no-such-mutation-1666'

# (script, arguments after the script name). The output directory is always the
# first positional argument and is created fresh per invocation; the extra
# entries are the parts each script requires before its selection guard.
SCRIPTS = (
    ('settings/negative_controls.py', ()),
    ('memory-maintenance/negative_controls.py', ('HEAD',)),
    ('upload-gateway/negative_controls.py', ()),
    ('html-timing/negative_controls.py', ()),
    ('interaction-bus/negative_controls.py', ()),
    ('node-io/negative_controls.py', ()),
    ('project-files-linux-dirent/negative_controls.py', ()),
    ('network-timing/negative_controls.py', ()),
    ('sdk-settings/negative_controls.py', ()),
    ('settings-gateway/negative_controls.py', ()),
)


def run(script, args, *, optimize):
    """Run ``script`` with a throwaway output directory; return the process."""
    out = Path(tempfile.mkdtemp(prefix='negctl-1666-')) / 'out'
    command = [sys.executable]
    if optimize:
        command.append('-O')
    command += [str(FIXTURES / script), str(out), *args, UNKNOWN]
    env = dict(os.environ)
    # Presence-only preconditions some scripts check before their roster guard.
    env.setdefault('CARGO_TARGET_DIR', str(out.parent / 'cargo-target'))
    env.pop('PYTHONOPTIMIZE', None)
    return subprocess.run(command, cwd=str(FIXTURES), env=env,
                          capture_output=True, text=True, timeout=300)


class OptimizeStripsAssert(unittest.TestCase):
    """The harness can see ``-O`` doing its job; otherwise nothing below means anything."""

    def test_assert_is_stripped_only_under_optimize(self):
        plain = subprocess.run([sys.executable, '-c', 'assert False'],
                               capture_output=True, text=True)
        optimized = subprocess.run([sys.executable, '-O', '-c', 'assert False'],
                                   capture_output=True, text=True)
        self.assertEqual(plain.returncode, 1,
                         'an `assert False` must fail without -O')
        self.assertEqual(optimized.returncode, 0,
                         'this interpreter does not strip asserts under -O; the '
                         'regression below would be untestable')


class UnknownControlIsRefused(unittest.TestCase):
    """#1666: unknown name -> exit 2 (never started), with and without ``-O``."""

    def test_every_script_refuses_an_unknown_name(self):
        for script, extra in SCRIPTS:
            for optimize in (False, True):
                with self.subTest(script=script, optimize=optimize):
                    result = run(script, extra, optimize=optimize)
                    self.assertEqual(
                        result.returncode, 2,
                        f'{script} (optimize={optimize}) must refuse to run: '
                        f'stdout={result.stdout!r} stderr={result.stderr!r}')
                    self.assertIn(UNKNOWN, result.stderr,
                                  f'{script} must name the unknown control on stderr')
                    self.assertNotIn('0/0', result.stdout,
                                     f'{script} must not report an empty run as a result')

    def test_optimize_and_plain_agree(self):
        # The point of the issue: the verdict must not depend on -O.
        for script, extra in SCRIPTS:
            with self.subTest(script=script):
                plain = run(script, extra, optimize=False)
                optimized = run(script, extra, optimize=True)
                self.assertEqual(plain.returncode, optimized.returncode)


if __name__ == '__main__':
    unittest.main()
