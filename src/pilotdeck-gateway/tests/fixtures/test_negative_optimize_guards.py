#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-test of the negative-control guards, spawns -O subprocesses only
"""#1666: negative-control guards must not disappear under ``python3 -O``.

``-O`` / ``PYTHONOPTIMIZE=1`` strips every ``assert`` statement. A control
script whose only guard is ``assert len(selected) == len(names)`` therefore
selected nothing, ran zero mutants, printed ``0/0 killed`` and exited 0 -- a
ruler that passes without measuring anything.

Every script below is invoked with a mutation name that does not exist. The
correct answer is exit code 2 ("never started") with the unknown name on
stderr, and the same answer whether or not assertions are stripped. Two more
matrices cover the other startup refusals the contract promises: an already
claimed output directory, and an unusable ``CARGO_TARGET_DIR`` -- absent, empty
or all-space. For
all three, the evidence directory must not exist afterwards. The first test is
the harness's own negative control: it proves this interpreter really does drop
``assert`` under ``-O``, so a green run here cannot be an artefact of a Python
that ignores the flag.

Run from this directory: ``python3 -m unittest test_negative_optimize_guards``.
"""
import ast
import os
import re
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
    # #1456: same entry shape after the r2/r4 migration to negative_selection.
    ('memory-gateway/negative_controls.py', (), ()),
    ('snapshot-projection/negative_controls.py', (), ()),
    # #1265: same entry shape after the fix-r1/fix-r2 migration to
    # negative_selection (claim_dir + missing_target/refuse_unusable_target
    # + select + registered + verdict_exit).
    ('regex-proto-budget/negative_controls.py', (), ()),
    # #297 r6 (DS r5 boundary P2 / MM r5 concurrency P2, both families): the
    # two #297 entries complete the shared refusal contract (unknown or
    # repeated name, roster drift, unusable target, existing output
    # directory -> 2, never an uncaught traceback), so their startup
    # matrix is machine-checked like the other migrated entries. r7 (DS r6
    # sibling P2#3): this matrix exercises unknown-name / target modes /
    # existing-dir only; the repeated-name and roster-drift refusals come
    # from the shared select()/registered() helpers (their own unit tests
    # in test_negative_selection.py), and guard_failed/signal exits stay
    # code-review-verified -- #1657 tracks the entry-level test gap.
    ('issue-297/negative_controls.py', (), ()),
    ('issue-297-domain-fold/negative_controls.py', (), ()),
    # #1433 fix-r1: same entry shape after the migration to negative_selection
    # (review P1-1); its --check is also wired into CI.
    ('sdk-mcp-session-tools/negative_controls.py', (), ()),
    # #1354: same entry shape from the start (registered + select + Case rows
    # + claim_dir/missing_target/refuse_unusable_target + verdict_exit).
    ('mode-routing/negative_controls.py', (), ()),
    # #2425: same entry shape after the CLI alignment from ``--case NAME`` to
    # positional names (select + claim_dir + registered + verdict_exit).
    ('same-json-surfaces/negative_controls.py', (), ('-I',)),
    # #2391 fix-r1: the two session-family entries born on the
    # negative_selection contract (registered + refuse + claim_dir +
    # missing_target/refuse_unusable_target + verdict_exit); their --check is
    # also wired into CI.
    ('session-reservation-2391/negative_controls.py', (), ()),
    ('session-delete-2392/negative_controls.py', (), ()),
    # #2309 fix-r2: same entry shape after the migration off the shared
    # `negative_suite.run_cases` loop (registered + select + claim_dir +
    # missing_target/refuse_unusable_target + verdict_exit); its positional
    # names and `--only a,b` both feed the shared selection rule.
    ('manager-gateway/negative_controls.py', (), ()),
    # #2131 fix-r3: same entry shape after the migration to negative_selection
    # (registered + select + claim_dir + missing_target/refuse_unusable_target
    # + verdict_exit).
    ('utf16-residual-sort/negative_controls.py', (), ()),
    # #2590 fix-r2 (DS-sibling#2 P2 / MM-sibling#2 P1, 取高按 P1): the
    # issue-2495 fixture migrated to the negative_selection contract in
    # fix-r1 (registered + refuse + claim_dir + missing_target/
    # refuse_unusable_target + verdict_exit); its startup refusals join the
    # machine-checked matrix here, and its --check is also wired into CI.
    ('issue-2495/negative_controls.py', (), ()),
    # #2795 fix-r1（MM sibling#3）: the #2069 host-model-port fixture was born
    # off the SCRIPTS matrix (snapshot-era gap); its main() already speaks the
    # full contract (registered + select + claim_dir + missing_target/
    # refuse_unusable_target + verdict_exit), so its startup refusals join the
    # machine-checked matrix here. The target modes never reach cargo —
    # select() with no names returns the whole roster, then missing_target()/
    # refuse_unusable_target() refuse before any build -- no exemption
    # needed (the review's timeout caveat does not apply).
    ('host-model-port/negative_controls.py', (), ()),
)

