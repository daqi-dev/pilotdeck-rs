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
sibling mutation round can hold it for the length of a full round, and that
timeout is the same "never started" refusal, never an uncaught traceback
a host reads as rc 1; fix-r3 bounds the guard's wait to 60 s and extends the
same rc 2 refusal to the mutation round's own lock acquisition, which also
publishes no verdict; fix-r3b scopes that refusal to the ACQUISITION phase --
a lock timeout or a not-pristine tree at entry refuses and withdraws the
empty claimed directory, while any failure after acquisition, including the
restore window, keeps the red-run shape of INVALID rows, a published summary
and rc 1; within the acquisition phase the refusal maps only TimeoutError
and SourceBaselineError -- raw acquisition failures (a bare git RuntimeError,
a cold-boot cargo failure, a bare snapshot-read OSError) stay red runs at
rc 1, and signals are outside this table entirely, 128+signum per the README
signal paragraph). A stray positional argument, an empty output slot or a
``--compile-only`` flag next to ``--check`` is refused too (fix-r3/r3b).
Instead of duplicating the guard logic here, each
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
import json
import os
from pathlib import Path
import shutil
import signal
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

# fix-r3 (MM r2 concurrency#2 P2): every in-process case runs with the
# guard's tree-lock wait shrunk from the 60 s production bound to 2 s --
# identical while the lock is free (which it is in every case here), but a
# sibling round holding the tree lock during a test run fails the case in
# seconds instead of hanging on the full wait before refusing.
GUARD_LOCK_FAST = ('        with tree_lock(ROOT, timeout=60):\n'
                   '            stale = stale_names()',
                   '        with tree_lock(ROOT, timeout=2):\n'
                   '            stale = stale_names()')

# fix-r3 (MM r2 concurrency#3 P2): the lock holder runs in its own session
# with a bounded wait and a 30 s hold (an order of magnitude past what the
# 2 s acquisition waits need), so cleanup can group-kill it and a SIGKILLed
# test run cannot leave an orphan holding the repository tree lock for
# minutes.
LOCK_HOLDER_SOURCE = (
    'import sys, time\n'
    'from pathlib import Path\n'
    'sys.path.insert(0, sys.argv[1])\n'
    'from negative_process import tree_lock\n'
    'with tree_lock(sys.argv[2], timeout=10):\n'
    '    Path(sys.argv[3] + ".tmp").write_text("held")\n'
    '    Path(sys.argv[3] + ".tmp").replace(sys.argv[3])\n'
    '    time.sleep(30)\n'
)

_RUN_BASES = []


