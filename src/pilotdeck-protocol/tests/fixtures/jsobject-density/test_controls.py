#!/usr/bin/env python3
"""#1128 fast guard tests; --negative runs all in-memory Python mutants.

Git tests create only a few synthetic text inputs, never a product checkout
or a second Cargo target. Python mutants are compiled/executed in memory;
no product source is mutated. Every kill binds a full unittest name and its
single-line assertion message. Unexpected exceptions are INVALID.
"""
from contextlib import contextmanager, redirect_stdout
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
fixture = Path(__file__).resolve().parent
root = fixture.parents[4]
sys.path.insert(0, str(fixture))
import tree_guard
import measure
import negative_controls


class GuardTests(unittest.TestCase):
    def setUp(self):
        (root / '_tmp').mkdir(exist_ok=True)
        self.scratch = tempfile.TemporaryDirectory(prefix='1128-guard-', dir=root / '_tmp')
        self.addCleanup(self.scratch.cleanup)
        self.repo = Path(self.scratch.name)
        self.git('init', '-q')
        self.lib = self.repo / tree_guard.LIB_RS
        self.lib.parent.mkdir(parents=True)
        self.lib.write_text('synthetic lib input\n')
        (self.repo / 'other.txt').write_text('synthetic other input\n')
        (self.repo / '.gitignore').write_text('ignored-output/\n')
        self.git('add', '.')
        self.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                 '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false',
                 'commit', '-qm', 'synthetic guard input')
        tree_guard.tree_matches_head(self.repo)

    def git(self, *args):
        return subprocess.run(['git', '-C', str(self.repo), *args], check=True,
                              capture_output=True, text=True)

    def test_hidden_flags(self):
        for flag in ('skip-worktree', 'assume-unchanged'):
            with self.subTest(flag=flag):
                self.git('update-index', '--' + flag, tree_guard.LIB_RS)
                self.lib.write_text('hidden changed input\n')
                try:
                    with self.assertRaises(RuntimeError, msg='#1128 hidden index flag rejected'):
                        tree_guard.tree_matches_head(self.repo, allow=(tree_guard.LIB_RS,))
                    print(f'#1128 four-state {flag}: rejected')
                finally:
                    self.lib.write_text('synthetic lib input\n')
                    self.git('update-index', '--no-' + flag, tree_guard.LIB_RS)
                tree_guard.tree_matches_head(self.repo)

    def test_untracked_inputs(self):
        paths = ('src/pilotdeck-protocol/src/injected.rs', 'src/pilotdeck-protocol/build.rs',
                 'src/pilotdeck-cli/src/injected.rs', 'src/Cargo.toml', 'src/Cargo.lock',
                 '.cargo/config.toml', 'src/.cargo/config.toml',
                 'rust-toolchain', 'rust-toolchain.toml')
        for name in paths:
            with self.subTest(path=name):
                path = self.repo / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('untracked build input\n')
                try:
                    with self.assertRaises(RuntimeError, msg='#1128 untracked build input rejected'):
                        tree_guard.tree_matches_head(self.repo)
                    print(f'#1128 untracked {name}: rejected')
                finally:
                    path.unlink()
                    if path.parent.name == '.cargo':
                        path.parent.rmdir()
                tree_guard.tree_matches_head(self.repo)

    def test_config_directories(self):
        for name in ('.cargo', 'src/.cargo'):
            with self.subTest(path=name):
                path = self.repo / name
                path.mkdir(parents=True)
                try:
                    # Empty dirs are invisible to ls-files; ignored config is too.
                    for ignored in (False, True):
                        if ignored:
                            (path / '.gitignore').write_text('*\n')
                            (path / 'config.toml').write_text('[build]\n')
                        with self.assertRaises(RuntimeError, msg='#1128 config directory rejected'):
                            tree_guard.tree_matches_head(self.repo)
                finally:
                    for child in path.iterdir():
                        child.unlink()
                    path.rmdir()
                tree_guard.tree_matches_head(self.repo)

    def test_tracked_and_allow(self):
        with self.assertRaises(ValueError, msg='#1128 allow restricted to lib.rs'):
            tree_guard.tree_matches_head(self.repo, allow=('other.txt',))
        self.lib.write_text('intentional pre window\n')
        with self.assertRaises(RuntimeError, msg='#1128 unallowed worktree drift rejected'):
            tree_guard.tree_matches_head(self.repo)
        tree_guard.tree_matches_head(self.repo, allow=(tree_guard.LIB_RS,))
        other = self.repo / 'other.txt'
        other.write_text('unrelated drift\n')
        with self.assertRaises(RuntimeError, msg='#1128 allow does not hide sibling drift'):
            tree_guard.tree_matches_head(self.repo, allow=(tree_guard.LIB_RS,))
        other.write_text('synthetic other input\n')
        self.git('add', tree_guard.LIB_RS)
        with self.assertRaises(RuntimeError, msg='#1128 allow does not hide staged drift'):
            tree_guard.tree_matches_head(self.repo, allow=(tree_guard.LIB_RS,))

    def test_git_errors(self):
        real_run = subprocess.run
        for failing in ('staged', 'unstaged', 'flags', 'untracked'):
            def fail(command, **kwargs):
                selected = {'staged': '--cached' in command,
                            'unstaged': '--quiet' in command and '--cached' not in command,
                            'flags': '-v' in command, 'untracked': '--others' in command}
                if selected[failing]:
                    if kwargs.get('check'):
                        raise subprocess.CalledProcessError(128, command)
                    return subprocess.CompletedProcess(command, 128, '', 'injected git error')
                return real_run(command, **kwargs)
            with self.subTest(command=failing), patch.object(tree_guard.subprocess, 'run', fail):
                with self.assertRaises((RuntimeError, subprocess.CalledProcessError),
                                       msg='#1128 git failure rejected'):
                    tree_guard.tree_matches_head(self.repo)

    def test_measure_uses_guard(self):
        for flag in ('skip-worktree', 'assume-unchanged'):
            self.git('update-index', '--' + flag, tree_guard.LIB_RS)
            self.lib.write_text('hidden measurement input\n')
            try:
                with patch.object(measure, 'root', self.repo):
                    for stage, allow in (('measurement startup', ()), ('post build', ()),
                                         ('pre build', (tree_guard.LIB_RS,)), ('restored build', ())):
                        with self.subTest(flag=flag, stage=stage):
                            with self.assertRaises(SystemExit, msg='#1128 measurement uses HEAD guard'):
                                measure.require_clean_tree(stage, allow=allow)
            finally:
                self.lib.write_text('synthetic lib input\n')
                self.git('update-index', '--no-' + flag, tree_guard.LIB_RS)
        # Exercise the actual startup call, stopping before any build/Node run.
        with patch.object(measure, 'new_output_directory', return_value=self.repo), \
                patch.object(measure.subprocess, 'run', return_value=types.SimpleNamespace(stdout='v22.22.0')), \
                patch.object(measure, 'require_clean_tree', side_effect=SystemExit('startup guard')) as guard, \
                patch.dict(os.environ, {'CARGO_TARGET_DIR': 'unused'}), \
                patch.object(sys, 'argv', ['measure.py', 'unused', 'baseline']):
            with self.assertRaises(SystemExit, msg='#1128 measurement startup uses HEAD guard'):
                measure.main()
            self.assertTrue(guard.call_args.args == ('measurement startup',),
                            '#1128 measurement startup uses HEAD guard')


