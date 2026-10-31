# anchor-selfcheck: unsupported synthetic Git and binary snapshot contract tests
"""#1166 uses tiny synthetic repositories, never copies product/build state.

These repos are the input under test (including dirty index flags), not an
alternate HEAD for executing product negative controls. No Cargo is launched.
"""
from contextlib import contextmanager
import ast
import contextlib
import io
import runpy
import hashlib
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

FIXTURES = Path(__file__).resolve().parents[1]
ROOT = FIXTURES.parents[3]
sys.path.insert(0, str(FIXTURES))
import negative_process as api


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git(root, *args):
    return subprocess.run(['git', '-C', str(root), *args], check=True,
                          capture_output=True, timeout=10)


INPUTS = (
    'src/pilotdeck-agent/src/lib.rs', 'src/pilotdeck-cli/src/main.rs',
    'src/pilotdeck-gateway/src/lib.rs', 'src/pilotdeck-protocol/tests/density.rs',
    'src/pilotdeck-tokenizer/src/lib.rs', 'src/pilotdeck-transport/build.rs',
    'src/pilotdeck-yaml/tests/fixture.json', 'src/pilotdeck-child-signals/Cargo.toml',
    'src/vendor/example/src/lib.rs', 'src/data/example/src/lib.rs',
    'src/Cargo.toml', 'src/Cargo.lock', '.cargo/config.toml', 'src/.cargo/config',
    'rust-toolchain', 'rust-toolchain.toml', 'src/rust-toolchain.toml',
    'src/pilotdeck-cli/tests/space and\nnewline.json',
    'src/pilotdeck-agent/target/tracked.rs', 'src/pilotdeck-agent/tests/tracked.pyc',
)


