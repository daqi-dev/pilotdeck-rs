# anchor-selfcheck: unsupported isolated runner protocol tests; no source replacement table
"""#1642 fix-r4: the frozen guard-failure phases, pinned the native-management way.

review P1-A: a SIGTERM deferred through the restore window re-enters the
handler as SystemExit(128+signum) and must survive it untouched -- no
invalidate, no summary, no rewrite of 143 to 1 (the r3 handler swallowed it
with its own `return`).

review P1-B: the enter/exit phase split the r3 handler only promised in its
comment -- a guard that never started is a refusal (rc 2, no summary, the
claim_dir claim withdrawn so the host can re-run the same output path), a
guard that fails after the body ran is a red run (rc 1, every row revoked,
evidence kept).

#1642 fix-r5 additions: the reuse refusal is claim_dir (rc 2, the same
admission as the three siblings, never the rc 1 a bare SystemExit(str)
carried), the frozen roster is registered + refuse, and the enter
withdrawal is one atomic rename (a later runner claiming the same --output
in the withdrawal window keeps its evidence directory).
"""
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

SPEC = importlib.util.spec_from_file_location('frozen_reinsert_controls',
                                              Path(__file__).with_name('negative_controls.py'))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)

GREEN_LOG = ('{method} (__main__.{test}) ... ok\n'
             '\n'
             '----------------------------------------------------------------------\n'
             'Ran 1 test in 0.001s\n'
             '\n'
             'OK\n')
RED_LOG = ('{method} (__main__.{test}) ... FAIL\n'
            '\n'
            '======================================================================\n'
            'FAIL: {method} (__main__.{test})\n'
            '----------------------------------------------------------------------\n'
            'Traceback (most recent call last):\n'
            '  File "test_entry_guard.py", line 9, in {method}\n'
            '    self.fail("#1066 reinsertion must stop before stdlib")\n'
            'AssertionError: {assertion}\n'
            '\n'
            '----------------------------------------------------------------------\n'
            'Ran 1 test in 0.001s\n'
            '\n'
            'FAILED (failures=1)\n')


