# anchor-selfcheck: unsupported subprocess isolation and candidate-baseline regression tests
"""#994: real HEAD-guarded Python probes; no synthetic Git or Cargo workspace."""
import importlib.util
import fcntl
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


def hold_source_lock(root, ready):
    from negative_process import tree_lock
    with tree_lock(root, timeout=1):
        pending = ready.with_suffix('.tmp')
        pending.write_text('locked')
        pending.replace(ready)
        while True:
            signal.pause()


def source_lock_is_busy(root):
    with (root / '.negative-source.lock').open('a') as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return 0
    return 10


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
                    if break_restore and path == copied and data == contents[case[1]]:
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

    def test_copied_children_ignore_caller_python_paths(self):
        runner = self.runner
        foreign = self.out / 'foreign-python-path'
        foreign.mkdir()
        marker = self.out / 'foreign-imported'
        (foreign / 'sitecustomize.py').write_text(
            'from pathlib import Path\n'
            f'marker = Path({str(marker)!r})\n'
            "pending = marker.with_suffix('.tmp')\n"
            "pending.write_text('caller PYTHONPATH reached copied interpreter')\n"
            'pending.replace(marker)\n')
        environments = []
        with runner.verified_inputs() as contents:
            with runner.isolated_inputs(runner.root, contents, self.out) as directory:
                api = runner._process_api.get()
                run_owned = api.run_owned
                def observe(*args, **kwargs):
                    environments.append(dict(kwargs['env']))
                    return run_owned(*args, **kwargs)
                with patch.dict(os.environ, {'PYTHONPATH': str(foreign), 'PYTHONHOME': sys.base_prefix}), \
                        patch.object(api, 'run_owned', side_effect=observe):
                    row = runner.run_case(runner.cases[0], directory, self.out, contents[runner.shared])
        self.assertEqual(len(environments), 4, '#994 inspect positive compile negative restored child phases')
        self.assertFalse(marker.exists(), '#994 copied children must reject caller PYTHONPATH')
        self.assertTrue(all('PYTHONPATH' not in env for env in environments),
                        '#994 all copied child phases must clear PYTHONPATH')
        self.assertTrue(all('PYTHONHOME' not in env for env in environments),
                        '#994 all copied child phases must clear PYTHONHOME')
        self.assertEqual(row['status'], 'KILLED', '#994 isolated child still kills bound assertion')
        self.assertTrue(row['compiled'], '#994 isolated child mutant compiles')
        self.assertTrue(row['restoredPass'], '#994 isolated child restoration passes')
        self.assert_pristine()

    def test_new_shared_imports_are_copied_and_executable(self):
        runner = self.runner
        dependency = HERE / 'pending_reanchor.py'
        initializer = HERE / '_shared/pending_support/__init__.py'
        nested = HERE / '_shared/pending_support/value.py'
        injected = {
            dependency: b'def read():\n    from _shared.pending_support import VALUE\n    return VALUE\nassert read() == 994\n',
            initializer: b'from .value import VALUE\n',
            nested: b'VALUE = 994\n',
            runner.shared: self.before[runner.shared] + b'\nimport pending_reanchor\n',
            # The bootstrap itself may gain the same dependency in a later PR.
            runner.process: self.before[runner.process] + b'\nimport pending_reanchor\n',
        }
        git, read, is_file = runner.git_bytes, Path.read_bytes, Path.is_file
        def git_with_dependency(*args):
            if args[0] == 'ls-tree':
                return git(*args) + b''.join(
                    str(path.relative_to(runner.root)).encode() + b'\0' for path in (dependency, initializer, nested))
            if args[0] == 'show':
                path = runner.root / args[1].split(':', 1)[1]
                if path in injected:
                    return injected[path]
            return git(*args)
        with patch.object(runner, 'git_bytes', side_effect=git_with_dependency), \
                patch.object(Path, 'read_bytes', lambda path: injected[path] if path in injected else read(path)), \
                patch.object(Path, 'is_file', lambda path: path in injected or is_file(path)):
            commit = runner.git_bytes('rev-parse', 'HEAD').decode().strip()
            self.assertTrue(set(injected).issubset(runner.input_paths(commit)),
                            '#994 import closure must include transitive relative and function imports')
            with runner.verified_inputs() as contents:
                with runner.isolated_inputs(runner.root, contents, self.out) as directory:
                    self.assertTrue((directory / dependency.relative_to(runner.root)).is_file(),
                                    '#994 new shared dependency must be copied')
                    row = runner.run_case(runner.cases[0], directory, self.out, contents[runner.shared])
        self.assertEqual(row['status'], 'KILLED', '#994 added import must execute in positive and mutant')
        self.assertTrue(row['restoredPass'], '#994 added import must survive restoration')
        self.assert_pristine()

    def test_head_change_invalidates_copy_without_rewriting_inputs(self):
        changed = False
        git = self.runner.git_bytes
        def moving_head(*args):
            if changed and args == ('rev-parse', 'HEAD'):
                return b'changed-head\n'
            return git(*args)
        with patch.object(self.runner, 'git_bytes', side_effect=moving_head), \
                self.assertRaisesRegex(self.runner.SourceBaselineError, 'HEAD changed',
                                       msg='#994 HEAD movement must invalidate isolated controls'):
            with self.runner.verified_inputs():
                changed = True
        self.assert_pristine()

    def test_bootstrap_rejects_dirty_code_before_execution_and_ignores_cached_module(self):
        runner = self.runner
        from types import ModuleType
        poisoned = ModuleType('negative_process')
        def forbidden(*_args, **_kwargs):
            self.fail('#994 bootstrap must not execute cached unverified helpers')
        poisoned.mutating_sources = poisoned._head_bytes = poisoned.run_owned = forbidden
        with patch.dict(sys.modules, {'negative_process': poisoned}):
            with runner.verified_inputs() as contents:
                self.assertEqual(contents[runner.process], self.before[runner.process],
                                 '#994 bootstrap must read independent HEAD bytes')
        read = Path.read_bytes
        with patch.object(Path, 'read_bytes', lambda path:
                          b'raise AssertionError("unverified module executed")\n'
                          if path == runner.process else read(path)), \
                self.assertRaisesRegex(runner.SourceBaselineError, 'differs from HEAD',
                                       msg='#994 dirty bootstrap must be refused before import'):
            with runner.verified_inputs():
                self.fail('#994 dirty bootstrap must never yield')

    def test_empty_controls_fail_closed(self):
        output = self.out / 'empty'
        with patch.object(self.runner, 'cases', []), \
                patch.object(sys, 'argv', ['runner', str(output)]), \
                patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.out / 'unused')}):
            code = self.runner.main()
        self.assertEqual(code, 1, '#994 empty controls must fail closed')
        self.assertEqual(json.loads((output / 'summary.json').read_text())['controls'], [],
                         '#994 empty controls must remain explicit in summary')

    def test_bootstrap_refuses_unverified_dependencies_even_if_cached(self):
        from types import ModuleType
        for name in ('_shared.untracked_helper', 'untracked_helper'):
            cached = ModuleType(name)
            cached.MARKER = 'unverified'
            contents = {self.runner.process: f'from {name} import MARKER\n'.encode()}
            with self.subTest(name=name), patch.dict(sys.modules, {name: cached}), \
                    self.assertRaisesRegex(ImportError, '#994 unverified shared import',
                                           msg='#994 cached unverified dependency must be refused'):
                with self.runner.trusted_process(contents):
                    self.fail('#994 unverified dependency must never load')

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
                        self.runner.SourceBaselineError, 'differs from HEAD', msg='#994 dirty shared input must fail closed'):
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
        self.assertEqual({key: summary[key] for key in ('commit', 'dirty', 'candidateDirty')},
                         self.fixture.candidate_identity(), '#994 summary must identify candidate bytes')

    def test_identity_distinguishes_clean_and_dirty_candidates(self):
        for dirty in ('', ' M src/pilotdeck-gateway/tests/network_timing.rs\n'):
            with self.subTest(dirty=dirty), patch.object(self.fixture.subprocess, 'check_output') as git:
                git.side_effect = [dirty.encode(), b'0123456789abcdef\n']
                identity = self.fixture.candidate_identity()
            self.assertEqual(identity, dict(commit='0123456789abcdef', dirty=dirty, candidateDirty=bool(dirty)),
                             '#994 provenance must distinguish dirty candidates')
            self.assertEqual(git.call_args_list[0].args[0][-5:],
                             ['status', '--porcelain', '--untracked-files=all', '--', 'src'],
                             '#994 candidate provenance must include all src changes')

    def test_candidate_environment_clears_oracles_and_wrapper(self):
        oracles = ('PDRS_RECORDER_ORACLE', 'PDRS_SESSION_DOMAIN_ORACLE', 'PDRS_TRANSCRIPT_ADAPTER_ORACLE')
        contaminated = {key: 'injected-oracle' for key in oracles}
        contaminated.update(CARGO_TARGET_DIR=str(self.out / 'target'), RUSTC_WRAPPER='foreign-wrapper',
                            CARGO_INCREMENTAL='1', TMPDIR='foreign-tmp')
        output = self.out / 'environment'
        with patch.dict(os.environ, contaminated), patch.object(self.fixture, 'CargoTests') as constructor:
            self.fixture.baseline_only([], output)
        env = constructor.call_args.args[2]
        for key in oracles:
            self.assertNotIn(key, env, '#994 candidate must clear oracle injection')
        self.assertEqual((env['RUSTC_WRAPPER'], env['CARGO_INCREMENTAL'], env['TMPDIR']),
                         ('', '0', str(output.resolve())), '#994 candidate must use normalized build environment')

    def test_candidate_holds_source_lock_during_cargo(self):
        # A private lock path prevents an unrelated suite holding the real tree
        # lock from making a missing-lock mutant look correctly locked.
        self.enterContext(patch.object(self.fixture, 'root', self.out))
        self.enterContext(patch.object(self.fixture, 'candidate_identity',
                                      return_value=dict(commit='lock-probe', dirty='', candidateDirty=False)))
        def run(*_args, **_kwargs):
            result = subprocess.run([sys.executable, '-B', str(Path(__file__).resolve()),
                                     '--check-lock', str(self.fixture.root)],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, '#994 candidate cargo must hold source lock')
            test = self.fixture.cases[0][-1][0]
            return subprocess.CompletedProcess([], 0,
                f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;', '')
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.out / 'target')}), \
                patch.object(self.fixture, 'CargoTests') as constructor:
            constructor.return_value.run.side_effect = run
            code = self.fixture.baseline_only([self.fixture.cases[0]], self.out / 'locked')
        self.assertEqual(code, 0, '#994 locked candidate must pass its bound test')

    def test_never_finishing_lock_holder_times_out_and_cancelled_holder_releases(self):
        self.enterContext(patch.object(self.fixture, 'root', self.out))
        self.enterContext(patch.object(self.fixture, 'candidate_identity',
                                      return_value=dict(commit='lock-probe', dirty='', candidateDirty=False)))
        ready = self.out / 'ready'
        child = subprocess.Popen([sys.executable, '-B', str(Path(__file__).resolve()), '--hold-lock',
                                  str(self.fixture.root), str(ready)],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                 start_new_session=True)
        try:
            deadline = time.monotonic() + 10
            while not ready.exists() and child.poll() is None and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(ready.exists(), '#994 lock predecessor must publish readiness')
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.out / 'target')}), \
                    patch.object(self.fixture, 'BASELINE_LOCK_SECONDS', .1), \
                    patch.object(self.fixture, 'CargoTests') as constructor:
                with self.assertRaisesRegex(TimeoutError, 'source lock busy',
                                            msg='#994 never finishing predecessor must time out'):
                    self.fixture.baseline_only([], self.out / 'busy')
                constructor.assert_not_called()
                child.terminate()
                child.communicate(timeout=5)
                self.assertEqual(child.returncode, -signal.SIGTERM, '#994 cancel lock predecessor')
                self.assertEqual(self.fixture.baseline_only([], self.out / 'released'), 1,
                                 '#994 empty candidate remains nonpassing after cancellation')
                constructor.assert_called_once()
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
            child.communicate(timeout=5)

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
    elif len(sys.argv) == 4 and sys.argv[1] == '--hold-lock':
        hold_source_lock(Path(sys.argv[2]), Path(sys.argv[3]))
    elif len(sys.argv) == 3 and sys.argv[1] == '--check-lock':
        raise SystemExit(source_lock_is_busy(Path(sys.argv[2])))
    else:
        unittest.main(verbosity=2)
