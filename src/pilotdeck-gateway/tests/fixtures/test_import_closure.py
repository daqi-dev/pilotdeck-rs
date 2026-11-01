# anchor-selfcheck: unsupported unit tests of the count runners' closure guards
"""Run directly with Python; no tree lock and no Cargo is launched here.

Covers both count runners (#1129 / #1139): unrecognized dynamic imports must
fail closed instead of being silently skipped, the only allowed computed
arguments are the two fixed baseline loaders verbatim (never a shape that
merely mentions baseline), literal dynamic imports — including trailing
comments and commas — must join the closure, static imports join it in any
spacing, file: URLs follow the absolute-path rules, the real generators'
closures stay the documented file sets, and the two runners' closure helpers
stay synchronized. CriterionCoverageTests rides on the shared #1166
proc-macro2 scanner instead of a hand-written Rust lexer (#1174): the
recorder criterion chain is a scanner input set, and the runner's entry
anchor fails closed when the criterion is not in that set. The scanner tests
read the prebuilt example binary (build it with
``cargo build --example workspace_inputs`` first); the fail-closed anchor test
is a pure mock. Everything here only reads the repository, so an uncommitted
tree is fine.
"""
import ast
import importlib.util
import re
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True
FIXTURES = Path(__file__).resolve().parent
REPO = FIXTURES.parents[3]
# negative_process.py sits in the fixtures root; only negative_control.py,
# imported first by the runners, would otherwise put it on the path.
sys.path.insert(0, str(FIXTURES / '_shared'))
sys.path.insert(0, str(FIXTURES))

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
# The chain rust_criterion_inputs() documents as the recorder criterion:
# tests/recorder.rs pulls its inputs in through mod/#[path]/include
# declarations, and mutating_sources proves all of them equal HEAD through
# the shared #1166 scanner.
CRITERION_CHAIN = {
    'src/pilotdeck-gateway/tests/recorder.rs',
    'src/pilotdeck-gateway/tests/common/baseline.rs',
    'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
    'src/pilotdeck-gateway/tests/fixtures/_shared/regeneration-masks.tsv',
}
# The closure/guard helpers below are a hand-maintained double copy (M6 #1099
# will extract them); the synchronization test keeps the two from drifting.
SHARED_HELPERS = (
    '_skip_spacing', '_argument_without_trailing', '_dynamic_import_arguments',
    '_static_import_specifiers', '_dynamic_import_targets',
    'local_import_closure', 'copy_local_import_closure',
)
# The hand-written Rust lexers of the pre-rework implementation; a runner
# carrying one again would violate #1174's single-scanner rule.
REMOVED_RUST_LEXERS = ('_rust_masked', '_rust_declarations', 'rust_criterion_closure')


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
        # errors, never silently skipped dependencies; the baseline-mentioning
        # shapes are the review counterexamples: only the two verbatim loaders
        # are allowed, never a word match (#1139).
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
                        '#1139: unrecognized dynamic import must fail closed')

    def test_absolute_dynamic_import_outside_repository_is_rejected(self):
        for name in COUNT_RUNNERS:
            with self.subTest(runner=name):
                error = self.closure_error(name, 'await import(\'/etc/hosts\');')
                self.assertTrue(
                    error is not None
                    and 'outside the repository' in error,
                    '#1139: out-of-root absolute dynamic import must fail closed')

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
                            f'#1139: literal dynamic imports must join the '
                            f'closure: {error}')
                    self.assertTrue(
                        {path.name for path in closure} == {entry.name, 'dep.mjs'},
                        '#1139: literal dynamic imports must join the closure')

    def test_static_import_edge_forms_join_the_closure(self):
        # Zero whitespace and comment-separated static imports are legal JS
        # the old \bimport\s+ regex silently dropped; they must join too (#1139).
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
                        '#1139: static import edge forms must join the closure')

    def test_file_url_dynamic_imports_follow_absolute_rules(self):
        # A file: URL is a local dependency, not a bare external module: an
        # in-root one joins the closure and an out-of-root one is refused,
        # exactly like a plain absolute path (#1139).
        for name in COUNT_RUNNERS:
            with self.subTest(runner=name):
                entry = self.entry(
                    'generate.mts',
                    f"await import('file://{(self.root / 'abs.mjs').as_posix()}');")
                self.entry('abs.mjs', 'export const value = 1;\n')
                closure = runner(name).local_import_closure(entry, self.root)
                self.assertTrue(
                    {path.name for path in closure} == {'generate.mts', 'abs.mjs'},
                    '#1139: in-root file: URL dynamic import must join the closure')
                error = self.closure_error(name, "await import('file:///etc/hosts');")
                self.assertTrue(
                    error is not None
                    and 'outside the repository' in error,
                    '#1139: out-of-root file: URL dynamic import must fail closed')

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
                    '#1139: in-repository absolute dynamic import must join the closure')

    def test_baseline_loader_dynamic_import_is_allowed(self):
        # The only allowed computed specifiers: the two fixed TS baseline
        # loaders, verbatim, whose source hashes oracle.json records
        # (recorder/generate.mts and run-registry/generate.mts).
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
                        '#1139: baseline loader dynamic imports must stay allowed')

    def test_real_generator_closures_are_the_documented_files(self):
        for name in COUNT_RUNNERS:
            with self.subTest(runner=name):
                closure = runner(name).local_import_closure(
                    FIXTURES / f'{name}/generate.mts', REPO)
                relative = {path.relative_to(REPO).as_posix() for path in closure}
                self.assertTrue(
                    relative == DOCUMENTED_CLOSURES[name],
                    '#1139: real generator closure must stay the documented file set')

    def test_runner_closure_helpers_stay_synchronized(self):
        # The closure helpers are a hand-maintained double copy (M6 #1099 will
        # extract them); until then, drift between the two runners must fail
        # here instead of silently diverging (#1139 sibling review). Issue
        # markers are the only allowed difference.
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
            '#1139: both runners must define the shared closure helpers')
        self.assertTrue(
            normalized['recorder'] == normalized['run-registry'],
            '#1139: the runners\' closure helpers must stay synchronized')