# #1666: entries whose target guard cannot refuse *every* mode. Each exemption
# lists the modes it skips, plus the reason and the source fact that has to stay
# true, so the matrix cannot shrink by adding a name alone:
# `interaction-values` and `turn-submit-admission` let
# `negative_process.target_environment` default the shared tree target when the
# variable is *absent* (#1166) -- a present-but-blank value is refused by their
# own `blank_target()` guard, so only `missing` is exempt -- and
# `cli-parity-mock-gateway` and `same-json-surfaces` never read the target in
# their own source (machine-checked below), while the `mutating_sources` window
# they delegate to does read it (and may cold-boot the scanner with cargo
# build, #2552 DS boundary#2): `missing` is not hostable for either (it would
# start a full guarded batch past the 300s matrix subprocess), and the other
# three modes are not hostable yet -- same-json refuses them rc 2 (live-probed
# #2552 r1) but this registry has no shape for an entry whose own source never
# calls target_environment; see the per-entry reasons for the rc shapes.
MODES = ('missing', 'empty', 'blank', 'dot')
TARGET_OPTIONAL = {
    'interaction-values/negative_controls.py':
        ('target_environment defaults the shared tree target when the variable is absent (#1166)',
         'target_environment(', ('missing',)),
    'turn-submit-admission/negative_controls.py':
        ('target_environment defaults the shared tree target when the variable is absent (#1166)',
         'target_environment(', ('missing',)),
    'cli-parity-mock-gateway/negative_controls.py':
        ('own source never reads the target in any spelling (machine-checked); the '
         'delegated mutating_sources window does: _ensure_workspace_scanner reads '
         'CARGO_TARGET_DIR through the shared rule (absent defaults to the shared tree '
         'target, blank/dot are refused there) and may cargo-build the scanner, so '
         '"never compiles Cargo" is false (#2552 DS boundary#2); missing starts a full '
         'guarded batch past the 300s matrix subprocess, the others surface as this '
         'entry\'s rc 1 with the startup exception recorded as INVALID rows and the '
         'evidence directory left claimed (live-probed #2552 r1)', None, MODES),
    'same-json-surfaces/negative_controls.py':
        ('own source never reads the target in any spelling (machine-checked); the '
         'delegated mutating_sources window does: _ensure_workspace_scanner reads '
         'CARGO_TARGET_DIR through the shared rule (absent defaults to the shared tree '
         'target, blank/dot are refused there) and may cargo-build the scanner, so '
         '"never compiles Cargo" is false (#2552 DS boundary#2); missing would start '
         'a full guarded batch past the 300s matrix subprocess; empty/blank/dot are '
         'refused rc 2 by that shared rule plus the started flag (#2431, live-probed '
         '#2552 r1: rc 2, stderr names the target, no evidence directory) but this '
         'registry has no shape for an entry whose own source never calls '
         'target_environment, so they stay exempt until it does', None, MODES),
}

