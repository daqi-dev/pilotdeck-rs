# anchor-selfcheck: unsupported synthetic Git and binary snapshot contract tests
"""#1166 uses tiny synthetic repositories, never copies product/build state.

These repos are the input under test (including dirty index flags), not an
alternate HEAD for executing product negative controls. Shared entry points build
a missing workspace_inputs helper offline inside a pristine source lease.
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
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
import uuid

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

# The one audited set of fixture scripts allowed to mention a build-target
# directory literal: the shared admission and its unset default, this suite's
# own admission/mutation text, #994's inverse forbid-list (it *rejects* the
# product target and requires a separate persistent one), and the
# session-admission scratch copy (its target belongs to the disposable /tmp
# copy tree, not to this checkout).
TARGET_LITERAL_EXEMPT = {
    'src/pilotdeck-gateway/tests/fixtures/negative_process.py':
        'the shared target admission and its unset default live here',
    'src/pilotdeck-gateway/tests/fixtures/workspace-pristine/negative_controls.py':
        'mutation rows quote the sibling lines they replace',
    'src/pilotdeck-gateway/tests/fixtures/workspace-pristine/test_controls.py':
        'admission probes and this literal scan itself',
    'src/pilotdeck-gateway/tests/fixtures/_shared/negative_control.py':
        '#994 inverse forbid-list rejects the product target',
    'src/pilotdeck-gateway/tests/fixtures/test_negative_adaptive.py':
        '#994 forbid-list unit test',
    'src/pilotdeck-gateway/tests/fixtures/session-admission/negative.py':
        'the scratch copy tree carries its own disposable target',
}


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
        # A deletion that also removed the worktree file is caught by the
        # scanner's missing-input fail-closed. An index-only deletion of a
        # cache-named tracked input (worktree intact, *.pyc excluded from the
        # untracked check, non-.rs so the scanner never enumerates it) is
        # visible to the index-vs-baseline diff alone.
        for path, cached in ((INPUTS[3], False), (INPUTS[19], True)):
            with self.subTest(cached=cached), api.mutating_sources(self.root, []):
                git(self.root, *(['rm'] + (['--cached'] if cached else []) + [path]))
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

    def test_deep_target_is_not_a_cache(self):
        path = self.root / 'src/vendor/nested/target/generated.rs'
        path.parent.mkdir(parents=True)
        path.write_text('// generated input')
        with self.assertRaises(api.SourceBaselineError,
                               msg='#1166 nested target inputs cannot be exempt'):
            api._assert_product_pristine(self.root, 'HEAD')

    def test_literal_batches_are_bounded_and_complete(self):
        paths = tuple(':(top,literal)assets/' + str(i) + '/' + '界' * 80 for i in range(1000))
        batches = list(api._pathspec_batches(paths))
        self.assertTrue(len(batches) > 1 and all(
            sum(len(os.fsencode(path)) + 1 for path in batch) <= 32768 for batch in batches),
            '#1166 literal argv batches stay bounded')
        self.assertTrue(tuple(path for batch in batches for path in batch) == paths,
                        '#1166 every literal survives batching')
        with patch.object(api, '_workspace_literal_inputs', return_value=paths), \
                patch.object(api, '_assert_input_areas') as check:
            api._assert_product_pristine(self.root, 'HEAD')
        literal_calls = check.call_args_list[1:]
        self.assertTrue(tuple(path for call in literal_calls for path in call.args[2]) == paths
                        and all(call.kwargs == {'exclude_caches': False} for call in literal_calls),
                        '#1166 every literal batch is checked without cache exemptions')

    def test_last_real_literal_batch_is_guarded(self):
        files = {'assets/' + f'{i:04}-' + 'x' * 180: 'data' for i in range(200)}
        source = ''.join('const X' + str(i) + ': &str = include_str!("../../../' + name + '");\n'
                         for i, name in enumerate(files))
        self.commit_source(source, files)
        inputs = api._workspace_literal_inputs(self.root, 'HEAD')
        self.assertTrue(sum(len(os.fsencode(path)) + 1 for path in inputs) > 32768,
                        '#1166 actual inventory crosses the literal batch budget')
        target = self.root / next(reversed(files))
        with api.mutating_sources(self.root, [target]):
            target.write_text('residue')
            with self.assertRaises(api.SourceBaselineError,
                                   msg='#1166 final real literal batch remains guarded'):
                api._assert_product_pristine(self.root, 'HEAD')

    def test_crate_root_and_module_seed_contexts(self):
        for relative in ('tests/probe.rs', 'examples/probe.rs', 'src/bin/probe.rs',
                         'build.rs', 'custom/entry.rs'):
            with self.subTest(root=relative):
                base = Path('src/pilotdeck-cli') / relative
                parent = base.parent
                nested = parent / base.stem
                # Both roots point to distinct external data, so a fallback
                # choosing the first existing module cannot pass this test.
                import os.path
                files = {str(base): 'mod child;',
                         str(parent / 'child.rs'): 'const X: &str = include_str!("' +
                            os.path.relpath('assets/root-data', parent) + '");',
                         str(nested / 'child.rs'): 'const X: &str = include_str!("' +
                            os.path.relpath('assets/module-data', nested) + '");',
                         'assets/root-data': 'root', 'assets/module-data': 'module'}
                root_only = {name: content for name, content in files.items()
                             if name != str(nested / 'child.rs')}
                self.commit_source('// fixture', root_only)
                failure = None
                try:
                    api._workspace_literal_inputs(self.root, 'HEAD')
                except Exception as error:
                    failure = error
                self.assertTrue(failure is None,
                                '#1166 crate roots and module seeds retain both real inputs')
                self.commit_source('// fixture', files)
                failure = None
                try:
                    inputs = api._workspace_literal_inputs(self.root, 'HEAD')
                except Exception as error:
                    failure = error
                    inputs = ()
                self.assertTrue(failure is None and all(':(top,literal)assets/' + name in inputs
                                for name in ('root-data', 'module-data')),
                                '#1166 crate roots and module seeds retain both real inputs')
                for name in ('root-data', 'module-data'):
                    path = self.root / 'assets' / name
                    with api.mutating_sources(self.root, [path]):
                        path.write_text('dirty')
                        with self.assertRaises(api.SourceBaselineError,
                                msg='#1166 both seed ownership inputs stay guarded'):
                            api._assert_product_pristine(self.root, 'HEAD')

    def test_ordinary_lib_and_main_modules_use_stem_directories(self):
        files = {}
        for name in ('lib', 'main'):
            base = 'src/pilotdeck-cli/custom_' + name + '/'
            files[base + 'entry.rs'] = 'mod ' + name + ';'
            files[base + name + '.rs'] = 'mod child;'
            files[base + name + '/child.rs'] = '// ordinary module child'
        self.commit_source('// fixture', files)
        failure = None
        try:
            api._workspace_literal_inputs(self.root, 'HEAD')
        except Exception as error:
            failure = error
        self.assertTrue(failure is None,
                        '#1166 ordinary lib and main modules use stem directories')

    def test_seed_union_does_not_stop_at_first_existing_module(self):
        self.commit_source('// fixture', {
            'src/pilotdeck-cli/custom/entry.rs': 'mod child;',
            'assets/union-root': 'root', 'assets/union-module': 'module'})
        # Keep children out of HEAD so they cannot be independently seeded.
        # This tests parser discovery directly; the pristine guard separately
        # rejects untracked source inputs before scanning them.
        for relative, data in (('custom/child.rs', 'union-root'),
                               ('custom/entry/child.rs', 'union-module')):
            path = self.root / 'src/pilotdeck-cli' / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('const X: &str = include_str!("' +
                            os.path.relpath(self.root / 'assets' / data, path.parent) + '");')
        failure = None
        try:
            inputs = api._workspace_literal_inputs(self.root, 'HEAD')
        except Exception as error:
            failure, inputs = error, ()
        self.assertTrue(failure is None and all(':(top,literal)assets/' + name in inputs
                        for name in ('union-root', 'union-module')),
                        '#1166 seed ownership union cannot drop a candidate')

    def test_ambiguous_seed_module_fails_closed(self):
        self.commit_source('// fixture', {
            'src/pilotdeck-cli/tests/ambiguous.rs': 'mod child;',
            'src/pilotdeck-cli/tests/child.rs': '// child',
            'src/pilotdeck-cli/tests/child/mod.rs': '// ambiguous child'})
        with self.assertRaises(api.SourceBaselineError,
                               msg='#1166 each ownership context rejects ambiguous modules'):
            api._workspace_literal_inputs(self.root, 'HEAD')

    def test_only_untracked_caches_are_excluded(self):
        for name in ('src/target/debug/cache', 'src/pilotdeck-cli/target/debug/cache',
                     'src/.DS_Store',
                     'src/pilotdeck-cli/.pytest_cache/state',
                     'src/pilotdeck-cli/tests/__pycache__/fixture.pyc'):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('cache')
        error = None
        try:
            api._assert_product_pristine(self.root, 'HEAD')
        except Exception as caught:
            error = caught
        self.assertTrue(error is None, '#1166 disposable untracked caches are not build inputs')

    def commit_source(self, text, files):
        source = self.root / INPUTS[1]
        source.write_text(text)
        for name, content in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        git(self.root, 'add', '-f', '.')
        git(self.root, 'commit', '-qm', 'new literal inputs')

    def test_new_external_literal_inputs_are_automatically_guarded(self):
        # These paths did not exist when the helper was written. Include a
        # recursive Rust source with a nonstandard extension too.
        self.commit_source(
            'const A: &str = include_str!("../../../new-assets/help.txt");\n'
            'const B: &[u8] = r#include_bytes! { r#"../../../new-assets/blob"# };\n'
            '#[path = "../../../new-assets/extra.inc"] mod extra;\n',
            {'new-assets/help.txt': 'help', 'new-assets/blob': 'blob',
             'new-assets/extra.inc': 'const C: &str = include_str!("transitive.txt");',
             'new-assets/transitive.txt': 'transitive'})
        failure = None
        try:
            api._assert_product_pristine(self.root, 'HEAD')
        except Exception as error:
            failure = error
        self.assertTrue(failure is None, '#1166 literal discovery accepts valid Rust paths')
        for name in ('help.txt', 'blob', 'extra.inc', 'transitive.txt'):
            path = self.root / 'new-assets' / name
            with self.subTest(input=name), api.mutating_sources(self.root, [path]):
                path.write_text('// changed input')
                with self.assertRaises(api.SourceBaselineError,
                                       msg='#1166 new external literal inputs must be guarded'):
                    api._assert_product_pristine(self.root, 'HEAD')

    def test_attribute_data_inputs_are_guarded(self):
        self.commit_source('#[doc = include_str!("../../../assets/doc.md")] fn documented() {}',
                           {'assets/doc.md': 'documentation'})
        failure = None
        try:
            api._assert_product_pristine(self.root, 'HEAD')
        except Exception as error:
            failure = error
        self.assertTrue(failure is None, '#1166 attribute include must parse')
        with api.mutating_sources(self.root, [self.root / 'assets/doc.md']):
            (self.root / 'assets/doc.md').write_text('residue')
            with self.assertRaises(api.SourceBaselineError,
                                   msg='#1166 attribute data inputs remain guarded'):
                api._assert_product_pristine(self.root, 'HEAD')

    def test_nonliteral_and_unparseable_inputs_fail_closed(self):
        for source in ('const X: &str = include_str!(concat!("x", "y"));',
                       'const X: &[u8] = include_bytes!(env!("INPUT"));',
                       '#[path = concat!("x", "y")] mod x;',
                       '#[cfg_attr(any(), path = "x.rs")] mod x;',
                       'mod missing;',
                       'const X: &str = "unterminated', '/* unfinished',
                       'fn x() {', 'include_str!("missing");',
                       '#[path = "../.."] mod x;'):
            with self.subTest(source=source):
                self.commit_source(source, {})
                with self.assertRaises(api.SourceBaselineError,
                                       msg='#1166 unresolved Rust inputs must fail closed'):
                    api._assert_product_pristine(self.root, 'HEAD')

    def test_comments_strings_and_raw_paths(self):
        self.commit_source(
            '// include_str!(dynamic())\n/* outer /* include_bytes!(x) */ end */\n'
            'const A: &str = r##"include_str!(not_a_path)"##;\n'
            'const B: &[u8] = br#"include_bytes!(not_a_path)"#;\n'
            'const C: &CStr = cr#"include_bytes!(not_a_path)"#;\n'
            'const Q: char = \'"\'; fn f<\'a>(x: &\'a str) {}\n'
            'macro_rules! module { ($name:ident) => { mod $name { pub fn f() {} } } } module!(demo);\n'
            'const X: &str = include_str!(r###"../../../new-assets/help.txt"###,);',
            {'new-assets/help.txt': 'help'})
        error = None
        try:
            api._assert_product_pristine(self.root, 'HEAD')
            inputs = api._workspace_literal_inputs(self.root, 'HEAD')
        except Exception as caught:
            error, inputs = caught, ()
        self.assertTrue(error is None and ':(top,literal)new-assets/help.txt' in inputs,
                        '#1166 Rust lexing distinguishes literals from comments and data')

    def test_escaped_and_nonstring_literals_are_rejected(self):
        for literal in ('"../../../new-assets/hel\\u{70}.txt"',
                        '"../../../new-assets/help\\x2etxt"',
                        'b"../../../new-assets/help.txt"', 'br"../../../new-assets/help.txt"',
                        'c"../../../new-assets/help.txt"', 'cr"../../../new-assets/help.txt"',
                        '"../../../new-assets/help.txt"suffix'):
            for macro in ('include_str', 'include_bytes'):
                with self.subTest(literal=literal, macro=macro):
                    self.commit_source(f'const X: &str = {macro}!({literal});',
                                       {'new-assets/help.txt': 'help'})
                    with self.assertRaises(api.SourceBaselineError,
                                           msg='#1166 data includes require an unescaped string literal'):
                        api._assert_product_pristine(self.root, 'HEAD')

    def test_conditional_and_nested_paths_are_rejected(self):
        for source in ('#[cfg_attr(any(), path="../../../assets/decoy.rs")] mod child;',
                       'mod nested { #[path="../../../assets/decoy.rs"] mod child; }',
                       '#[path="../../../assets/decoy.rs"] struct Decoy; mod child;'):
            self.commit_source(source, {'assets/decoy.rs': '// decoy',
                                        'src/pilotdeck-cli/src/child.rs': '// real',
                                        'src/pilotdeck-cli/src/nested/.keep': ''})
            with self.assertRaises(api.SourceBaselineError,
                                   msg='#1166 ambiguous path attributes must fail closed'):
                api._assert_product_pristine(self.root, 'HEAD')

    def scanner_reply(self):
        paths = (ROOT / 'src/pilotdeck-gateway/examples/workspace_inputs.rs',
                 ROOT / 'src/pilotdeck-gateway/tests/common/source_inputs.rs',
                 ROOT / 'src/pilotdeck-gateway/Cargo.toml',
                 ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock')
        return dict(head=git(self.root, 'rev-parse', 'HEAD').stdout.decode().strip(),
                    root=str(self.root), inputs=[],
                    scanner_sha256=hashlib.sha256(b''.join(path.read_bytes() for path in paths)).hexdigest())

    def test_scanner_failure_is_rejected(self):
        result = subprocess.CompletedProcess([], 1, json.dumps(self.scanner_reply()), 'lexer failure')
        with patch.object(api, 'run_owned', return_value=result):
            with self.assertRaises(api.SourceBaselineError, msg='#1166 scanner nonzero must fail closed'):
                api._workspace_literal_inputs(self.root, 'HEAD')

    def test_scanner_identity_is_rejected(self):
        for field in ('head', 'root', 'scanner_sha256'):
            document = self.scanner_reply()
            document[field] = 'wrong'
            result = subprocess.CompletedProcess([], 0, json.dumps(document), '')
            with self.subTest(field=field), patch.object(api, 'run_owned', return_value=result):
                with self.assertRaises(api.SourceBaselineError, msg='#1166 stale scanner identity must fail closed'):
                    api._workspace_literal_inputs(self.root, 'HEAD')

    def test_scanner_malformed_json_is_rejected(self):
        for output in ('{', '[]', 'null'):
            result = subprocess.CompletedProcess([], 0, output, '')
            with patch.object(api, 'run_owned', return_value=result):
                with self.assertRaises(api.SourceBaselineError, msg='#1166 malformed scanner output must fail closed'):
                    api._workspace_literal_inputs(self.root, 'HEAD')

    def test_scanner_input_schema_is_rejected(self):
        for inputs in (None, 'src', [None], [''], ['../outside'], ['/outside'], ['src/../x'],
                       ['src//x'], ['src/x', 'src/x']):
            document = self.scanner_reply()
            document['inputs'] = inputs
            result = subprocess.CompletedProcess([], 0, json.dumps(document), '')
            with self.subTest(inputs=inputs), patch.object(api, 'run_owned', return_value=result):
                with self.assertRaises(api.SourceBaselineError, msg='#1166 malformed scanner paths must fail closed'):
                    api._workspace_literal_inputs(self.root, 'HEAD')

    def test_scanner_absent_or_timeout_is_rejected(self):
        for error in (FileNotFoundError('helper'), subprocess.TimeoutExpired('helper', 60)):
            with patch.object(api, 'run_owned', side_effect=error):
                with self.assertRaises(api.SourceBaselineError, msg='#1166 unavailable scanner must fail closed'):
                    api._workspace_literal_inputs(self.root, 'HEAD')

    def test_explicit_cache_input_is_not_exempt(self):
        self.commit_source('const X: &str = include_str!("../.DS_Store");',
                           {'src/pilotdeck-cli/.DS_Store': 'build input'})
        git(self.root, 'rm', '--cached', 'src/pilotdeck-cli/.DS_Store')
        git(self.root, 'commit', '-qm', 'make literal input untracked')
        with self.assertRaises(api.SourceBaselineError,
                               msg='#1166 included cache names remain build inputs'):
            api._assert_product_pristine(self.root, 'HEAD')

    def test_external_literal_index_flags_and_symlinks_fail_closed(self):
        self.commit_source('const X: &str = include_str!("../../../assets/help");',
                           {'assets/help': 'help'})
        for flag in ('assume-unchanged', 'skip-worktree'):
            git(self.root, 'update-index', '--' + flag, 'assets/help')
            try:
                with self.assertRaises(api.SourceBaselineError,
                                       msg='#1166 external input index flags must be rejected'):
                    api._assert_product_pristine(self.root, 'HEAD')
            finally:
                git(self.root, 'update-index', '--no-' + flag, 'assets/help')
        with tempfile.TemporaryDirectory() as outside:
            external = Path(outside) / 'help'
            external.write_text('outside')
            target = self.root / 'assets/help'
            target.unlink()
            target.symlink_to(external)
            git(self.root, 'add', 'assets/help')
            git(self.root, 'commit', '-qm', 'external symlink input')
            with self.assertRaises(api.SourceBaselineError,
                                   msg='#1166 literal inputs cannot escape the repository'):
                api._assert_product_pristine(self.root, 'HEAD')

    def test_external_path_escape_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix='pdrs1166-outside-') as directory:
            outside = Path(directory) / 'payload'
            outside.write_text('outside')
            self.commit_source(f'const X: &str = include_str!("{outside.as_posix()}");', {})
            with self.assertRaises(api.SourceBaselineError,
                                   msg='#1166 literal inputs cannot escape the repository'):
                api._assert_product_pristine(self.root, 'HEAD')

    def test_external_default_modules_are_guarded(self):
        self.commit_source('#[path = "../../../assets/custom.inc"] mod external;',
                           {'assets/custom.inc': 'mod child;',
                            'assets/child.rs': 'include!("fragment.inc");',
                            'assets/fragment.inc': 'const X: &str = include_str!("payload");',
                            'assets/payload': 'original'})
        for name in ('child.rs', 'fragment.inc', 'payload'):
            path = self.root / 'assets' / name
            with self.subTest(input=name), api.mutating_sources(self.root, [path]):
                path.write_text('// residue')
                with self.assertRaises(api.SourceBaselineError,
                                       msg='#1166 external default modules remain guarded'):
                    api._assert_product_pristine(self.root, 'HEAD')

    def test_macro_expansion_and_aliases_fail_closed(self):
        for source in (
                'macro_rules! load { () => { include_str!("../../../assets/payload") } }',
                'macro_rules! load { () => { #[path="../../../assets/child.rs"] mod child; } }',
                'use std::include_str as load; const X: &str = load!("../../../assets/payload");',
                'macro_rules! load { ($m:ident) => { const X: &str = $m!("../../../assets/payload"); } } load!(include_str);'):
            with self.subTest(source=source):
                self.commit_source(source, {'assets/payload': 'data', 'assets/child.rs': '// source'})
                with self.assertRaises(api.SourceBaselineError,
                                       msg='#1166 ambiguous macro input locations must fail closed'):
                    api._assert_product_pristine(self.root, 'HEAD')

    def test_internal_symlink_input_is_rejected(self):
        self.commit_source('const X: &str = include_str!("../../../assets/link/payload");',
                           {'assets/one/payload': 'one', 'assets/two/payload': 'two'})
        (self.root / 'assets/link').symlink_to('one', target_is_directory=True)
        git(self.root, 'add', 'assets/link')
        git(self.root, 'commit', '-qm', 'symlink input')
        with self.assertRaises(api.SourceBaselineError,
                               msg='#1166 input symlink identities cannot be hidden'):
            api._assert_product_pristine(self.root, 'HEAD')

    def test_symlink_workspace_source_is_rejected(self):
        source = self.root / INPUTS[0]
        (self.root / 'linked.rs').write_text('// source outside src')
        source.unlink()
        source.symlink_to('../../../linked.rs')
        git(self.root, 'add', '-f', '.')
        git(self.root, 'commit', '-qm', 'symlink workspace source')
        with self.assertRaises(api.SourceBaselineError,
                               msg='#1166 source symlink identities cannot be hidden'):
            api._assert_product_pristine(self.root, 'HEAD')

    def test_git_failure_is_not_clean(self):
        with patch.object(api, '_git', side_effect=RuntimeError('synthetic git error')):
            with self.assertRaises(RuntimeError, msg='#1166 git error must fail closed'):
                api._assert_product_pristine(self.root, 'HEAD')

    def test_sigkill_predecessor_other_crate_is_rejected(self):
        helper = str(FIXTURES)
        # A non-.rs input in another crate: the scanner's per-file literal
        # checks never enumerate it, so only the whole-workspace source areas
        # can catch this predecessor's residue.
        target = self.root / INPUTS[6]
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


class RunnerTargets(unittest.TestCase):
    # #1166 redesign: every runner admits its Cargo target through the one
    # shared rule in negative_process. Four cells per runner: an explicit
    # in-tree target, an unset variable (the shared default), a target inside
    # src (refused by the shared rule before any observation) and a dedicated
    # out-of-tree target (the host chains' configuration, accepted).
    def check_runner(self, relative, env_name, arguments, marker, *, skills_copy=False,
                     cargo_call=False, hook='mutating_sources', fresh_out=False):
        # Execute the real script up to its first source lease. Stop before any
        # product mutation, but use the real shared target admission.
        class Admitted(Exception):
            pass

        script = FIXTURES / relative
        code = compile(script.read_bytes(), str(script), 'exec')
        if cargo_call:
            # Evaluate the real Python call expression with inert values, so
            # omitting env=env cannot hide behind a correct local dictionary.
            calls = [node for node in ast.walk(ast.parse(script.read_bytes()))
                     if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                     and node.func.id == 'run_owned']
            self.assertTrue(len(calls) == 1, marker)
            call_code = compile(ast.Expression(calls[0]), str(script), 'eval')
        # The shared admission hands Cargo the resolved target (a host chain's
        # _build can be a symlink), so the expectation resolves it too.
        tree_target = str((ROOT / '_build/cargo-target').resolve())
        for supplied in (tree_target, None, str(ROOT / 'src/target'),
                         str(ROOT.parent / 'other-target')):
            with self.subTest(target=supplied), tempfile.TemporaryDirectory(
                    prefix='pdrs1166-runner-') as holder:
                if fresh_out:
                    # This runner refuses an existing output directory and
                    # requires it inside this tree's evidence area: hand it a
                    # unique not-yet-created path and remove it afterwards.
                    (ROOT / '_tmp').mkdir(exist_ok=True)
                    out = ROOT / '_tmp' / ('pdrs1166-admission-' + uuid.uuid4().hex)
                    self.addCleanup(shutil.rmtree, out, True)
                elif skills_copy:
                    out = Path(holder)
                else:
                    # mkdir(exist_ok=False) runners refuse an existing
                    # directory; exist_ok=True runners create this one.
                    out = Path(holder) / 'out'
                namespace = dict(__file__=str(script),
                                 __name__='admission_probe' if skills_copy else '__main__')
                observed = []

                def observe(root, cargo_env):
                    if cargo_call:
                        cargo_env = eval(call_code, dict(namespace, target=['--lib'], test='admission',
                                                        run_owned=lambda *args, **kwargs: kwargs.get('env', os.environ)))
                    scanner_env = api._scanner_environment()
                    observed.append((root, cargo_env['CARGO_TARGET_DIR'], scanner_env['CARGO_TARGET_DIR'],
                                     api._target_environment(root, cargo_env)['CARGO_TARGET_DIR']))
                    raise Admitted()

                def admission(*call_args, **call_kwargs):
                    cargo_env = call_kwargs.get('cargo_env')
                    observe(call_args[0], namespace[env_name] if cargo_env is None else cargo_env)

                def owned(*call_args, **call_kwargs):
                    observe(call_kwargs['cwd'], call_kwargs['env'])

                # run_owned runners would first take the real tree lock and a
                # real cargo slot; neither is needed to observe the admission.
                hook_patches = {
                    'mutating_sources': [patch.object(api, 'mutating_sources', side_effect=admission)],
                    'tree_lock': [patch.object(api, 'tree_lock', side_effect=admission)],
                    'run_owned': [patch.object(api, 'run_owned', side_effect=owned),
                                  patch.object(api, 'tree_lock'),
                                  patch.object(api, 'wait_for_slot',
                                               return_value=subprocess.CompletedProcess([], 0, stdout='', stderr=''))],
                }[hook]
                env = dict(os.environ)
                env['PDRS_BASELINE'] = str(ROOT / 'resources/PilotDeck')
                env.pop('CARGO_TARGET_DIR', None)
                if supplied is not None:
                    env['CARGO_TARGET_DIR'] = supplied
                admitted, error = False, None
                with patch.dict(os.environ, env, clear=True), \
                        patch.object(sys, 'argv', [str(script), *[arg.format(out=out) for arg in arguments]]), \
                        contextlib.ExitStack() as stack:
                    for pending in hook_patches:
                        stack.enter_context(pending)
                    try:
                        exec(code, namespace)
                        if skills_copy:
                            # Exercise the existing run() through its actual
                            # Popen environment; never create a source copy.
                            namespace['LOGS'] = Path(out)
                            with patch.object(subprocess, 'run'), patch.object(
                                    subprocess, 'Popen', side_effect=lambda *args, **kwargs:
                                    admission(ROOT, [], kwargs['env'])):
                                namespace['run'](ROOT, 'admission', 'unused')
                    except Admitted:
                        admitted = True
                    except (AssertionError, RuntimeError, KeyError) as caught:
                        error = caught
                if supplied == str(ROOT / 'src/target'):
                    self.assertTrue(not admitted and not observed and isinstance(error, RuntimeError)
                                    and '#1166 target cannot be inside src' in str(error), marker)
                else:
                    expected = tree_target if supplied is None else str(Path(supplied).resolve())
                    self.assertTrue(admitted and error is None, marker)
                    self.assertTrue(observed == [(ROOT, expected, expected, expected)], marker)

    def test_tool_budget_target_admission(self):
        self.check_runner('tool-budget/negative_project.py', 'ENV', [],
                          '#1166 tool-budget runner shares the admissible target')

    def test_round6_target_admission(self):
        self.check_runner('edgeclaw-entries/negative_round6.py', 'env', ['--out', '{out}'],
                          '#1166 round6 runner shares the admissible target')

    def test_round7_target_admission(self):
        self.check_runner('edgeclaw-entries/negative_round7.py', 'env', ['{out}'],
                          '#1166 round7 runner shares the admissible target')

    def test_plugin_panic_target_admission(self):
        self.check_runner('plugin-runtime/panic-negative.py', 'ENV', ['{out}/evidence'],
                          '#1166 plugin-panic runner shares the admissible target')

    def test_plugin_notify_target_admission(self):
        self.check_runner('plugin-runtime/notify-window-negative.py', 'env', ['{out}/evidence'],
                          '#1166 plugin-notify runner shares the admissible target')

    def test_model_factory_target_admission(self):
        self.check_runner('model-factory/negative-controls.py', 'env', ['{out}/evidence'],
                          '#1166 model-factory runner shares the admissible target', cargo_call=True)

    def test_turn_gateway_target_admission(self):
        self.check_runner('turn-gateway/negative.py', 'env', ['{out}/evidence'],
                          '#1166 turn-gateway runner shares the admissible target')

    def test_turn_fence_target_admission(self):
        self.check_runner('turn-gateway/negative-fence.py', 'env', ['{out}/evidence'],
                          '#1166 turn-fence runner shares the admissible target')

    def test_skills_tree_target_admission(self):
        self.check_runner('skills-tree-files/negative.py', 'ENV', [],
                          '#1166 skills-tree runner shares the admissible target', skills_copy=True)

    def test_spill_isolation_target_admission(self):
        self.check_runner('spill-isolation/negative_controls.py', 'env', ['{out}'],
                          '#1166 spill-isolation runner shares the admissible target')

    def test_session_owner_hooks_target_admission(self):
        self.check_runner('session-owner-hooks/negative.py', 'env', ['{out}'],
                          '#1166 session-owner-hooks runner shares the admissible target')

    def test_hooks_output_target_admission(self):
        self.check_runner('hooks-output/negative.py', 'env',
                          ['{out}', '0' * 64],
                          '#1166 hooks-output runner shares the admissible target',
                          hook='tree_lock')

    def test_turn_submit_admission_target_admission(self):
        self.check_runner('turn-submit-admission/negative_controls.py', 'env', ['{out}'],
                          '#1166 turn-submit-admission runner shares the admissible target')

    def test_interaction_values_target_admission(self):
        self.check_runner('interaction-values/negative_controls.py', 'env', ['{out}'],
                          '#1166 interaction-values runner shares the admissible target')

    def test_skills_target_admission(self):
        self.check_runner('skills/negative_controls.py', 'env', ['--out', '{out}'],
                          '#1166 skills runner shares the admissible target',
                          hook='run_owned', fresh_out=True)

    def test_shared_target_admission_resolves_symlinked_build(self):
        # A host chain's _build is a symlink to a dedicated build directory.
        # The shared admission must hand every runner the resolved target, so
        # the unset default and an explicit symlinked value normalize alike.
        with tempfile.TemporaryDirectory(prefix='pdrs1166-link-') as directory:
            outside = Path(directory) / 'shared-target'
            outside.mkdir()
            root = Path(directory) / 'tree'
            root.mkdir()
            (root / '_build').symlink_to(outside, target_is_directory=True)
            expected = str((root / '_build/cargo-target').resolve())
            unset = api.target_environment(root, {})
            explicit = api.target_environment(root, {'CARGO_TARGET_DIR': str(root / '_build/cargo-target')})
            self.assertTrue(unset['CARGO_TARGET_DIR'] == expected
                            and explicit['CARGO_TARGET_DIR'] == expected,
                            '#1166 shared admission resolves the symlinked _build target')

    def test_sibling_scripts_have_no_local_target_allowlist(self):
        # The shared admission is the only place a target directory may be
        # named; a sibling script that grows its own allow-list (r5/r6
        # regression) breaks host chains that use a dedicated out-of-tree
        # target. Tracked and unignored untracked python under any
        # tests/fixtures directory are scanned.
        listing = git(ROOT, 'ls-files', '-z', '--cached', '--others',
                      '--exclude-standard')
        offenders = []
        for raw in listing.stdout.split(b'\0'):
            name = os.fsdecode(raw) if raw else ''
            if '/tests/fixtures/' not in name or not name.endswith('.py'):
                continue
            if name in TARGET_LITERAL_EXEMPT:
                continue
            text = (ROOT / name).read_text(encoding='utf-8', errors='replace')
            if 'src/target' in text or '_build/cargo-target' in text:
                offenders.append(name)
        self.assertTrue(not offenders,
                        '#1166 sibling scripts must share the one target admission rule')

    def test_source_identity_target_admission(self):
        name = '#1166 build evidence target precedence keeps default outside src'
        result = subprocess.run(['node', '--test', '--test-reporter=tap', '--test-name-pattern=^' + name + '$',
                                 str(FIXTURES.parent / 'upgrade/source_identity.test.mjs')],
                                capture_output=True, text=True, timeout=30)
        # Keep the marker at the end of one line: the bound negative control
        # judges the AssertionError line by that suffix.
        detail = ' '.join((result.stdout + result.stderr).split())
        self.assertTrue(result.returncode == 0 and ('ok 1 - \\' + name) in result.stdout,
                        detail[:400] + ' #1166 source-identity helper defaults outside src')


class ScannerBootstrap(unittest.TestCase):
    def test_fresh_target_first_mutation_bootstraps_inside_pristine_lock(self):
        with tempfile.TemporaryDirectory(prefix='pdrs1166-cold-target-') as directory:
            target = Path(directory).resolve()
            executable = target / 'debug/examples/workspace_inputs'
            events = []
            def build(command, **kwargs):
                events.append('build')
                self.assertTrue(command[:3] == ['cargo', 'build', '--locked']
                                and kwargs['env']['CARGO_TARGET_DIR'] == str(target)
                                and kwargs['cwd'] == ROOT
                                and kwargs['timeout'] == 1800 and kwargs['idle_timeout'] == 600,
                                '#1166 cold scanner build uses locked bounded caller target')
                executable.parent.mkdir(parents=True)
                executable.write_text('synthetic Cargo artifact')
                return subprocess.CompletedProcess(command, 0, '', '')
            @contextmanager
            def lock(*args, **kwargs):
                events.append('lock')
                yield
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(target)}), \
                    patch.object(api, 'wait_for_slot', side_effect=lambda *a, **k:
                        (events.append('slot') or subprocess.CompletedProcess([], 0, '', ''))), \
                    patch.object(api, 'run_owned', side_effect=build), \
                    patch.object(api, 'tree_lock', lock), \
                    patch.object(api, '_assert_workspace_sources', side_effect=lambda *a: events.append('preflight')), \
                    patch.object(api, '_assert_product_pristine', side_effect=lambda *a: events.append('clean')):
                failure = None
                try:
                    with api.mutating_sources(ROOT, []):
                        events.append('body')
                    with api.mutating_sources(ROOT, []):
                        events.append('warm-body')
                except Exception as error:
                    failure = error
                self.assertTrue(failure is None and events ==
                                ['lock', 'preflight', 'lock', 'preflight', 'slot', 'build', 'clean', 'body',
                                 'lock', 'preflight', 'clean', 'warm-body'],
                                '#1166 cold first mutation prepares inside pristine lease and warm reuses helper')

    def test_dirty_sources_never_bootstrap(self):
        for entry in ('mutation', 'helper'):
            events = []
            @contextmanager
            def lock(*args, **kwargs):
                events.append('lock')
                try:
                    yield
                finally:
                    events.append('unlock')
            def dirty(*args):
                events.append('preflight')
                raise api.SourceBaselineError(ROOT, 'src/dirty.rs', 'synthetic residue')
            with self.subTest(entry=entry), tempfile.TemporaryDirectory() as target, \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': target}), \
                    patch.object(api, 'tree_lock', lock), \
                    patch.object(api, '_assert_workspace_sources', side_effect=dirty), \
                    patch.object(api, '_assert_product_pristine'), \
                    patch.object(api, 'wait_for_slot') as slot, \
                    patch.object(api, 'run_owned') as build:
                failure = None
                try:
                    if entry == 'helper':
                        api._ensure_workspace_scanner()
                    else:
                        with api.mutating_sources(ROOT, []):
                            events.append('body')
                except Exception as error:
                    failure = error
                self.assertTrue(isinstance(failure, api.SourceBaselineError)
                                and events == ['lock', 'preflight', 'unlock']
                                and not slot.called and not build.called,
                                '#1166 dirty cold sources cannot bootstrap before admission')

    def test_waiting_cold_helper_reuses_predecessor(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory)
            executable = target / 'debug/examples/workspace_inputs'
            @contextmanager
            def predecessor(*args, **kwargs):
                executable.parent.mkdir(parents=True)
                executable.write_text('predecessor completed under lease')
                yield
            with patch.object(api, 'tree_lock', predecessor), \
                    patch.object(api, '_assert_workspace_sources'), \
                    patch.object(api, 'wait_for_slot') as slot, \
                    patch.object(api, 'run_owned') as build:
                api._ensure_workspace_scanner({'CARGO_TARGET_DIR': directory})
            self.assertFalse(slot.called or build.called,
                             '#1166 cold waiter rechecks completed helper under lease')

    def test_bootstrap_failure_never_enters_mutation(self):
        for outcome in ('failed', 'missing', 'cancelled'):
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as directory, \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': directory}), \
                    patch.object(api, 'wait_for_slot', return_value=subprocess.CompletedProcess([], 0, '', '')), \
                    patch.object(api, 'tree_lock') as lock, \
                    patch.object(api, '_assert_workspace_sources'), \
                    patch.object(api, '_assert_product_pristine'), \
                    patch.object(api, 'run_owned', side_effect=(KeyboardInterrupt('cancel') if outcome == 'cancelled'
                        else None), return_value=subprocess.CompletedProcess([], int(outcome == 'failed'), '', '')):
                with self.assertRaises((RuntimeError, subprocess.CalledProcessError, KeyboardInterrupt),
                        msg='#1166 failed scanner bootstrap must fail closed'):
                    with api.mutating_sources(ROOT, []):
                        pass
                self.assertTrue(lock.called and lock.return_value.__exit__.called,
                                '#1166 failed bootstrap must release source lease')


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
    def dependencies(self, *, failed=False, missing=False, distractor=False, kind='test',
                     copy_hook=None, before_copy=None, warm=False):
        @contextmanager
        def lock(*args, **kwargs):
            self.events.append('lock')
            try:
                yield
            finally:
                self.events.append('unlock')
        def clean(*args, **kwargs):
            self.events.append('clean')
            self.assertTrue(kwargs['env'] == self.env, '#1166 scanner and Cargo share target environment')
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
            # A warm target reuses an artifact written long before this build
            # started; cargo legitimately skips recompilation (#1166 redesign).
            stamp = 1 if warm else time.time_ns()
            os.utime(self.source, ns=(stamp, stamp))
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
            if before_copy is not None:
                before_copy()
            binary = actual_copy(*args, **kwargs)
            if copy_hook is not None:
                copy_hook()
            return binary
        with patch.object(api, '_ensure_workspace_scanner', side_effect=lambda env: self.events.append('prepare')), \
                patch.object(api, 'tree_lock', lock), \
                patch.object(api, '_assert_workspace_sources', side_effect=lambda *a: self.events.append('preflight')), \
                patch.object(api, '_git', return_value=b'fixture-head\n'), \
                patch.object(api, '_assert_product_pristine', clean), \
                patch.object(api, 'wait_for_slot', slot), patch.object(api, 'run_owned', build), \
                patch.object(api, 'copy_binary', copy):
            yield

    def build(self, name='run'):
        return api.build_snapshot(self.root, self.root / name, package='fixture',
                                  target='probe', env=self.env)

    def test_build_copy_and_slot_are_inside_lock(self):
        with self.dependencies():
            binary = self.build()
        self.assertTrue(self.events == ['lock', 'preflight', 'prepare', 'clean', 'slot', 'build', 'copy', 'unlock'],
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

    def test_dirty_snapshot_never_bootstraps(self):
        failure = None
        with self.dependencies(), \
                patch.object(api, '_assert_workspace_sources',
                             side_effect=api.SourceBaselineError(self.root, 'src/dirty.rs', 'residue')):
            try:
                self.build()
            except Exception as error:
                failure = error
        self.assertTrue(isinstance(failure, api.SourceBaselineError)
                        and self.events == ['lock', 'unlock'],
                        '#1166 dirty snapshot cannot bootstrap or build')

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
        self.reject_target(None, '#1166 absent target must fail before build')

    def test_external_target_is_rebuilt_and_copied(self):
        with tempfile.TemporaryDirectory(prefix='pdrs1166-external-') as other:
            self.env = {'CARGO_TARGET_DIR': str(Path(other).resolve())}
            failure = None
            with self.dependencies():
                try:
                    binary = self.build()
                except Exception as error:
                    failure = error
                    binary = ''
            self.assertTrue(failure is None and Path(binary).is_file(),
                            '#1166 external target must build and snapshot successfully')
            self.assertTrue(self.events == ['lock', 'preflight', 'prepare', 'clean', 'slot', 'build', 'copy', 'unlock'],
                            '#1166 external target provenance requires locked rebuild and copy')

    def reject_target(self, value, marker):
        self.env['CARGO_TARGET_DIR'] = value
        with self.assertRaises(RuntimeError, msg=marker):
            api._scanner_environment(self.env)
        failure = None
        with self.dependencies():
            try:
                self.build()
            except Exception as error:
                failure = error
        self.assertTrue(isinstance(failure, RuntimeError) and not self.events
                        and not (self.root / 'run').exists(), marker)

    def test_whitespace_target_is_refused(self):
        self.reject_target(' \t\n ', '#1166 whitespace target refused before side effects')

    def test_dot_target_is_refused(self):
        self.reject_target('.', '#1166 dot target refused before side effects')

    def test_filesystem_root_target_is_refused(self):
        self.reject_target('/', '#1166 filesystem root target refused before side effects')

    def test_repository_root_target_is_refused(self):
        self.reject_target(str(ROOT), '#1166 repository root target refused before side effects')

    def test_source_target_is_refused(self):
        self.reject_target(str(ROOT / 'src/target'), '#1166 source target refused before side effects')

    def test_build_owner_is_atomic_and_unique(self):
        marker = Path(self.env['CARGO_TARGET_DIR']) / '.negative-build-owner.json'
        marker.parent.mkdir()
        marker.write_text('previous owner')
        replace = Path.replace
        publications = []
        def publish(path, destination):
            publications.append((marker.read_text(), path.read_text()))
            return replace(path, destination)
        failure = None
        try:
            with patch.object(api, '_git', return_value=b'fixture-head\n'), \
                    patch.object(Path, 'replace', publish):
                _, first = api._publish_build_owner(self.root, self.env)
                _, second = api._publish_build_owner(self.root, self.env)
            owners = [json.loads(first), json.loads(second)]
            valid = (len(publications) == 2
                     and publications == [('previous owner', first), (first, second)]
                     and marker.read_text() == second
                     and all(row.get('root') == str(self.root) and row.get('head') == 'fixture-head'
                             and len(row.get('nonce', '')) == 32 for row in owners)
                     and owners[0]['nonce'] != owners[1]['nonce'])
        except Exception as error:
            failure, valid = error, False
        self.assertTrue(failure is None and valid,
                        '#1166 build owner atomically binds root HEAD and fresh nonce')

    def reject_provenance(self, hook, message):
        failure = None
        with self.dependencies(copy_hook=hook):
            try:
                self.build()
            except Exception as error:
                failure = error
        self.assertTrue(isinstance(failure, RuntimeError) and str(failure) == message
                        and not (self.root / 'run/sha256').exists(),
                        '#1166 ' + message.removeprefix('#1166 ') + ' must reject snapshot')

    def test_foreign_build_between_build_and_copy_is_refused(self):
        def takeover():
            marker = Path(self.env['CARGO_TARGET_DIR']) / '.negative-build-owner.json'
            program = (
                'from pathlib import Path; import sys, json\n'
                'artifact, marker = map(Path, sys.argv[1:])\n'
                'pending = marker.with_suffix(".foreign.tmp")\n'
                'pending.write_text(json.dumps(dict(root="foreign-tree", head="foreign-head", nonce="foreign")))\n'
                'pending.replace(marker)\n'
                'pending = artifact.with_suffix(".foreign.tmp")\n'
                'pending.write_text("foreign build bytes")\n'
                'pending.replace(artifact)\n')
            result = subprocess.run([sys.executable, '-I', '-c', program,
                                     str(self.source), str(marker)], capture_output=True, timeout=5)
            self.assertTrue(result.returncode == 0,
                            '#1166 second builder must complete its deterministic takeover')
        failure = None
        with self.dependencies(before_copy=takeover):
            try:
                self.build()
            except Exception as error:
                failure = error
        self.assertTrue(isinstance(failure, RuntimeError)
                        and str(failure) == '#1166 build owner changed during snapshot'
                        and (self.root / 'run/executable').read_text() == 'foreign build bytes'
                        and not (self.root / 'run/sha256').exists(),
                        '#1166 second builder between build and copy cannot claim snapshot')

    def test_owner_replacement_after_copy_is_refused(self):
        def takeover():
            marker = Path(self.env['CARGO_TARGET_DIR']) / '.negative-build-owner.json'
            other = marker.with_suffix('.other.tmp')
            other.write_text(json.dumps(dict(root='other-tree', head='other-head', nonce='other-nonce')))
            other.replace(marker)
        self.reject_provenance(takeover, '#1166 build owner changed during snapshot')

    def test_artifact_replacement_after_copy_is_refused(self):
        def takeover():
            other = self.source.with_suffix('.other.tmp')
            other.write_text('foreign artifact')
            other.replace(self.source)
        self.reject_provenance(takeover, '#1166 build artifact changed during snapshot')

    def test_warm_artifact_is_accepted(self):
        # The r4 mtime lower bound rejected every warm target as a false FAIL
        # on host chains (#1166 redesign): ownership is provenance, not age.
        # The lock-internal owner marker plus the post-copy re-verify carry
        # the guard; adversarial writers are a separate known boundary.
        failure = None
        with self.dependencies(warm=True):
            try:
                binary = self.build()
            except Exception as error:
                failure, binary = error, ''
        self.assertTrue(failure is None
                        and self.events == ['lock', 'preflight', 'prepare', 'clean', 'slot', 'build', 'copy', 'unlock']
                        and Path(binary).is_file(),
                        '#1166 reused artifact predating this build is admitted')

    def test_missing_owner_after_copy_is_refused(self):
        def remove():
            (Path(self.env['CARGO_TARGET_DIR']) / '.negative-build-owner.json').unlink()
        self.reject_provenance(remove, '#1166 build provenance unavailable after copy')

    def test_owner_publication_precedes_cargo_inside_lock(self):
        actual_publish = api._publish_build_owner
        def publish(*args):
            self.assertTrue(self.events == ['lock', 'preflight', 'prepare', 'clean', 'slot'],
                            '#1166 owner publication precedes cargo inside pristine lease')
            return actual_publish(*args)
        with self.dependencies(), patch.object(api, '_publish_build_owner', side_effect=publish):
            binary = self.build()
        owner = json.loads((Path(binary).parent / 'build-owner.json').read_text())
        self.assertTrue(owner['root'] == str(self.root) and owner['head'] == 'fixture-head',
                        '#1166 successful snapshot records its build owner')

    def test_relative_target_is_independent_of_process_cwd(self):
        expected = str((ROOT / '../relative-target').resolve())
        for cwd in (ROOT, self.root):
            with contextlib.chdir(cwd):
                env = api._scanner_environment({'CARGO_TARGET_DIR': '../relative-target'})
            self.assertTrue(env['CARGO_TARGET_DIR'] == expected,
                            '#1166 target resolves relative to repository root')
        with patch.dict(os.environ):
            os.environ.pop('CARGO_TARGET_DIR', None)
            env = api._scanner_environment()
        self.assertTrue(env['CARGO_TARGET_DIR'] == str((ROOT / '_build/cargo-target').resolve()),
                        '#1166 scanner has a default target without caller setup')

    def test_runner_build_uses_normalized_environment(self):
        source = Path(__file__).with_name('negative_controls.py')
        main = next(node for node in ast.parse(source.read_text()).body
                    if isinstance(node, ast.FunctionDef) and node.name == 'main')
        build = next(node for node in main.body
                     if isinstance(node, ast.FunctionDef) and node.name == 'build_helper')
        env = api._scanner_environment({'CARGO_TARGET_DIR': '../caller-target'})
        calls = []
        def run(command, **kwargs):
            calls.append(kwargs)
            return subprocess.CompletedProcess(command, 0, '', '')
        namespace = dict(ROOT=ROOT, env=env, out=self.root, run_owned=run,
                         wait_for_slot=lambda *a, **k: subprocess.CompletedProcess([], 0, '', ''),
                         exclusive_write=lambda *a: None)
        exec(compile(ast.Module(body=[build], type_ignores=[]), str(source), 'exec'), namespace)
        namespace['build_helper']('probe')
        self.assertTrue(bool(calls) and all(call.get('env') == env for call in calls),
                        '#1166 runner Cargo uses normalized caller target')
        self.assertTrue(all(call.get('timeout') == 1800 and call.get('idle_timeout') == 600 for call in calls),
                        '#1166 runner helper keeps bounded progress deadlines')

    def test_yaml_has_only_python_measurement_entrypoint(self):
        source = (ROOT / 'src/pilotdeck-yaml/tests/resource_scaling.rs').read_text()
        self.assertFalse('fn resource_scaling()' in source,
                         '#1166 YAML must not nest a Cargo build in its ignored driver')
        self.assertTrue('fn resource_worker()' in source,
                        '#1166 Python driver retains the resource worker')

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
