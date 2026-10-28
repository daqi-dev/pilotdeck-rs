# anchor-selfcheck: unsupported synthetic adaptive build/run and scratch lifecycle regression tests
"""#994 process probes; fake Cargo is a test double, never a product baseline."""
from contextlib import contextmanager
import io
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import negative_process
from _shared import negative_control as control


class ProgressTests(unittest.TestCase):
    def test_invalid_idle_deadline_fails_before_spawn(self):
        for value in (0, -1, float('nan'), float('inf')):
            with self.subTest(value=value), patch.object(negative_process.subprocess, 'Popen',
                    side_effect=AssertionError('#994 invalid idle deadline must not spawn')) as spawn:
                with self.assertRaises(ValueError, msg='#994 invalid idle deadline must fail'):
                    negative_process.run_owned(['unused'], timeout=None, idle_timeout=value)
                spawn.assert_not_called()

    def test_progress_can_exceed_test_wall_clock(self):
        # Each output chunk is progress, even without a newline.
        script = "import time; [(print('tick', end='', flush=True), time.sleep(.12)) for _ in range(5)]"
        result = negative_process.run_owned([sys.executable, '-c', script],
                                             timeout=None, idle_timeout=.4)
        self.assertEqual(result.stdout, 'tick' * 5, '#994 progress must retain output and finish')

    def test_silent_compiler_is_bounded_and_retains_output(self):
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired, msg='#994 silent build must time out') as found:
            negative_process.run_owned([sys.executable, '-c',
                "import time; print('started', flush=True); time.sleep(60)"],
                timeout=None, idle_timeout=.15, kill_timeout=.3)
        self.assertIn('started', found.exception.stdout, '#994 timeout keeps compiler diagnostics')
        self.assertLess(time.monotonic() - started, 3, '#994 compiler wait is bounded')

    def test_progress_does_not_disable_explicit_wall_clock(self):
        with self.assertRaises(subprocess.TimeoutExpired, msg='#994 explicit wall clock must win'):
            negative_process.run_owned([sys.executable, '-c',
                "import time\nwhile True: print('tick', flush=True); time.sleep(.03)"],
                timeout=.15, idle_timeout=.3, kill_timeout=.3)

    def test_legacy_wall_clock_still_applies(self):
        with self.assertRaises(subprocess.TimeoutExpired, msg='#994 legacy wall timeout unchanged'):
            negative_process.run_owned([sys.executable, '-c', 'import time; time.sleep(60)'],
                                         timeout=.05, kill_timeout=.3)


class AdaptiveTests(unittest.TestCase):
    def test_floor_and_multiplier(self):
        self.assertEqual(control.adaptive_timeout(.1), 20, '#994 minimum runtime is 20 seconds')
        self.assertEqual(control.adaptive_timeout(6), 30, '#994 runtime limit is baseline times five')
        for value in (-1, float('nan'), float('inf')):
            with self.subTest(value=value), self.assertRaises(ValueError, msg='#994 invalid timing must fail'):
                control.adaptive_timeout(value)

    def test_target_is_caller_owned_and_separate(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name) / 'scratch'
            target = Path(name) / 'negctl-target'
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(target)}):
                env = control.persistent_environment(directory)
                self.assertEqual(env['CARGO_TARGET_DIR'], str(target.resolve()), '#994 honor caller persistent target')
                self.assertEqual(env['CARGO_INCREMENTAL'], '1', '#994 enable incremental compilation')
                self.assertEqual(control.environment(directory)['CARGO_INCREMENTAL'], '0',
                                 '#994 legacy environment must remain unchanged')
            for target in (control.REPO / '_build/cargo-target', control.SRC / 'target', directory / 'target'):
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(target)}), self.assertRaises(
                        ValueError, msg='#994 reject product/disposable target'):
                    control.persistent_environment(directory)
            with patch.dict(os.environ, {}, clear=True), self.assertRaises(
                    SystemExit, msg='#994 require caller target'):
                control.persistent_environment(directory)


class CargoTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='pdrs994-unit-')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.source = self.root / 'src'
        (self.source / 'pilotdeck-gateway').mkdir(parents=True)
        self.output = self.root / 'logs'
        self.output.mkdir()
        self.target = self.root / 'negctl-target'
        self.target.mkdir()
        self.executable = self.target / 'current-test'
        self.executable.touch()
        self.env = dict(os.environ, CARGO_TARGET_DIR=str(self.target))
        self.runner = control.CargoTests(self.source, self.output, self.env)
        self.calls = []
        self.compile_code = 0
        self.compile_output = None
        self.run_code = 0
        self.run_text = 'test cell ... ok\ntest result: ok. 1 passed; 0 failed;\n'
        self.clock = 10.0
        self.compile_duration = 300.0
        self.runtime = 6.0
        self.addCleanup(patch.stopall)
        patch.object(control, 'wait_for_slot', return_value=subprocess.CompletedProcess([], 0, '', '')).start()
        patch.object(control, 'run_owned', side_effect=self.run_owned).start()
        patch.object(control.time, 'monotonic', side_effect=lambda: self.clock).start()

    def run_owned(self, command, **kwargs):
        self.calls.append((command, kwargs))
        if command[0] == 'cargo':
            self.clock += self.compile_duration
            output = self.compile_output
            if output is None:
                output = '\n'.join(json.dumps(row) for row in [
                    {'reason': 'compiler-artifact', 'target': {'name': 'pilotdeck_gateway'},
                     'profile': {'test': True}, 'executable': str(self.executable)},
                    {'reason': 'build-finished', 'success': True}])
            return subprocess.CompletedProcess(command, self.compile_code, output, 'compiler output\n')
        self.clock += self.runtime
        return subprocess.CompletedProcess(command, self.run_code, self.run_text, '')

    def test_compile_is_separate_and_runtime_uses_baseline(self):
        self.runner.run('positive', ('--lib',), ['cell', '--exact'], positive=True)
        self.runner.run('negative', ('--lib',), ['cell', '--exact'])
        build, run = self.calls[-2:]
        self.assertIn('--no-run', build[0], '#994 must compile without running tests')
        self.assertIn('--message-format=json', build[0], '#994 select current Cargo artifact')
        self.assertIsNone(build[1]['timeout'], '#994 compile must not use test wall clock')
        self.assertEqual(build[1]['idle_timeout'], 900, '#994 compile has progress deadline')
        self.assertEqual(run[0], [str(self.executable), 'cell', '--exact'], '#994 runtime invokes artifact, not Cargo')
        self.assertEqual(run[1]['timeout'], 30, '#994 exclude compile duration from baseline times five')
        self.assertEqual(run[1]['cwd'], self.source / 'pilotdeck-gateway', '#994 Cargo package cwd preserved')
        record = json.loads((self.output / 'negative-compile.json').read_text())
        self.assertEqual(record['target'], str(self.target), '#994 evidence records exact persistent target')

    def test_baselines_are_per_selection_and_only_passes_calibrate(self):
        self.runner.run('positive', ('--lib',), ['cell'], positive=True)
        with self.assertRaisesRegex(ValueError, 'baseline', msg='#994 must not reuse another selection baseline'):
            self.runner.run('unknown', ('--lib',), ['other'])
        self.run_code = 101
        self.run_text = 'test result: FAILED. 0 passed; 1 failed;'
        self.runner.run('failed', ('--lib',), ['failed'], positive=True)
        with self.assertRaisesRegex(ValueError, 'baseline', msg='#994 failed positive cannot calibrate'):
            self.runner.run('negative-failed', ('--lib',), ['failed'])

    def test_restored_rebuilds_in_the_same_target_and_does_not_recalibrate(self):
        self.runner.run('positive', ('--lib',), ['cell'], positive=True)
        self.runtime = 1
        self.runner.run('restored', ('--lib',), ['cell'], restored=True)
        self.runner.run('next', ('--lib',), ['cell'])
        self.assertEqual(sum(cmd[0] == 'cargo' for cmd, _ in self.calls), 3,
                         '#994 restored must rebuild the original source')
        self.assertTrue(all(kwargs['env']['CARGO_TARGET_DIR'] == str(self.target)
                            for _, kwargs in self.calls), '#994 all phases use same persistent target')
        self.assertEqual(self.calls[-1][1]['timeout'], 30, '#994 restored run must not replace baseline')

    def test_compile_failure_never_runs_tests(self):
        self.compile_code = 1
        with self.assertRaisesRegex(RuntimeError, 'compilation failed', msg='#994 compile failure is invalid'):
            self.runner.run('bad-build', ('--lib',), ['cell'], positive=True)
        self.assertEqual(len(self.calls), 1, '#994 compile error cannot reach test executable')

    def test_runtime_timeout_is_recorded_as_compiled(self):
        self.runner.run('positive', ('--lib',), ['cell'], positive=True)
        original = self.run_owned
        def run(command, **kwargs):
            if command[0] != 'cargo':
                raise subprocess.TimeoutExpired(command, kwargs['timeout'], 'before hang', '')
            return original(command, **kwargs)
        with patch.object(control, 'run_owned', side_effect=run), self.assertRaises(
                subprocess.TimeoutExpired, msg='#994 runtime timeout propagates'):
            self.runner.run('timeout', ('--lib',), ['cell'])
        self.assertTrue(self.runner.compiled['timeout'], '#994 runtime hang is not a compile failure')
        self.assertIn('before hang', (self.output / 'timeout-run.log').read_text(),
                      '#994 retain runtime timeout output')

    def test_integration_target_keeps_libtest_arguments(self):
        self.compile_output = '\n'.join(json.dumps(row) for row in [
            {'reason': 'compiler-artifact', 'target': {'name': 'network_timing'},
             'profile': {'test': True}, 'executable': str(self.executable)},
            {'reason': 'build-finished', 'success': True}])
        args = ['cell', '--exact', '--show-output', '--test-threads=1']
        self.runner.run('integration', ('--test', 'network_timing'), args, positive=True)
        self.assertEqual(self.calls[-1][0], [str(self.executable), *args],
                         '#994 preserve exact/integration/show-output arguments')
        self.assertEqual(self.calls[0][0][5:7], ['--test', 'network_timing'],
                         '#994 select the requested integration artifact')

    def test_missing_ambiguous_outside_and_invalid_artifacts_fail_closed(self):
        artifact = {'reason': 'compiler-artifact', 'target': {'name': 'pilotdeck_gateway'},
                    'profile': {'test': True}, 'executable': str(self.executable)}
        finished = {'reason': 'build-finished', 'success': True}
        values = [json.dumps(finished), 'not json', json.dumps(artifact),
                  '\n'.join(map(json.dumps, [artifact, dict(artifact, executable='/tmp/other'), finished])),
                  '\n'.join(map(json.dumps, [dict(artifact, executable='/tmp/other'), finished]))]
        for index, output in enumerate(values):
            self.compile_output = output
            with self.subTest(index=index), self.assertRaises((ValueError, RuntimeError),
                    msg='#994 reject invalid artifact inventory'):
                self.runner.run(f'invalid-{index}', ('--lib',), ['cell'], positive=True)
        self.assertTrue(all(cmd[0] == 'cargo' for cmd, _ in self.calls), '#994 never execute invalid artifacts')


