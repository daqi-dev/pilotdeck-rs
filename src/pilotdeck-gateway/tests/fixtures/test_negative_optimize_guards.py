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
import ast
import os
from pathlib import Path
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

# #1666 is *not* closed for the whole family: these hand-rolled entries still
# carry bare `assert` guards and are migrated batch by batch (tracked in the
# issue). This register is shrink-only: a file that is not listed here may not
# contain a judgment `assert`, and migrating a listed file must remove it here
# in the same change. Every entry needs a reason.
#
# Scope: the scan covers every ``negative_controls.py`` at any depth plus the
# entries named in ``SCANNED_EXTRA``. Same-shaped entries with other names
# (``negative.py``, ``negative-controls.py``, ``negative_round*.py``,
# ``negative_ids.py`` …) are *not* scanned yet — that is #1666 batch 3, not a
# claim that they are clean.
PENDING_BARE_ASSERTS = {
    'cli-parity-lock-bounded/negative_controls.py':
        'frozen positive/negative pairing pins the assert text; needs its own round',
    'host-event-pump/negative_controls.py': 'not migrated yet (batch 3)',
    'mcp-gateway/negative_controls.py': 'not migrated yet (batch 3)',
    'mcp-hooks-spawn/negative_controls.py': 'not migrated yet (batch 3)',
    'native-settings/negative/negative_controls.py':
        'not migrated yet (batch 3); nested one level deeper than the flat entries',
    'pump-order-cases/negative_controls.py': 'not migrated yet (batch 3)',
    'runtime-spawn-panic/negative_controls.py': 'not migrated yet (batch 3)',
    'skills/negative_controls.py': 'not migrated yet (batch 3)',
    'skills-external/negative_controls.py': 'not migrated yet (batch 3)',
    'spill-isolation/negative_controls.py': 'not migrated yet (batch 3)',
    'transcript-resolve/negative_controls.py': 'not migrated yet (batch 3)',
    'turn-runner/negative_controls.py': 'not migrated yet (batch 3)',
    'workspace-snapshot-gate/negative_controls.py': 'not migrated yet (batch 3)',
    'workspace-snapshot-nonutf8/negative_controls.py': 'not migrated yet (batch 3)',
}

# Migrated entries whose file name is not ``negative_controls.py``. They are
# scanned too, so re-introducing an `assert` (or a bare one in the shared
# helper they import) is a CI failure rather than untested drift.
SCANNED_EXTRA = (
    'turn-runner/runner_negative.py',
    'turn-runner/_negative_common.py',
    'network-timing/negative_private.py',
    # Shared harness with live callers (`session-domain/fix2-negative.py`,
    # `recorder/fix3-negative.py`, `run-registry/count-negative.py`,
    # `background-output/`, `test_negative_adaptive.py`): its `run()` decides
    # KILLED vs PASS and `scratch()` checks the tree is untouched.
    '_shared/negative_control.py',
)
_CLEANUP = []


def scanned_entries():
    """Every entry the register accounts for, at any nesting depth."""
    return ({str(path.relative_to(FIXTURES))
             for path in FIXTURES.rglob('negative_controls.py')}
            | set(SCANNED_EXTRA))


def uses_a_bare_assert(path):
    """True when the file contains an ``assert`` *statement* anywhere.

    A line-anchored regex misses ``if cond: assert x`` and ``x; assert y``,
    which `-O` strips just the same. Parse instead; fall back to the regex only
    for a file this interpreter cannot parse, so the answer stays fail-closed.
    """
    source = path.read_text(encoding='utf-8')
    try:
        tree = ast.parse(source)
    except SyntaxError:
        # Fail closed: a file this interpreter cannot parse is not evidence of
        # cleanliness, and the line-anchored regex would miss exactly the
        # inline forms AST was adopted for.
        return True
    return any(isinstance(node, ast.Assert) for node in ast.walk(tree))