class DumpTests(unittest.TestCase):
    def test_stack_error(self):
        error = 'RangeError: Maximum call stack size exceeded'
        for errors in ([None, error], [error, None], [error, error]):
            self.assertTrue(measure.missing_ts_dump_error('deep', errors, 'deep-1') == error,
                            '#1128 exact stack overflow accepted')
        for errors in ([None, None], ['RangeError: Invalid string length', None],
                       ['TypeError: Maximum call stack size exceeded', None],
                       [error, 'RangeError: Invalid string length']):
            with self.assertRaises(SystemExit, msg='#1128 unrelated stringify error rejected'):
                measure.missing_ts_dump_error('deep', errors, 'deep-1')
        with self.assertRaises(SystemExit, msg='#1128 stack exemption is deep only'):
            measure.missing_ts_dump_error('small', [error, None], 'small-1')


class RunnerTests(unittest.TestCase):
    def test_restored_lock_and_guard(self):
        # Run the real row loop with a synthetic mutation and fake cargo. Its
        # actual shared tree_lock is probed from another process, no waits.
        (root / '_tmp').mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='1128-runner-', dir=root / '_tmp') as directory:
            repo = Path(directory)
            source = repo / 'synthetic.rs'
            source.write_text('before')
            row = negative_controls.Mutation('sample', source, 'unused', 'unused',
                                             'before', 'after', 'bound', '#1128 synthetic bound')
            events = []
            lock_results = []

            @contextmanager
            def window(_root, _paths):
                with measure.tree_lock(repo, timeout=1):
                    yield {source: b'before'}
                    source.write_text('before')

            def guard(_root):
                events.append('guard')

            def cargo(command, **kwargs):
                phase = 'compile' if '--no-run' in command else (
                    'negative' if source.read_text() == 'after' else
                    'positive' if 'positive' not in events else 'restored')
                if phase == 'restored':
                    probe = subprocess.run([sys.executable, '-I', '-c',
                        'import fcntl, sys\nf = open(sys.argv[1], "a")\n'
                        'try: fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\n'
                        'except BlockingIOError: sys.exit(23)\n', str(repo / '.negative-source.lock')],
                        capture_output=True, timeout=5)
                    lock_results.append(probe.returncode)
                    print(f'#1128 restored cargo: competing flock exit={probe.returncode}')
                events.append(phase)
                failed = phase == 'negative'
                output = ("test bound ... FAILED\nthread 'bound' panicked at synthetic.rs:1:1:\n"
                          "#1128 synthetic bound\n\ntest result: FAILED. 0 passed; 1 failed;\n" if failed else
                          'test bound ... ok\ntest result: ok. 1 passed; 0 failed;\n')
                return subprocess.CompletedProcess(command, 101 if failed else 0, output, '')

            with patch.object(negative_controls, 'root', repo), \
                    patch.object(negative_controls, 'mutations', [row]), \
                    patch.object(negative_controls, 'check'), \
                    patch.object(negative_controls, 'crate_files', return_value=[]), \
                    patch.object(negative_controls, 'mutating_sources', window), \
                    patch.object(negative_controls, 'tree_matches_head', guard), \
                    patch.object(negative_controls, 'wait_for_slot',
                                 return_value=types.SimpleNamespace(stdout='', stderr='')), \
                    patch.object(negative_controls, 'run_owned', cargo), \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': 'unused'}), \
                    patch.object(sys, 'argv', ['negative_controls.py', str(repo / 'out')]):
                self.assertTrue(negative_controls.main() == 0, '#1128 synthetic runner completes')
            self.assertTrue(events == ['guard', 'positive', 'compile', 'negative', 'guard', 'restored'],
                            '#1128 restored cargo rechecks HEAD')
            self.assertTrue(lock_results == [23], '#1128 restored cargo holds tree lock')


