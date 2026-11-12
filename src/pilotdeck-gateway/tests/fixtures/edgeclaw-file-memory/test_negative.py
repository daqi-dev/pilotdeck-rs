# anchor-selfcheck: unsupported unit tests of the summary publication, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r5 (sibling #3) controls for the file-memory runner.

Two control families over the delivered runner (``negative.py``), neither of
which runs cargo:

* ``FileMemoryJudgeControls`` — the inline kill judge keeps requiring the
  libtest summary line and the bound marker (``a|b`` split) in the named
  test's own panic payload.
* ``FileMemoryPublicationControls`` — the verdict rows stay in memory for the
  whole guarded window and one final ``summary.json`` is published by a
  single rename only after ``mutating_sources`` restored and byte-verified
  the guarded source. An abort inside the window — a cancellation, a
  restore failure — voids the recorded rows, publishes INVALID and exits 3;
  a red positive baseline keeps exit 1 but still publishes; a stale anchor
  is a refusal (exit 2, nothing published).

The delivered script is a top-level main, so the publication control executes
it a second time with only the process layer mocked (cargo, slots, guard,
source writes); the bookkeeping under test is the delivered code. The
guarded source stays on disk untouched: the mocked writes record instead of
writing, and the restore verification reads the same real bytes the guard
snapshotted.

Run with:

    python3 -B edgeclaw-file-memory/test_negative.py
"""
import ast
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

_FIXTURES = Path(__file__).resolve().parents[1]
if str(_FIXTURES) not in sys.path:
    sys.path.insert(0, str(_FIXTURES))
import negative_process as shared_process
from negative_panic import named_test_killed


def load_table():
    """Read the ``mutants`` rows from the delivered script."""
    source = Path(__file__).with_name('negative.py')
    tree = ast.parse(source.read_text())
    for node in tree.body:
        if (isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name)
                and node.targets[0].id == 'mutants'
                and isinstance(node.value, ast.List)):
            return [ast.literal_eval(item) for item in node.value.elts]
    raise AssertionError('negative.py must keep the module-level mutants table')


MUTANTS = load_table()


def extract_kill_expression():
    """Pull the delivered inline ``killed = (...)`` assign verbatim."""
    source = Path(__file__).with_name('negative.py')
    tree = ast.parse(source.read_text())
    for node in ast.walk(tree):
        if (isinstance(node, ast.Assign) and node.targets
                and isinstance(node.targets[0], ast.Name)
                and node.targets[0].id == 'killed'):
            names = {child.id for child in ast.walk(node.value)
                     if isinstance(child, ast.Name)}
            # Skip the final `killed = [...]` row count: the judge is the
            # assignment that reads the transcript and the panic helper.
            if {'raw', 'named_test_killed'} <= names:
                return ast.Expression(node.value)
    raise AssertionError('the inline killed assign is missing')


KILLED = compile(extract_kill_expression(), 'negative.py:killed', 'eval')


def judge(code, raw, test, marker):
    return eval(KILLED, {'__builtins__': {}}, {
        'code': code, 'raw': raw, 'test': test, 'marker': marker,
        'named_test_killed': named_test_killed,
    })


def libtest_panic_log(payload, test, *, summary=True):
    """A one-test libtest transcript with the test's own anchor panic."""
    lines = [
        'running 1 test',
        f'test {test} ... FAILED',
        '',
        'failures:',
        '',
        f'---- {test} stdout ----',
        f"thread '{test}' panicked at tests/file_memory.rs:41:13:",
        payload,
        'note: run with `RUST_BACKTRACE=1` environment variable to display '
        'a backtrace',
    ]
    if summary:
        lines.append('')
        lines.append('test result: FAILED. 0 passed; 1 failed; 0 ignored; '
                     '0 measured; 0 filtered out')
    return '\n'.join(lines) + '\n'