class SuiteTests(unittest.TestCase):
    def exercise(self, fault=None):
        with tempfile.TemporaryDirectory(prefix='pdrs994-suite-unit-') as name:
            root = Path(name)
            real = root / 'src/example.rs'
            real.parent.mkdir()
            real.write_text('original')
            copied = root / 'scratch/src/example.rs'
            copied.parent.mkdir(parents=True)
            copied.write_text('original')
            observations = []
            exited = []
            @contextmanager
            def guard(*_args, **_kwargs):
                if fault == 'guard':
                    raise negative_process.SourceBaselineError(root, 'src/example.rs', 'test refusal')
                try:
                    yield {real: b'original'}
                finally:
                    exited.append(True)
                    if fault == 'guard-exit':
                        raise RuntimeError('guard restoration verification failed')
            @contextmanager
            def scratch(*_args, **_kwargs):
                yield root / 'scratch'
            def run(_runner, label, *_args, **_kwargs):
                observations.append((label, copied.read_text()))
                _runner.compiled[label] = True
                if fault == 'interrupt' and label.endswith('-negative'):
                    raise KeyboardInterrupt()
                if fault == 'compile' and label.startswith('first-') and label.endswith('-negative'):
                    raise RuntimeError('compile failure')
                code = 101 if label.endswith('-negative') else 0
                if fault == 'restore' and label == 'first-0-restored':
                    code = 1
                return subprocess.CompletedProcess([], code, '', '')
            rows = [(key, real, 'missing' if fault == 'anchor' and key == 'first' else 'original',
                     'changed', ['cell']) for key in ('first', 'second')]
            with patch.object(control, 'REPO', root), patch.object(control, 'mutating_sources', guard), \
                 patch.object(control, 'scratch', scratch), \
                 patch.object(control, 'source_copy', return_value=root / 'scratch/src'), \
                 patch.object(control, 'persistent_environment', return_value={'CARGO_TARGET_DIR': str(root / 'target')}), \
                 patch.object(control.CargoTests, 'run', run), \
                 __import__('contextlib').redirect_stdout(io.StringIO()):
                try:
                    code = control.mutation_suite('probe', rows, root / 'out',
                        lambda _test: (('--lib',), ['cell']), lambda result, _test: result.returncode == 0,
                        lambda _name, result, _test: ('SURVIVED' if fault == 'survived' else 'KILLED',
                                                      'bound result'))
                except KeyboardInterrupt:
                    self.assertEqual(fault, 'interrupt', '#994 only the cancellation probe may interrupt')
                    code = 1
            summary = json.loads((root / 'out/summary.json').read_text())
            self.assertEqual(real.read_text(), 'original', '#994 original worktree never mutated')
            self.assertEqual(copied.read_text(), 'original', '#994 scratch restored after every outcome')
            for label, text in observations:
                self.assertEqual(text, 'changed' if label.endswith('-negative') else 'original',
                                 '#994 positive/restored must see original; negative sees mutant')
            if fault != 'guard':
                self.assertTrue(exited, '#994 source guard must unwind')
            return code, summary, observations

    def test_all_mutations_restore_and_pass(self):
        code, summary, observations = self.exercise()
        self.assertEqual(code, 0, '#994 all killed is success')
        self.assertEqual(summary['counts'], {'KILLED': 2, 'SURVIVED': 0, 'INVALID': 0}, '#994 complete accounting')
        self.assertEqual(len(observations), 6, '#994 positive/negative/restored for each row')

    def test_compile_or_anchor_error_continues_and_never_counts_as_kill(self):
        for fault in ('compile', 'anchor'):
            with self.subTest(fault=fault):
                code, summary, observations = self.exercise(fault)
                self.assertEqual(code, 1, '#994 any invalid fails suite')
                self.assertEqual(summary['counts'], {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 1},
                                 '#994 must run successor after invalid row')
                self.assertIn(('first-0-restored', 'original'), observations, '#994 invalid rows also restore')

    def test_source_guard_refusal_never_launches(self):
        code, summary, observations = self.exercise('guard')
        self.assertEqual(code, 1, '#994 guard refusal is nonzero')
        self.assertEqual(summary['counts']['INVALID'], 2, '#994 guard refusal accounts every row')
        self.assertEqual(observations, [], '#994 no execution when source differs from HEAD')

    def test_interrupt_restores_and_accounts_unfinished_rows(self):
        code, summary, _ = self.exercise('interrupt')
        self.assertEqual(code, 1, '#994 cancellation cannot pass')
        self.assertEqual(summary['counts']['INVALID'], 2, '#994 cancelled/unrun rows are invalid')

    def test_survival_and_restore_failure_are_nonzero(self):
        for fault, expected in [('survived', {'KILLED': 0, 'SURVIVED': 2, 'INVALID': 0}),
                                ('restore', {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 1}),
                                ('guard-exit', {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2})]:
            with self.subTest(fault=fault):
                code, summary, _ = self.exercise(fault)
                self.assertEqual(code, 1, '#994 survival/restore failure must fail')
                self.assertEqual(summary['counts'], expected, '#994 final accounting must remain honest')