# These are Python-only value/branch mutations, compiled in memory. No
# shared executor changes, index changes in the product tree, or cargo runs.
MUTANTS = [
    ('skip-worktree', tree_guard, "or line[:1] == 'S'", 'or False',
     GuardTests, 'test_hidden_flags', '#1128 hidden index flag rejected'),
    ('assume-unchanged', tree_guard, 'line[:1].islower()', 'False',
     GuardTests, 'test_hidden_flags', '#1128 hidden index flag rejected'),
    ('untracked-inputs', tree_guard, 'if untracked:', 'if False:',
     GuardTests, 'test_untracked_inputs', '#1128 untracked build input rejected'),
    ('config-directories', tree_guard, 'if config.exists():', 'if False:',
     GuardTests, 'test_config_directories', '#1128 config directory rejected'),
    ('staged-diff', tree_guard, 'if staged.returncode != 0', 'if False',
     GuardTests, 'test_tracked_and_allow', '#1128 allow does not hide staged drift'),
    ('worktree-diff', tree_guard, 'unstaged.returncode != 0', 'False',
     GuardTests, 'test_tracked_and_allow', '#1128 unallowed worktree drift rejected'),
    ('allow-boundary', tree_guard, 'if any(path != LIB_RS for path in allow):', 'if False:',
     GuardTests, 'test_tracked_and_allow', '#1128 allow restricted to lib.rs'),
    ('diff-error', tree_guard, 'staged.returncode != 0 or unstaged.returncode != 0',
     'staged.returncode == 1 or unstaged.returncode == 1',
     GuardTests, 'test_git_errors', '#1128 git failure rejected'),
    ('measure-guard', measure, 'tree_matches_head(root, allow=allow)', 'None',
     GuardTests, 'test_measure_uses_guard', '#1128 measurement uses HEAD guard'),
    ('measure-startup', measure, "    require_clean_tree('measurement startup')", '    pass',
     GuardTests, 'test_measure_uses_guard', '#1128 measurement startup uses HEAD guard'),
    ('rangeerror-class', measure, 'error == expected', "'RangeError' in error",
     DumpTests, 'test_stack_error', '#1128 unrelated stringify error rejected'),
    ('deep-only', measure, "kind == 'deep'", 'True',
     DumpTests, 'test_stack_error', '#1128 stack exemption is deep only'),
    ('error-required', measure, 'recorded and all(', 'all(',
     DumpTests, 'test_stack_error', '#1128 unrelated stringify error rejected'),
    ('mixed-errors', measure, 'all(error == expected for error in recorded)',
     'any(error == expected for error in recorded)',
     DumpTests, 'test_stack_error', '#1128 unrelated stringify error rejected'),
    ('restored-guard', negative_controls,
     '                tree_matches_head(root)\n                if not compile_only',
     '                if not compile_only', RunnerTests, 'test_restored_lock_and_guard',
     '#1128 restored cargo rechecks HEAD'),
]