# #2309 fix-r3 (MM sibling P2): the entries that refuse an empty or blank
# ``PDRS_BASELINE`` before claiming their evidence directory. The matrix used
# to hardcode the one entry that carried the guard when it was written
# (project-files-linux-dirent); manager-gateway grew the same
# ``(os.environ.get('PDRS_BASELINE') or '').strip()`` refusal without joining
# the denominator, so reverting its guard would not have failed anything. A
# future entry with the same guard must join this list in the same change.
BASELINE_GUARDED = (
    'project-files-linux-dirent/negative_controls.py',
    'manager-gateway/negative_controls.py',
)

# Any read of the baseline variable is the guard's subject. Matching the call
# shapes (not one spelling) keeps the register honest the same way
# ``TARGET_READ`` does for the target guard above.
_BASELINE_NAME = r"""f?['"]PDRS_BASELINE['"]"""
BASELINE_READ = re.compile(
    r'\w+\.environ\s*\[\s*' + _BASELINE_NAME + r'\s*\]'
    + r'| \w+\.(?:get|setdefault|pop)\s*\(\s*' + _BASELINE_NAME
    + r'| \w+\s*\[\s*' + _BASELINE_NAME + r'\s*\]'
    + r'| \w*getenv\s*\(\s*' + _BASELINE_NAME,
    re.X)

# Any read of the target variable is a target guard. Matching the call shapes
# (not one spelling) keeps the exemption honest when a guard is reintroduced
# with double quotes, `os.getenv`, an alias (`env = os.environ.copy()` then
# `env.get(...)`, or `from os import environ`), a subscription, `setdefault` /
# `pop`, a bare `getenv` after `from os import getenv`, an f-string name, or the
# shared predicates. An alias that only matched `os.environ...` would let the
# fully exempt entry grow a guard that none of the modes exercises
# (#1666 fix-r8, raised by all four review roles; the missing spellings were
# #1666 fix-r9 boundary P2).
_TARGET_NAME = r"""f?['"]CARGO_TARGET_DIR['"]"""
TARGET_READ = re.compile(
    r'\w+\.environ\s*\[\s*' + _TARGET_NAME + r'\s*\]'
    + r'| \w+\.(?:get|setdefault|pop)\s*\(\s*' + _TARGET_NAME
    + r'| \w+\s*\[\s*' + _TARGET_NAME + r'\s*\]'
    # `os.getenv(...)` and a bare `getenv(...)` after `from os import getenv`.
    + r'| \w*getenv\s*\(\s*' + _TARGET_NAME
    # The shared predicates: presence half, value half, and the value half's
    # refusing wrapper.
    + r'| (?:missing|blank|unusable)_target\s*\('
    + r'| refuse_unusable_target\s*\(',
    re.X)


def calls(source, name):
    """True when the source *calls* ``name`` (an import or a docstring is not a call).

    A source that does not parse is a failure, not a "no": returning False on
    ``SyntaxError`` would let the "must really call target_environment"
    assertion pass on a file whose syntax is broken (#1666 fix-r8).
    """
    tree = ast.parse(source)
    return any(isinstance(node, ast.Call) and (
        (isinstance(node.func, ast.Name) and node.func.id == name) or
        (isinstance(node.func, ast.Attribute) and node.func.attr == name))
        for node in ast.walk(tree))

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
    # #1922: the judgment layer has three more shared implementations with the
    # same "no bare assert under -O" contract, all at zero assert statements
    # today. Registering them makes a reintroduced `assert` a CI failure
    # instead of untested drift (selection / verdict exits, process and source
    # ownership, the precompiled-switch executor).
    'negative_selection.py',
    'negative_process.py',
    '_shared/mutant_switch.py',
    # Review r1 sibling / boundary P2: the same argument reaches the shared
    # verdict helpers those three build on. `mutant_switch.run()` classifies
    # through `negative_suite.passed`, `sdk-settings` and `network-timing`
    # decide KILLED through `negative_panic`, and `negative_scope` holds the
    # predicates both use. All three are at zero assert statements today, so
    # registering them only shrinks the same drift surface.
    'negative_suite.py',
    'negative_panic.py',
    'negative_scope.py',
    # Review r2 sibling P2: two more shared executors guard anchors, execution
    # and restoration with `raise`. Both are at zero assert statements today.
    '_shared/regeneration-negative.py',
    '_shared/negative_sqlite_header.py',
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


