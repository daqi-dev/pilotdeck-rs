# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r2 controls for the llm-envelope kill judge.

The delivered judge (``negative.py::classify``) is the shared #813 criterion:
the named test's own ``FAILED`` status line plus the failing oracle case's
``<fn> args=`` assertion message inside that test's own panic payload. The
controls below pin the positive shape and the false-kill shapes the old judge
(any non-zero exit with ``test result: FAILED.``) accepted — a sibling test's
panic, an anchor only in captured stdout, and an unrelated unwrap in the named
test. Every log is a synthetic libtest transcript; no cargo run happens here
(the real-sample kill is proven by the coordinator's full run).
``negative.py`` is a top-level script, so it is loaded through ``--list`` —
the one dispatch mode with no side effects.


#1087 fix-r3 (boundary #1) publication controls: the rows stay in memory
for the whole guarded window and one final per-run summary
(``summary.<token>.json``, #1087 fix-r9) is published by a single rename
only after the guard restored and byte-verified every
guarded source; an abort inside the window voids the rows, publishes
INVALID and exits 3 (never the "never started" 2). ``PublicationControls``
executes the delivered script a second time with only the process layer
mocked — no cargo run happens here either.

Run with:

    python3 -B edgeclaw-llm-envelope/test_negative.py
"""
import ast
import contextlib
import importlib.util
import io
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
    spec = importlib.util.spec_from_file_location('llm_envelope_negative', source)
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


class LlmEnvelopeJudgeControls(unittest.TestCase):
    """The llm-envelope kill judge must only accept the named test's anchor."""

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
            'tests/edgeclaw_llm_envelope.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def judge(self, log, returncode, expected, test):
        return self.runner.classify(log, returncode, expected, test)

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own ``<fn> args=`` panic."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = self._panic_log(
            test,
            'assertion `left == right` failed: '
            'extractFirstJsonObject args=["{\\"a\\":1}{\\"b\\":2}"] clock=0')
        self.assertTrue(self.judge(log, 101, anchor, test) == 'KILLED',
                        "#1087 the named test's own anchor panic must kill")

    def test_alternative_anchor_kills(self):
        """A group's alternative anchor still binds the kill to that test."""
        test = 'string_arrays_match_ts'
        anchor = ('normalizeStringArray args=', 'uniqueStrings args=')
        log = self._panic_log(
            test, 'assertion `left == right` failed: uniqueStrings args=[["a","a"]] clock=0')
        self.assertTrue(self.judge(log, 101, anchor, test) == 'KILLED',
                        '#1087 an alternative anchor of the group must kill')

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f'---- {test} stdout ----\n'
            'replay dump for debugging: extractFirstJsonObject args=[["x"]] clock=0\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_llm_envelope.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor, test) == 'INVALID',
                        '#1087 an anchor in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """An anchor in a sibling test's panic is not a kill."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = (
            '\nrunning 2 tests\n'
            f'test {test} ... ok\n'
            'test json_envelope_matches_ts ... FAILED\n'
            '\nfailures:\n'
            "thread 'json_envelope_matches_ts' (2841) panicked at "
            'tests/edgeclaw_llm_envelope.rs:44:9:\n'
            'assertion `left == right` failed: '
            'extractLooseJsonEnvelope args=["json {} end"] clock=0\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, 'extractLooseJsonEnvelope args=', test)
                        == 'INVALID',
                        "#1087 a sibling test's panic is not a kill")

    def test_unrelated_unwrap_is_not_a_kill(self):
        """A FAILED run without the bound anchor message is not a kill."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = self._panic_log(test, 'called `Option::unwrap()` on a `None` value')
        self.assertTrue(self.judge(log, 101, anchor, test) == 'INVALID',
                        '#1087 an unrelated unwrap panic is not a kill')

    def test_green_run_survives(self):
        """A passing bound test is SURVIVED, never INVALID."""
        test = 'json_object_matches_ts'
        anchor = 'extractFirstJsonObject args='
        log = self._panic_log(test, 'no failure', status='ok')
        self.assertTrue(self.judge(log, 0, anchor, test) == 'SURVIVED',
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
    """The llm-envelope runner must never keep a stale green summary.

    #1087 fix-r3 (boundary #1): the rows stay in memory for the whole guarded
    window and one final per-run summary (``summary.<token>.json``,
    #1087 fix-r9) is published by a single rename only after
    ``mutating_sources`` restored and byte-verified every guarded source. An abort inside the window — a cancellation, a restore failure,
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
        self.tmp = tempfile.TemporaryDirectory(prefix='llmenvelope-negctl-')
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
               baseline_fails=False, stale_anchor=False, arguments=None,
               no_target=False):
        """Execute the delivered script once with the process layer mocked.

        ``fail_at`` is the 1-based ``run_owned`` call index (1 is the baseline
        run) that raises ``fail_with``; ``restore_fails_at`` is the 1-based
        ``write_source_atomic`` call index that raises instead of recording
        (5 is the first restore of the guarded window's own ``finally``).
        ``stale_anchor`` makes every guarded file lose its anchors, driving
        the exit-2 stale-anchor refusal. ``arguments`` replaces the selected
        control names on argv, and ``no_target`` drops CARGO_TARGET_DIR, for
        the pre-guard refusal controls (#1087 fix-r5, concurrency #1).
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
                'tests/edgeclaw_llm_envelope.rs:12:5:\n'
                f'{expected[name]} expected Some("p"), actual None\n'
                'note: run with `RUST_BACKTRACE=1` environment variable to display '
                'a backtrace\n'
                'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
                '0 filtered out\n'
            )
            return subprocess.CompletedProcess(command, 101, '', log)

        def fake_mutating(root, paths, **kwargs):
            originals = {path: path.read_bytes() for path in paths}
            if stale_anchor:
                # Every guarded file loses its anchors, so the exit-2
                # stale-anchor refusal runs while (for the reused-directory
                # control) a previous green summary sits in the out path.
                originals = {path: b'stale twin bytes: no anchors remain\n'
                             for path in originals}

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
            # #1087 fix-r9 (P1): the summary probe follows the per-run
            # summary shape — the fixed ``summary.json`` name is gone.
            state['summary_existed'] = (state['summary_existed']
                                        or bool(list(out.glob('summary.*.json'))))
            if restore_fails_at is not None and state['writes'] == restore_fails_at:
                raise RuntimeError('twin restore failure')

        def is_summary_tmp(name):
            # #1087 fix-r5 (concurrency #3): the staging name is unique per
            # run; #1087 fix-r9 (P1): it prefixes the per-run summary name,
            # not the fixed ``summary.json.``.
            return (name.startswith('summary.')
                    and name.endswith('.tmp'))

        class Model:
            real_write_text = Path.write_text
            real_replace = Path.replace

            def model_write_text(self, data, *args, **kwargs):
                # #1087 fix-r9 (P1): the verdict is published under the
                # per-run name; a direct in-place write to any summary file
                # is the defect the one-rename rule exists to prevent.
                if (self.parent == out and self.name.startswith('summary.')
                        and self.name.endswith('.json')):
                    disk['direct'] = data
                    return None
                if self.parent == out and is_summary_tmp(self.name):
                    disk['staged'] = data
                return Model.real_write_text(self, data, *args, **kwargs)

            def model_replace(self, destination, *args, **kwargs):
                if self.parent == out and is_summary_tmp(self.name):
                    disk['renames'] = disk.get('renames', 0) + 1
                    disk['summary'] = disk['staged']
                return Model.real_replace(self, destination, *args, **kwargs)

        source = Path(__file__).with_name('negative.py')
        if arguments is None:
            arguments = [name for name, _test in selected]
        argv = [str(source), str(out)] + list(arguments)
        fake = types.ModuleType('negative_process')
        fake.mutating_sources = fake_mutating
        fake.run_owned = fake_run_owned
        fake.wait_for_slot = fake_slot
        fake.write_source_atomic = fake_atomic
        fake.target_environment = lambda root, env=None: None
        # #1087 fix-r5: the shared publication helpers stay the shipped ones
        # so the twins exercise the real unique-staging behavior.
        fake.unique_summary_tmp = shared_process.unique_summary_tmp
        fake.detach_dead_streams = shared_process.detach_dead_streams
        # #1087 fix-r8 (S1-a/S1-b/S1-c): the guarded print, the per-run
        # token, and the shared refused-marker writer are bound the same way
        # — the twins exercise the delivered code against the real ones.
        fake.dead_stream_print = shared_process.dead_stream_print
        fake.new_run_token = shared_process.new_run_token
        fake.refuse_summary = shared_process.refuse_summary
        saved_process = sys.modules.get('negative_process')
        saved_argv = sys.argv
        saved_target = os.environ.get('CARGO_TARGET_DIR')
        if no_target and saved_target is not None:
            del os.environ['CARGO_TARGET_DIR']
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
            if saved_target is not None:
                os.environ['CARGO_TARGET_DIR'] = saved_target
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
        # #1087 fix-r9 (P1): the voided verdict lives at this run's per-run
        # summary name; the glob finds exactly that one file.
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue([row['outcome'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: KeyboardInterrupt' in row['reason']
                for row in payload['cases']),
            message + ' (the invalidation reason was not recorded)')
        self.assertTrue('voided' in payload['cases'][0]['reason'],
                        message + ' (the already-KILLED row must be voided too)')

    def test_a_voided_run_reports_its_row_count(self):
        """#1087 fix-r9 (P2-a #1): the void diagnostic carries the count.

        The fix-r8 mechanical replacement ate the f-string braces, so the
        abort path printed ``voided len(results) row(s):`` — the literal,
        not the row count this twin must see.
        """
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            code, state, disk, out = self._drive(
                fail_at=3, fail_with=KeyboardInterrupt('red run interrupted'))
        message = '#1087 the void diagnostic must name the row count'
        self.assertIsInstance(code, KeyboardInterrupt,
                              message + ' (the cancellation must be re-raised)')
        diagnostic = stderr.getvalue()
        self.assertTrue('voided 2 row(s):' in diagnostic,
                        message + f' (stderr was {diagnostic!r})')
        self.assertTrue('voided len(' not in diagnostic,
                        message + ' (the literal len(results) came back)')

    def test_systemexit_is_normalized_to_three(self):
        """An aborting SystemExit pairs with exit code 3, never 2 or the raw code."""
        code, state, disk, out = self._drive(fail_at=3, fail_with=SystemExit(130))
        message = '#1087 an INVALID summary must pair with exit code 3'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the SystemExit must propagate)')
        self.assertTrue(code.code == 3,
                        message + ' (the raw code must be normalized away from 2)')
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
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
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the failed run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
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
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
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
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue([row['outcome'] for row in payload['cases']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')

    def test_a_refusal_in_a_reused_directory_never_touches_the_other_run(self):
        """#1087 fix-r9 (P1, review concurrency #1): a refused re-run leaves
        the other run's green verdict alone.

        The OUT_DIR is a plain reused mkdir, so a host re-run can land in the
        same path after a green run. With the per-run summary names the
        exit-2 stale-anchor refusal writes its own refused marker under its
        own token and the previous run's green summary stays byte-identical
        — run B can no longer destroy run A's verdict (the fix-r4 marker
        overwrote the shared fixed name, which is the exact defect this
        round closes). This replaces the fix-r4 twin that pinned the
        overwrite as the intended behavior.
        """
        # Run A: green, into this test's reused directory.
        code_a, _state_a, _disk_a, out = self._drive()
        message = '#1087 a refused re-run must not touch the other run\'s summary'
        self.assertIsInstance(code_a, SystemExit,
                              message + ' (the first run must exit cleanly)')
        self.assertTrue(code_a.code == 0,
                        message + ' (the first run into the directory must stay '
                        'green)')
        green = list(out.glob('summary.*.json'))
        self.assertTrue(len(green) == 1,
                        message + ' (the green run published exactly one '
                        f'summary; got {[p.name for p in green]})')
        green_payload = json.loads(green[0].read_text())
        self.assertTrue('cases' in green_payload,
                        message + ' (the green run\'s verdict must carry its rows)')
        green_name = green[0].name
        green_bytes = green[0].read_bytes()
        # Run B: same directory, stale anchors — the exit-2 refusal.
        code_b, _state_b, _disk_b, out = self._drive(stale_anchor=True)
        self.assertIsInstance(code_b, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code_b.code == 2,
                        message + ' (the stale-anchor refusal must stay exit 2)')
        # The other run's green verdict is untouched — same name, same bytes.
        self.assertTrue(green[0].name == green_name,
                        message + ' (the green summary was renamed)')
        self.assertTrue(green[0].read_bytes() == green_bytes,
                        message + ' (the green summary was rewritten)')
        # This run's own refused marker exists, carries the reason, and no rows.
        marker = [path for path in out.glob('summary.*.json')
                  if path.name != green_name]
        self.assertTrue(len(marker) == 1,
                        message + ' (the refused run must leave its own marker)')
        payload = json.loads(marker[0].read_text())
        self.assertTrue('refused' in payload,
                        message + ' (the refused marker must carry the refusal)')
        self.assertTrue('stale anchors' in payload['refused'],
                        message + ' (the refused marker must carry the reason)')
        self.assertTrue('cases' not in payload and 'baseline' not in payload,
                        message + ' (the refused marker must carry no verdict '
                        'rows)')
        self.assertTrue(payload.get('runToken') != green_payload.get('runToken'),
                        message + ' (the marker must name a different run than '
                        'the green one)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (the refusal must not leave staging behind)')

    def test_refusal_leaves_a_fresh_directory_without_a_verdict(self):
        """A first-run refusal publishes no verdict: nothing was measured.

        #1087 fix-r9 (P1): the refusal now leaves its own refused marker
        under its per-run name (the record that this run refused), but no
        verdict rows may exist — "never started" is not a green summary.
        """
        code, state, disk, out = self._drive(stale_anchor=True)
        message = '#1087 a fresh refused directory must carry no verdict'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit 2)')
        self.assertTrue(code.code == 2, message + ' (the refusal must exit 2)')
        summaries = list(out.glob('summary.*.json'))
        # fix-r9 (review DS concurrency #2): an all() over an empty glob is
        # vacuously true — the marker's existence is itself the contract.
        self.assertTrue(len(summaries) == 1,
                        message + ' (the refusal must leave exactly its own '
                        'marker — an empty glob is not a refusal)')
        self.assertTrue(all('refused' in json.loads(path.read_text())
                            for path in summaries),
                        message + ' (a verdict summary was published for a '
                        'never-started run)')
        self.assertTrue(all('cases' not in json.loads(path.read_text())
                            and 'baseline' not in json.loads(path.read_text())
                            for path in summaries),
                        message + ' (the refused marker must carry no verdict '
                        'rows)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')


    def test_stale_anchor_refusal_survives_a_dead_stdout(self):
        """#1087 fix-r5 (boundary #1 family): the stale-anchor report print
        must not turn the exit-2 refusal into an in-window abort.

        A host reader that closed the pipe (``| head -1``) makes the report
        print raise; without the guard the refusal collapses into a voided
        summary plus exit 3, and "never started" is read as "aborted".
        """
        class DeadStdout:
            def write(self, data):
                raise BrokenPipeError('stdout is closed')

            def flush(self):
                raise BrokenPipeError('stdout is closed')

        with patch.object(sys, 'stdout', DeadStdout()):
            code, state, disk, out = self._drive(stale_anchor=True)
        message = '#1087 a dead stdout must not replace the refusal exit code'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit through SystemExit)')
        self.assertTrue(code.code == 2, message + ' (the refusal must stay exit 2)')
        # #1087 fix-r9 (P1): no verdict may be readable — the only thing a
        # refusal may leave is its own refused marker.
        marker = list(out.glob('summary.*.json'))
        # fix-r9 (review DS sibling #5): pin the marker's existence — an
        # all() over an empty glob is vacuously true.
        self.assertTrue(len(marker) == 1,
                        message + ' (the refusal must leave its own marker — '
                        'an empty glob is not a refusal)')
        self.assertTrue(all('refused' in json.loads(path.read_text())
                            for path in marker),
                        message + ' (nothing may be published for a refusal)')

    def test_unknown_control_name_is_a_refusal_not_a_crash(self):
        """#1087 fix-r5 (concurrency #1): a bad argv is exit 2, never an assert.

        The old ``assert not unknown`` crashed with a traceback (rc=1, "ran
        and was red") and — on a reused directory — left the previous run's
        green summary readable as this run's evidence. #1087 fix-r9 (P1):
        with the per-run summary names the other run's green stays
        byte-identical and this run leaves its own refused marker instead.
        """
        out = self.root / 'run'
        out.mkdir()
        # Another run's already-published green verdict, per-run named.
        (out / 'summary.999.1.123.json').write_text(json.dumps(
            {'baseline': {'outcome': 'PASS'},
             'cases': [{'name': self.selected[0][0], 'outcome': 'KILLED'}]}) + '\n')
        green_bytes = (out / 'summary.999.1.123.json').read_bytes()
        code, state, disk, out = self._drive(
            arguments=[self.selected[0][0], 'no-such-control'])
        message = '#1087 an unknown control name must refuse, not crash'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit 2)')
        self.assertTrue(code.code == 2, message + ' (the refusal must exit 2)')
        self.assertTrue((out / 'summary.999.1.123.json').read_bytes()
                        == green_bytes,
                        message + ' (the other run\'s green verdict was '
                        'rewritten)')
        payload = json.loads((out / 'summary.999.1.123.json').read_text())
        self.assertTrue('refused' not in payload,
                        message + ' (the other run\'s green verdict was '
                        'replaced by a refusal marker)')
        marker = [path for path in out.glob('summary.*.json')
                  if path.name != 'summary.999.1.123.json']
        self.assertTrue(len(marker) == 1,
                        message + ' (the refused run must leave its own marker)')
        refused = json.loads(marker[0].read_text())
        self.assertTrue('refused' in refused,
                        message + ' (the refusal marker must carry the refusal)')
        self.assertTrue('cases' not in refused and 'baseline' not in refused,
                        message + ' (the marker must carry no verdict rows)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_a_refusal_before_the_runners_mkdir_still_leaves_its_marker(self):
        """#1087 fix-r12 (review DS r11 boundary #1): the unknown-control
        refusal fires before the runner's own ``mkdir``, and the refusal
        itself must create the directory and land its marker (the fix-r11
        ``refuse_summary`` contract) — a pre-created directory is not a
        precondition of the one-marker-per-refusal contract.
        """
        code, state, disk, out = self._drive(arguments=['no-such-mutant'])
        message = '#1087 a pre-mkdir refusal must still leave its marker'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit 2)')
        self.assertTrue(code.code == 2, message + ' (the refusal must exit 2)')
        marker = list(out.glob('summary.*.json'))
        self.assertTrue(len(marker) == 1,
                        message + ' (the refusal must create the directory '
                        'and leave exactly its own marker)')
        refused = json.loads(marker[0].read_text())
        self.assertTrue('unknown controls' in refused['refused'],
                        message + ' (the marker must carry the reason)')
        self.assertTrue('cases' not in refused and 'baseline' not in refused,
                        message + ' (the marker must carry no verdict rows)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_missing_target_dir_is_a_refusal_not_a_crash(self):
        """#1087 fix-r5 (concurrency #1): a missing CARGO_TARGET_DIR is exit 2.

        Same shape as the unknown control: the old ``assert target`` crashed
        with rc=1 and kept a reused directory's stale green summary readable.
        #1087 fix-r9 (P1): the other run's green stays byte-identical and
        this run leaves its own refused marker instead.
        """
        out = self.root / 'run'
        out.mkdir()
        # Another run's already-published green verdict, per-run named.
        (out / 'summary.999.1.123.json').write_text(json.dumps(
            {'baseline': {'outcome': 'PASS'},
             'cases': [{'name': self.selected[0][0], 'outcome': 'KILLED'}]}) + '\n')
        green_bytes = (out / 'summary.999.1.123.json').read_bytes()
        code, state, disk, out = self._drive(no_target=True)
        message = '#1087 a missing CARGO_TARGET_DIR must refuse, not crash'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the refusal must exit 2)')
        self.assertTrue(code.code == 2, message + ' (the refusal must exit 2)')
        self.assertTrue((out / 'summary.999.1.123.json').read_bytes()
                        == green_bytes,
                        message + ' (the other run\'s green verdict was '
                        'rewritten)')
        marker = [path for path in out.glob('summary.*.json')
                  if path.name != 'summary.999.1.123.json']
        self.assertTrue(len(marker) == 1,
                        message + ' (the refused run must leave its own marker)')
        refused = json.loads(marker[0].read_text())
        self.assertTrue('refused' in refused,
                        message + ' (the refusal marker must carry the refusal)')
        self.assertTrue('cases' not in refused and 'baseline' not in refused,
                        message + ' (the marker must carry no verdict rows)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

if __name__ == '__main__':
    unittest.main()
