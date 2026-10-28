# anchor-selfcheck: unsupported synthetic compiler/artifact/copy regression tests, no product mutations
"""#743 regression checks for the standalone rustc negative harness."""
import ast
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import negative_toolchain as toolchain


class CompilerTests(unittest.TestCase):
    def run_probe(self, env=None, *, actual='rustc 1.99.0 (build A)\n', harness=None,
                  sysroot='/selected/toolchain\n', returncode=0, failed_label='cargo-sysroot'):
        self.commands = []

        def run(command, label):
            self.commands.append((command, label))
            output = {'cargo-sysroot': sysroot, 'cargo-rustc-version': actual,
                      'harness-rustc-version': actual if harness is None else harness}[label]
            return subprocess.CompletedProcess(command, returncode if label == failed_label else 0, output, '')

        with patch.object(toolchain.shutil, 'which', return_value='/chosen/bin/cargo') as which:
            selected = toolchain.cargo_compiler(env or {}, run)
        return selected, which

    def test_explicit_cargo_uses_sibling_rustc(self):
        selected, which = self.run_probe({'CARGO': '/chosen/bin/cargo', 'PATH': '/wrong/bin'})
        self.assertEqual(which.call_args, (('/chosen/bin/cargo',), {'path': '/wrong/bin'}),
                         '#743 explicit Cargo must be looked up')
        self.assertEqual(selected, ('/chosen/bin/cargo', '/chosen/bin/rustc'),
                         '#743 compiler must follow CARGO, not PATH rustc')
        self.assertEqual(self.commands[0][0][0], selected[0], '#743 query must use caller Cargo')
        self.assertEqual(self.commands[1][0], ['/selected/toolchain/bin/rustc', '-V'],
                         '#743 query must identify Cargo actual compiler')
        self.assertEqual(self.commands[2][0], [selected[1], '-V'],
                         '#743 harness compiler must be version checked')

    def test_rustc_override_is_honored(self):
        selected, _ = self.run_probe({'RUSTC': '/override/rustc'})
        self.assertEqual(selected[1], '/override/rustc', '#743 explicit RUSTC must win')

    def test_default_cargo_lookup_keeps_rustup_proxy_name(self):
        selected, which = self.run_probe()
        which.assert_called_once_with('cargo', path=None)
        self.assertEqual(selected[1], '/chosen/bin/rustc', '#743 do not dereference rustup proxy')

    def test_relative_cargo_path_is_anchored_before_changing_cwd(self):
        def run(command, _label):
            value = '/selected/toolchain\n' if '--print' in command else 'rustc 1.99.0\n'
            return subprocess.CompletedProcess(command, 0, value, '')

        with patch.object(toolchain.shutil, 'which', return_value='tools/cargo'):
            cargo, rustc = toolchain.cargo_compiler({'CARGO': 'tools/cargo'}, run)
        expected = Path('tools/cargo').absolute()
        self.assertEqual(cargo, str(expected), '#743 relative Cargo must retain caller location')
        self.assertEqual(rustc, str(expected.with_name('rustc')),
                         '#743 relative Cargo sibling must retain caller location')

    def test_version_mismatch_is_invalid(self):
        with self.assertRaisesRegex(RuntimeError, '#743 toolchain mismatch',
                                    msg='#743 mismatch must fail before baseline'):
            self.run_probe(harness='rustc 1.99.1 (build B)\n')

    def test_empty_version_is_invalid(self):
        with self.assertRaisesRegex(RuntimeError, '#743 toolchain mismatch',
                                    msg='#743 empty compiler identity must fail'):
            self.run_probe(actual='')

    def test_invalid_sysroot_is_rejected(self):
        for value in ('', 'relative/path\n'):
            with self.subTest(value=value), self.assertRaisesRegex(
                    RuntimeError, '#743 toolchain: Cargo returned no absolute sysroot',
                    msg='#743 invalid sysroot must fail'):
                self.run_probe(sysroot=value)

    def test_query_process_failure_is_invalid(self):
        with self.assertRaises(subprocess.CalledProcessError,
                               msg='#743 failed compiler queries must fail'):
            self.run_probe(returncode=1)

    def test_version_process_failures_are_invalid(self):
        for label in ('cargo-rustc-version', 'harness-rustc-version'):
            with self.subTest(label=label), self.assertRaises(
                    subprocess.CalledProcessError, msg='#743 failed version process must fail'):
                self.run_probe(returncode=1, failed_label=label)

    def test_missing_cargo_is_invalid(self):
        with patch.object(toolchain.shutil, 'which', return_value=None):
            with self.assertRaisesRegex(RuntimeError, '#743 toolchain: caller Cargo executable not found',
                                        msg='#743 missing Cargo must fail'):
                toolchain.cargo_compiler({}, lambda *_: self.fail('#743 missing Cargo cannot run'))


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix='pdrs743-toolchain-')
        self.addCleanup(self.scratch.cleanup)
        self.target = Path(self.scratch.name)
        self.deps = self.target / 'debug/deps'
        self.deps.mkdir(parents=True)
        self.messages = []
        for crate in toolchain.HARNESS_CRATES:
            artifact = self.deps / f'lib{crate}-current.rlib'
            artifact.touch()
            self.messages.append({'reason': 'compiler-artifact', 'target': {'name': crate},
                                  'filenames': [str(artifact)], 'fresh': True})

    def output(self):
        return '\n'.join(json.dumps(row) for row in self.messages)

    def test_only_current_cargo_artifacts_are_selected(self):
        for crate in toolchain.HARNESS_CRATES:
            (self.deps / f'lib{crate}-newer-but-stale.rlib').touch()
        self.messages.append({'reason': 'build-finished', 'success': True})
        selected = toolchain.cargo_externs(self.output(), self.target)
        expected = []
        for crate in toolchain.HARNESS_CRATES:
            expected.extend(['--extern', f'{crate}={self.deps / ("lib" + crate + "-current.rlib")}'])
        self.assertEqual(selected, expected, '#743 externs must come from current Cargo JSON')

    def test_missing_artifact_is_invalid(self):
        self.messages.pop()
        with self.assertRaisesRegex(RuntimeError, '#743 extern selection: serde_json has 0 Cargo artifacts',
                                    msg='#743 incomplete extern build must fail'):
            toolchain.cargo_externs(self.output(), self.target)

    def test_ambiguous_artifact_is_invalid(self):
        other = self.deps / 'libsha2-other.rlib'
        other.touch()
        self.messages[1]['filenames'].append(str(other))
        with self.assertRaisesRegex(RuntimeError, '#743 extern selection: sha2 has 2 Cargo artifacts',
                                    msg='#743 ambiguous extern build must fail'):
            toolchain.cargo_externs(self.output(), self.target)

    def test_outside_or_missing_artifact_is_invalid(self):
        (self.target / 'libsha2-outside.rlib').touch()
        for filename in (self.target / 'libsha2-outside.rlib', self.deps / 'missing.rlib'):
            self.messages[1]['filenames'] = [str(filename)]
            with self.subTest(filename=filename), self.assertRaisesRegex(
                    RuntimeError, '#743 extern path outside caller target or missing',
                    msg='#743 wrong target/missing extern must fail'):
                toolchain.cargo_externs(self.output(), self.target)