def run(*argv, patch_source=None, environ=None, output_exists=False):
    """Exec the control script in-process; return (code, stdout, stderr, out).

    ``patch_source`` is one ``(before, after)`` edit applied to the real
    script text, or a sequence of such pairs applied in order after the
    shared guard-lock bound (fix-r3: the mutation-lock case needs two).
    ``output_exists`` makes argv[1] point at a directory that already
    exists. A leading ``'--check'`` replaces the positional output slot,
    which is the fixture's own CLI shape. Every ``base`` is registered for
    ``tearDownModule`` so a case leaves no mkdtemp residue behind (fix-r3,
    MM r2 concurrency#4: one CI run used to leak ~15 directories).
    """
    source = SCRIPT.read_text(encoding='utf-8')
    edits = [GUARD_LOCK_FAST]
    if patch_source is not None:
        edits += (list(patch_source) if isinstance(patch_source[0], tuple)
                  else [patch_source])
    for before, after in edits:
        if source.count(before) != 1:
            raise AssertionError(f'test anchor occurs {source.count(before)} times: {before!r}')
        source = source.replace(before, after, 1)
    base = Path(tempfile.mkdtemp(prefix='issue-2495-2590-'))
    _RUN_BASES.append(base)
    out = base / 'out'
    if output_exists:
        out.mkdir()
    if argv and argv[0] == '--check':
        script_argv = [str(SCRIPT), '--check', *argv[1:]]
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


    def test_check_with_positional_args_is_refused(self):
        # fix-r3 (MM r2 boundary#1 P2a): --check's argv contract (no output
        # directory, no names) is machine-checked, not just documented in the
        # usage line -- a typo'd `--check stray-name` used to return the same
        # green "OK" line as the real roster check without ever reading it.
        code, _stdout, stderr, out = run('--check', 'stray-output-dir')
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('--check takes no output directory', stderr)
        self.assertFalse(out.exists())

    def test_check_with_empty_output_arg_is_refused(self):
        # fix-r3b (DS r3 boundary#1 P2a): `--check ''` is the same contract
        # hole with a legal-looking shape -- argparse assigns the empty
        # string to the positional slot, the old `args.output or ...` truth
        # check let it through and the guard's answer never ran. The
        # refusal now keys on ``is not None``.
        code, _stdout, stderr, out = run('--check', '')
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('--check takes no output directory', stderr)
        self.assertFalse(out.exists())

    def test_check_with_compile_only_is_refused(self):
        # fix-r3b (MM r3 boundary#5 P3, folded into the same guard line):
        # `--check --compile-only` used to be accepted and silently ignored
        # -- the --check branch returned before args.compile_only was ever
        # read, so the caller believed a compile-only roster check had run.
        code, _stdout, stderr, out = run('--check', '--compile-only')
        self.assertEqual(code, 2)
        self.assertIn('INVALID:', stderr)
        self.assertIn('--check takes no output directory', stderr)
        self.assertFalse(out.exists())


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
        # `specs/negative-controls/README.md:149-153`: no guard may depend on
        # an ``assert`` that ``-O`` strips. The shared matrix
        # (test_negative_optimize_guards.py) has listed this fixture since
        # fix-r2; this case stays as the local pin of the same contract for
        # the ``--check`` mode, which the matrix does not drive.
        for optimize in (False, True):
            with self.subTest(optimize=optimize):
                # fix-r3b (DS r3 concurrency#5 P3 / boundary#4 P3 / MM r3
                # boundary#4 P3): this case spawns its own subprocess, so it
                # never goes through run(); its base is registered for the
                # module teardown too -- otherwise the "no mkdtemp residue"
                # claim above only holds for the in-process cases.
                base = Path(tempfile.mkdtemp(prefix='issue-2495-2590-o-'))
                _RUN_BASES.append(base)
                out = base / 'out'
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
        # fix-r3 (DS r2 boundary#1/#2, MM r2 boundary#4 P2): the needles pin
        # code shapes, not bare words -- 'roster-mismatch.json' alone was
        # satisfiable by the module docstring, and neither the drift branch
        # nor the summary call site had any machine check (both fix-r2
        # regressions could return with this suite green).
        source = SCRIPT.read_text(encoding='utf-8')
        for needle in ('return verdict_exit(results, key=',
                       'output = claim_dir(args.output)',
                       'if drift is not None:',
                       "mismatch.replace(output / 'roster-mismatch.json')",
                       'expected = [name for name in EXPECTED_NAMES',
                       'summary_line(counts, len(expected), args.compile_only)'):
            self.assertIn(needle, source,
                          f'the startup/exit contract lost its shared shape: {needle!r}')


