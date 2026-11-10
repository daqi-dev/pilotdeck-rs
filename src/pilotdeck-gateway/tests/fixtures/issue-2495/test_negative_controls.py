#!/usr/bin/env python3
# anchor-selfcheck: unsupported guard tests for the issue-2495 negative controls, exec modified source and spawn -O subprocesses only
"""#2590: the issue-2495 control's startup guards and exit-code taxonomy.

The script refuses to run (exit 2, ``INVALID:`` on stderr) when its static
roster has drifted, when the caller names an unknown or repeated mutation,
when the caller target is unusable (absent / blank / dot — #2590 item 1), when
the output directory already exists, when an anchor went stale, when a
bound assertion is a ``format!`` template instead of a static literal
(#2590 item 3: the receipt compares byte-for-byte, so a template can only
come out NO_RECEIPT and the drift must be a ``--check`` failure, not a host
round), or when the tree lock stays busy past the guard's wait (fix-r2: a
sibling mutation round can hold it far past the 900 s default, and that
timeout is the same "never started" refusal, never an uncaught traceback
a host reads as rc 1). Instead of duplicating the guard logic here, each
case execs the real
script with one source-level edit injected -- the same shape an accidental
edit would take -- and checks the refusal message, the exit code and that no
evidence directory was created (#2590 item 5, the settings-gateway shape).
The summary denominator is the static roster (#2590 item 2): a row that
records no verdict is a published roster mismatch, never a shorter green
run, the exit contract is the shared ``verdict_exit``, and under
``--compile-only`` the summary reports ``COMPILED=n/N`` -- the verdict the
exit contract counts -- instead of a false ``KILLED=0/N`` (fix-r2).

Run from this directory: ``python3 -m unittest issue-2495/test_negative_controls.py``.
"""
import importlib.util
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch

FIXTURES = Path(__file__).resolve().parents[1]
SCRIPT = FIXTURES / 'issue-2495' / 'negative_controls.py'
sys.path.insert(0, str(FIXTURES))


def run(*argv, patch_source=None, environ=None, output_exists=False):
    """Exec the control script in-process; return (exit code, stdout, stderr).

    ``patch_source`` is one ``(before, after)`` edit applied to the real script
    text. ``output_exists`` makes argv[1] point at a directory that already
    exists. A leading ``'--check'`` replaces the positional output slot, which
    is the fixture's own CLI shape.
    """
    source = SCRIPT.read_text(encoding='utf-8')
    if patch_source is not None:
        before, after = patch_source
        if source.count(before) != 1:
            raise AssertionError(f'test anchor occurs {source.count(before)} times: {before!r}')
        source = source.replace(before, after, 1)
    base = Path(tempfile.mkdtemp(prefix='issue-2495-2590-'))
    out = base / 'out'
    if output_exists:
        out.mkdir()
    if argv and argv[0] == '--check':
        script_argv = [str(SCRIPT), '--check']
    else:
        script_argv = [str(SCRIPT), str(out), *argv]
    env = {key: value for key, value in os.environ.items()
           if key != 'CARGO_TARGET_DIR'}
    env.update(environ or {})
    stdout, stderr = io.StringIO(), io.StringIO()
    code = 0
    with patch.dict(os.environ, env, clear=True), \
            patch.object(sys, 'argv', script_argv), \
            redirect_stdout(stdout), redirect_stderr(stderr):
        try:
            exec(compile(source, str(SCRIPT), 'exec'),
                 {'__name__': '__main__', '__file__': str(SCRIPT)})
        except SystemExit as error:
            code = error.code if isinstance(error.code, int) else 1
    return code, stdout.getvalue(), stderr.getvalue(), out