class CopyTests(unittest.TestCase):
    def test_temporary_workspace_keeps_repo_toolchain(self):
        path = Path(__file__).resolve().parent / '_shared/negative_control.py'
        spec = importlib.util.spec_from_file_location('shared_negative_control', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory(prefix='pdrs743-copy-test-') as directory:
            root = Path(directory)
            origin, destination = root / 'origin', root / 'copy'
            (origin / 'src').mkdir(parents=True)
            (origin / 'src/Cargo.toml').write_text('[workspace]\n')
            (origin / 'LICENSE').write_text('fixture license\n')
            config = '[toolchain]\nchannel = "1.99"\n'
            (origin / 'rust-toolchain.toml').write_text(config)
            with patch.object(module, 'REPO', origin), patch.object(
                    module.subprocess, 'check_output', return_value=b'src/Cargo.toml\0'):
                source = module.source_copy(destination)
            self.assertEqual(source, destination / 'src', '#743 copy returns workspace source')
            copied = destination / 'rust-toolchain.toml'
            self.assertTrue(copied.is_file(), '#743 copied workspace must retain root toolchain')
            self.assertEqual(copied.read_text(), config, '#743 copied toolchain bytes must match')


def load_compile_runner(bounded):
    # Exercise the actual nested runner without entering its source-mutating
    # top level. Only command outcomes are substituted in these unit checks.
    path = Path(__file__).resolve().parent / 'session-domain/negative.py'
    tree = ast.parse(path.read_text())
    function = next(node for node in ast.walk(tree)
                    if isinstance(node, ast.FunctionDef) and node.name == 'run')
    namespace = dict(bounded=bounded, harness=lambda: ['rustc'],
                     compile_only=True, cargo='/chosen/bin/cargo')
    exec(compile(ast.Module(body=[function], type_ignores=[]), str(path), 'exec'), namespace)
    return namespace['run']


class CompileOnlyTests(unittest.TestCase):
    def test_compile_only_checks_gateway_tests(self):
        commands = []

        def bounded(command, label, **_kwargs):
            commands.append((command, label))
            return subprocess.CompletedProcess(command, 0, '', '')

        load_compile_runner(bounded)('sample', True)
        self.assertEqual(commands[-1], (['/chosen/bin/cargo', 'check', '--offline', '-p',
                                        'pilotdeck-gateway', '--tests'], 'sample-cargo-check'),
                         '#743 compile-only must check actual gateway tests')

    def test_cargo_compile_failure_is_invalid(self):
        def bounded(command, _label, **_kwargs):
            return subprocess.CompletedProcess(command, int('check' in command), '', '')

        with self.assertRaisesRegex(AssertionError, 'Cargo compile failure is INVALID',
                                    msg='#743 Cargo compilation failure cannot be COMPILED'):
            load_compile_runner(bounded)('sample', True)


if __name__ == '__main__':
    unittest.main(verbosity=2)
