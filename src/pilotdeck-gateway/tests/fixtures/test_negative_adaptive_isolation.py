# anchor-selfcheck: unsupported subprocess isolation and candidate-baseline regression tests
"""#994: real HEAD-guarded Python probes; no synthetic Git or Cargo workspace."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from negative_process import SourceBaselineError


def load_runner():
    spec = importlib.util.spec_from_file_location('adaptive_isolation_probe',
                                                 HERE / 'negative-adaptive/negative_controls.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def hold_mutant(out, ready):
    runner = load_runner()
    with runner.verified_inputs() as contents:
        with runner.isolated_inputs(runner.root, contents, out) as directory:
            copied = directory / runner.shared.relative_to(runner.root)
            copied.write_text(contents[runner.shared].decode().replace('MIN_RUN_SECONDS = 20',
                                                                       'MIN_RUN_SECONDS = 1', 1))
            # Existence is the reader's readiness gate: publish complete bytes.
            pending = ready.with_suffix('.tmp')
            pending.write_text(str(copied))
            pending.replace(ready)
            while True:
                signal.pause()


class IsolationTests(unittest.TestCase):
    def setUp(self):
        self.runner = load_runner()
        self.directory = tempfile.TemporaryDirectory(prefix='pdrs994-isolation-test-')
        self.addCleanup(self.directory.cleanup)
        self.out = Path(self.directory.name)
        self.paths = [self.runner.shared, self.runner.process, self.runner.tests]
        self.before = {path: path.read_bytes() for path in self.paths}

    def assert_pristine(self):
        for path, data in self.before.items():
            self.assertEqual(path.read_bytes(), data, '#994 real input must stay unchanged')

    def exercise(self, case, *, break_restore=False):
        runner = self.runner
        with runner.verified_inputs() as contents:
            with runner.isolated_inputs(runner.root, contents, self.out) as directory:
                self.assertNotEqual(directory.resolve(), runner.root.resolve(),
                                    '#994 isolation must not be a no-op')
                copied = directory / case[1].relative_to(runner.root)
                self.assertEqual(copied.read_bytes(), contents[case[1]], '#994 copied verified input')
                write = Path.write_bytes
                def restore(path, data):
                    if break_restore and path == copied:
                        return len(data)  # deliberate no-op restoration
                    return write(path, data)
                with patch.object(Path, 'write_bytes', restore):
                    row = runner.run_case(case, directory, self.out, contents[case[1]])
                self.assert_pristine()
        return row

    def test_real_mutant_is_killed_and_restored_in_copy(self):
        row = self.exercise(self.runner.cases[0])
        self.assertEqual(row['status'], 'KILLED', '#994 copied real assertion must kill')
        self.assertTrue(row['compiled'], '#994 Python mutant must compile')
        self.assertTrue(row['restoredPass'], '#994 restored copy must pass')

    def test_guard_never_writes_real_inputs_even_on_exit(self):
        write_bytes, write_text = Path.write_bytes, Path.write_text
        def write_bytes_checked(path, data):
            self.assertNotIn(path, self.paths, '#994 real files must never be rewritten')
            return write_bytes(path, data)
        def write_text_checked(path, data, *args, **kwargs):
            self.assertNotIn(path, self.paths, '#994 real files must never be rewritten')
            return write_text(path, data, *args, **kwargs)
        with patch.object(Path, 'write_bytes', write_bytes_checked), \
                patch.object(Path, 'write_text', write_text_checked):
            row = self.exercise(self.runner.cases[0])
        self.assertEqual(row['status'], 'KILLED', '#994 read-only guard must still run controls')

    def test_dirty_shared_or_process_input_is_refused_before_copy(self):
        read = Path.read_bytes
        for dirty in self.paths:
            with self.subTest(path=dirty):
                def read_dirty(path):
                    return b'killed predecessor residue' if path == dirty else read(path)
                with patch.object(Path, 'read_bytes', read_dirty), self.assertRaisesRegex(
                        SourceBaselineError, 'differs from HEAD', msg='#994 dirty shared input must fail closed'):
                    with self.runner.verified_inputs():
                        self.fail('#994 dirty shared input must never reach copying')
        self.assert_pristine()

    def test_changed_input_on_exit_invalidates_previously_killed_record(self):
        changed = False
        read = Path.read_bytes
        def read_changed(path):
            return b'concurrent residue' if changed and path == self.runner.shared else read(path)
        def run_case(*_args):
            nonlocal changed
            changed = True
            return dict(name='floor-too-small', status='KILLED', reason='bound assertion',
                        compiled=True, restoredPass=True)
        output = self.out / 'changed-on-exit'
        with patch.object(Path, 'read_bytes', read_changed), \
                patch.object(self.runner, 'run_case', side_effect=run_case), \
                patch.object(self.runner, 'cases', [self.runner.cases[0]]), \
                patch.object(sys, 'argv', ['runner', str(output)]), \
                patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.out / 'unused')}):
            code = self.runner.main()
        row = json.loads((output / 'summary.json').read_text())['controls'][0]
        self.assertEqual(code, 1, '#994 changed input on exit must invalidate suite')
        self.assertEqual(row['status'], 'INVALID', '#994 exit guard must invalidate prior KILLED')
        self.assertFalse(row['restoredPass'], '#994 exit guard must revoke prior restoredPass')
        self.assertIn('changed during isolated controls', row['reason'], '#994 exit guard diagnosis')
        self.assert_pristine()

    def test_noop_mutant_survives_and_suite_exits_nonzero(self):
        name, path, before, _after, test, marker = self.runner.cases[0]
        case = (name, path, before, before, test, marker)
        out = self.out / 'noop'
        with patch.object(self.runner, 'cases', [case]), patch.object(sys, 'argv', ['runner', str(out)]), \
                patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.out / 'unused')}):
            code = self.runner.main()
        summary = json.loads((out / 'summary.json').read_text())
        self.assertEqual(code, 1, '#994 no-op must fail the negative suite')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 1, 'INVALID': 0},
                         '#994 no-op must not produce a false KILLED')
        self.assert_pristine()

    def test_noop_restoration_invalidates_kill(self):
        row = self.exercise(self.runner.cases[0], break_restore=True)
        self.assertEqual(row['status'], 'INVALID', '#994 restoration no-op must invalidate kill')
        self.assertFalse(row['restoredPass'], '#994 dirty copy cannot claim restoredPass')
        self.assertIn('scratch restoration failed', row['reason'], '#994 exact restoration diagnosis')

    def test_stale_anchor_accounts_for_later_controls(self):
        name, path, _before, after, test, marker = self.runner.cases[0]
        stale = (name, path, 'missing #994 anchor', after, test, marker)
        out = self.out / 'stale'
        with patch.object(self.runner, 'cases', [stale, self.runner.cases[1]]), \
                patch.object(sys, 'argv', ['runner', str(out)]), \
                patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.out / 'unused')}):
            code = self.runner.main()
        summary = json.loads((out / 'summary.json').read_text())
        self.assertEqual(code, 1, '#994 stale anchors must exit nonzero')
        self.assertEqual(summary['counts'], {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 1},
                         '#994 stale anchors must not abort later controls')
        self.assertIn('stale anchor', summary['controls'][0]['reason'], '#994 stale anchor diagnosis')
        self.assert_pristine()

    def test_term_and_kill_leave_real_inputs_pristine_and_next_run_passes(self):
        for signum in (signal.SIGTERM, signal.SIGKILL):
            with self.subTest(signal=signum):
                output = self.out / str(signum)
                output.mkdir()
                ready = output / 'ready'
                child = subprocess.Popen([sys.executable, '-B', str(Path(__file__).resolve()),
                                          '--hold', str(output), str(ready)],
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                         start_new_session=True)
                try:
                    deadline = time.monotonic() + 10
                    while not ready.exists() and child.poll() is None and time.monotonic() < deadline:
                        time.sleep(.01)
                    self.assertTrue(ready.exists(), '#994 child must publish mutant readiness')
                    copied = Path(ready.read_text())
                    self.assertNotEqual(copied.resolve(), self.runner.shared.resolve(),
                                        '#994 signal probe must mutate only its copy')
                    self.assertIn('MIN_RUN_SECONDS = 1', copied.read_text(), '#994 signal hits a real mutant')
                    self.assert_pristine()
                    child.send_signal(signum)
                    stdout, stderr = child.communicate(timeout=5)
                    self.assertEqual(child.returncode, -signum, '#994 intended signal must terminate child')
                    print(f'#994 signal={signum} rc={child.returncode} real inputs unchanged', flush=True)
                    self.assertEqual(stdout + stderr, '', '#994 signal probe has no hidden exception')
                    self.assert_pristine()
                    # Reacquire the real HEAD guard after the killed holder,
                    # import freshly copied executors and kill/restore again.
                    restart = output / 'restart'
                    restart.mkdir()
                    with self.runner.verified_inputs() as contents:
                        with self.runner.isolated_inputs(self.runner.root, contents, restart) as directory:
                            row = self.runner.run_case(self.runner.cases[0], directory, restart,
                                                       contents[self.runner.shared])
                    self.assertEqual(row['status'], 'KILLED', '#994 next run must use pristine executor')
                    self.assertTrue(row['restoredPass'], '#994 next restored check must pass')
                finally:
                    if child.poll() is None:
                        os.killpg(child.pid, signal.SIGKILL)
                    child.communicate(timeout=5)


class CandidateBaselineTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('network_candidate_probe',
                                                     HERE / 'network-timing/negative_controls.py')
        self.fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.fixture)
        self.directory = tempfile.TemporaryDirectory(prefix='pdrs994-baseline-test-')
        self.addCleanup(self.directory.cleanup)
        self.out = Path(self.directory.name)

    def test_baseline_only_uses_candidate_and_never_claims_kills(self):
        name = 'background_io_headers_fulfilled_tie_parent_last'
        result = subprocess.CompletedProcess([], 0,
            f'test {name} ... ok\ntest result: ok. 1 passed; 0 failed;', '')
        target = (self.out / 'product-target').resolve()
        with patch.object(self.fixture, 'CargoTests') as constructor, \
                patch.object(self.fixture, 'mutation_suite', side_effect=AssertionError('#994 no mutation in baseline mode')), \
                patch.dict(os.environ, {'CARGO_TARGET_DIR': str(target)}), \
                patch.object(sys, 'argv', ['runner', '--baseline-only', str(self.out / 'baseline'), 'io-as-microtask']):
            constructor.return_value.run.return_value = result
            code = self.fixture.main()
        self.assertEqual(code, 0, '#994 candidate baseline should pass')
        self.assertEqual(constructor.call_args.args[0], self.fixture.root / 'src', '#994 candidate current tree')
        self.assertEqual(constructor.call_args.args[2]['CARGO_TARGET_DIR'], str(target), '#994 caller target unchanged')
        self.assertEqual(constructor.return_value.run.call_args.kwargs, {'positive': True}, '#994 positive phase only')
        summary = json.loads((self.out / 'baseline/summary.json').read_text())
        self.assertFalse(summary['mutationValidation'], '#994 baseline never certifies mutation validation')
        self.assertEqual(summary['selections'][0]['status'], 'BASELINE_PASS', '#994 explicit baseline status')
        self.assertNotIn('restoredPass', summary['selections'][0], '#994 baseline makes no restoration claim')

    def test_failure_zero_tests_and_compile_error_continue_and_fail(self):
        first, second = self.fixture.cases[0], self.fixture.cases[3]
        test = second[-1][0]
        passed = subprocess.CompletedProcess([], 0,
            f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;', '')
        failures = [subprocess.CompletedProcess([], 101, 'test result: FAILED.', ''),
                    subprocess.CompletedProcess([], 0, 'test result: ok. 0 passed; 0 failed;', ''),
                    RuntimeError('compile failed')]
        for index, failure in enumerate(failures):
            with self.subTest(index=index), patch.object(self.fixture, 'CargoTests') as constructor, \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.out / 'target')}):
                constructor.return_value.run.side_effect = [failure, passed]
                output = self.out / str(index)
                code = self.fixture.baseline_only([first, second], output)
            summary = json.loads((output / 'summary.json').read_text())
            self.assertEqual(code, 1, '#994 failed baseline must exit nonzero')
            self.assertEqual(len(summary['selections']), 2, '#994 later selection must run after failure')
            self.assertEqual(summary['selections'][1]['status'], 'BASELINE_PASS', '#994 later positive is accounted')
            self.assertEqual(summary['selections'][0]['status'],
                             'BASELINE_ERROR' if isinstance(failure, Exception) else 'BASELINE_FAIL',
                             '#994 distinguish baseline failure and execution error')

    def test_missing_target_and_empty_selection_do_not_pass(self):
        with patch.dict(os.environ, {}, clear=True), self.assertRaisesRegex(
                SystemExit, 'CARGO_TARGET_DIR', msg='#994 candidate must require caller target'):
            self.fixture.baseline_only([self.fixture.cases[0]], self.out / 'missing')
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.out / 'target')}):
            self.assertEqual(self.fixture.baseline_only([], self.out / 'empty'), 1,
                             '#994 empty candidate selection cannot pass')

    def test_formal_mode_still_uses_head_guarded_suite(self):
        with patch.object(self.fixture, 'mutation_suite', return_value=7) as suite, \
                patch.dict(os.environ, {'PDRS_NEGATIVE_BASELINE': 'not-HEAD'}), \
                patch.object(self.fixture, 'baseline_only', side_effect=AssertionError('#994 formal mode must mutate')), \
                patch.object(sys, 'argv', ['runner', str(self.out / 'formal'), 'io-as-microtask']):
            self.assertEqual(self.fixture.main(), 7, '#994 formal suite exit code propagates')
        self.assertEqual(suite.call_args.args[1], [self.fixture.cases[0]], '#994 formal selection preserved')
        self.assertNotIn('baseline', suite.call_args.kwargs, '#994 never supply alternate Git baseline')


if __name__ == '__main__':
    if len(sys.argv) == 4 and sys.argv[1] == '--hold':
        hold_mutant(Path(sys.argv[2]), Path(sys.argv[3]))
    else:
        unittest.main(verbosity=2)