class FileMemoryJudgeControls(unittest.TestCase):
    """The inline kill judge must only accept a completed failing run."""

    ROW = MUTANTS[0]

    def test_named_marker_with_libtest_summary_kills(self):
        raw = libtest_panic_log(self.ROW[4].split('|')[0], self.ROW[3])
        self.assertTrue(
            judge(101, raw, self.ROW[3], self.ROW[4]),
            '#1087 the named marker panic in a completed run kills')

    def test_pipe_marker_alternative_disjunct_kills(self):
        marker = MUTANTS[1][4]
        raw = libtest_panic_log(marker.split('|')[1], MUTANTS[1][3])
        self.assertTrue(
            judge(101, raw, MUTANTS[1][3], marker),
            '#1087 the second a|b disjunct kills in its own test payload')

    def test_aborted_run_without_summary_is_not_a_kill(self):
        raw = libtest_panic_log(self.ROW[4].split('|')[0], self.ROW[3],
                                summary=False)
        self.assertTrue(
            not judge(-9, raw, self.ROW[3], self.ROW[4]),
            '#1087 an aborted run without the FAILED summary line is not a kill')

    def test_compile_failure_and_unrelated_marker_are_not_kills(self):
        raw = ('error: could not compile `pilotdeck-gateway` '
               '(lib test) due to 1 previous error\n'
               'test result: FAILED. 0 passed; 1 failed;\n')
        self.assertTrue(
            not judge(101, raw, self.ROW[3], self.ROW[4]),
            '#1087 a compile failure is excluded')
        raw = libtest_panic_log('called `Option::unwrap()` on a `None` value',
                                self.ROW[3])
        self.assertTrue(
            not judge(101, raw, self.ROW[3], self.ROW[4]),
            '#1087 a FAILED run without the bound marker is not a kill')