class CriterionCoverageTests(unittest.TestCase):
    """recorder's criterion anchor rides on the shared #1166 scanner (#1174)."""

    def test_criterion_entry_is_a_scanner_input(self):
        module = runner('recorder')
        self.assertIn(
            module.CRITERION_ENTRY, module.rust_criterion_inputs(),
            '#1139: the recorder criterion entry must be a #1166 scanner input')

    def test_documented_criterion_chain_is_scanner_covered(self):
        # The whole chain the runner documents — recorder.rs through its
        # mod/#[path]/include declarations down to the masks TSV — must be in
        # the scanner's input set, so mutating_sources guards all of it.
        inputs = runner('recorder').rust_criterion_inputs()
        missing = CRITERION_CHAIN - inputs
        self.assertTrue(
            not missing,
            f'#1139: criterion chain fell out of the scanner input set: {sorted(missing)}')

    def test_criterion_entry_outside_scanner_inputs_fails_closed(self):
        # The entry anchor must error, never silently pass, when the criterion
        # is not among the scanner's inputs (moved or renamed criterion).
        module = runner('recorder')
        with mock.patch.object(module, '_workspace_literal_inputs',
                               return_value=()):
            with self.assertRaises(RuntimeError) as caught:
                module.rust_criterion_inputs()
        self.assertIn(
            'not a #1166 scanner input', str(caught.exception),
            '#1139: a criterion outside the scanner input set must fail closed')

    def test_runners_keep_no_handwritten_rust_lexer(self):
        # #1174: the #1166 scanner is the only Rust lexer; the reworked
        # runners must not resurrect a private one.
        for name in COUNT_RUNNERS:
            module = runner(name)
            for lexer in REMOVED_RUST_LEXERS:
                self.assertFalse(
                    hasattr(module, lexer),
                    f'#1174: {name} runner carries a hand-written Rust lexer: {lexer}')


if __name__ == '__main__':
    unittest.main(verbosity=2)
