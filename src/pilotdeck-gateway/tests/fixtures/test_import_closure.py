# anchor-selfcheck: unsupported unit tests of the count runners' closure guards
"""Run directly with Python on a committed, clean checkout; no Cargo needed.

Covers both count runners (#1129 / #1139 fix-r1, fix-r2): unrecognized dynamic
imports must fail closed instead of being silently skipped, the only allowed
computed arguments are the two fixed baseline loaders verbatim (never a shape
that merely mentions baseline), literal dynamic imports — including trailing
comments and commas — must join the closure, static imports join it in any
spacing, file: URLs follow the absolute-path rules, the real generators'
closures stay the documented file sets, the two runners' closure helpers stay
synchronized, the Rust criterion inputs are discovered from the test's own
declarations rather than a preset list, and a dirty Rust criterion input must
be refused before any positive control. The dirty-input cases mutate repository
files in place, so they run inside the shared source guard and restore
byte-for-byte on exit; on a tree whose guarded inputs differ from HEAD they
fail by naming the offending file.
"""
import ast
import importlib.util
import re
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
FIXTURES = Path(__file__).resolve().parent
REPO = FIXTURES.parents[3]
# negative_process.py sits in the fixtures root; only negative_control.py,
# imported first by the runners, would otherwise put it on the path.
sys.path.insert(0, str(FIXTURES / '_shared'))
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources

COUNT_RUNNERS = {
    'recorder': FIXTURES / 'recorder/fix3-negative.py',
    'run-registry': FIXTURES / 'run-registry/count-negative.py',
}
DOCUMENTED_CLOSURES = {
    'recorder': {
        'src/pilotdeck-gateway/tests/fixtures/recorder/generate.mts',
        'src/pilotdeck-gateway/tests/fixtures/_shared/baseline.mts',
        'src/pilotdeck-gateway/tests/fixtures/_shared/generator_guard.mjs',
        'src/pilotdeck-gateway/tests/baseline_source_guard.mjs',
    },
    'run-registry': {
        'src/pilotdeck-gateway/tests/fixtures/run-registry/generate.mts',
        'src/pilotdeck-gateway/tests/fixtures/_shared/baseline.mts',
        'src/pilotdeck-gateway/tests/fixtures/_shared/generator_guard.mjs',
        'src/pilotdeck-gateway/tests/baseline_source_guard.mjs',
    },
}
# The closure/guard helpers below are a hand-maintained double copy (M6 #1099
# will extract them); the synchronization test keeps the two from drifting.
SHARED_HELPERS = (
    '_skip_spacing', '_argument_without_trailing', '_dynamic_import_arguments',
    '_static_import_specifiers', '_dynamic_import_targets',
    'local_import_closure', 'copy_local_import_closure',
)


