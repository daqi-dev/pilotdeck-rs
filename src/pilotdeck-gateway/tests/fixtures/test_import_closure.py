# anchor-selfcheck: unsupported unit tests of the count runners' closure guards
"""Run directly with Python on a committed, clean checkout; no Cargo needed.

Covers both count runners (#1129 / #1139 fix-r1): unrecognized dynamic imports
must fail closed instead of being silently skipped, literal dynamic imports
must join the closure, the fixed baseline loader stays allowed, the real
generators' closures stay the documented file sets, and a dirty Rust
criterion input must be refused before any positive control. The dirty-input
cases mutate repository files in place, so they run inside the shared source
guard and restore byte-for-byte on exit; on a tree whose guarded inputs differ
from HEAD they fail by naming the offending file.
"""
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
FIXTURES = Path(__file__).resolve().parent
REPO = FIXTURES.parents[3]
sys.path.insert(0, str(FIXTURES / '_shared'))
from negative_process import mutating_sources

COUNT_RUNNERS = {
    'recorder': FIXTURES / 'recorder/fix3-negative.py',
    'run-registry': FIXTURES / 'run-registry/count-negative.py',
}
# tests/recorder.rs binds #[path] common/baseline.rs -> regeneration_masks.rs
# (include_str! of _shared/regeneration-masks.tsv) plus the crate manifest.
RECORDER_CRITERION_INPUTS = (
    'src/pilotdeck-gateway/tests/recorder.rs',
    'src/pilotdeck-gateway/tests/common/baseline.rs',
    'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
    'src/pilotdeck-gateway/tests/fixtures/_shared/regeneration-masks.tsv',
    'src/pilotdeck-gateway/Cargo.toml',
)
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
        # errors, never silently skipped dependencies (#1139 fix-r1 P1).
        for name in COUNT_RUNNERS:
            for source in (
                'await import(`./scenario-${index}.mts`);',
                'await import(resolved);',
                "await import('./' + name + '.mjs');",
            ):
                with self.subTest(runner=name, source=source):
                    error = self.closure_error(name, source)
                    self.assertTrue(
                        error is not None
                        and 'unrecognized dynamic import' in error,
                        '#1139 fix-r1: unrecognized dynamic import must fail closed')

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
        # literal specifiers; they must join the closure, not fall out of it.
        for name in COUNT_RUNNERS:
            for index, form in enumerate((
                "await import /* loader */ ('./dep.mjs');",
                'await import(`./dep.mjs`);',
            )):
                with self.subTest(runner=name, form=form):
                    entry = self.entry(f'entry{index}.mts', form)
                    self.entry('dep.mjs', 'export const value = 1;\n')
                    closure = runner(name).local_import_closure(entry, self.root)
                    self.assertTrue(
                        {path.name for path in closure} == {entry.name, 'dep.mjs'},
                        '#1139 fix-r1: literal dynamic imports must join the closure')

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
        # The one allowed computed specifier: the fixed TS baseline loader
        # whose source hashes oracle.json records (generate.mts:34 shape).
        for name in COUNT_RUNNERS:
            with self.subTest(runner=name):
                error = self.closure_error(
                    name,
                    "const load = (path) => import(pathToFileURL("
                    "join(baseline, 'src', path)).href);\n")
                self.assertTrue(
                    error is None,
                    '#1139 fix-r1: baseline loader dynamic import must stay allowed')

    def test_real_generator_closures_are_the_documented_files(self):
        for name in COUNT_RUNNERS:
            with self.subTest(runner=name):
                closure = runner(name).local_import_closure(
                    FIXTURES / f'{name}/generate.mts', REPO)
                relative = {path.relative_to(REPO).as_posix() for path in closure}
                self.assertTrue(
                    relative == DOCUMENTED_CLOSURES[name],
                    '#1139 fix-r1: real generator closure must stay the documented file set')


class GuardedInputTests(unittest.TestCase):
    """recorder's guard must refuse a dirty Rust criterion before any control."""

    def test_dirty_rust_criterion_is_refused_before_positive(self):
        for relative in RECORDER_CRITERION_INPUTS:
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
                        '#1139 fix-r1: dirty Rust criterion input must be refused: ' + relative)


if __name__ == '__main__':
    unittest.main(verbosity=2)