def run(script, args, flags=(), *, optimize, base, claim_exists=False):
    """Run ``script`` with a throwaway output directory.

    Returns ``(process, claimed_dir)``: the output directory path is part of
    the result because "refused before it started" is only honest if the
    evidence directory was never created (a host reads that directory as this
    run's verdict).
    """
    directory = Path(tempfile.mkdtemp(prefix='negctl-1666-', dir=base))
    _CLEANUP.append(directory)
    out = directory / 'out'
    command = [sys.executable, *flags]
    if optimize:
        command.append('-O')
    if claim_exists:
        out.mkdir()
        command_entry = [str(out), *args]
    else:
        command_entry = [str(out), *args, UNKNOWN]
    command += [str(FIXTURES / script), *command_entry]
    env = dict(os.environ)
    # Presence-only preconditions some scripts check before their roster guard.
    env.setdefault('CARGO_TARGET_DIR', str(out.parent / 'cargo-target'))
    env.setdefault('PDRS_BASELINE', str(out.parent / 'pinned-ts-checkout'))
    env.pop('PYTHONOPTIMIZE', None)
    result = subprocess.run(command, cwd=str(FIXTURES), env=env,
                            capture_output=True, text=True, timeout=300)
    return result, out


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
                    result, out = run(script, extra, flags, optimize=optimize,
                                      base=self.base)
                    self.assertEqual(
                        result.returncode, 2,
                        f'{script} (optimize={optimize}) must refuse to run: '
                        f'stdout={result.stdout!r} stderr={result.stderr!r}')
                    self.assertIn(UNKNOWN, result.stderr,
                                  f'{script} must name the unknown control on stderr')
                    self.assertNotIn('0/0', result.stdout,
                                     f'{script} must not report an empty run as a result')
                    # A refusal is "never started": it must not leave an
                    # evidence directory for a host to read as this run.
                    self.assertFalse(
                        out.exists(),
                        f'{script} (optimize={optimize}) refused but still created '
                        f'its evidence directory {out}')

    def test_existing_output_directory_is_refused(self):
        """A taken evidence directory means "never started" (2), not a run (1).

        `mkdir(exist_ok=False)` used to raise `FileExistsError`, whose traceback
        exits 1 -- the host reads that as "ran and did not pass" and investigates
        a run that never happened. The directory is still empty afterwards: this
        run wrote nothing.
        """
        for script, extra, flags in SCRIPTS:
            with self.subTest(script=script):
                result, out = run(script, extra, flags, optimize=True,
                                  base=self.base, claim_exists=True)
                self.assertEqual(
                    result.returncode, 2,
                    f'{script} must refuse an existing output directory: '
                    f'stdout={result.stdout!r} stderr={result.stderr!r}')
                self.assertIn('already exists', result.stderr,
                              f'{script} must say why it refused')
                self.assertEqual(list(out.iterdir()), [],
                                 f'{script} must not write into another run\'s directory')

    def test_optimize_and_plain_agree(self):
        # The point of the issue: the verdict must not depend on -O.
        for script, extra, flags in SCRIPTS:
            with self.subTest(script=script):
                plain, _ = run(script, extra, flags, optimize=False, base=self.base)
                optimized, _ = run(script, extra, flags, optimize=True, base=self.base)
                self.assertEqual(plain.returncode, optimized.returncode)


class BareAssertInventory(unittest.TestCase):
    """The class invariant has an explicit, shrink-only register (#1666).

    The fifteen entries this PR migrated are not the whole family. Rather than
    let a hand-written list look like a closed set, the remaining debt is named
    here: a file in ``scanned_entries()`` that is not in
    ``PENDING_BARE_ASSERTS`` may not contain a judgment ``assert``, and each
    listed file carries a reason. Migrating one must delete its entry in the
    same change. The scan reaches every nesting depth; entries with other file
    names are batch 3 (see the comment on the register).
    """

    def test_every_script_entry_exists(self):
        for script, _extra, _flags in SCRIPTS:
            self.assertTrue((FIXTURES / script).is_file(),
                            f'{script} is listed but does not exist')

    def test_no_unregistered_script_uses_a_bare_assert(self):
        found = {name for name in scanned_entries()
                 if uses_a_bare_assert(FIXTURES / name)}
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