def runner(name):
    """Load a count runner fresh from disk, so mutations are always visible."""
    path = COUNT_RUNNERS[name]
    spec = importlib.util.spec_from_file_location(f'count_runner_{name}', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ClosureTests(unittest.TestCase):
    """Both runners' local_import_closure must fail closed on the unprovable."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='pdrs222-closure-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()

    def entry(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        return path

    def closure_error(self, name, text):
        """Return the ValueError message, or None when the closure accepted."""
        try:
            runner(name).local_import_closure(self.entry('generate.mts', text),
                                              self.root)
        except ValueError as error:
            return str(error)
        return None

    def test_unrecognized_dynamic_import_is_rejected(self):
        # Templates with substitutions, variables and concatenations are
        # errors, never silently skipped dependencies (#1139 fix-r1 P1). The
        # baseline-mentioning shapes are the fix-r2 review counterexamples:
        # only the two verbatim loaders are allowed, never a word match.
        for name in COUNT_RUNNERS:
            for source in (
                'await import(`./scenario-${index}.mts`);',
                'await import(resolved);',
                "await import('./' + name + '.mjs');",
                'await import(`./scenario-${baseline}.mts`);',
                'await import(baseline);',
                'await import("./" + baseline + ".mjs");',
                'await import(resolved /* baseline */);',
                'await import(join(baseline, name));',
                'await import(`./${baseline}-x.mjs`);',
                'await import(baseline + localPath);',
            ):
                with self.subTest(runner=name, source=source):
                    error = self.closure_error(name, source)
                    self.assertTrue(
                        error is not None
                        and 'unrecognized dynamic import' in error,
                        '#1139 fix-r2: unrecognized dynamic import must fail closed')

    def test_absolute_dynamic_import_outside_repository_is_rejected(self):
        for name in COUNT_RUNNERS:
            with self.subTest(runner=name):
                error = self.closure_error(name, 'await import(\'/etc/hosts\');')
                self.assertTrue(
                    error is not None
                    and 'outside the repository' in error,
                    '#1139 fix-r1: out-of-root absolute dynamic import must fail closed')

    def test_literal_dynamic_imports_join_the_closure(self):
        # Comment-separated quotes and substitution-free templates are still
        # literal specifiers, and so are trailing comments and trailing
        # commas; they must join the closure, not fall out of it.
        for name in COUNT_RUNNERS:
            for index, form in enumerate((
                "await import /* loader */ ('./dep.mjs');",
                'await import(`./dep.mjs`);',
                "await import('./dep.mjs' /* loader */);",
                "await import('./dep.mjs',);",
            )):
                with self.subTest(runner=name, form=form):
                    entry = self.entry(f'entry{index}.mts', form)
                    self.entry('dep.mjs', 'export const value = 1;\n')
                    try:
                        closure = runner(name).local_import_closure(entry, self.root)
                    except ValueError as error:
                        self.fail(
                            f'#1139 fix-r2: literal dynamic imports must join the '
                            f'closure: {error}')
                    self.assertTrue(
                        {path.name for path in closure} == {entry.name, 'dep.mjs'},
                        '#1139 fix-r2: literal dynamic imports must join the closure')

    def test_static_import_edge_forms_join_the_closure(self):
        # Zero whitespace and comment-separated static imports are legal JS
        # the old \bimport\s+ regex silently dropped; they must join too
        # (#1139 fix-r2).
        for name in COUNT_RUNNERS:
            for index, form in enumerate((
                "import'./dep.mjs';",
                "import /* c */ './dep.mjs';",
                "export * from /* c */ './dep.mjs';",
            )):
                with self.subTest(runner=name, form=form):
                    entry = self.entry(f'entry{index}.mts', form)
                    self.entry('dep.mjs', 'export const value = 1;\n')
                    closure = runner(name).local_import_closure(entry, self.root)
                    self.assertTrue(
                        {path.name for path in closure} == {entry.name, 'dep.mjs'},
                        '#1139 fix-r2: static import edge forms must join the closure')

    def test_file_url_dynamic_imports_follow_absolute_rules(self):
        # A file: URL is a local dependency, not a bare external module: an
        # in-root one joins the closure and an out-of-root one is refused,
        # exactly like a plain absolute path (#1139 fix-r2).
        for name in COUNT_RUNNERS:
            with self.subTest(runner=name):
                entry = self.entry(
                    'generate.mts',
                    f"await import('file://{(self.root / 'abs.mjs').as_posix()}');")
                self.entry('abs.mjs', 'export const value = 1;\n')
                closure = runner(name).local_import_closure(entry, self.root)
                self.assertTrue(
                    {path.name for path in closure} == {'generate.mts', 'abs.mjs'},
                    '#1139 fix-r2: in-root file: URL dynamic import must join the closure')
                error = self.closure_error(name, "await import('file:///etc/hosts');")
                self.assertTrue(
                    error is not None
                    and 'outside the repository' in error,
                    '#1139 fix-r2: out-of-root file: URL dynamic import must fail closed')

    def test_in_repository_absolute_dynamic_import_joins_the_closure(self):
        for name in COUNT_RUNNERS:
            with self.subTest(runner=name):
                entry = self.entry(
                    'generate.mts',
                    f"await import('{(self.root / 'abs.mjs').as_posix()}');")
                self.entry('abs.mjs', 'export const value = 1;\n')
                closure = runner(name).local_import_closure(entry, self.root)
                self.assertTrue(
                    {path.name for path in closure} == {'generate.mts', 'abs.mjs'},
                    '#1139 fix-r1: in-repository absolute dynamic import must join the closure')

    def test_baseline_loader_dynamic_import_is_allowed(self):
        # The only allowed computed specifiers: the two fixed TS baseline
        # loaders, verbatim, whose source hashes oracle.json records
        # (recorder/generate.mts:34 and run-registry/generate.mts:16).
        for name in COUNT_RUNNERS:
            for shape in (
                "const load = (path) => import(pathToFileURL("
                "join(baseline, 'src', path)).href);\n",
                "const load = (source) => import(pathToFileURL("
                "join(baseline, source)).href);\n",
            ):
                with self.subTest(runner=name):
                    error = self.closure_error(name, shape)
                    self.assertTrue(
                        error is None,
                        '#1139 fix-r2: baseline loader dynamic imports must stay allowed')

    def test_real_generator_closures_are_the_documented_files(self):
        for name in COUNT_RUNNERS:
            with self.subTest(runner=name):
                closure = runner(name).local_import_closure(
                    FIXTURES / f'{name}/generate.mts', REPO)
                relative = {path.relative_to(REPO).as_posix() for path in closure}
                self.assertTrue(
                    relative == DOCUMENTED_CLOSURES[name],
                    '#1139 fix-r1: real generator closure must stay the documented file set')

    def test_runner_closure_helpers_stay_synchronized(self):
        # The closure helpers are a hand-maintained double copy (M6 #1099 will
        # extract them); until then, drift between the two runners must fail
        # here instead of silently diverging (#1139 fix-r2 sibling review).
        # Issue markers are the only allowed difference.
        normalized = {}
        for name, path in COUNT_RUNNERS.items():
            text = re.sub(r'#11[23]9', '#N', path.read_text())
            tree = ast.parse(text)
            helpers = {
                node.name: ast.get_source_segment(text, node)
                for node in tree.body
                if isinstance(node, ast.FunctionDef) and node.name in SHARED_HELPERS}
            helpers['BASELINE_LOADERS'] = ''.join(
                ast.get_source_segment(text, node) for node in tree.body
                if isinstance(node, ast.Assign)
                and isinstance(node.targets[0], ast.Name)
                and node.targets[0].id == 'BASELINE_LOADERS')
            normalized[name] = helpers
        self.assertTrue(
            all(set(normalized[name]) == set(SHARED_HELPERS) | {'BASELINE_LOADERS'}
                and normalized[name]['BASELINE_LOADERS'] for name in COUNT_RUNNERS),
            '#1139 fix-r2: both runners must define the shared closure helpers')
        self.assertTrue(
            normalized['recorder'] == normalized['run-registry'],
            '#1139 fix-r2: the runners\' closure helpers must stay synchronized')


class GuardedInputTests(unittest.TestCase):
    """recorder's guard must refuse a dirty Rust criterion before any control."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='pdrs222-criterion-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()

    def criterion_inputs(self):
        """Discovered criterion inputs plus the manifest, repo-relative."""
        module = runner('recorder')
        inputs = {path.relative_to(REPO).as_posix()
                  for path in module.rust_criterion_closure(module.CRITERION_ENTRY)}
        inputs.add(module.CRITERION_MANIFEST.relative_to(REPO).as_posix())
        return inputs

    def test_criterion_inputs_are_discovered_from_declarations(self):
        # The guard's Rust inputs come from tests/recorder.rs's own mod/#[path]/
        # include_*! declarations plus the crate manifest — no preset list to
        # drift (#1139 fix-r2 P1-2).
        module = runner('recorder')
        discovered = {path.relative_to(REPO).as_posix()
                      for path in module.rust_criterion_closure(module.CRITERION_ENTRY)}
        expected = {
            'src/pilotdeck-gateway/tests/recorder.rs',
            'src/pilotdeck-gateway/tests/common/baseline.rs',
            'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
            'src/pilotdeck-gateway/tests/fixtures/_shared/regeneration-masks.tsv',
        }
        self.assertTrue(
            discovered == expected,
            '#1139 fix-r2: the real criterion chain must be discovered as declared')

    def test_declared_rust_dependency_joins_the_criterion_guard(self):
        # A helper declared in the criterion entry — not in any preset list —
        # is discovered together with its own include inputs, recursively.
        module = runner('recorder')
        (self.root / 'entry.rs').write_text(
            '#[path = "mods/helper.rs"]\n'
            'mod helper;\n'
            'mod plain;\n'
            'const TABLE: &str = include_str!("table.tsv");\n')
        (self.root / 'plain.rs').write_text('const PLAIN: u8 = 1;\n')
        (self.root / 'table.tsv').write_text('column\n')
        (self.root / 'mods').mkdir()
        (self.root / 'mods/helper.rs').write_text(
            'const DATA: &[u8] = include_bytes!("../data.bin");\n')
        (self.root / 'data.bin').write_bytes(b'#1139 binary input\n')
        discovered = {path.relative_to(self.root).as_posix()
                      for path in module.rust_criterion_closure(self.root / 'entry.rs')}
        self.assertTrue(
            discovered == {'entry.rs', 'mods/helper.rs', 'plain.rs', 'table.tsv', 'data.bin'},
            '#1139 fix-r2: declared Rust dependencies must join the criterion guard')

    def test_unresolvable_rust_declarations_fail_closed(self):
        module = runner('recorder')
        for source, fragment in (
            ('mod ghost;\n', 'does not resolve'),
            ('#[path = "missing.rs"]\nmod helper;\n', 'missing Rust criterion module file'),
            ('#[path = "helper.rs"]\nconst X: u8 = 0;\n', 'without a module declaration'),
            ('const T: &str = include_str!(name);\n', 'non-literal include'),
            ('include!("gen.rs");\n', 'plain include! is unsupported'),
            ('mod inner {\n}\n', 'inline module'),
            ('const S: &str = "unterminated\n', 'unterminated Rust string'),
        ):
            with self.subTest(fragment=fragment):
                (self.root / 'entry.rs').write_text(source)
                error = None
                try:
                    module.rust_criterion_closure(self.root / 'entry.rs')
                except (ValueError, FileNotFoundError) as caught:
                    error = str(caught)
                self.assertTrue(
                    error is not None and fragment in error,
                    '#1139 fix-r2: unresolvable Rust declarations must fail closed: ' + fragment)
        # Both candidate files at once is ambiguous, never a silent pick.
        (self.root / 'entry.rs').write_text('mod both;\n')
        (self.root / 'both.rs').write_text('')
        (self.root / 'both').mkdir()
        (self.root / 'both/mod.rs').write_text('')
        error = None
        try:
            module.rust_criterion_closure(self.root / 'entry.rs')
        except ValueError as caught:
            error = str(caught)
        self.assertTrue(
            error is not None and 'does not resolve' in error,
            '#1139 fix-r2: an ambiguous module declaration must fail closed')

    def test_dirty_rust_criterion_is_refused_before_positive(self):
        for relative in sorted(self.criterion_inputs()):
            with self.subTest(path=relative):
                target = REPO / relative
                with mutating_sources(REPO, [target]):
                    target.write_bytes(
                        target.read_bytes() + b'\n// #1139 dirty criterion probe\n')
                    failure = None
                    try:
                        with runner('recorder').guarded_inputs():
                            pass
                    except Exception as error:
                        failure = error
                    self.assertTrue(
                        failure is not None
                        and type(failure).__name__ == 'SourceBaselineError'
                        and relative in str(failure),
                        '#1139 fix-r2: dirty Rust criterion input must be refused: ' + relative)


if __name__ == '__main__':
    unittest.main(verbosity=2)
