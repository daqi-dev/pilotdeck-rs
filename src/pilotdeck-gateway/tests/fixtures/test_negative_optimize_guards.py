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
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

FIXTURES = Path(__file__).resolve().parent
UNKNOWN = 'no-such-mutation-1666'

# (script, extra arguments after the output directory, interpreter flags).
# The output directory is always the first positional argument and is created
# fresh per invocation; the extra entries are the parts each script requires
# before its selection guard. `-I` scripts reject a non-isolated launch, so
# their flags are part of the contract, not a convenience.
SCRIPTS = (
    ('settings/negative_controls.py', (), ()),
    ('memory-maintenance/negative_controls.py', ('HEAD',), ()),
    ('upload-gateway/negative_controls.py', (), ()),
    ('html-timing/negative_controls.py', (), ()),
    ('interaction-bus/negative_controls.py', (), ()),
    ('node-io/negative_controls.py', (), ()),
    ('project-files-linux-dirent/negative_controls.py', (), ()),
    ('network-timing/negative_controls.py', (), ()),
    ('sdk-settings/negative_controls.py', (), ()),
    ('settings-gateway/negative_controls.py', (), ()),
    # #1666 batch 2: the same "filter by selected name, then judge" entry shape.
    ('interaction-values/negative_controls.py', (), ()),
    ('turn-submit-admission/negative_controls.py', (), ()),
    ('turn-runner/runner_negative.py', (), ()),
    ('network-timing/negative_private.py', (), ()),
    ('cli-parity-mock-gateway/negative_controls.py', (), ('-I',)),
)

# #1666 is *not* closed for the whole family: these `*/negative_controls.py`
# scripts still carry bare `assert` guards and are migrated batch by batch
# (tracked in the issue). This register is shrink-only: a file that is not
# listed here may not contain a judgment `assert`, and migrating a listed file
# must remove it here in the same change. Every entry needs a reason.
PENDING_BARE_ASSERTS = {
    'cli-parity-lock-bounded/negative_controls.py':
        'frozen positive/negative pairing pins the assert text; needs its own round',
    'grep-etxtbsy/negative_controls.py': 'not migrated yet (batch 3)',
    'host-event-pump/negative_controls.py': 'not migrated yet (batch 3)',
    'mcp-gateway/negative_controls.py': 'not migrated yet (batch 3)',
    'mcp-hooks-spawn/negative_controls.py': 'not migrated yet (batch 3)',
    'pump-order-cases/negative_controls.py': 'not migrated yet (batch 3)',
    'runtime-spawn-panic/negative_controls.py': 'not migrated yet (batch 3)',
    'shared-runner-failclosed/negative_controls.py':
        'documented exemption: hand-run, fails closed, no exit-code consumer',
    'skills/negative_controls.py': 'not migrated yet (batch 3)',
    'skills-external/negative_controls.py': 'not migrated yet (batch 3)',
    'spill-isolation/negative_controls.py': 'not migrated yet (batch 3)',
    'transcript-resolve/negative_controls.py': 'not migrated yet (batch 3)',
    'turn-runner/negative_controls.py': 'not migrated yet (batch 3)',
    'workspace-snapshot-gate/negative_controls.py': 'not migrated yet (batch 3)',
    'workspace-snapshot-nonutf8/negative_controls.py': 'not migrated yet (batch 3)',
}
BARE_ASSERT = re.compile(r'^\s*assert\b', re.MULTILINE)
_CLEANUP = []


def run(script, args, flags=(), *, optimize, base):
    """Run ``script`` with a throwaway output directory; return the process."""
    directory = Path(tempfile.mkdtemp(prefix='negctl-1666-', dir=base))
    _CLEANUP.append(directory)
    out = directory / 'out'
    command = [sys.executable, *flags]
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

    base = ''

    @classmethod
    def setUpClass(cls):
        cls.base = tempfile.mkdtemp(prefix='negctl-1666-suites-')

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.base, ignore_errors=True)

    def test_every_script_refuses_an_unknown_name(self):
        for script, extra, flags in SCRIPTS:
            for optimize in (False, True):
                with self.subTest(script=script, optimize=optimize):
                    result = run(script, extra, flags, optimize=optimize,
                                 base=self.base)
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
        for script, extra, flags in SCRIPTS:
            with self.subTest(script=script):
                plain = run(script, extra, flags, optimize=False, base=self.base)
                optimized = run(script, extra, flags, optimize=True, base=self.base)
                self.assertEqual(plain.returncode, optimized.returncode)


class BareAssertInventory(unittest.TestCase):
    """The class invariant has an explicit, shrink-only register (#1666).

    The ten scripts this PR migrated are not the whole ``*/negative_controls.py``
    family. Rather than let a hand-written list look like a closed set, the
    remaining debt is named here: a file that is not in ``PENDING_BARE_ASSERTS``
    may not contain a judgment ``assert``, and each listed file carries a
    reason. Migrating one must delete its entry in the same change.
    """

    def test_every_script_entry_exists(self):
        for script, _extra, _flags in SCRIPTS:
            self.assertTrue((FIXTURES / script).is_file(),
                            f'{script} is listed but does not exist')

    def test_no_unregistered_script_uses_a_bare_assert(self):
        found = {str(path.relative_to(FIXTURES))
                 for path in FIXTURES.glob('*/negative_controls.py')
                 if BARE_ASSERT.search(path.read_text(encoding='utf-8'))}
        self.assertEqual(found, set(PENDING_BARE_ASSERTS),
                         'migrate the script (see #1666) and update '
                         'PENDING_BARE_ASSERTS, or remove the bare assert')

    def test_register_entries_carry_a_reason(self):
        for script, reason in PENDING_BARE_ASSERTS.items():
            self.assertTrue(reason.strip(), f'{script} needs a reason')
            self.assertTrue((FIXTURES / script).is_file(),
                            f'{script} is registered but does not exist')


def tearDownModule():
    for directory in _CLEANUP:
        shutil.rmtree(directory, ignore_errors=True)


if __name__ == '__main__':
    unittest.main()