class NetworkBindingTests(unittest.TestCase):
    def setUp(self):
        path = Path(__file__).resolve().parent / 'network-timing/negative_controls.py'
        spec = importlib.util.spec_from_file_location('network_negative_994_test', path)
        self.fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.fixture)

    def result(self, test, payload, *, thread=None, extra=''):
        return subprocess.CompletedProcess([], 101,
            f'running 1 test\ntest {test} ... FAILED\n{extra}\n'
            f"thread '{thread or test}' panicked at example.rs:1:1:\n{payload}\n"
            'test result: FAILED. 0 passed; 1 failed;', '')

    def test_exact_and_matrix_bind_named_panic(self):
        cell = 'fetch_redirect/headers/rejected/parent-first'
        test = cell.replace('/', '_').replace('-', '_')
        marker = f'assertion `left == right` failed: network timing cell: {cell} #994 network-timing assertion'
        self.assertEqual(self.fixture.classify('parent-first', self.result(test, marker), test)[0],
                         'KILLED', '#994 exact test assertion kills')
        for result in (self.result(test, '#994 setup failure', extra=marker),
                       self.result(test, marker, thread='other_test')):
            self.assertEqual(self.fixture.classify('parent-first', result, test)[0], 'INVALID',
                             '#994 unrelated network panic/stdout must not kill')
        cell = self.fixture.required_failures['matrix-response']
        test = cell.replace('/', '_').replace('-', '_')
        marker = f'assertion `left == right` failed: network timing cell: {cell} #994 network-timing assertion'
        self.assertEqual(self.fixture.classify('matrix-response', self.result(test, marker), '*')[0],
                         'KILLED', '#994 matrix must hit its named exact cell')
        self.assertEqual(self.fixture.classify('matrix-response', self.result(test, 'wrong cell'), '*')[0],
                         'INVALID', '#994 wrong matrix cell cannot kill')

    def test_zero_tests_are_not_pass_or_survival(self):
        result = subprocess.CompletedProcess([], 0, 'test result: ok. 0 passed; 0 failed;', '')
        self.assertFalse(self.fixture.passed(result, 'cell'), '#994 zero selected tests is invalid')
        self.assertEqual(self.fixture.classify('parent-first', result, 'cell')[0], 'INVALID',
                         '#994 zero-test run cannot survive')


if __name__ == '__main__':
    unittest.main(verbosity=2)