class FileMemoryPublicationControls(unittest.TestCase):
    """The file-memory runner must never keep a stale or partial summary."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='filememory-negctl-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        # The delivered script defaults CARGO_TARGET_DIR inside the worktree
        # when unset; point it at the twin's scratch tree instead.
        self.saved_target = os.environ.get('CARGO_TARGET_DIR')
        os.environ['CARGO_TARGET_DIR'] = str(self.root / 'target')
        if self.saved_target is None:
            self.addCleanup(os.environ.pop, 'CARGO_TARGET_DIR', None)
        else:
            self.addCleanup(os.environ.__setitem__, 'CARGO_TARGET_DIR',
                            self.saved_target)

    def _drive(self, *, fail_at=None, fail_with=None, restore_fails_at=None,
               baseline_fails=False, stale_anchors=False):
        """Execute the delivered script once with the process layer mocked.

        ``fail_at`` is the 1-based ``run_owned`` call index (1 is the positive
        baseline, 2-4 the three mutant runs, 5 the restored suite) that
        raises ``fail_with``; ``restore_fails_at`` is the 1-based
        ``write_source_atomic`` call index (odd = a mutation write, even = the
        matching in-loop restore).
        """
        out = (self.root / 'run').resolve()
        state = {'runs': 0, 'writes': 0, 'summary_existed': False}
        disk = {}

        def fake_run_owned(command, *, cwd=None, env=None, timeout=None):
            state['runs'] += 1
            if fail_at == state['runs']:
                raise fail_with
            if state['runs'] == 1:
                # The positive baseline: red on demand, green otherwise.
                if baseline_fails:
                    log = ('running 3 tests\n'
                           'test rust_reads_and_writes_the_real_ts_transcripts ... FAILED\n'
                           'test result: FAILED. 1 passed; 2 failed; 0 ignored; '
                           '0 measured; 0 filtered out\n')
                    return subprocess.CompletedProcess(command, 101, '', log)
                log = ('running 3 tests\n'
                       'test result: ok. 3 passed; 0 failed; 0 ignored; '
                       '0 measured; 0 filtered out\n')
                return subprocess.CompletedProcess(command, 0, '', log)
            if state['runs'] == 5:
                log = ('running 3 tests\n'
                       'test result: ok. 3 passed; 0 failed; 0 ignored; '
                       '0 measured; 0 filtered out\n')
                return subprocess.CompletedProcess(command, 0, '', log)
            _name, _before, _after, test, marker = MUTANTS[state['runs'] - 2]
            log = libtest_panic_log(marker.split('|')[0], test)
            return subprocess.CompletedProcess(command, 101, '', log)

        def fake_mutating(root, paths, **kwargs):
            originals = {path: path.read_bytes() for path in paths}
            if stale_anchors:
                # Every anchor now matches zero times: the runner must refuse
                # before any compile and publish nothing.
                originals = {path: b'fn unrelated() {}' for path in paths}

            class Guard:
                def __enter__(self):
                    return originals

                def __exit__(self, *exc):
                    return False

            return Guard()

        def fake_slot(script, **kwargs):
            return types.SimpleNamespace(stdout='', stderr='')

        def fake_atomic(root, path, data, *, mode=None):
            state['writes'] += 1
            state['summary_existed'] = (state['summary_existed']
                                        or (out / 'summary.json').exists())
            if restore_fails_at is not None and state['writes'] == restore_fails_at:
                raise RuntimeError('twin restore failure')

        def is_summary_tmp(path):
            return (path.parent == out and path.name.startswith('summary.json.')
                    and path.name.endswith('.tmp'))

        class Model:
            real_write_text = Path.write_text
            real_replace = Path.replace

            def model_write_text(self, data, *args, **kwargs):
                if self.parent == out and self.name == 'summary.json':
                    disk['direct'] = data
                    return None
                if is_summary_tmp(self):
                    disk['staged'] = data
                return Model.real_write_text(self, data, *args, **kwargs)

            def model_replace(self, destination, *args, **kwargs):
                if is_summary_tmp(self):
                    disk['renames'] = disk.get('renames', 0) + 1
                    disk['summary'] = disk['staged']
                return Model.real_replace(self, destination, *args, **kwargs)

        source = Path(__file__).with_name('negative.py')
        argv = [str(source), str(out)]
        fake = types.ModuleType('negative_process')
        fake.mutating_sources = fake_mutating
        fake.run_owned = fake_run_owned
        fake.wait_for_slot = fake_slot
        fake.write_source_atomic = fake_atomic
        # #1087 fix-r5: the shared helpers stay the shipped ones so the twins
        # exercise the real staging-name and dead-fd behavior.
        fake.unique_summary_tmp = shared_process.unique_summary_tmp
        fake.detach_dead_streams = shared_process.detach_dead_streams
        saved_process = sys.modules.get('negative_process')
        saved_argv = sys.argv
        sys.modules['negative_process'] = fake
        sys.argv = argv
        try:
            with patch.object(Path, 'write_text', Model.model_write_text), \
                 patch.object(Path, 'replace', Model.model_replace):
                exec(compile(source.read_text(), str(source), 'exec'),
                     {'__name__': '__main__', '__file__': str(source)})
        except BaseException as error:  # the delivered script always exits
            code = error
        else:  # pragma: no cover - the script never falls off the end
            code = None
        finally:
            sys.argv = saved_argv
            if saved_process is None:
                sys.modules.pop('negative_process', None)
            else:
                sys.modules['negative_process'] = saved_process
        return code, state, disk, out

    def test_green_run_publishes_via_one_rename(self):
        """The final summary is one rename, never an in-place write."""
        code, state, disk, out = self._drive()
        message = '#1087 the final summary must be one rename, never an in-place write'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the sample run must exit cleanly)')
        self.assertTrue(code.code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(disk.get('direct') is None,
                        message + ' (summary.json was written in place)')
        self.assertTrue(disk.get('renames') == 1,
                        message + ' (the summary was not published by exactly one rename)')
        payload = json.loads(disk['summary'])
        self.assertTrue([row['killed'] for row in payload['results']]
                        == [True, True, True],
                        message + ' (the final payload must keep every row)')
        self.assertTrue(payload['restored'] is True,
                        message + ' (the restored flag must record the verified tree)')
        self.assertTrue(len(payload['beforeSha256']) == 64
                        and payload['afterSha256'] == payload['beforeSha256'],
                        message + ' (the sha256 pair must bracket the window)')

    def test_no_summary_is_readable_inside_the_guarded_window(self):
        """No summary may exist before the guard restored the tree."""
        code, state, disk, out = self._drive()
        message = '#1087 no summary may be readable before restore verification'
        self.assertTrue(code.code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(state['summary_existed'] is False,
                        message + ' (a summary was readable inside the guarded window)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_keyboardinterrupt_voids_rows_and_propagates(self):
        """A mid-suite cancellation voids every row, then propagates."""
        # Run 3 is the second mutant's run: the first row is already recorded
        # KILLED, so the voiding of a recorded row is exercised too.
        code, state, disk, out = self._drive(
            fail_at=3, fail_with=KeyboardInterrupt('red run interrupted'))
        message = '#1087 a cancellation must void the rows and publish INVALID'
        self.assertIsInstance(code, KeyboardInterrupt,
                              message + ' (the cancellation must be re-raised)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue(len(payload['results']) == 3,
                        message + ' (the unstarted mutants must be recorded too)')
        self.assertTrue(all(row['killed'] is False for row in payload['results']),
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: KeyboardInterrupt' in row['voided']
                for row in payload['results']),
            message + ' (the invalidation reason was not recorded)')

    def test_systemexit_is_normalized_to_three(self):
        """An aborting SystemExit pairs with exit code 3, never 2 or the raw code."""
        code, state, disk, out = self._drive(fail_at=2, fail_with=SystemExit(130))
        message = '#1087 an INVALID summary must pair with exit code 3'
        self.assertIsInstance(code, SystemExit,
                             message + ' (the SystemExit must propagate)')
        self.assertTrue(code.code == 3,
                        message + ' (the raw code must be normalized away from 2)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue(all(row['killed'] is False for row in payload['results']),
                        message + ' (the on-disk rows were not voided)')

    def test_baseline_failure_publishes_and_exits_one(self):
        """A red positive baseline publishes its rows and keeps exit code 1."""
        code, state, disk, out = self._drive(baseline_fails=True)
        message = '#1087 a failed baseline must publish and exit 1'
        self.assertIsInstance(code, SystemExit, message + ' (the run must exit)')
        self.assertTrue(code.code == 1,
                        message + ' (the baseline-failure exit code must stay 1)')
        payload = json.loads(disk['summary'])
        self.assertTrue(payload['results'] == [],
                        message + ' (no mutant may run after a red baseline)')
        self.assertTrue(payload['restored'] is True,
                        message + ' (the guard must still have restored the tree)')

    def test_restore_failure_voids_rows_and_exits_three(self):
        """A source-write failure voids the rows and exits 3."""
        # Write 3 is the second mutant's mutation publish.
        code, state, disk, out = self._drive(restore_fails_at=3)
        message = '#1087 a source-write failure must void the rows and exit 3'
        self.assertIsInstance(code, SystemExit,
                             message + ' (the failure must exit through SystemExit)')
        self.assertTrue(code.code == 3, message + ' (a source-write failure must exit 3)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue(len(payload['results']) == 3,
                        message + ' (the unstarted mutants must be recorded too)')
        self.assertTrue(
            all('runner: RuntimeError' in row['voided'] for row in payload['results']),
            message + ' (the invalidation reason was not recorded)')

    def test_stale_anchor_is_a_refusal_that_publishes_nothing(self):
        """A stale anchor is exit 2 with no summary, not a voided run."""
        code, state, disk, out = self._drive(stale_anchors=True)
        message = '#1087 a stale anchor must be a never-started refusal'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code.code == 2,
                        message + ' (the stale-anchor refusal must stay exit 2)')
        self.assertTrue(not (out / 'summary.json').exists(),
                        message + ' (a refusal must not publish a summary)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a refusal must not leave staging behind)')


if __name__ == '__main__':
    unittest.main()