def load_module():
    """The fixture as a module, for the pure helpers (main never runs)."""
    spec = importlib.util.spec_from_file_location('issue_2495_negative_controls', SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class RosterGuards(unittest.TestCase):
    """#2590 item 5: a roster edit must refuse before anything runs."""

    def assertRefused(self, result, needle):
        code, stdout, stderr, out = result
        self.assertEqual(code, 2, f'expected refusal, got {code}: {stderr!r}{stdout!r}')
        self.assertIn('INVALID:', stderr)
        self.assertIn(needle, stderr)
        # The evidence directory is claimed after every startup guard, so a
        # "never started" refusal must not even create it: a host that finds
        # the directory reads it as this run's verdict.
        self.assertFalse(out.exists(),
                         'a refusal before the run must not create the evidence directory')

    def test_roster_row_deleted_is_refused(self):
        self.assertRefused(
            run(patch_source=("    'note-dedup-dropped',\n", '')),
            'control roster drifted')

    def test_roster_rows_reordered_is_refused(self):
        self.assertRefused(
            run(patch_source=(
                "    'dedup-gating-restored',\n    'inspectable-registered-domain-restored',\n",
                "    'inspectable-registered-domain-restored',\n    'dedup-gating-restored',\n")),
            'control roster drifted')


class ExitCodeTaxonomy(unittest.TestCase):
    """#2590 item 1: 2 means "never started"; 1 is reserved for a run that happened."""

    def test_unknown_mutation_is_refused(self):
        code, _stdout, stderr, out = run('no-such-mutation-2590',
                                         environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('no-such-mutation-2590', stderr)
        self.assertIn('unknown', stderr)
        self.assertFalse(out.exists())

    def test_repeated_mutation_is_refused(self):
        code, _stdout, stderr, out = run('note-dedup-dropped', 'note-dedup-dropped',
                                         environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('repeated mutation names', stderr)
        self.assertIn('note-dedup-dropped', stderr)
        self.assertFalse(out.exists())

    def test_missing_cargo_target_is_refused(self):
        code, _stdout, stderr, out = run()
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('CARGO_TARGET_DIR', stderr)
        self.assertFalse(out.exists(),
                         'the CARGO_TARGET_DIR refusal happens before the directory claim')

    def test_blank_cargo_target_is_refused(self):
        # A presence-only check used to let a blank value through (#2590
        # item 1); it must refuse exactly like an absent one.
        code, _stdout, stderr, out = run(environ={'CARGO_TARGET_DIR': '   '})
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('CARGO_TARGET_DIR', stderr)
        self.assertFalse(out.exists())

    def test_dot_cargo_target_is_refused(self):
        # Presence-valid but unusable: the shared rule rejects it, so no
        # artifact can head for the repository root.
        code, _stdout, stderr, out = run(environ={'CARGO_TARGET_DIR': '.'})
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('target cannot be dot', stderr)
        self.assertFalse(out.exists())

    def test_existing_output_directory_is_refused(self):
        code, _stdout, stderr, out = run(output_exists=True,
                                          environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('already exists', stderr)
        self.assertEqual(list(out.iterdir()), [])


class StaleAndTemplateGuards(unittest.TestCase):
    """A stale anchor or a template bound assertion is a refusal, not a traceback."""

    def test_stale_anchor_is_refused(self):
        code, _stdout, stderr, out = run(
            patch_source=("'''        has_inspectable |= is_read_file_inspectable(entry);\n"
                          "        if !seen.insert(lexical.clone()) {",
                          "'''        has_inspectable |= is_read_file_inspectable(entry);\n"
                          "        if !seen.insert(lexical.clone( )) {"),
            environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('stale anchor/test/assertion bindings', stderr)
        self.assertIn('dedup-gating-restored', stderr)
        self.assertFalse(out.exists())

    def test_template_assertion_is_refused(self):
        # #2590 item 3: an interpolated bound message can never match the
        # byte-for-byte receipt, so the row could only come out NO_RECEIPT.
        # The template text also sits in the test body, so the plain binding
        # scan stays green -- the brace check must refuse it up front.
        code, _stdout, stderr, out = run(
            patch_source=("\"#2495 the deduped second entry's inspectable must still select the read_file form\"",
                          "\"#2495 the deduped second entry {} must still select the read_file form\""),
            environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('format! template', stderr)
        self.assertIn('dedup-gating-restored', stderr)
        self.assertFalse(out.exists())

    def test_binding_scope_failure_is_refused(self):
        # The binding self-check runs before any mutation, so its failure is
        # a "never started" refusal (rc 2), not an uncaught traceback (1).
        code, _stdout, stderr, out = run(
            patch_source=("    if 'only sibling' in test_body('target', {TESTS: fixture}):\n",
                          "    if True:\n"),
            environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('binding checker', stderr)
        self.assertFalse(out.exists())


class CheckMode(unittest.TestCase):
    """The unmodified script's --check is the positive control."""

    def test_check_passes_on_the_current_tree(self):
        code, stdout, stderr, _out = run('--check')
        self.assertEqual(code, 0, f'stderr={stderr!r}')
        self.assertIn('registered mutations present', stdout)
        self.assertIn('OK', stdout)

    def test_check_refuses_a_template_assertion(self):
        # The brace guard fires in --check mode too, so the r1->r2 regression
        # shape (a static literal replaced by a format! template) is a
        # machine-checked --check failure, not a silent green.
        code, _stdout, stderr, _out = run(
            '--check',
            patch_source=("\"#2495 the deduped second entry's inspectable must still select the read_file form\"",
                          "\"#2495 the deduped second entry {} must still select the read_file form\""))
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('format! template', stderr)


class RosterDenominator(unittest.TestCase):
    """#2590 item 2: the summary denominator is the static roster, not the rows."""

    def setUp(self):
        self.module = load_module()

    def row(self, name):
        return dict(name=name, status='KILLED')

    def test_all_roster_names_recorded_is_no_drift(self):
        rows = [self.row(name) for name in self.module.EXPECTED_NAMES]
        self.assertIsNone(self.module.roster_drift(self.module.EXPECTED_NAMES, rows))

    def test_a_row_without_a_verdict_is_drift(self):
        # The skip-row shape item 2 guards against: the survivors are all
        # KILLED, but the run has measured one row less than the roster.
        rows = [self.row(name) for name in self.module.EXPECTED_NAMES[:-1]]
        drift = self.module.roster_drift(self.module.EXPECTED_NAMES, rows)
        self.assertIsNotNone(drift)
        recorded, expected = drift
        missing = [name for name in expected if name not in recorded]
        self.assertEqual(missing, [self.module.EXPECTED_NAMES[-1]])

    def test_zero_rows_is_never_no_drift(self):
        self.assertIsNotNone(
            self.module.roster_drift(self.module.EXPECTED_NAMES, []))

    def test_an_extra_or_duplicate_verdict_is_drift(self):
        rows = [self.row(name) for name in self.module.EXPECTED_NAMES]
        self.assertIsNotNone(
            self.module.roster_drift(self.module.EXPECTED_NAMES,
                                     rows + [rows[0]]))


class SharedExitContract(unittest.TestCase):
    """The exit contract stays the shared helpers, not a hand-rolled fold."""

    def test_unknown_name_is_refused_the_same_way_under_dash_o(self):
        # `specs/negative-controls/README.md:139-143`: no guard may depend on
        # an ``assert`` that ``-O`` strips. The shared matrix
        # (test_negative_optimize_guards.py) does not list this fixture, so
        # this is the local pin of the same contract.
        for optimize in (False, True):
            with self.subTest(optimize=optimize):
                out = Path(tempfile.mkdtemp(prefix='issue-2495-2590-o-')) / 'out'
                command = [sys.executable, *(['-O'] if optimize else []),
                           '-I', str(SCRIPT), str(out), 'no-such-mutation-2590']
                result = subprocess.run(command, capture_output=True, text=True,
                                        timeout=120,
                                        env={**os.environ,
                                             'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
                self.assertEqual(result.returncode, 2,
                                 f'-O={optimize} must refuse: {result.stderr!r}{result.stdout!r}')
                self.assertIn('no-such-mutation-2590', result.stderr)
                self.assertFalse(out.exists())

    def test_the_script_uses_verdict_exit_and_claim_dir(self):
        source = SCRIPT.read_text(encoding='utf-8')
        for needle in ('return verdict_exit(results, key=',
                       'output = claim_dir(args.output)',
                       'roster-mismatch.json'):
            self.assertIn(needle, source,
                          f'the startup/exit contract lost its shared shape: {needle!r}')


class TreeLockBusy(unittest.TestCase):
    """fix-r2: a busy tree lock is a refusal (rc 2), never a traceback (rc 1)."""

    def test_busy_tree_lock_is_refused_not_a_traceback(self):
        # DS-boundary#1 / MM-concurrency#1 (取高 P1): a sibling mutation round
        # holds the tree lock far past the 900 s default (every row is up to
        # four cargo calls), so a --check guards run can queue behind it and
        # time out. That timeout must be the same "never started" refusal
        # every other startup guard produces: an uncaught TimeoutError exits
        # 1 with a traceback and no evidence, which a host reads as a failed
        # control. A real subprocess holds the flock (the same
        # ROOT/.negative-source.lock the mutation round takes) and a patched
        # --check keeps the case fast; the ready file is published atomically
        # (tmp + replace) before the script under test is allowed to run.
        base = Path(tempfile.mkdtemp(prefix='issue-2495-2590-lock-'))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        ready = base / 'held'
        holder = subprocess.Popen([
            sys.executable, '-I', '-c',
            'import sys, time\n'
            'from pathlib import Path\n'
            'sys.path.insert(0, sys.argv[1])\n'
            'from negative_process import tree_lock\n'
            'with tree_lock(sys.argv[2]):\n'
            '    Path(sys.argv[3] + ".tmp").write_text("held")\n'
            '    Path(sys.argv[3] + ".tmp").replace(sys.argv[3])\n'
            '    time.sleep(600)\n',
            str(FIXTURES), str(FIXTURES.parents[3]), str(ready)])
        try:
            deadline = time.monotonic() + 30
            while not ready.exists():
                self.assertIsNone(
                    holder.poll(),
                    '#2590 fix-r2 the lock holder died before holding the tree lock')
                self.assertLess(
                    time.monotonic(), deadline,
                    '#2590 fix-r2 the lock holder never published its ready file')
                time.sleep(0.05)
            try:
                result = run(
                    '--check',
                    patch_source=(
                        '        with tree_lock(ROOT):\n'
                        '            stale = stale_names()',
                        '        with tree_lock(ROOT, timeout=2):\n'
                        '            stale = stale_names()'))
            except TimeoutError as error:
                self.fail(f'#2590 fix-r2 a busy tree lock must refuse (rc 2), '
                          f'not raise out of the script: {error!r}')
            code, stdout, stderr, out = result
            self.assertEqual(
                code, 2,
                f'#2590 fix-r2 the busy-lock timeout must be a refusal, got '
                f'{code}: stderr={stderr!r} stdout={stdout!r}')
            self.assertIn('INVALID:', stderr)
            self.assertIn('TimeoutError', stderr,
                          '#2590 fix-r2 the refusal must name the lock timeout')
            self.assertIn('source lock busy', stderr)
            self.assertFalse(out.exists())
        finally:
            holder.kill()
            holder.wait(timeout=30)


class SummaryWording(unittest.TestCase):
    """fix-r2: --compile-only reports COMPILED, not a false ``KILLED=0/N``."""

    def setUp(self):
        self.module = load_module()

    def test_compile_only_reports_compiled_not_a_false_killed_zero(self):
        # DS-boundary#2: under --compile-only every row records COMPILED and
        # the exit contract counts exactly that, so the summary line must
        # not read `KILLED=0/N` ("everything survived") next to a green exit.
        counts = {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 0, 'COMPILED': 7}
        line = self.module.summary_line(counts, 7, True)
        self.assertTrue(line.startswith('summary: COMPILED=7/7'),
                        f'#2590 fix-r2 --compile-only must report COMPILED, '
                        f'got {line!r}')
        self.assertNotIn('KILLED=', line,
                         '#2590 fix-r2 a COMPILED run must not read as '
                         'all-survived')

    def test_the_default_run_still_reports_killed(self):
        counts = {'KILLED': 7, 'SURVIVED': 0, 'INVALID': 0, 'COMPILED': 7}
        line = self.module.summary_line(counts, 7, False)
        self.assertTrue(line.startswith('summary: KILLED=7/7'),
                        f'#2590 fix-r2 the default run must keep KILLED, '
                        f'got {line!r}')


if __name__ == '__main__':
    unittest.main()