class TreeLockBusy(unittest.TestCase):
    """A busy tree lock is a refusal (rc 2), never a traceback or a failed round."""

    def hold_the_tree_lock(self, prefix):
        """A real subprocess holds ROOT/.negative-source.lock; ready-file handshake."""
        base = Path(tempfile.mkdtemp(prefix=prefix))
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        ready = base / 'held'
        holder = subprocess.Popen(
            [sys.executable, '-I', '-c', LOCK_HOLDER_SOURCE,
             str(FIXTURES), str(FIXTURES.parents[3]), str(ready)],
            start_new_session=True)
        self.addCleanup(self._kill_holder, holder)
        return holder, ready

    @staticmethod
    def _kill_holder(holder):
        # Group kill, not a bare kill(): the holder runs in its own session,
        # so a plain kill() only reaches the leader (fix-r3, MM r2
        # concurrency#3 P2). The holder may exit between the poll and the
        # group signal (its 30 s hold expiring, its own acquisition failing);
        # a vanished group is not an error, the same tolerance
        # negative_process._kill_group has (fix-r3b, DS r3 concurrency#3 P3
        # / boundary#3 P3).
        if holder.poll() is None:
            try:
                os.killpg(os.getpgid(holder.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
        holder.wait(timeout=30)

    def wait_until_held(self, holder, ready):
        # The ready file is published atomically (tmp + replace) before the
        # script under test is allowed to run.
        deadline = time.monotonic() + 30
        while not ready.exists():
            self.assertIsNone(
                holder.poll(),
                'the lock holder died before holding the tree lock')
            self.assertLess(
                time.monotonic(), deadline,
                'the lock holder never published its ready file')
            time.sleep(0.05)

    def test_busy_tree_lock_is_refused_not_a_traceback(self):
        # DS-boundary#1 / MM-concurrency#1 (取高 P1): a sibling mutation round
        # holds the tree lock far past any wait (every row is up to four
        # cargo calls), so a --check guards run can queue behind it and time
        # out. That timeout must be the same "never started" refusal every
        # other startup guard produces: an uncaught TimeoutError exits 1
        # with a traceback and no evidence, which a host reads as a failed
        # control. The shared GUARD_LOCK_FAST bound keeps the case fast.
        holder, ready = self.hold_the_tree_lock('issue-2495-2590-lock-')
        self.wait_until_held(holder, ready)
        try:
            result = run('--check')
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

    def test_mutation_lock_timeout_is_refused_not_a_failed_round(self):
        # fix-r3 (MM r2 concurrency#1 P1 / DS r2 concurrency#1 / MM r2
        # sibling#2, 两家同撞取高 P1): a sibling can hold the tree lock when
        # the mutation round goes to acquire it -- the guard scan and the
        # mutation round are separate critical sections -- and that timeout
        # must land on the same "never started" side as the guard's (rc 2,
        # refuse), not exit 1 over INVALID rows a host would read as a
        # finished, not-green round (fix-r2's README/CI wording carries no
        # stage qualifier; issue-297 r7 is the precedent). The guard's lock
        # is bypassed here (it already has the busy-lock case above and the
        # lock is held from the start, so the interleaving-specific case is
        # the mutation acquisition itself); the wait is shrunk to 2 s so the
        # case stays fast. fix-r3b (MM r3 boundary#1 P2a, utf16
        # startup_refusal precedent): the "never started" signal is rc 2
        # plus the empty evidence directory withdrawn -- no summary, and no
        # claimed-looking directory either.
        holder, ready = self.hold_the_tree_lock('issue-2495-2590-mlock-')
        self.wait_until_held(holder, ready)
        result = run(
            environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'},
            patch_source=[
                ('        with tree_lock(ROOT, timeout=2):\n'
                 '            stale = stale_names()',
                 '        stale = stale_names()'),
                ('            with mutating_sources(ROOT, guarded) as originals:',
                 '            with mutating_sources(ROOT, guarded, lock_timeout=2) as originals:'),
            ])
        code, stdout, stderr, out = result
        self.assertEqual(
            code, 2,
            f'#2590 fix-r3 the mutation-round lock timeout must be a '
            f'refusal, got {code}: stderr={stderr!r} stdout={stdout!r}')
        self.assertIn('INVALID:', stderr)
        self.assertIn('source lock busy', stderr)
        self.assertIn('never started', stderr)
        self.assertFalse(out.exists(),
                         'a never-started refusal withdraws the empty '
                         'claimed directory (utf16 startup_refusal)')
        self.assertFalse((out / 'summary.json').exists(),
                         'a never-started round must publish no verdict')
        self.assertFalse((out / 'roster-mismatch.json').exists())

    def test_restore_timeout_after_entry_is_a_failed_round(self):
        # fix-r3b (DS r3 concurrency#1 P1, 实测 TimeoutError ⊂ OSError 且
        # errno.ETIMEDOUT 映射到它): the rc 2 refusal is scoped to the
        # acquisition phase, and within it to the two mapped exception
        # types, not to every exception the phase can raise. A TimeoutError raised
        # while EXITING mutating_sources -- an ETIMEDOUT on the restore
        # write-back -- happens after the round has run and possibly after
        # mutants were written, so it must fall to the generic handler:
        # INVALID rows, a published summary.json, rc 1. Refusing there would
        # misfile a started, possibly still-dirty round as "never started".
        # mutating_sources is replaced by a stub that enters cleanly and
        # raises TimeoutError on exit; the row loop is emptied so the case
        # runs no cargo at all.
        import contextlib
        import negative_process

        @contextlib.contextmanager
        def fake_mutating_sources(root, paths, **kwargs):
            yield {path: b'fixture bytes' for path in paths}
            raise TimeoutError('simulated ETIMEDOUT on the restore write-back')

        with patch.object(negative_process, 'mutating_sources',
                          fake_mutating_sources):
            code, stdout, stderr, out = run(
                environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'},
                patch_source=(
                    '                for name, path, old, new, test, assertion in selected:',
                    '                for name, path, old, new, test, assertion in []:'))
        self.assertEqual(
            code, 1,
            f'a restore-phase timeout is a failed round (rc 1), not a '
            f'refusal: stderr={stderr!r} stdout={stdout!r}')
        self.assertNotIn('never started', stderr)
        self.assertNotIn('source lock busy', stderr)
        self.assertTrue((out / 'summary.json').exists(),
                        'a round that started must publish its verdict')
        self.assertIn('INVALID=7', stdout)
        summary = json.loads((out / 'summary.json').read_text(encoding='utf-8'))
        self.assertEqual(summary['counts']['INVALID'], 7)
        self.assertIn('source guard: TimeoutError',
                      summary['controls'][0]['reason'])

    def test_restore_source_baseline_error_after_entry_is_a_failed_round(self):
        # r3b4 DS sibling#1 P2a（2×2 相位表缺 exit×SourceBaselineError 格）:
        # the acquired guard applies to both mapped exception types alike.
        # The cell is reachable: the contextmanager's finally re-verifies
        # via _head_bytes, which wraps git failures into SourceBaselineError
        # — a restore-window git failure with the flag already set must land
        # on the red-run side (rc 1, published summary, revoked rows), never
        # a "never started" refusal, exactly like the TimeoutError twin.
        import contextlib
        import negative_process

        @contextlib.contextmanager
        def fake_mutating_sources(root, paths, **kwargs):
            yield {path: b'fixture bytes' for path in paths}
            raise negative_process.SourceBaselineError(
                root, 'src/pilotdeck-gateway/src/native_process/services/attachments.rs',
                'simulated git failure on the restore re-verify')

        with patch.object(negative_process, 'mutating_sources',
                          fake_mutating_sources):
            code, stdout, stderr, out = run(
                environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'},
                patch_source=(
                    '                for name, path, old, new, test, assertion in selected:',
                    '                for name, path, old, new, test, assertion in []:'))
        self.assertEqual(
            code, 1,
            f'a restore-phase SourceBaselineError is a failed round (rc 1), '
            f'not a refusal: stderr={stderr!r} stdout={stdout!r}')
        self.assertNotIn('never started', stderr)
        self.assertNotIn('tree not pristine', stderr)
        self.assertTrue((out / 'summary.json').exists(),
                        'a round that started must publish its verdict')
        self.assertIn('INVALID=7', stdout)
        summary = json.loads((out / 'summary.json').read_text(encoding='utf-8'))
        self.assertEqual(summary['counts']['INVALID'], 7)
        self.assertIn('source guard: SourceBaselineError',
                      summary['controls'][0]['reason'])

    def test_pristine_failure_at_entry_is_refused_not_a_failed_round(self):
        # fix-r3b (MM r3 concurrency#1 P2a): a sibling round SIGKILLed
        # mid-mutation leaves its mutant on disk; this round's acquisition
        # must refuse that as never-started (rc 2, spec rc table: 树脏 → 2),
        # not fold it into seven INVALID rows and a published summary the
        # host would read as a finished, not-green round. The stub raises
        # SourceBaselineError from __enter__, the same point the real
        # _assert_workspace_sources / _assert_product_pristine run.
        import contextlib
        import negative_process

        @contextlib.contextmanager
        def fake_mutating_sources(root, paths, **kwargs):
            raise negative_process.SourceBaselineError(
                root, 'src/pilotdeck-gateway/src/native_process/services/attachments.rs',
                'simulated sibling residue on disk')
            yield {path: b'fixture bytes' for path in paths}

        with patch.object(negative_process, 'mutating_sources',
                          fake_mutating_sources):
            code, stdout, stderr, out = run(
                environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(
            code, 2,
            f'a pristine failure at entry is a refusal, got {code}: '
            f'stderr={stderr!r} stdout={stdout!r}')
        self.assertIn('INVALID:', stderr)
        self.assertIn('tree not pristine', stderr)
        self.assertIn('never started', stderr)
        self.assertFalse(out.exists(),
                         'a never-started refusal withdraws the empty '
                         'claimed directory (utf16 startup_refusal)')
        self.assertFalse((out / 'summary.json').exists())
        self.assertFalse((out / 'roster-mismatch.json').exists())

    def test_acquisition_phase_raw_failure_is_a_red_run(self):
        # fix-r3b3 (final DS sibling#2 P2 / boundary#1+sib#3 精化): within
        # the acquisition phase only TimeoutError and SourceBaselineError
        # map to the rc 2 refusal; the WRAPPED git/scanner failures already
        # reach SourceBaselineError, but a RAW acquisition failure -- here
        # a bare RuntimeError from the preflight _assert_workspace_sources,
        # the same call shape _git uses when a preflight diff/ls-files
        # call itself fails -- must stay a red run: INVALID rows, a
        # published summary.json, rc 1. This runs the REAL
        # mutating_sources (only the preflight is patched), so it also pins
        # that the wrapping boundary is exactly where the README says.
        import negative_process

        def raw_preflight_failure(root, baseline):
            raise RuntimeError(
                'git diff --name-only HEAD -- src: simulated hard failure')

        with patch.object(negative_process, '_assert_workspace_sources',
                          raw_preflight_failure):
            code, stdout, stderr, out = run(
                environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(
            code, 1,
            f'a raw acquisition-phase failure is a red run (rc 1), not a '
            f'refusal: stderr={stderr!r} stdout={stdout!r}')
        self.assertNotIn('never started', stderr)
        self.assertNotIn('tree not pristine', stderr)
        self.assertTrue((out / 'summary.json').exists(),
                        'a raw acquisition failure still publishes its '
                        'verdict as a failed round')
        self.assertIn('INVALID=7', stdout)
        summary = json.loads((out / 'summary.json').read_text(encoding='utf-8'))
        self.assertEqual(summary['counts']['INVALID'], 7)
        self.assertIn('source guard: RuntimeError',
                      summary['controls'][0]['reason'])


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


def tearDownModule():
    # fix-r3 (MM r2 concurrency#4 P2): every in-process case's mkdtemp base
    # is registered by run(); one CI run used to leak ~15 directories.
    for base in _RUN_BASES:
        shutil.rmtree(base, ignore_errors=True)


if __name__ == '__main__':
    unittest.main()
