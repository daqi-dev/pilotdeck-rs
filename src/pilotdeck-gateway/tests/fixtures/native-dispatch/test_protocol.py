# anchor-selfcheck: unsupported isolated runner protocol tests; no product source mutations
"""Exercise runner lease/error handling with temporary fake sources and processes."""
from contextlib import contextmanager, redirect_stderr
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('native_dispatch_controls', Path(__file__).with_name('negative_controls.py'))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class Protocol(unittest.TestCase):
    def exercise(self, *, guard_error=False, compile_error=False, cancel=False, unrelated=False,
                 compile_only=False, fail_enter=False, signal_exit=False):
        with tempfile.TemporaryDirectory(prefix='pdrs-1305-protocol-') as temporary:
            root = Path(temporary)
            source = root / 'fake-source.txt'
            source.write_text('original')
            out = root / 'result'
            rows = [(name, source, 'original', 'changed', 'bound_test', '#1305 bound')
                    for name in ('first', 'second')]
            active = False
            restored = 0
            classified = 0
            compilations = 0
            pending_restore = False
            # #1642 fix-r4 (review P1-D, DS sibling#1): write_source_atomic is
            # patched unconditionally, exactly like native-management's drive()
            # (a7). With the real writer, the mutation bytes were staged
            # through the real tree's `.negctl-publish/` with no source lease
            # held, and a TMPDIR on another device failed every write with
            # EXDEV -- an all-INVALID rc 1 batch that happened to satisfy the
            # failure assertions (a false PASS). plain_writer also proves the
            # "never touches the real tree" half: every byte it lands stays
            # inside this temporary root, and the writes counter below is what
            # makes the patch load-bearing (dropping it empties the counter).
            writes = []

            def plain_writer(_root, path, data, **_kwargs):
                path = Path(path)
                self.assertTrue(root in path.parents,
                                '#1642 fix-r4 the protocol writes must stay inside the '
                                'temporary root, never the real tree')
                writes.append(path)
                path.write_bytes(data)

            @contextmanager
            def guard(_root, _paths):
                nonlocal active
                if fail_enter:
                    raise RuntimeError('#1305 injected guard enter failure')
                active = True
                try:
                    yield {source: b'original'}
                finally:
                    # A failed runner must still attempt its per-case restore.
                    self.assertTrue(source.read_text() == 'original', '#1305 protocol restores source')
                    active = False
                    if guard_error:
                        raise RuntimeError('#1305 injected guard exit failure')
                    if signal_exit:
                        raise SystemExit(143)

            def run(command, **kwargs):
                nonlocal restored, compilations, pending_restore
                self.assertTrue(active, '#1305 protocol cargo stays inside lease')
                self.assertFalse(any(key in kwargs['env'] for key in
                                     ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT')),
                                 '#1305 protocol discards inherited receipts')
                if '--no-run' in command:
                    compilations += 1
                    pending_restore = True
                    if cancel and compilations == 2:
                        raise KeyboardInterrupt('#1305 injected cancellation')
                    return subprocess.CompletedProcess(command, int(compile_error and compilations == 1), '', '')
                if source.read_text() == 'original' and pending_restore:
                    restored += 1
                    pending_restore = False
                return subprocess.CompletedProcess(command, 0,
                    'test bound_test ... ok\n\ntest result: ok. 1 passed; 0 failed;\n', '')

            def classify(_result, _case):
                nonlocal classified
                classified += 1
                return ('INVALID', 'wrong panic') if unrelated and classified == 1 else ('KILLED', 'bound assertion')

            argv = ['controls', str(out)] + (['--compile-only'] if compile_only else [])
            stderr = io.StringIO()
            with redirect_stderr(stderr), \
                 patch.object(RUNNER, 'mutations', rows), \
                 patch.object(RUNNER, 'mutating_sources', guard), \
                 patch.object(RUNNER, 'run_owned', run), \
                 patch.object(RUNNER, 'wait_for_slot', return_value=subprocess.CompletedProcess([], 0, '', '')), \
                 patch.object(RUNNER, 'classify', classify), \
                 patch.object(RUNNER, 'write_source_atomic', plain_writer), \
                 patch.object(sys, 'argv', argv), \
                 patch.dict(os.environ, CARGO_TARGET_DIR=str(root / 'unused-target'),
                            PDRS_NEGATIVE_RECEIPTS='stale', PDRS_NEGATIVE_PARENT_TEST='stale', PDRS_MUTANT='stale'):
                try:
                    code = RUNNER.main()
                except SystemExit as refusal:
                    # #1642 fix-r3: a refusal (rc 2) or a delayed signal
                    # (rc 128+signum) leaves main() through SystemExit; the
                    # code is the process exit shape.
                    code = refusal.code
            # #1642 fix-r4 (review P1-B): `claimed` is read inside the
            # temporary directory's lifetime (this with-block deletes the
            # tree on return) -- the enter-refusal question is whether the
            # claim_dir claim was withdrawn.
            claimed = out.exists()
            summary = None
            if claimed and (out / 'summary.json').is_file():
                summary = json.loads((out / 'summary.json').read_text())['controls']
            # #1642 fix-r4 (review P1-D, DS sibling#1): the patch above is
            # load-bearing. If it were dropped, the real write_source_atomic
            # would stage through the real tree's `.negctl-publish/` and every
            # write would leave this temporary root (and die on EXDEV with an
            # off-TMPDIR TMPDIR) -- both pinned here.
            if not fail_enter:
                self.assertTrue(writes,
                                '#1642 fix-r4 the batch must publish its mutant bytes through '
                                'the patched writer (the writes counter is what makes the '
                                'plain_writer patch load-bearing)')
            # #3143 fix-r1 (P2#4, review concurrency#2): the shared
            # staging directory is no longer observed across the run. Its
            # existence is a cross-process fact -- a concurrent unpatched
            # writer (another suite's full negative-control round) can create
            # `.negctl-publish` between two looks and false-red this suite.
            # The load-local facts above already pin what this run does:
            # every write the batch made stayed inside the temporary root
            # (plain_writer's own assertion), and the writes counter
            # catches a dropped writer patch.
            return code, summary, restored, compilations, claimed, stderr.getvalue()

    def test_restored_runs_inside_lease(self):
        code, rows, restored, compilations, claimed, _ = self.exercise()
        self.assertTrue(code == 0 and all(row['status'] == 'KILLED' for row in rows),
                        '#1305 protocol successful batch')
        self.assertTrue(restored == 2 and compilations == 2, '#1305 protocol compiles and restores every case')

    def test_guard_exit_invalidates_completed(self):
        code, rows, _, _, claimed, _ = self.exercise(guard_error=True)
        # #1642 fix-r3 (P1-2): the exit-phase failure now returns the red-run
        # code (1) with its all-INVALID summary published -- the old inverted
        # map returned 2 here, and a host reading "rc 2 = never started"
        # dropped the published failure evidence.
        self.assertTrue(code == 1 and len(rows) == 2, '#1642 fix-r3 protocol failed exit is a red run')
        self.assertTrue(all(row['status'] == 'INVALID' and not row['compiled'] for row in rows),
                        '#1305 protocol failed exit revokes every kill')
        # #1642 fix-r4 (review P1-B): the red-run half of the phase map
        # keeps its evidence -- only the never-started half withdraws.
        self.assertTrue(claimed, '#1642 fix-r4 a red run keeps its evidence directory')

    # #1642 fix-r4 (review P1-B): the enter refusal is now pinned the way the
    # four-way exit-code table reads it -- rc 2, no summary published, the
    # claim_dir claim withdrawn (the host can re-run the same output path),
    # and the refusal names the guard failure. fix-r3's version here pinned
    # an all-INVALID summary over rc 2, which contradicted the table.
    def test_guard_enter_failure_refuses(self):
        code, rows, _, _, claimed, stderr = self.exercise(fail_enter=True)
        self.assertTrue(code == 2,
                        '#1642 fix-r4 a guard that never started refuses with rc 2, not a red run')
        self.assertTrue(rows is None,
                        '#1642 fix-r4 an enter-phase refusal publishes no summary')
        self.assertFalse(claimed,
                        '#1642 fix-r4 an enter-phase refusal withdraws the claimed '
                        'evidence directory (no leftover claim shape)')
        self.assertTrue('guard refused to start' in stderr,
                        '#1642 fix-r4 the enter refusal must name the guard failure')

    def test_compile_failure_continues(self):
        code, rows, _, compilations, _, _ = self.exercise(compile_error=True)
        self.assertTrue(code == 1 and compilations == 2, '#1305 protocol compile error continues batch')
        self.assertTrue([row['status'] for row in rows] == ['INVALID', 'KILLED'],
                        '#1305 protocol compile error is never a kill')

    def test_unrelated_panic_is_invalid(self):
        code, rows, _, _, _, _ = self.exercise(unrelated=True)
        self.assertTrue(code == 1 and [row['status'] for row in rows] == ['INVALID', 'KILLED'],
                        '#1305 protocol preserves shared wrong-panic verdict')

    def test_cancellation_invalidates_completed(self):
        code, rows, _, _, _, _ = self.exercise(cancel=True)
        # #1642 fix-r3 (P1-2): a cancellation after the body ran is a red run
        # (rc 1) like every other post-enter guard failure; the isinstance
        # special case used to send it out as 2 ("never started") over an
        # all-INVALID summary that was in fact published.
        self.assertTrue(code == 1 and len(rows) == 2, '#1642 fix-r3 protocol cancellation is a red run')
        self.assertTrue(all(row['status'] == 'INVALID' and not row['compiled'] for row in rows),
                        '#1305 protocol cancellation revokes earlier kill')

    # #1642 fix-r3 (review a9 sibling shape): the delayed-signal SystemExit
    # that the guard raises after a verified restore survives main()'s
    # handler untouched -- rc 143, and no summary published over the
    # restored tree.
    def test_deferred_signal_survives_the_guard_handler(self):
        code, rows, restored, compilations, _, _ = self.exercise(signal_exit=True)
        self.assertTrue(code == 143, '#1642 fix-r3 deferred SIGTERM exits 128+signum, not the rewritten rc 1')
        self.assertTrue(rows is None, '#1642 fix-r3 deferred signal publishes no summary')
        self.assertTrue(restored == 2 and compilations == 2,
                        '#1642 fix-r3 the restored batch finished before the signal landed')

    def test_compile_only_is_not_a_kill(self):
        code, rows, restored, compilations, _, _ = self.exercise(compile_only=True)
        self.assertTrue(code == 0 and compilations == 2 and restored == 0,
                        '#1305 protocol compile-only compiles every case')
        self.assertTrue(all(row['status'] == 'COMPILED' for row in rows),
                        '#1305 protocol compile-only never claims kill')

    # #1642 fix-r4 (review P1-B sibling#3): selection errors are refusals
    # (rc 2) raised before the evidence directory is claimed, with the
    # offending names on stderr. The old `raise SystemExit(str)` carried the
    # message but read as rc 1 ("ran and not green") on the four-way table.
    def refuse_with(self, argv, environment):
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            with patch.object(sys, 'argv', argv), \
                 patch.dict(os.environ, environment):
                with self.assertRaises(SystemExit) as raised:
                    RUNNER.main()
        return raised.exception.code, stderr.getvalue()

    def test_duplicate_names_are_rejected(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1305-protocol-') as temporary:
            out = Path(temporary) / 'result'
            code, stderr = self.refuse_with(
                ['controls', str(out), 'omit-runs', 'omit-runs'],
                {'CARGO_TARGET_DIR': str(Path(temporary) / 't')})
            self.assertTrue(code == 2,
                            '#1642 fix-r4 duplicate names refuse with rc 2, not the rc 1 '
                            'the bare SystemExit(str) used to carry')
            self.assertTrue('repeated mutation name' in stderr and 'omit-runs' in stderr,
                            '#1642 fix-r4 the refusal must name the repeated mutation')
            self.assertFalse(out.exists(),
                            '#1642 fix-r4 the refusal must not claim the evidence directory')

    def test_unknown_name_refuses_with_rc2(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1305-protocol-') as temporary:
            out = Path(temporary) / 'result'
            code, stderr = self.refuse_with(
                ['controls', str(out), 'no-such-mutation-1642'],
                {'CARGO_TARGET_DIR': str(Path(temporary) / 't')})
            self.assertTrue(code == 2,
                            '#1642 fix-r4 an unknown mutation name refuses with rc 2')
            self.assertTrue('unknown mutation name' in stderr and 'no-such-mutation-1642' in stderr,
                            '#1642 fix-r4 the refusal must name the unknown mutation')
            self.assertFalse(out.exists(),
                            '#1642 fix-r4 the refusal must not claim the evidence directory')

    # #1642 fix-r4 (review P1-B sibling#3, spec specs/negative-controls/
    # README.md four-way table): an existing output directory is another
    # "never started" refusal -- claim_dir instead of the #953-shaped
    # FileExistsError traceback that reads as rc 1.
    def test_existing_output_directory_refuses_with_rc2(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1305-protocol-') as temporary:
            out = Path(temporary) / 'result'
            out.mkdir()
            code, stderr = self.refuse_with(
                ['controls', '--compile-only', str(out)],
                {'CARGO_TARGET_DIR': str(Path(temporary) / 't')})
            self.assertTrue(code == 2,
                            '#1642 fix-r4 an existing output directory refuses with rc 2, '
                            'not the FileExistsError traceback it used to raise')
            self.assertTrue('already exists' in stderr,
                            '#1642 fix-r4 the refusal must say the directory is taken')
            self.assertEqual(list(out.iterdir()), [],
                            '#1642 fix-r4 the refusal must not write into another '
                            'run\'s directory')

    # #1642 fix-r4 (review P1-B sibling#3, spec specs/negative-controls/
    # README.md:174,221-225): a blank CARGO_TARGET_DIR refuses with rc 2
    # before the evidence directory is claimed, instead of dying inside the
    # first cargo invocation after the claim.
    def test_missing_target_refuses_with_rc2(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1305-protocol-') as temporary:
            out = Path(temporary) / 'result'
            code, stderr = self.refuse_with(
                ['controls', '--compile-only', str(out)],
                {'CARGO_TARGET_DIR': ''})
            self.assertTrue(code == 2,
                            '#1642 fix-r4 a blank CARGO_TARGET_DIR refuses with rc 2')
            self.assertTrue('CARGO_TARGET_DIR' in stderr,
                            '#1642 fix-r4 the refusal must name the unusable target')
            self.assertFalse(out.exists(),
                            '#1642 fix-r4 the refusal must not claim the evidence directory')

    # #1642 fix-r5 (review sibling P1#2, the drift-probe shape
    # native-registry-switch/test_roster.py and native-management both use):
    # the roster is the shared registered + refuse pair, so a drifted
    # EXPECTED_NAMES is a refusal (rc 2, never started) that names the drift
    # -- deleting a row from the table can no longer shrink the run into a
    # smaller green batch.
    def test_roster_drift_refuses_with_rc2(self):
        source = Path(RUNNER.__file__).read_text()
        pattern = "'management-phase-swap',\n)"
        self.assertTrue(source.count(pattern) == 1,
                        '#1642 fix-r5 the roster drift probe must replace exactly one '
                        'EXPECTED_NAMES entry')
        drifted = source.replace(pattern, "'management-phase-swap-renamed',\n)", 1)
        # #3143 fix-r1 (P1#3, review concurrency#1): the copy can no longer
        # sit beside the original -- ``mkstemp(dir=RUNNER.__file__.parent)``
        # put it inside ``src/``, which the clean-tree preflight scans with
        # ``git ls-files --others`` (ignored files count), so a copy alive
        # while a concurrent guarded round preflights the tree read as an
        # "untracked build input" and refused that real round (a false
        # rc 2), and a SIGKILL inside the mkstemp-unlink window left the
        # residue there permanently locking the worktree out. ``_tmp/`` is
        # gitignored and outside every scanned area; the three padding
        # levels keep the copy at the original's directory depth, so the
        # module-level ``ROOT = Path(__file__).resolve().parents[5]`` still
        # resolves the repo root (a shallow CI /tmp path died with an
        # IndexError before the roster refusal could even raise, job
        # 131861169897), and the real fixtures directory is already on
        # sys.path -- this module's own import put it there -- so the
        # copy's sibling imports resolve unchanged. Bytecode caching is
        # off so the scratch tree gains no __pycache__ from the copy.
        root = Path(RUNNER.__file__).resolve().parents[5]
        (root / '_tmp').mkdir(exist_ok=True)
        scratch = Path(tempfile.mkdtemp(dir=root / '_tmp', prefix='pdrs-3143-drift-'))
        copy_dir = scratch / 'a' / 'b' / 'c'
        copy = None
        stderr = io.StringIO()
        previous = sys.dont_write_bytecode
        sys.dont_write_bytecode = True
        try:
            # The fallible setup lives under the guard too: a mkdir or
            # mkstemp failure must unwind through this block's cleanup
            # instead of leaking the scratch tree (#3143 fix-r1 P1#3
            # residue case).
            copy_dir.mkdir(parents=True)
            descriptor, copy_name = tempfile.mkstemp(dir=copy_dir,
                                                      prefix='drifted-1642-', suffix='.py')
            copy = Path(copy_name)
            with os.fdopen(descriptor, 'w') as handle:
                handle.write(drifted)
            spec = importlib.util.spec_from_file_location('drifted_native_dispatch_controls', copy)
            module = importlib.util.module_from_spec(spec)
            with redirect_stderr(stderr):
                code = None
                try:
                    spec.loader.exec_module(module)
                except SystemExit as refusal:
                    code = refusal.code
        finally:
            sys.dont_write_bytecode = previous
            if copy is not None:
                copy.unlink(missing_ok=True)
            shutil.rmtree(scratch, ignore_errors=True)
        self.assertTrue(code == 2,
                        '#1642 fix-r5 roster drift is a refusal (rc 2, never started), '
                        'never a failed control (rc 1) or a silent pass')
        self.assertTrue('management-phase-swap-renamed' in stderr.getvalue()
                        and 'roster' in stderr.getvalue(),
                        '#1642 fix-r5 the roster refusal must name the drift')


if __name__ == '__main__':
    unittest.main(verbosity=2)