class WorkspacePristine(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='pdrs1166-unit-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        git(self.root, 'init', '-q')
        git(self.root, 'config', 'user.name', 'synthetic fixture')
        git(self.root, 'config', 'user.email', 'fixture@example.invalid')
        for name in INPUTS:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('// fixture HEAD\n')
        (self.root / '.gitignore').write_text('*.zip\n__pycache__/\n*.pyc\ntarget/\n')
        git(self.root, 'add', '-f', '.')
        git(self.root, 'commit', '-qm', 'synthetic fixture, no product source')

    def test_clean_and_all_input_categories(self):
        with api.mutating_sources(self.root, []):
            pass
        for name in INPUTS:
            with self.subTest(path=name):
                path = self.root / name
                with api.mutating_sources(self.root, [path]):
                    path.write_text('// residue\n')
                    with self.assertRaises(api.SourceBaselineError,
                                           msg='#1166 all workspace inputs reject residue'):
                        with api.mutating_sources(self.root, []):
                            pass
                    self.assertTrue(path.read_text() == '// residue\n',
                                    '#1166 rejected residue must remain untouched')

    def test_staged_cancellation_is_rejected(self):
        path = self.root / INPUTS[0]
        with api.mutating_sources(self.root, [path]) as original:
            path.write_text('// staged mutant\n')
            git(self.root, 'add', str(path))
            path.write_bytes(original[path])
            try:
                with self.assertRaises(api.SourceBaselineError,
                                       msg='#1166 staged drift cannot hide behind HEAD worktree'):
                    api._assert_product_pristine(self.root, 'HEAD')
            finally:
                git(self.root, 'reset', '-q', 'HEAD', '--', str(path))

    def test_staged_deletion_is_rejected(self):
        path = INPUTS[3]
        with api.mutating_sources(self.root, []):
            git(self.root, 'rm', path)
            try:
                with self.assertRaises(api.SourceBaselineError,
                                       msg='#1166 staged deletion must remain guarded'):
                    api._assert_product_pristine(self.root, 'HEAD')
            finally:
                git(self.root, 'restore', '--source=HEAD', '--staged', '--worktree', '--', path)

    def test_hidden_index_flags_are_rejected(self):
        path = self.root / INPUTS[1]
        for flag in ('assume-unchanged', 'skip-worktree'):
            with self.subTest(flag=flag), api.mutating_sources(self.root, [path]):
                git(self.root, 'update-index', '--' + flag, '--', str(path))
                path.write_text('// hidden residue\n')
                try:
                    with self.assertRaises(api.SourceBaselineError,
                                           msg='#1166 hidden index flags must fail closed'):
                        api._assert_product_pristine(self.root, 'HEAD')
                finally:
                    git(self.root, 'update-index', '--no-' + flag, '--', str(path))

    def test_untracked_and_ignored_inputs_are_rejected(self):
        paths = ('src/new-crate/src/lib.rs', 'src/pilotdeck-cli/tests/new.rs',
                 'src/pilotdeck-cli/Cargo.toml', 'src/pilotdeck-cli/build.rs',
                 'src/pilotdeck-cli/tests/ignored.zip', '.cargo/new-config',
                 'src/.cargo/new-config', 'src/rust-toolchain', 'rust-toolchain.local')
        for name in paths:
            with self.subTest(path=name), api.mutating_sources(self.root, []):
                path = self.root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('untracked input\n')
                try:
                    with self.assertRaises(api.SourceBaselineError,
                                           msg='#1166 ignored and untracked inputs must fail closed'):
                        api._assert_product_pristine(self.root, 'HEAD')
                finally:
                    path.unlink()

    def test_only_untracked_caches_are_excluded(self):
        for name in ('src/target/debug/cache', 'src/pilotdeck-cli/target/debug/cache',
                     'src/pilotdeck-cli/tests/__pycache__/fixture.pyc'):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('cache')
        api._assert_product_pristine(self.root, 'HEAD')

    def test_git_failure_is_not_clean(self):
        with patch.object(api, '_git', side_effect=RuntimeError('synthetic git error')):
            with self.assertRaises(RuntimeError, msg='#1166 git error must fail closed'):
                api._assert_product_pristine(self.root, 'HEAD')

    def test_sigkill_predecessor_other_crate_is_rejected(self):
        helper = str(FIXTURES)
        target = self.root / INPUTS[3]
        control = self.root / 'control'
        control.mkdir()
        holder = r'''
from pathlib import Path
import sys, time
sys.path.insert(0, sys.argv[1])
from negative_process import mutating_sources
root, target, control = map(Path, sys.argv[2:])
with mutating_sources(root, [target]):
    (control / 'held.tmp').write_text('held')
    (control / 'held.tmp').replace(control / 'held')
    deadline = time.monotonic() + 10
    while not (control / 'mutate').exists():
        if time.monotonic() >= deadline: raise TimeoutError('parent absent')
        time.sleep(.005)
    target.write_text('// killed residue\n')
    (control / 'mutated.tmp').write_text('mutated')
    (control / 'mutated.tmp').replace(control / 'mutated')
    time.sleep(15)
'''
        waiter = r'''
from pathlib import Path
import sys
sys.path.insert(0, sys.argv[1])
from negative_process import mutating_sources, SourceBaselineError
root, control = map(Path, sys.argv[2:])
(control / 'waiting.tmp').write_text('waiting')
(control / 'waiting.tmp').replace(control / 'waiting')
try:
    with mutating_sources(root, [], lock_timeout=10): print('ACCEPTED')
except SourceBaselineError: print('REFUSED')
'''
        def wait(name):
            end = time.monotonic() + 5
            while not (control / name).exists():
                self.assertTrue(time.monotonic() < end, '#1166 handshake has a deadline')
                time.sleep(.005)
        children = []
        try:
            children.append(subprocess.Popen([sys.executable, '-B', '-c', holder, helper,
                                              str(self.root), str(target), str(control)],
                                             stdout=subprocess.PIPE, stderr=subprocess.PIPE))
            wait('held')
            children.append(subprocess.Popen([sys.executable, '-B', '-c', waiter, helper,
                                              str(self.root), str(control)],
                                             stdout=subprocess.PIPE, stderr=subprocess.PIPE))
            wait('waiting')
            (control / 'mutate.tmp').write_text('mutate')
            (control / 'mutate.tmp').replace(control / 'mutate')
            wait('mutated')
            children[0].kill()
            children[0].communicate(timeout=5)
            stdout, stderr = children[1].communicate(timeout=12)
            self.assertTrue(children[1].returncode == 0 and stdout.strip() == b'REFUSED',
                            '#1166 next holder must reject killed other-crate residue')
            self.assertTrue(target.read_text() == '// killed residue\n',
                            '#1166 killed residue belongs to its owner')
        finally:
            for child in children:
                if child.poll() is None:
                    child.kill()
                child.communicate(timeout=5)


class SnapshotControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='pdrs1166-binary-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / 'cargo-artifact'
        self.source.write_text('#!/bin/sh\nprintf original')
        self.source.chmod(0o755)
        self.events = []
        self.env = {'CARGO_TARGET_DIR': str(self.root / 'target')}

    @contextmanager
    def dependencies(self, *, failed=False, missing=False, distractor=False, kind='test'):
        @contextmanager
        def lock(*args, **kwargs):
            self.events.append('lock')
            try:
                yield
            finally:
                self.events.append('unlock')
        def clean(*args):
            self.events.append('clean')
        def slot(*args, **kwargs):
            self.events.append('slot')
            return subprocess.CompletedProcess([], 0, 'slot\n', '')
        def build(command, **kwargs):
            self.events.append('build')
            self.assertTrue(('--no-run' in command) == (kind == 'test')
                            and '--message-format=json' in command,
                            '#1166 measurement test build must not execute tests')
            self.assertTrue(kwargs['env'] == self.env, '#1166 caller target environment survives')
            self.assertTrue(kwargs['timeout'] == 1800 and kwargs['idle_timeout'] == 600,
                            '#1166 cargo build must retain bounded progress deadlines')
            self.assertTrue(command[1] == ('test' if kind == 'test' else 'build'),
                            '#1166 select cargo command for target kind')
            row = dict(reason='compiler-artifact', target={'name': 'probe', 'kind': [kind]},
                       executable=str(self.source))
            rows = [row]
            if distractor:
                rows.append(dict(row, target={'name': 'probe', 'kind': ['lib']}, executable='wrong-kind'))
            return subprocess.CompletedProcess(command, int(failed),
                                               '' if missing else '\n'.join(map(json.dumps, rows)), 'build stderr')
        actual_copy = api.copy_binary
        def copy(*args, **kwargs):
            self.events.append('copy')
            return actual_copy(*args, **kwargs)
        with patch.object(api, 'tree_lock', lock), patch.object(api, '_assert_product_pristine', clean), \
                patch.object(api, 'wait_for_slot', slot), patch.object(api, 'run_owned', build), \
                patch.object(api, 'copy_binary', copy):
            yield

    def build(self, name='run'):
        return api.build_snapshot(self.root, self.root / name, package='fixture',
                                  target='probe', env=self.env)

    def test_build_copy_and_slot_are_inside_lock(self):
        with self.dependencies():
            binary = self.build()
        self.assertTrue(self.events == ['lock', 'clean', 'slot', 'build', 'copy', 'unlock'],
                        '#1166 clean slot build copy must stay inside tree lease')
        self.assertTrue(Path(binary).parent == self.root / 'run', '#1166 return private executable')
        # Replace the shared cargo name, then run twice: neither launch may
        # follow it back to the new build.
        other = self.root / 'replacement'
        other.write_text('#!/bin/sh\nprintf replaced')
        other.chmod(0o755)
        other.replace(self.source)
        for _ in range(2):
            result = subprocess.run([binary], capture_output=True, timeout=5)
            self.assertTrue(result.returncode == 0 and result.stdout == b'original',
                            '#1166 repeated samples must execute original snapshot bytes')

    def test_artifact_kind_filter(self):
        failure = None
        with self.dependencies(distractor=True):
            try:
                self.build()
            except Exception as error:
                failure = error
        self.assertTrue(failure is None, '#1166 select only requested executable kind')

    def test_bin_command_and_invalid_kind(self):
        with self.dependencies(kind='bin'):
            api.build_snapshot(self.root, self.root / 'bin', package='fixture',
                               target='probe', kind='bin', release=False, env=self.env)
        self.assertTrue((self.root / 'bin/executable').is_file(), '#1166 bin build creates snapshot')
        with self.assertRaises(ValueError, msg='#1166 invalid kind rejected before side effects'):
            api.build_snapshot(self.root, self.root / 'bad', package='fixture',
                               target='probe', kind='bad', env=self.env)
        self.assertFalse((self.root / 'bad').exists(), '#1166 invalid kind rejected before side effects')

    def test_digest_metadata(self):
        with self.dependencies():
            binary = self.build()
        metadata = self.root / 'run/sha256'
        self.assertTrue(metadata.is_file(), '#1166 snapshot digest is recorded')
        self.assertTrue(metadata.read_text().strip() == hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                        '#1166 snapshot digest matches executable')

    def test_no_target_no_build(self):
        with self.dependencies(), self.assertRaises(RuntimeError,
                msg='#1166 absent target must fail before build'):
            api.build_snapshot(self.root, self.root / 'run', package='fixture', target='probe', env={})
        self.assertFalse(self.events, '#1166 absent target cannot touch sources')

    def test_failed_or_missing_build_never_copies(self):
        for failed, missing in ((True, False), (False, True)):
            self.events.clear()
            with self.subTest(failed=failed, missing=missing), \
                    self.dependencies(failed=failed, missing=missing):
                with self.assertRaises((RuntimeError, subprocess.CalledProcessError),
                                       msg='#1166 bad build must not create measurement snapshot'):
                    self.build(str((failed, missing)))


    def test_missing_build_is_attributable(self):
        failure = None
        with self.dependencies(missing=True):
            try:
                self.build()
            except Exception as error:
                failure = error
        self.assertTrue(isinstance(failure, RuntimeError) and
                        str(failure) == '#1166 expected exactly one measurement executable',
                        '#1166 missing artifact must fail explicitly')

    def test_existing_empty_run_is_refused(self):
        (self.root / 'run').mkdir()
        with self.dependencies(), self.assertRaises(FileExistsError,
                msg='#1166 existing empty run must be refused'):
            self.build()

    def test_existing_run_is_refused(self):
        with self.dependencies():
            self.build()
            with self.assertRaises((SystemExit, FileExistsError),
                                   msg='#1166 run evidence must be exclusive'):
                self.build()

    def test_copy_preserves_mode(self):
        destination = self.root / 'executable'
        api.copy_binary(self.source, destination)
        self.assertTrue(destination.stat().st_mode & 0o777 == self.source.stat().st_mode & 0o777,
                        '#1166 snapshot must retain executable mode')

    def test_pinned_copy_is_verified_and_exclusive(self):
        digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        destination = self.root / 'copy'
        api.copy_binary(self.source, destination, expected_sha256=digest)
        with self.assertRaises(FileExistsError, msg='#1166 binary copy cannot overwrite evidence'):
            api.copy_binary(self.source, destination, expected_sha256=digest)
        self.source.write_text('changed')
        with self.assertRaises(RuntimeError, msg='#1166 pinned digest must match copied bytes'):
            api.copy_binary(self.source, self.root / 'changed', expected_sha256=digest)

    def test_yaml_pinned_snapshot_does_not_wait_for_parent(self):
        yaml = load('yaml_measure1166', 'src/pilotdeck-yaml/tests/run_resource_scaling.py')
        digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        with patch.object(api, 'tree_lock', side_effect=AssertionError('#1166 child must not wait on parent')):
            with api.TreeLock(self.root / '.negative-source.lock'):
                binary = yaml.prepare_binary(self.root / 'yaml', str(self.source), digest)
        self.assertTrue(Path(binary).read_bytes() == self.source.read_bytes(),
                        '#1166 pinned yaml snapshot must preserve bytes')
        with self.assertRaises(ValueError, msg='#1166 explicit binary requires pinned digest'):
            yaml.prepare_binary(self.root / 'unverified', str(self.source))

    def test_yaml_output_is_exclusive_and_default_is_unique(self):
        yaml = load('yaml_output1166', 'src/pilotdeck-yaml/tests/run_resource_scaling.py')
        with patch.object(yaml, 'REPO', self.root):
            first, second = yaml.measurement_output(), yaml.measurement_output()
            self.assertTrue(first != second, '#1166 default measurement runs must not collide')
            with self.assertRaises(FileExistsError, msg='#1166 explicit measurement output is exclusive'):
                yaml.measurement_output(first)
            self.assertTrue(json.loads(first.read_text()) == {'status': 'incomplete'},
                            '#1166 partial measurement cannot look complete')

    def test_yaml_digest_without_binary_is_rejected(self):
        yaml = load('yaml_pin1166', 'src/pilotdeck-yaml/tests/run_resource_scaling.py')
        with patch.object(yaml, 'build', return_value='unexpected-build'), \
                self.assertRaises(ValueError, msg='#1166 digest requires an explicit binary'):
            yaml.prepare_binary(self.root / 'yaml', sha256='digest')

    def test_runner_code_stream_keeps_real_sources_pristine(self):
        # Round6 is an executable script, so extract its actual helper without
        # executing the product mutation table at import time.
        source = FIXTURES / 'edgeclaw-entries/negative_round6.py'
        definition = next(node for node in ast.parse(source.read_text()).body
                          if isinstance(node, ast.FunctionDef) and node.name == 'runner_control')
        script = self.root / 'test_negative_runners.py'
        runner = self.root / 'runner.py'
        original = "print('HEAD')"
        runner.write_text(original)
        script.write_text('import runpy\nrunpy.run_path(' + repr(str(runner)) + ", run_name='__main__')\n")
        namespace = dict(contextlib=contextlib, io=io, Path=Path, runpy=runpy, subprocess=subprocess,
                         sys=sys, patch=patch, __file__=str(self.root / 'negative_round6.py'))
        exec(compile(ast.Module(body=[definition], type_ignores=[]), str(source), 'exec'), namespace)
        result = namespace['runner_control']('synthetic', runner, "print('MUTANT')")
        self.assertTrue(result.returncode == 0 and result.stdout.strip() == 'MUTANT',
                        '#1166 runner meta-control must execute candidate code stream')
        self.assertTrue(runner.read_text() == original,
                        '#1166 runner meta-control must preserve disk HEAD')
        self.assertTrue(runpy.run_path(str(runner)) is not None,
                        '#1166 code stream override must be restored')

    def test_yaml_and_cli_use_shared_builder(self):
        yaml = load('yaml_builder1166', 'src/pilotdeck-yaml/tests/run_resource_scaling.py')
        cli = load('cli_builder1166', 'src/pilotdeck-cli/tests/fixtures/sdk-decode-budget/measure.py')
        with patch.object(yaml, 'build_snapshot', return_value='snapshot') as build:
            self.assertTrue(yaml.prepare_binary(self.root / 'yaml') == 'snapshot',
                            '#1166 YAML must measure shared builder snapshot')
            self.assertTrue(build.call_args.kwargs == dict(package='pilotdeck-yaml', target='resource_scaling'),
                            '#1166 YAML selects the resource worker')
        with patch.object(cli, 'build_snapshot', return_value='snapshot') as build:
            self.assertTrue(cli.rust_binary(self.root) == 'snapshot',
                            '#1166 CLI must measure shared builder snapshot')
            self.assertTrue(build.call_args.kwargs == dict(package='pilotdeck-cli', target='pilotdeck',
                                                          kind='bin', release=False),
                            '#1166 CLI selects the debug product binary')


if __name__ == '__main__':
    unittest.main(verbosity=2)