def bound_run(cls, method, log):
    with redirect_stdout(log):
        return unittest.TextTestRunner(stream=log, verbosity=2).run(cls(method))


def run_negative():
    counts = dict(KILLED=0, SURVIVED=0, INVALID=0)
    for name, module, old, new, cls, method, marker in MUTANTS:
        status, reason = 'INVALID', 'not run'
        log = io.StringIO()
        try:
            source = Path(module.__file__).read_text()
            if source.count(old) != 1:
                raise ValueError('stale Python mutation anchor')
            if not bound_run(cls, method, log).wasSuccessful():
                raise ValueError('positive bound test failed')
            candidate = types.ModuleType(module.__name__)
            candidate.__file__ = module.__file__
            exec(compile(source.replace(old, new, 1), module.__file__, 'exec'), candidate.__dict__)
            with patch.dict(globals(), {module.__name__: candidate}):
                result = bound_run(cls, method, log)
            failures = result.failures
            if result.wasSuccessful():
                status, reason = 'SURVIVED', 'bound test passed'
            elif not result.errors and failures and all(
                    getattr(test, 'test_case', test).id() == f'{cls.__module__}.{cls.__name__}.{method}' and
                    any(line.startswith('AssertionError:') and marker in line
                        for line in trace.splitlines()) for test, trace in failures):
                status, reason = 'KILLED', marker
            else:
                reason = 'missing bound assertion (unexpected error or different failure)'
            if not bound_run(cls, method, log).wasSuccessful():
                status, reason = 'INVALID', 'restored bound test failed'
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        counts[status] += 1
        print(log.getvalue(), end='')
        print(f'{name}: {status}: {cls.__module__}.{cls.__name__}.{method}: {reason}', flush=True)
    print(f'#1128 Python negative controls: {counts}', flush=True)
    return int(counts['SURVIVED'] != 0 or counts['INVALID'] != 0)


if __name__ == '__main__':
    if sys.argv[1:] == ['--negative']:
        raise SystemExit(run_negative())
    unittest.main(verbosity=2)