def run(script, args, flags=(), *, optimize, base, claim_exists=False,
        target=None, baseline=None):
    """Run ``script`` with a throwaway output directory.

    Returns ``(process, claimed_dir)``: the output directory path is part of
    the result because "refused before it started" is only honest if the
    evidence directory was never created (a host reads that directory as this
    run's verdict).

    ``target`` selects one unusable-caller-target mode and drops the unknown
    mutation name, so the target guard is the first thing the script can hit:
    that refusal must also be rc 2 and must also leave no evidence directory.
    The modes are ``'missing'`` (variable unset), ``'empty'`` (``''``),
    ``'blank'`` (spaces) and ``'dot'`` (``'.'``): all four are equally unusable.
    A presence-only check let the middle two reach the shared target
    normalisation, which rejects them only after the evidence directory existed
    (#1666 fix-r7); ``'.'`` is presence-valid but rejected by the same rule, and
    was refused by two entries only (#1666 fix-r9).

    ``baseline`` overrides ``PDRS_BASELINE`` so the same empty/blank modes can
    be exercised for that variable; passing ``None`` leaves the ``setdefault``
    value in place, so the *absent* mode is not expressible here (#1666
    fix-r9; the guards under test only fire on empty/blank).
    """
    directory = Path(tempfile.mkdtemp(prefix='negctl-1666-', dir=base))
    _CLEANUP.append(directory)
    out = directory / 'out'
    command = [sys.executable, *flags]
    if optimize:
        command.append('-O')
    if claim_exists:
        out.mkdir()
    if claim_exists or target or baseline is not None:
        command_entry = [str(out), *args]
    else:
        command_entry = [str(out), *args, UNKNOWN]
    command += [str(FIXTURES / script), *command_entry]
    env = dict(os.environ)
    # Presence-only preconditions some scripts check before their roster guard.
    env.setdefault('PDRS_BASELINE', str(out.parent / 'pinned-ts-checkout'))
    if baseline is not None:
        env['PDRS_BASELINE'] = baseline
    if target == 'missing':
        env.pop('CARGO_TARGET_DIR', None)
    elif target in ('empty', 'blank'):
        env['CARGO_TARGET_DIR'] = '' if target == 'empty' else '   '
    elif target == 'dot':
        # Set, non-blank, and still unusable: the shared normalisation rejects
        # it (``#1166 target cannot be dot``). That refusal must be the same
        # "never started" (2) before the evidence directory is claimed
        # (#1666 fix-r8).
        env['CARGO_TARGET_DIR'] = '.'
    else:
        env.setdefault('CARGO_TARGET_DIR', str(out.parent / 'cargo-target'))
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

    def test_every_target_guard_refuses_an_unusable_caller_target(self):
        """An absent, empty or blank target is "never started" (2), not a failure (1).

        `specs/negative-controls/README.md` promises this for every migrated
        entry, but the rest of this file always supplies the variable, so the
        promise had no denominator. All four unusable modes (absent / empty /
        blank / ``.``) are tested: three
        entries used to check presence only, and a host that exports
        ``CARGO_TARGET_DIR=`` (or only spaces) would have run them after
        claiming the evidence directory and then failed inside the shared
        target normalisation -- exit 1, plus a directory a host reads as a
        finished run (#1666 fix-r7).
        """
        listed = {script for script, _extra, _flags in SCRIPTS}
        self.assertLessEqual(set(TARGET_OPTIONAL), listed,
                             'TARGET_OPTIONAL must only name listed scripts')
        for script, extra, flags in SCRIPTS:
            exempt = TARGET_OPTIONAL.get(script, ('', None, ()))[2]
            for mode in MODES:
                if mode in exempt:
                    continue
                with self.subTest(script=script, mode=mode):
                    result, out = run(script, extra, flags, optimize=True,
                                      base=self.base, target=mode)
                    self.assertEqual(
                        result.returncode, 2,
                        f'{script} must refuse with a {mode} caller target: '
                        f'stdout={result.stdout!r} stderr={result.stderr!r}')
                    # The dot message comes from the one shared rule and
                    # names the cause rather than the variable.
                    needle = 'CARGO_TARGET_DIR' if mode != 'dot' else 'target'
                    self.assertIn(needle, result.stderr,
                                  f'{script} must name the unusable target on stderr')
                    self.assertFalse(
                        out.exists(),
                        f'{script} refused but still created its evidence directory {out}')

    def test_target_optional_entries_hold_their_reason(self):
        """An exemption names the modes it skips and the source fact behind them.

        Without this, adding a name to ``TARGET_OPTIONAL`` silently removes an
        entry from the matrix -- the opposite of what the register is for.
        """
        for script, (reason, needle, modes) in TARGET_OPTIONAL.items():
            with self.subTest(script=script):
                self.assertTrue(reason.strip(), '#1666 every exemption needs a reason')
                self.assertTrue(set(modes) <= set(MODES),
                                f'{script}: unknown exempt mode(s) {modes}')
                source = (FIXTURES / script).read_text()
                if set(modes) == set(MODES):
                    # Exempt in every mode: the script must not touch the
                    # target at all, however it is spelled.
                    self.assertIsNone(TARGET_READ.search(source),
                                      f'{script} grew a target guard; drop the exemption')
                    self.assertFalse(
                        calls(source, 'target_environment'),
                        f'{script} says it has no target guard: {reason}')
                else:
                    # Only "absent" is exempt, because a helper defaults the
                    # target. That defaulting must be a real call: an import
                    # line or a docstring mentioning the helper is not a guard.
                    self.assertEqual(set(modes), {'missing'},
                                     f'{script}: only "absent" may be exempt this way')
                    self.assertTrue(
                        calls(source, 'target_environment'),
                        f'{script} no longer matches its exemption reason: {reason}')
                if needle is not None:
                    self.assertIn(needle, source,
                                  f'{script} no longer matches its exemption reason: {reason}')

    def test_every_entry_refuses_a_blank_baseline(self):
        """``PDRS_BASELINE`` has the same absent/empty/blank split and needs a denominator.

        The guard became ``(value or '').strip()`` in fix-r8, but this
        harness always supplies a real baseline path, so reverting it to a
        presence-only check would not have failed anything (#1666 fix-r9).
        Every entry that carries the guard is in ``BASELINE_GUARDED`` (the
        register itself stays honest: a listed entry really reads the
        variable, so the list cannot grow a name whose guard disappeared).
        """
        for script in BASELINE_GUARDED:
            source = (FIXTURES / script).read_text()
            self.assertTrue(BASELINE_READ.search(source) is not None,
                            f'{script} is registered as baseline-guarded but '
                            'no longer reads PDRS_BASELINE itself')
            for name, value in (('empty', ''), ('blank', '   ')):
                with self.subTest(script=script, mode=name):
                    result, out = run(script, [], optimize=True, base=self.base,
                                      baseline=value)
                    self.assertEqual(
                        result.returncode, 2,
                        f'{script} must refuse a {name} PDRS_BASELINE: '
                        f'stdout={result.stdout!r} stderr={result.stderr!r}')
                    self.assertIn('PDRS_BASELINE', result.stderr,
                                  f'{script} must name the unusable variable on stderr')
                    self.assertFalse(out.exists(),
                                     f'{script} refused but still created {out}')

    def test_optimize_and_plain_agree(self):
        # The point of the issue: the verdict must not depend on -O.
        for script, extra, flags in SCRIPTS:
            with self.subTest(script=script):
                plain, _ = run(script, extra, flags, optimize=False, base=self.base)
                optimized, _ = run(script, extra, flags, optimize=True, base=self.base)
                self.assertEqual(plain.returncode, optimized.returncode)


class BareAssertInventory(unittest.TestCase):
    """The class invariant has an explicit, shrink-only register (#1666).

    The twenty-eight entries migrated so far (twenty-seven fixture directories;
    ``network-timing`` counts two scripts) are not the whole family. Rather than
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