class Protocol(unittest.TestCase):
    def exercise(self, *, fail_enter=False, fail_exit=False, signal_exit=False):
        with tempfile.TemporaryDirectory(prefix='pdrs-1066-protocol-') as temporary:
            root = Path(temporary)
            source = root / 'fake-source.py'
            source.write_text('import sys\nif any(\n        sys.argv):\n    pass\n')
            out = root / 'result'
            rows = [('fake-1642-mutation', source, 'if any(', 'if False and any(',
                     'EntryGuardTests.test_fake_1642_mutation', '#1066 fake exclusive assertion')]
            active = False

            def run(_out, label, test):
                self.assertTrue(active, '#1066 protocol test runs stay inside lease')
                method = test.split('.')[-1]
                if label.endswith('-negative'):
                    log = RED_LOG.format(method=method, test=test, assertion='#1066 fake exclusive assertion')
                    return subprocess.CompletedProcess([], 1, log, '')
                log = GREEN_LOG.format(method=method, test=test)
                return subprocess.CompletedProcess([], 0, log, '')

            @contextmanager
            def guard(_root, paths, **_kwargs):
                nonlocal active
                if fail_enter:
                    raise RuntimeError('#1066 injected guard enter failure')
                active = True
                try:
                    # main() restores every selected path out of this one
                    # mapping, so the fake bytes cover the whole guarded set
                    # without reading or writing the real tree.
                    originals = {path: b'guarded-bytes' for path in paths}
                    originals[source] = source.read_bytes()
                    yield originals
                finally:
                    active = False
                    if fail_exit:
                        raise RuntimeError('#1066 injected guard exit failure')
                    if signal_exit:
                        raise SystemExit(143)

            argv = ['controls', '--output', str(out)]
            stderr = io.StringIO()
            with redirect_stderr(stderr), \
                 patch.object(RUNNER, 'MUTATIONS', rows), \
                 patch.object(RUNNER, 'mutating_sources', guard), \
                 patch.object(RUNNER, 'run', run), \
                 patch.object(sys, 'argv', argv):
                try:
                    code = RUNNER.main()
                except SystemExit as refusal:
                    # A refusal (rc 2) or a delayed signal (rc 128+signum)
                    # leaves main() through SystemExit; the code is the
                    # process exit shape.
                    code = refusal.code
            # The evidence is read inside the temporary directory's lifetime
            # (this with-block deletes the tree on return): `claimed` is the
            # enter-refusal question (was the claim withdrawn?), `summary`
            # the publish question.
            claimed = out.exists()
            summary = None
            if claimed and (out / 'summary.json').is_file():
                summary = json.loads((out / 'summary.json').read_text())['controls']
            return code, summary, claimed, stderr.getvalue()

    def test_green_batch_kills_every_row(self):
        code, rows, claimed, _ = self.exercise()
        self.assertTrue(code == 0 and all(row['status'] == 'KILLED' for row in rows),
                        '#1066 protocol successful batch')
        self.assertTrue(claimed, '#1066 protocol successful batch keeps its evidence')

    # #1642 fix-r4 (review P1-B): the enter refusal is pinned the way the
    # four-way exit-code table reads it -- rc 2, no summary, the claim
    # withdrawn, and the refusal names the guard failure.
    def test_guard_enter_failure_refuses(self):
        code, rows, claimed, stderr = self.exercise(fail_enter=True)
        self.assertTrue(code == 2,
                        '#1642 fix-r4 a guard that never started refuses with rc 2, '
                        'not the flat rc 1 the r3 handler returned')
        self.assertTrue(rows is None,
                        '#1642 fix-r4 an enter-phase refusal publishes no summary')
        self.assertFalse(claimed,
                        '#1642 fix-r4 an enter-phase refusal must withdraw the claimed '
                        'evidence directory (claim removed, no leftover claim shape)')
        self.assertTrue('guard refused to start' in stderr,
                        '#1642 fix-r4 the enter refusal must name the guard failure')

    def test_guard_exit_invalidates_completed(self):
        code, rows, claimed, _ = self.exercise(fail_exit=True)
        self.assertTrue(code == 1,
                        '#1642 fix-r4 a guard that fails after the body ran is a red run (rc 1), '
                        'not the rc 2 the inverted r3 map returned')
        self.assertTrue(claimed,
                        '#1642 fix-r4 a red run keeps its evidence directory')
        self.assertTrue(all(row['status'] == 'INVALID' for row in rows),
                        '#1642 fix-r4 a failed guard revokes every row it recorded')
        self.assertTrue(all('source guard' in row['reason']
                            and 'guard exit failure' in row['reason'] for row in rows),
                        '#1642 fix-r4 the rows are revoked by the guard failure itself, '
                        'not by some earlier per-case error')

    # #1642 fix-r4 (review P1-A, same shape as native-management:1160): the
    # delayed-signal SystemExit the guard raises after a verified restore
    # survives main()'s handler untouched -- rc 143, no summary published
    # over the restored tree. The r3 handler's trailing `return 2 if ...`
    # swallowed it and rewrote the exit to the four-way table.
    def test_deferred_signal_survives_the_guard_handler(self):
        code, rows, _, _ = self.exercise(signal_exit=True)
        self.assertTrue(code == 143,
                        '#1642 fix-r4 deferred SIGTERM exits 128+signum, not the rewritten rc 1')
        self.assertTrue(rows is None,
                        '#1642 fix-r4 a deferred signal publishes no summary')

    # #1642 fix-r5 (review sibling P1#1, same shape as native-management's
    # test_existing_output_directory_refuses / native-dispatch's
    # test_existing_output_directory_refuses_with_rc2): a taken output
    # directory is "never started" on the four-way exit-code table -- rc 2
    # through negative_selection.claim_dir, never the rc 1 the old
    # `new_output_directory` raise SystemExit(str) carried while no summary
    # existed on disk. The directory here is non-empty (an earlier run's
    # leftover log), which is the exact #953 shape that used to die on rc 1.
    def test_existing_output_directory_refuses_with_rc2(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1066-refuse-') as temporary:
            out = Path(temporary) / 'result'
            out.mkdir()
            (out / 'earlier-run.log').write_text('evidence of an earlier run')
            stderr = io.StringIO()
            with redirect_stderr(stderr), \
                 patch.object(sys, 'argv', ['controls', '--output', str(out)]):
                code = None
                try:
                    RUNNER.main()
                except SystemExit as refusal:
                    code = refusal.code
            self.assertTrue(code == 2,
                            '#1642 fix-r5 an existing output directory refuses with rc 2, '
                            'not the rc 1 the bare SystemExit(str) reuse refusal carried')
            self.assertTrue('already exists' in stderr.getvalue(),
                            '#1642 fix-r5 the refusal must say the directory is taken')
            self.assertTrue((out / 'earlier-run.log').read_text() == 'evidence of an earlier run',
                            '#1642 fix-r5 the refusal must not touch another run\'s evidence')

    # #1642 fix-r5 (review sibling P1#2, the drift-probe shape
    # native-registry-switch/test_roster.py and native-management both use):
    # the frozen roster is the shared registered + refuse pair, so a drifted
    # EXPECTED_NAMES is a refusal (rc 2, never started) that names the drift,
    # never a smaller green run over a self-denominated table.
    def test_roster_drift_refuses_with_rc2(self):
        source = Path(RUNNER.__file__).read_text()
        pattern = "'test_approval_provenance_py_unloaded_os',\n)"
        self.assertTrue(source.count(pattern) == 1,
                        '#1642 fix-r5 the roster drift probe must replace exactly one '
                        'EXPECTED_NAMES entry')
        drifted = source.replace(pattern, "'test_approval_provenance_py_unloaded_os_renamed',\n)", 1)
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
            spec = importlib.util.spec_from_file_location('drifted_frozen_reinsert_controls', copy)
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
        self.assertTrue('test_approval_provenance_py_unloaded_os_renamed' in stderr.getvalue()
                        and 'roster' in stderr.getvalue(),
                        '#1642 fix-r5 the roster refusal must name the drift')

    # #1642 fix-r5 (review concurrency P1#1): two runs share one --output;
    # the first to exit (an enter-phase guard failure) must never delete the
    # later runner's evidence directory. The r4 withdrawal was two
    # destructive calls (unlink `.claim`, then rmdir), so a run claiming the
    # path in that window owned a directory the withdrawal's second call
    # then deleted. The withdrawal is one rename now, and this test drives a
    # full green batch -- the later runner -- inside the window, right
    # between the first runner's rename and its rmtree of the renamed
    # directory.
    def test_enter_withdrawal_never_deletes_a_later_runners_directory(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1066-race-') as temporary:
            root = Path(temporary)
            source = root / 'fake-source.py'
            source.write_text('import sys\nif any(\n        sys.argv):\n    pass\n')
            out = root / 'result'
            rows = [('fake-1642-mutation', source, 'if any(', 'if False and any(',
                     'EntryGuardTests.test_fake_1642_mutation', '#1066 fake exclusive assertion')]
            entries = {'count': 0}
            active = False

            def run(_out, label, test):
                self.assertTrue(active, '#1066 protocol test runs stay inside lease')
                method = test.split('.')[-1]
                if label.endswith('-negative'):
                    log = RED_LOG.format(method=method, test=test, assertion='#1066 fake exclusive assertion')
                    return subprocess.CompletedProcess([], 1, log, '')
                log = GREEN_LOG.format(method=method, test=test)
                return subprocess.CompletedProcess([], 0, log, '')

            @contextmanager
            def guard(_root, paths, **_kwargs):
                nonlocal active
                entries['count'] += 1
                if entries['count'] == 1:
                    raise RuntimeError('#1066 injected guard enter failure')
                active = True
                try:
                    originals = {path: b'guarded-bytes' for path in paths}
                    originals[source] = source.read_bytes()
                    yield originals
                finally:
                    active = False

            later = {}
            real_rename = os.rename

            def renaming(src, dst, *args, **kwargs):
                result = real_rename(src, dst, *args, **kwargs)
                if 'code' not in later:
                    # The path is free the instant the rename lands: this is
                    # the later runner's claim window. It runs a full green
                    # batch on the same --output and publishes its summary
                    # before the first runner's rmtree of the renamed
                    # directory executes.
                    try:
                        later['code'] = RUNNER.main()
                    except SystemExit as refusal:
                        later['code'] = refusal.code
                return result

            argv = ['controls', '--output', str(out)]
            stderr = io.StringIO()
            with redirect_stderr(stderr), \
                 patch.object(RUNNER, 'MUTATIONS', rows), \
                 patch.object(RUNNER, 'mutating_sources', guard), \
                 patch.object(RUNNER, 'run', run), \
                 patch.object(os, 'rename', renaming), \
                 patch.object(sys, 'argv', argv):
                first = None
                try:
                    first = RUNNER.main()
                except SystemExit as refusal:
                    first = refusal.code
            self.assertTrue(first == 2,
                            '#1642 fix-r5 the first runner (guard never started) '
                            'still refuses with rc 2')
            self.assertTrue(later.get('code') == 0,
                            '#1642 fix-r5 the later runner claims the freed path and '
                            'finishes its green batch')
            self.assertTrue((out / 'summary.json').is_file(),
                            '#1642 fix-r5 the later runner\'s evidence directory survives '
                            'the first runner\'s withdrawal')
            summary = json.loads((out / 'summary.json').read_text())['controls']
            self.assertTrue(all(row['status'] == 'KILLED' for row in summary),
                            '#1642 fix-r5 the later runner\'s published verdicts are intact')
            self.assertFalse(any(path.name.startswith(out.name + '.withdrawn-')
                                 for path in out.parent.iterdir()),
                            '#1642 fix-r5 the withdrawal leaves no renamed residue '
                            'beside the evidence directory')


if __name__ == '__main__':
    unittest.main(verbosity=2)
