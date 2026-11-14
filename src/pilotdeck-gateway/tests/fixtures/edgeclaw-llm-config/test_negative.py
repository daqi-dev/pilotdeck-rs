# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r2 controls for the llm-config kill judge.

The delivered judge (``negative.py::classify``) is the shared #813 criterion:
the named test's own ``FAILED`` status line plus the bound anchor inside that
test's own panic payload. The controls below pin the positive shape and the
false-kill shapes the old judge (any non-zero exit with ``test result:
FAILED.``) accepted — a sibling test's panic, an anchor only in captured
stdout, and an anchor inside build output. Every log is a synthetic libtest
transcript; no cargo run happens here (the real-sample kill is proven by the
coordinator's full run). ``negative.py`` is a top-level script, so it is
loaded through ``--list`` — the one dispatch mode with no side effects.


#1087 fix-r3 (boundary #1) publication controls: the rows stay in memory
for the whole guarded window and one final ``summary.json`` is published
by a single rename only after the guard restored and byte-verified every
guarded source; an abort inside the window voids the rows, publishes
INVALID and exits 3 (never the "never started" 2). ``PublicationControls``
executes the delivered script a second time with only the process layer
mocked — no cargo run happens here either.

Run with:

    python3 -B edgeclaw-llm-config/test_negative.py
"""
import ast
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import types
from pathlib import Path
import unittest
from unittest.mock import patch

_FIXTURES = Path(__file__).resolve().parents[1]
if str(_FIXTURES) not in sys.path:
    sys.path.insert(0, str(_FIXTURES))
import negative_process as shared_process


def load_runner():
    source = Path(__file__).with_name('negative.py')
    spec = importlib.util.spec_from_file_location('llm_config_negative', source)
    module = importlib.util.module_from_spec(spec)
    argv = sys.argv
    sys.argv = [str(source), '--list']
    try:
        try:
            spec.loader.exec_module(module)
        except SystemExit:
            pass  # --list prints the names and exits 0; the judge is bound.
    finally:
        sys.argv = argv
    return module


class LlmConfigJudgeControls(unittest.TestCase):
    """The llm-config kill judge must only accept the named test's anchor."""

    @classmethod
    def setUpClass(cls):
        cls.runner = load_runner()

    @staticmethod
    def _panic_log(test, payload, *, status='FAILED', thread=None):
        counts = ('0 passed; 1 failed' if status == 'FAILED' else '1 passed; 0 failed')
        summary = f'test result: {"FAILED" if status == "FAILED" else "ok"}. ' \
                  f'{counts}; 0 ignored; 0 measured; 0 filtered out'
        return (
            '\nrunning 1 test\n'
            f'test {test} ... {status}\n'
            '\nfailures:\n'
            f"thread '{thread or test}' (2841) panicked at "
            'tests/edgeclaw_llm_config.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def judge(self, log, returncode, expected, test):
        return self.runner.classify(log, returncode, expected, test)

    TEST = 'llm_config_matches_ts'

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own ``$``-path anchor panic."""
        anchor = '$'
        log = self._panic_log(
            self.TEST, '$.provider: expected Some("p"), actual None')
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'KILLED',
                        "#1087 the named test's own anchor panic must kill")

    def test_special_anchors_kill(self):
        """Each special (non-``$``) anchor also binds to the named test."""
        anchors = ('$.autoIndexIntervalMinutes: number bits',
                   'second resolve blocked: the cache lock covers I/O',
                   '#836: cached bad-file null must not re-probe home')
        payloads = ('$.autoIndexIntervalMinutes: number bits',
                    'second resolve blocked: the cache lock covers I/O',
                    '#836: cached bad-file null must not re-probe home')
        for anchor, payload in zip(anchors, payloads):
            log = self._panic_log(self.TEST, payload)
            with self.subTest(anchor=anchor):
                self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'KILLED',
                                '#1087 the special anchor must kill')

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        anchor = '$'
        log = (
            '\nrunning 1 test\n'
            f'test {self.TEST} ... FAILED\n'
            '\nfailures:\n'
            f'---- {self.TEST} stdout ----\n'
            'replay dump for debugging: $.provider expected Some("p")\n'
            f"thread '{self.TEST}' (2841) panicked at "
            'tests/edgeclaw_llm_config.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'INVALID',
                        '#1087 an anchor in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """An anchor in a sibling test's panic is not a kill."""
        anchor = '$'
        sibling = 'llm_dream_matches_ts'
        log = (
            '\nrunning 2 tests\n'
            f'test {self.TEST} ... ok\n'
            f'test {sibling} ... FAILED\n'
            '\nfailures:\n'
            f"thread '{sibling}' (2841) panicked at "
            'tests/edgeclaw_llm_dream.rs:44:9:\n'
            '$.apiKey: expected Some("k"), actual None\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'INVALID',
                        "#1087 a sibling test's panic is not a kill")

    def test_build_output_anchor_is_not_a_kill(self):
        """An anchor inside build output is never a kill."""
        anchor = '$'
        log = (
            'error[E0308]: mismatched types at `$.provider`\n'
            ' --> tests/edgeclaw_llm_config.rs:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "edgeclaw_llm_config") due to 1 previous error\n'
        )
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'INVALID',
                        '#1087 an anchor inside build output is never a kill')

    def test_unrelated_unwrap_is_not_a_kill(self):
        """A FAILED run without the bound anchor is not a kill."""
        anchor = '$'
        log = self._panic_log(self.TEST, 'called `Option::unwrap()` on a `None` value')
        self.assertTrue(self.judge(log, 101, anchor, self.TEST) == 'INVALID',
                        '#1087 an unrelated unwrap panic is not a kill')

    def test_green_run_survives(self):
        """A passing bound test is SURVIVED, never INVALID."""
        anchor = '$'
        log = self._panic_log(self.TEST, 'no failure', status='ok')
        self.assertTrue(self.judge(log, 0, anchor, self.TEST) == 'SURVIVED',
                        '#1087 a green bound test must read SURVIVED')

    def test_every_case_has_a_bound_anchor(self):
        """Every case name must carry an anchor in the EXPECTED table."""
        missing = [case[0] for case in self.runner.cases
                   if case[0] not in self.runner.EXPECTED]
        self.assertTrue(not missing,
                        f'#1087 every case must have a bound anchor: {missing}')



def load_publication_table():
    """Read ``cases``/``EXPECTED`` from the delivered script without running it.

    The module is a top-level script (the guarded main runs at import), so the
    twin parses the two module-level tables out of its AST instead of
    executing it a second time here. The first literal ``cases`` binding is
    the delivered table; the argv-selection rebind later never overwrites it.
    """
    source = Path(__file__).with_name('negative.py')
    tree = ast.parse(source.read_text())
    table = expected = None
    for node in tree.body:
        if not (isinstance(node, ast.Assign)
                and isinstance(node.targets[0], ast.Name)):
            continue
        if node.targets[0].id == 'cases' and table is None \
                and isinstance(node.value, ast.List):
            table = [(ast.literal_eval(item.elts[0]), ast.literal_eval(item.elts[4]))
                     for item in node.value.elts]
        elif node.targets[0].id == 'EXPECTED' and expected is None:
            expected = ast.literal_eval(node.value)
    assert table and expected, \
        'negative.py must keep module-level cases/EXPECTED tables'
    return table, expected


class PublicationControls(unittest.TestCase):
    """The llm-config runner must never keep a stale green summary.

    #1087 fix-r3 (boundary #1): the rows stay in memory for the whole guarded
    window and one final ``summary.json`` is published by a single rename
    only after ``mutating_sources`` restored and byte-verified every guarded
    source. An abort inside the window — a cancellation, a restore failure,
    the source guard — voids the recorded rows, publishes INVALID and exits
    3 (never 2: that is the "never started" usage refusal). A red baseline
    keeps exit 1 but still publishes its row. The delivered script is a
    top-level main, so this control executes it a second time with only the
    process layer mocked (cargo, slots, guard, source writes); the
    bookkeeping under test is the delivered code. The guarded sources stay
    on disk untouched: the mocked writes record instead of writing, and the
    restore verification reads the same real bytes the guard snapshotted.
    """

    @classmethod
    def setUpClass(cls):
        cls.table, cls.expected = load_publication_table()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='llmconfig-negctl-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()  # claim_dir resolves; /var is a symlink on this host
        # The delivered script refuses to run without a caller-owned build tree.
        self.saved_target = os.environ.get('CARGO_TARGET_DIR')
        os.environ['CARGO_TARGET_DIR'] = str(self.root / 'target')
        if self.saved_target is None:
            self.addCleanup(os.environ.pop, 'CARGO_TARGET_DIR', None)
        else:
            self.addCleanup(os.environ.__setitem__, 'CARGO_TARGET_DIR',
                            self.saved_target)
        # Two sample controls in table order, each with a dedicated anchor:
        # the shared ``$`` root marker cannot tell two kills apart.
        self.selected = [
            row for row in self.table
            if isinstance(self.expected.get(row[0]), str)
            and not self.expected[row[0]].startswith('$')][:2]
        self.assertTrue(len(self.selected) == 2,
                        'the table must offer two controls with dedicated anchors')

    def _drive(self, *, fail_at=None, fail_with=None, restore_fails_at=None,
               baseline_fails=False, stale_anchors=False):
        """Execute the delivered script once with the process layer mocked.

        ``fail_at`` is the 1-based ``run_owned`` call index (1 is the baseline
        run) that raises ``fail_with``; ``restore_fails_at`` is the 1-based
        ``write_source_atomic`` call index that raises instead of recording
        (5 is the first restore of the guarded window's own ``finally``).
        ``stale_anchors`` snapshots guard bytes that contain none of the
        anchors, so the delivered in-window stale check refuses before any
        cargo run.
        """
        out = self.root / 'run'
        state = {'runs': 0, 'writes': 0, 'summary_existed': False}
        disk = {}
        selected, expected = self.selected, self.expected

        def fake_run_owned(command, cwd, env, timeout):
            state['runs'] += 1
            if fail_at == state['runs']:
                raise fail_with
            if state['runs'] == 1:
                if baseline_fails:
                    log = ('running 1 test\n'
                           'test result: FAILED. 0 passed; 1 failed; 0 ignored; '
                           '0 measured; 0 filtered out\n')
                    return subprocess.CompletedProcess(command, 101, '', log)
                log = ('running 1 test\n'
                       'test result: ok. 1 passed; 0 failed; 0 ignored; '
                       '0 measured; 0 filtered out\n')
                return subprocess.CompletedProcess(command, 0, '', log)
            name, test = selected[state['runs'] - 2]
            log = (
                'running 1 test\n'
                f'test {test} ... FAILED\n'
                '\nfailures:\n'
                f"thread '{test}' (2841) panicked at "
                'tests/edgeclaw_llm_config.rs:12:5:\n'
                f'{expected[name]} expected Some("p"), actual None\n'
                'note: run with `RUST_BACKTRACE=1` environment variable to display '
                'a backtrace\n'
                'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
                '0 filtered out\n'
            )
            return subprocess.CompletedProcess(command, 101, '', log)

        def fake_mutating(root, paths, **kwargs):
            if stale_anchors:
                originals = {path: b'fn unrelated() {}' for path in paths}
            else:
                originals = {path: path.read_bytes() for path in paths}

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

        class Model:
            real_write_text = Path.write_text
            real_replace = Path.replace

            def model_write_text(self, data, *args, **kwargs):
                if self.parent == out and self.name == 'summary.json':
                    disk['direct'] = data
                    return None
                if self.parent == out and self.name == 'summary.json.tmp':
                    disk['staged'] = data
                return Model.real_write_text(self, data, *args, **kwargs)

            def model_replace(self, destination, *args, **kwargs):
                if self.parent == out and self.name == 'summary.json.tmp':
                    disk['renames'] = disk.get('renames', 0) + 1
                    disk['summary'] = disk['staged']
                return Model.real_replace(self, destination, *args, **kwargs)

        source = Path(__file__).with_name('negative.py')
        argv = [str(source), str(out)] + [name for name, _test in selected]
        fake = types.ModuleType('negative_process')
        fake.mutating_sources = fake_mutating
        fake.run_owned = fake_run_owned
        fake.wait_for_slot = fake_slot
        fake.write_source_atomic = fake_atomic
        fake.target_environment = lambda root, env=None: None
        # #1087 fix-r5: the shared detach helper stays the shipped one so the
        # twins exercise the real dead-fd detection.
        fake.detach_dead_streams = shared_process.detach_dead_streams
        # #1087 fix-r8 (S1-a/S1-b): the guarded print and the per-run token
        # are shared originals now; the fake keeps the shipped ones.
        fake.dead_stream_print = shared_process.dead_stream_print
        fake.new_run_token = shared_process.new_run_token
        saved_process = sys.modules.get('negative_process')
        saved_argv = sys.argv
        sys.modules['negative_process'] = fake
        sys.argv = argv
        try:
            with patch.object(Path, 'write_text', Model.model_write_text), \
                 patch.object(Path, 'replace', Model.model_replace):
                exec(compile(source.read_text(), str(source), 'exec'),
                     {'__name__': 'negative_publication_twin',
                      '__file__': str(source)})
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
        self.assertTrue(payload['baseline']['outcome'] == 'PASS',
                        message + ' (the baseline row must record the green run)')
        self.assertTrue([row['outcome'] for row in payload['cases']]
                        == ['KILLED', 'KILLED'],
                        message + ' (the final payload must keep every row)')

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
        code, state, disk, out = self._drive(
            fail_at=3, fail_with=KeyboardInterrupt('red run interrupted'))
        message = '#1087 a cancellation must void the rows and publish INVALID'
        self.assertIsInstance(code, KeyboardInterrupt,
                              message + ' (the cancellation must be re-raised)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['outcome'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: KeyboardInterrupt' in row['reason']
                for row in payload['cases']),
            message + ' (the invalidation reason was not recorded)')
        self.assertTrue('voided' in payload['cases'][0]['reason'],
                        message + ' (the already-KILLED row must be voided too)')

    def test_systemexit_is_normalized_to_three(self):
        """An aborting SystemExit pairs with exit code 3, never 2 or the raw code."""
        code, state, disk, out = self._drive(fail_at=3, fail_with=SystemExit(130))
        message = '#1087 an INVALID summary must pair with exit code 3'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the SystemExit must propagate)')
        self.assertTrue(code.code == 3,
                        message + ' (the raw code must be normalized away from 2)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['outcome'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')

    def test_baseline_failure_publishes_and_exits_one(self):
        """A red baseline publishes its row and keeps exit code 1."""
        code, state, disk, out = self._drive(baseline_fails=True)
        message = '#1087 a failed baseline must publish and exit 1'
        self.assertIsInstance(code, SystemExit, message + ' (the run must exit)')
        self.assertTrue(code.code == 1,
                        message + ' (the baseline-failure exit code must stay 1)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue(payload['baseline']['outcome'] == 'FAIL',
                        message + ' (the baseline row must record the failure)')
        self.assertTrue(payload['cases'] == [],
                        message + ' (no mutant may run after a red baseline)')

    def test_restore_failure_voids_rows_and_exits_three(self):
        """A guard failure voids the rows and exits 3."""
        code, state, disk, out = self._drive(restore_fails_at=5)
        message = '#1087 a guard failure must void the rows and exit 3'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the failure must exit through SystemExit)')
        self.assertTrue(code.code == 3, message + ' (a guard failure must exit 3)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['outcome'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: RuntimeError' in row['reason'] for row in payload['cases']),
            message + ' (the invalidation reason was not recorded)')

    def test_closed_stderr_keeps_the_abort_exit_code(self):
        """#1087 fix-r4 (concurrency #2): a closed stderr must not replace
        the exit code.

        The summary is already published when the diagnostic print runs, so
        an unwritable stderr (a host that closed `2>&-`) must not turn the
        "aborted = 3" contract into an unhandled OSError exit 1.
        """
        class ClosedStderr:
            def write(self, data):
                raise OSError('stderr is closed')

            def flush(self):
                pass

        with patch.object(sys, 'stderr', ClosedStderr()):
            code, state, disk, out = self._drive(
                fail_at=3, fail_with=SystemExit(130))
        message = '#1087 a closed stderr must not replace the exit code'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the SystemExit must propagate)')
        self.assertTrue(code.code == 3,
                        message + ' (the raw code must be normalized away from 2)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['outcome'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')

    def test_stale_anchor_refusal_survives_a_dead_stdout(self):
        """#1087 fix-r6 (P1b): a dead stdout must keep the refusal at exit 2.

        ``1>&-`` or a closed stream makes the in-window STALE ANCHOR prints
        raise; the old unguarded prints turned the never-started refusal into
        an in-window abort (exit 3 with a voided INVALID summary published).
        """
        class ClosedStdout:
            def write(self, data):
                raise OSError('stdout is closed')

            def flush(self):
                pass

        with patch.object(sys, 'stdout', ClosedStdout()):
            code, state, disk, out = self._drive(stale_anchors=True)
        message = '#1087 a dead stdout must keep the stale-anchor refusal at exit 2'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code.code == 2,
                        message + ' (the refusal must stay exit 2, not abort 3)')
        self.assertTrue(not (out / 'summary.json').exists(),
                        message + ' (nothing may be published for a refusal)')
        # #1087 fix-r8 (S2-c): the report name is per-run
        # (``stale-anchors.<token>.log``).
        self.assertTrue(any(path.name.startswith('stale-anchors.') and path.name.endswith('.log')
                            for path in out.iterdir()),
                        message + ' (the refusal report must still reach the disk)')

if __name__ == '__main__':
    unittest.main()
