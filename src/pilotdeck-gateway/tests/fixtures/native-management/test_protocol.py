# anchor-selfcheck: unsupported isolated runner protocol tests, no product source mutations
"""Exercise the #1440 restored-inside-lease window with temporary fake sources.

#1642 fix-r2 additions: the guard-failure exit codes follow the four-way table
(refusal before the lease, red run after it), SKIPPED rows survive a guard
failure, and a drifted frozen roster refuses with rc 2.
"""
from contextlib import contextmanager
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from unittest.mock import patch

FIXTURES = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('native_management_controls',
                                              Path(__file__).with_name('negative_controls.py'))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)

GREEN = 'test bound_test ... ok\n\ntest result: ok. 1 passed; 0 failed;\n'


class Protocol(unittest.TestCase):
    def exercise(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1440-protocol-') as temporary:
            root = Path(temporary)
            source = root / 'fake-source.txt'
            source.write_text('original')
            out = root / 'result'
            rows = [(name, source, 'original', 'changed', 'bound_test', '#1231 bound')
                    for name in ('first', 'second')]
            active = False
            restored = 0
            compilations = 0
            pending_restore = False

            @contextmanager
            def guard(_root, _paths):
                nonlocal active
                active = True
                try:
                    yield {source: b'original'}
                finally:
                    # A failed runner must still attempt its per-case restore.
                    self.assertTrue(source.read_text() == 'original', '#1440 protocol restores source')
                    active = False

            def run(command, **kwargs):
                nonlocal restored, compilations, pending_restore
                self.assertTrue(active, '#1440 protocol cargo stays inside the lease')
                if '--no-run' in command:
                    compilations += 1
                    pending_restore = True
                    return subprocess.CompletedProcess(command, 0, '', '')
                if source.read_text() == 'original' and pending_restore:
                    restored += 1
                    pending_restore = False
                return subprocess.CompletedProcess(command, 0, GREEN, '')

            def classify(_result, _case):
                return ('KILLED', 'bound assertion')

            # #1642 fix-r5 (review concurrency P2#1, the r4 P1-D / a7 shape
            # drive() below and native-dispatch:105 already carry): the real
            # write_source_atomic staged every write through the real tree's
            # `.negctl-publish/` with no source lease held, and a TMPDIR on
            # another device failed the restore-phase writes with EXDEV -- a
            # false red that happened to satisfy no assertion here while
            # leaving residue in the shared tree. plain_writer lands the
            # bytes directly, and the writes counter is what makes the patch
            # load-bearing (dropping it empties the counter).
            writes = []

            def plain_writer(_root, path, data, **_kwargs):
                path = Path(path)
                self.assertTrue(root in path.parents,
                                '#1642 fix-r5 the protocol writes must stay inside the '
                                'temporary root, never the real tree')
                writes.append(path)
                path.write_bytes(data)

            argv = ['controls', str(out)]
            with patch.object(RUNNER, 'mutations', rows), \
                 patch.object(RUNNER, 'mutating_sources', guard), \
                 patch.object(RUNNER, 'run_owned', run), \
                 patch.object(RUNNER, 'wait_for_slot', return_value=subprocess.CompletedProcess([], 0, '', '')), \
                 patch.object(RUNNER, 'classify', classify), \
                 patch.object(RUNNER, 'write_source_atomic', plain_writer), \
                 patch.object(sys, 'argv', argv), \
                 patch.dict(os.environ, CARGO_TARGET_DIR=str(root / 'unused-target')):
                code = RUNNER.main()
            self.assertTrue(writes,
                            '#1642 fix-r5 the batch must publish its mutant bytes through '
                            'the patched writer (the writes counter is what makes the '
                            'plain_writer patch load-bearing)')
            return code, json.loads((out / 'summary.json').read_text())['controls'], restored, compilations

    def test_restored_runs_inside_lease(self):
        code, rows, restored, compilations = self.exercise()
        self.assertTrue(code == 0 and all(row['status'] == 'KILLED' for row in rows),
                        '#1440 protocol successful batch')
        self.assertTrue(restored == 2 and compilations == 2,
                        '#1440 protocol compiles and restores every case inside the lease')

    # #1642 fix-r2: the shared driving rig for the guard-failure phases. The
    # rows reuse the LINUX_ONLY name for the platform-absent row so main()'s
    # own filter drops it from the loop, exactly like a darwin host.
    # #1642 fix-r3 (review a7, MM concurrency#2): write_source_atomic is
    # patched unconditionally -- with the real writer, a TMPDIR on another
    # device failed the guard-phase cases with EXDEV while the assertions
    # still read green (a false PASS), and the fake sources were published
    # through the real tree's `.negctl-publish/` with no source lease held.
    # The restore case injects its own writer; the default one just lands the
    # bytes, which is all the restoration re-read below checks.
    def drive(self, rows, guard, *, platform_skipped=(), restore=None):

        def plain_writer(_root, path, data, **_kwargs):
            path.write_bytes(data)

        with tempfile.TemporaryDirectory(prefix='pdrs-1642-protocol-') as temporary:
            root = Path(temporary)
            out = root / 'result'

            def run(command, **kwargs):
                return subprocess.CompletedProcess(command, 0, GREEN, '')

            def classify(_result, _case):
                return ('KILLED', 'bound assertion')

            argv = ['controls', str(out)]
            patches = [patch.object(RUNNER, 'mutations', rows),
                       patch.object(RUNNER, 'mutating_sources', guard),
                       patch.object(RUNNER, 'run_owned', run),
                       patch.object(RUNNER, 'wait_for_slot',
                                    return_value=subprocess.CompletedProcess([], 0, '', '')),
                       patch.object(RUNNER, 'classify', classify),
                       patch.object(RUNNER, 'skipped_on_this_platform',
                                    lambda selected: [case for case in selected
                                                      if case.mutation in platform_skipped]),
                       patch.object(RUNNER, 'write_source_atomic',
                                    restore if restore is not None else plain_writer),
                       patch.object(sys, 'argv', argv),
                       patch.dict(os.environ, CARGO_TARGET_DIR=str(root / 'unused-target'))]
            for applied in patches:
                applied.start()
            try:
                try:
                    code = RUNNER.main()
                except SystemExit as refusal:
                    # A refusal (rc 2) or a delayed signal (rc 128+signum)
                    # leaves main() through SystemExit; the code is the
                    # process exit shape, and the evidence shape below still
                    # tells whether anything was left behind.
                    code = refusal.code
            finally:
                for applied in patches:
                    applied.stop()
            # The evidence is read here, inside the temporary directory's
            # lifetime: this with-block deletes the tree on return, so a
            # returned path would read as withdrawn even for a green run.
            # `claimed` is the enter-refusal question (was the evidence
            # directory withdrawn?), `summary` the publish question (was a
            # summary.json left behind at all?).
            claimed = out.exists()
            summary = None
            if claimed and (out / 'summary.json').is_file():
                summary = json.loads((out / 'summary.json').read_text())['controls']
            return code, claimed, summary

    @staticmethod
    def rows(temporary):
        source = temporary / 'fake-source.txt'
        source.write_text('original')
        return source, [
            ('home-non-utf8-accepted', source, 'original', 'changed', 'bound_test', '#1231 bound'),
            ('second', source, 'original', 'changed', 'bound_test', '#1231 bound'),
        ]

    def entered_guard(self, source, *, fail_exit=False, fail_enter=False):
        @contextmanager
        def guard(_root, _paths):
            if fail_enter:
                raise RuntimeError('source lock busy')
            try:
                yield {source: b'original'}
            finally:
                if fail_exit:
                    raise RuntimeError('guard exit failed')
        return guard

    def test_enter_phase_failure_is_a_refusal(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-protocol-') as temporary:
            source, rows = self.rows(Path(temporary))
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                code, claimed, summary = self.drive(rows, self.entered_guard(source, fail_enter=True))
            self.assertTrue(code == 2,
                            '#1642 fix-r3 a guard that never started (busy lock, dirty tree) '
                            'refuses with rc 2')
            self.assertTrue('guard refused to start' in stderr.getvalue(),
                            '#1642 fix-r3 the enter-phase refusal must name the guard failure')
            # MM concurrency#1: the enter-phase guard failure used to publish
            # an all-INVALID summary over an empty OUT, and the host
            # re-running with rc 2 died on the leftover mkdir; a refusal
            # withdraws the empty claim and publishes nothing.
            self.assertFalse(claimed,
                            '#1642 fix-r3 an enter-phase refusal must withdraw the claimed '
                            'evidence directory (no summary, no leftover claim shape)')
            self.assertTrue(summary is None,
                            '#1642 fix-r3 an enter-phase refusal must not publish a summary')

    def test_exit_phase_failure_is_a_red_run(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-protocol-') as temporary:
            source, rows = self.rows(Path(temporary))
            code, claimed, summary = self.drive(rows, self.entered_guard(source, fail_exit=True))
            self.assertTrue(code == 1,
                            '#1642 fix-r3 a guard that fails after the body ran is a red run (rc 1), not a refusal')
            self.assertTrue(claimed,
                            '#1642 fix-r3 a red run keeps its evidence directory')
            self.assertTrue(all(row['status'] == 'INVALID' for row in summary),
                            '#1642 fix-r3 a failed guard revokes every row it recorded')
            self.assertTrue(all('source guard' in row['reason']
                                and 'guard exit failed' in row['reason'] for row in summary),
                            '#1642 fix-r3 the rows are revoked by the guard failure itself, '
                            'not by some earlier per-case error')

    def test_restore_phase_failure_is_a_red_run(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-protocol-') as temporary:
            source, rows = self.rows(Path(temporary))

            def corrupt(_root, path, _data):
                path.write_bytes(b'corrupted')

            code, claimed, summary = self.drive(rows, self.entered_guard(source),
                                                restore=corrupt)
            self.assertTrue(code == 1,
                            '#1642 fix-r3 a restore-verification failure is a red run (rc 1), not a refusal')
            self.assertTrue(claimed,
                            '#1642 fix-r3 a red run keeps its evidence directory')
            self.assertTrue(all(row['status'] == 'INVALID' for row in summary),
                            '#1642 fix-r3 a failed restore revokes every row it recorded')

    def test_guard_failure_preserves_skipped_rows(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-protocol-') as temporary:
            source, rows = self.rows(Path(temporary))
            code, claimed, summary = self.drive(rows, self.entered_guard(source, fail_exit=True),
                                                platform_skipped={'home-non-utf8-accepted'})
            self.assertTrue(code == 1,
                            '#1642 fix-r2 a failed guard after SKIPPED rows exist is still a red run')
            skipped = [row for row in summary if row['mutation'] == 'home-non-utf8-accepted']
            run_rows = [row for row in summary if row['mutation'] == 'second']
            self.assertTrue(len(skipped) == 1 and skipped[0]['status'] == 'SKIPPED'
                            and 'source guard' not in skipped[0]['reason'],
                            '#1642 fix-r2 a guard failure must not flip platform-absent SKIPPED rows to INVALID')
            self.assertTrue(len(run_rows) == 1 and run_rows[0]['status'] == 'INVALID'
                            and 'source guard' in run_rows[0]['reason'],
                            '#1642 fix-r2 a guard failure still revokes the rows it ran')

    # #1642 fix-r3 (review a9, DS r2c#2): the delayed-signal SystemExit that
    # mutating_sources raises after a verified restore must survive main()'s
    # handler untouched -- it used to be swallowed into invalidate + publish
    # + rc 1 (an all-INVALID summary over a restored tree, exit rewritten
    # from 143 to 1).
    def test_deferred_signal_survives_the_guard_handler(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-protocol-') as temporary:
            source, rows = self.rows(Path(temporary))

            @contextmanager
            def signal_guard(_root, _paths):
                yield {source: b'original'}
                raise SystemExit(143)

            with redirect_stderr(io.StringIO()):
                code, claimed, summary = self.drive(rows, signal_guard)
            self.assertTrue(code == 143,
                            '#1642 fix-r3 a SIGTERM deferred through the restore window '
                            'exits 128+signum, not the rewritten rc 1')
            self.assertTrue(summary is None,
                            '#1642 fix-r3 a deferred signal publishes no summary over the '
                            'restored tree')
            self.assertTrue(source.read_text() == 'original',
                            '#1642 fix-r3 the restored tree stays restored through the '
                            'signal exit')

    # #1642 fix-r3 (review b, item 4): the three startup refusals the four-way
    # table promises, driven in-process -- the same three the
    # test_negative_optimize_guards.py matrix checks through a real subprocess.
    def test_unknown_name_refuses_without_claiming_evidence(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-protocol-') as temporary:
            source, rows = self.rows(Path(temporary))
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                with patch.object(sys, 'argv',
                                  ['controls', str(Path(temporary) / 'result'),
                                   'no-such-mutation-1642']), \
                     patch.dict(os.environ, CARGO_TARGET_DIR=str(Path(temporary) / 't')):
                    with self.assertRaises(SystemExit) as raised:
                        RUNNER.main()
            self.assertTrue(raised.exception.code == 2,
                            '#1642 fix-r3 an unknown mutation name refuses with rc 2')
            self.assertTrue('no-such-mutation-1642' in stderr.getvalue()
                            and 'unknown' in stderr.getvalue(),
                            '#1642 fix-r3 the refusal must name the unknown mutation')
            self.assertFalse((Path(temporary) / 'result').exists(),
                            '#1642 fix-r3 the refusal must not claim the evidence directory')

    def test_existing_output_directory_refuses(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-protocol-') as temporary:
            out = Path(temporary) / 'result'
            out.mkdir()
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                # PDRS_BASELINE is present so the refusal under test really is
                # the claimed directory, not the oracle-corruption guard.
                with patch.object(sys, 'argv', ['controls', str(out)]), \
                     patch.dict(os.environ, CARGO_TARGET_DIR=str(Path(temporary) / 't'),
                                PDRS_BASELINE=str(Path(temporary) / 'baseline')):
                    with self.assertRaises(SystemExit) as raised:
                        RUNNER.main()
            self.assertTrue(raised.exception.code == 2,
                            '#1642 fix-r3 an existing output directory refuses with rc 2')
            self.assertTrue('already exists' in stderr.getvalue(),
                            '#1642 fix-r3 the refusal must say the directory is taken')
            self.assertEqual(list(out.iterdir()), [],
                            '#1642 fix-r3 the refusal must not write into another run\'s directory')

    def test_missing_target_refuses_without_claiming_evidence(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-protocol-') as temporary:
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                with patch.object(sys, 'argv',
                                  ['controls', str(Path(temporary) / 'result')]), \
                     patch.dict(os.environ, CARGO_TARGET_DIR=''):
                    with self.assertRaises(SystemExit) as raised:
                        RUNNER.main()
            self.assertTrue(raised.exception.code == 2,
                            '#1642 fix-r3 a blank CARGO_TARGET_DIR refuses with rc 2')
            self.assertTrue('CARGO_TARGET_DIR' in stderr.getvalue(),
                            '#1642 fix-r3 the refusal must name the unusable target')
            self.assertFalse((Path(temporary) / 'result').exists(),
                            '#1642 fix-r3 the refusal must not claim the evidence directory')

    def test_roster_drift_refuses_with_rc2(self):
        source = Path(RUNNER.__file__).read_text()
        pattern = "'runs-bound-omitted',\n)"
        self.assertTrue(source.count(pattern) == 1,
                        '#1642 fix-r2 the roster drift probe must replace exactly one EXPECTED_NAMES entry')
        drifted = source.replace(pattern, "'runs-bound-omitted-renamed',\n)", 1)
        # #1642 fix-r3 (P1-1): the drifted copy must sit beside the original
        # negative_controls.py, not in a shallow TemporaryDirectory. The copy
        # runs the module-level `ROOT = Path(__file__).resolve().parents[5]`
        # at import time, and a CI /tmp path (3 levels) died with an
        # IndexError before the roster refusal could even raise (job
        # 131861169897); beside the original the depth shape is identical by
        # construction. mkstemp keeps the name out of the tree's scans and
        # the finally unlinks it; bytecode caching is off so the fixtures
        # directory gains no __pycache__ from the copy.
        descriptor, copy_name = tempfile.mkstemp(dir=Path(RUNNER.__file__).parent,
                                                  prefix='drifted-1642-', suffix='.py')
        copy = Path(copy_name)
        sys.path.insert(0, str(FIXTURES))
        stderr = io.StringIO()
        previous = sys.dont_write_bytecode
        sys.dont_write_bytecode = True
        try:
            with os.fdopen(descriptor, 'w') as handle:
                handle.write(drifted)
            spec = importlib.util.spec_from_file_location('drifted_native_management_controls', copy)
            module = importlib.util.module_from_spec(spec)
            with redirect_stderr(stderr):
                code = None
                try:
                    spec.loader.exec_module(module)
                except SystemExit as refusal:
                    code = refusal.code
        finally:
            sys.dont_write_bytecode = previous
            sys.path.remove(str(FIXTURES))
            copy.unlink(missing_ok=True)
        self.assertTrue(code == 2,
                        '#1642 fix-r2 roster drift is a refusal (rc 2, never started), '
                        'never a failed control (rc 1) or a silent pass')
        self.assertTrue('runs-bound-omitted-renamed' in stderr.getvalue()
                        and 'roster' in stderr.getvalue(),
                        '#1642 fix-r2 the roster refusal must name the drift')

    def test_binding_checker_scope(self):
        # The fixture's own discrimination self-check must hold on this checker.
        RUNNER.check_binding_scope()
        management = RUNNER.NATIVE_PROCESS / 'management_tests.rs'
        # #1642 fix-r3: the local fixture carries the same shapes the
        # in-fixture self-check pins -- bound tests must be `#[test]`
        # functions (review a5: a plain same-named function cannot stand in
        # for a deleted test), and a marker only a comment carries anchors
        # nothing (review a1: comments are blanked before any literal is
        # matched, so `// panic!("marker")` is not an anchoring text).
        fixture = {
            management: '#[test]\n'
                        'fn target() {\n    helper();\n    own_template();\n'
                        '    tail_template();\n'
                        '    // panic!("#1642 comment carrier");\n'
                        '}\n'
                        '#[test]\n'
                        'fn sibling_test() {\n    panic!("#1642 marker of another test");\n}\n'
                        'fn helper() {\n    panic!("#1642 marker via helper");\n}\n'
                        'fn own_template() {\n    panic!("#1642 covers {method}");\n}\n'
                        'fn tail_template() {\n    panic!("#1642 head {method} tail");\n}\n'
                        'fn plain_carrier() {\n    panic!("#1642 plain non-test carrier");\n}\n'
                        '/*\n'
                        '#[test]\n'
                        'fn ghost() {\n'
                        '    panic!("#1642 block comment carrier");\n'
                        '}\n'
                        '*/\n',
        }

        def bound(test, assertion):
            return [RUNNER.Case('row', management, 'a', 'b', test, assertion)]

        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::target', '#1642 marker via helper'),
            sources=fixture) == [],
            '#1642 fix-r2 the binding checker must keep its own assertion')
        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::target', '#1642 covers apply'),
            sources=fixture) == [],
            '#1642 fix-r2 the binding checker must accept a template instantiation')
        # #1642 fix-r5 (review boundary P1#1): a template with static text
        # after the placeholder must not anchor an assertion that diverges
        # inside the placeholder -- the fallback's `str.endswith('')` used to
        # accept any tail, so "#1642 head XXX" read as an instantiation of
        # "#1642 head {method} tail".
        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::target', '#1642 head apply tail'),
            sources=fixture) == [],
            '#1642 fix-r5 the binding checker must keep accepting a full '
            'trailing-template instantiation')
        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::target', '#1642 head XXX'),
            sources=fixture) == ['row'],
            '#1642 fix-r5 the binding checker must refuse a placeholder-diverging '
            'assertion (the endswith(\'\') off-by-one used to accept it)')
        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::target', '#1642 marker of another test'),
            sources=fixture) == ['row'],
            '#1642 fix-r2 the binding checker must refuse a sibling-only assertion')
        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::renamed', '#1642 marker via helper'),
            sources=fixture) == ['row'],
            '#1642 fix-r2 the binding checker must refuse an unbound test name')
        # #1642 fix-r3 (review a1): a comment-only carrier is not an anchor --
        # neither a dead line inside the live body nor a whole block function
        # (which must not count as a live `#[test] fn` either).
        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::target', '#1642 comment carrier'),
            sources=fixture) == ['row'],
            '#1642 fix-r3 the binding checker must refuse a comment-only carrier')
        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::ghost', '#1642 block comment carrier'),
            sources=fixture) == ['row'],
            '#1642 fix-r3 the binding checker must refuse a block-comment function')
        # #1642 fix-r3 (review a5): a plain non-test function carrying the
        # assertion text cannot stand in for a deleted bound test.
        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::plain_carrier',
                  '#1642 plain non-test carrier'),
            sources=fixture) == ['row'],
            '#1642 fix-r3 the binding checker must refuse a non-test carrier')
        # #1642 fix-r4 (review P1-C): the real test is deleted and a raw
        # string is its only carrier -- `#[test]` on its own line inside the
        # literal used to satisfy the attribute walk and the `fn` used to be
        # a live candidate, so --check stayed green on prose. The candidate
        # scan now reads the literal-blanked structure view.
        literal_carrier = {
            management: '#[test]\n'
                        'fn sibling_test() {\n    panic!("#1642 marker of another test");\n}\n'
                        'fn documented() {\n'
                        '    let sample = r#"\n'
                        '#[test]\n'
                        'fn target() {\n'
                        '    panic!("#1642 literal carrier");\n'
                        '}\n'
                        '"#;\n'
                        '    let _ = sample;\n'
                        '}\n',
        }
        self.assertTrue(RUNNER.binding_failures(
            bound('native_process::management_tests::target',
                  '#1642 literal carrier'),
            sources=literal_carrier) == ['row'],
            '#1642 fix-r4 the binding checker must refuse a literal-only carrier')
        # #1642 fix-r4 (review P1-C): the two views the checker reads stay
        # equal to the shared single-view scanner, so the local dual view
        # cannot drift back into carrying literal prose.
        try:
            RUNNER.verify_structure_matches_shared_scanner(fixture[management])
            RUNNER.verify_structure_matches_shared_scanner(literal_carrier[management])
        except ValueError as error:
            self.assertTrue(False,
                            f'#1642 fix-r4 the binding views drifted from the shared scanner: {error}')
        # #1642 fix-r3 (reviews a2/a3/a4): the `crate::` branch needs its own
        # synthetic module on disk (`_module_file` resolves through a real
        # file), so these cells map a module name to a temporary file. The
        # recursion cell doubles as the termination proof: reaching the
        # assertion at all means `_reachable_text` came back.
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-scope-') as temporary:
            synthetic = Path(temporary) / 'synthetic_1642.rs'
            synthetic.write_text(
                'fn same_name_helper() {\n'
                '    panic!("#1642 marker via the other module\'s helper");\n'
                '}\n'
                'fn recursing_helper() {\n'
                '    crate::synthetic_1642::recursing_helper();\n'
                '}\n')
            cross = fixture[management] + (
                '#[test]\n'
                'fn cross_file_helper_test() {\n    same_name_helper();\n}\n'
                'fn same_name_helper() {\n'
                '    crate::synthetic_1642::same_name_helper();\n'
                '}\n'
                '#[test]\n'
                'fn recursion_test() {\n'
                '    crate::synthetic_1642::recursing_helper();\n'
                '}\n'
                '#[test]\n'
                'fn rotted_map_test() {\n'
                '    crate::rotted_1642::only_in_the_rotted_map();\n'
                '}\n')
            cross_fixture = {management: cross}

            def cross_bound(test, assertion):
                return [RUNNER.Case('row', management, 'a', 'b', test, assertion)]

            with patch.object(RUNNER, 'CRATE_MODULE_FILES',
                              {'synthetic_1642': synthetic}), \
                 patch.object(RUNNER, '_MODULE_FUNCTIONS', {}):
                # a3: the same-named helper is walked in the other module's
                # own scope, so its marker stays reachable instead of being
                # skipped by a cross-file `seen` set (false stale).
                self.assertTrue(RUNNER.binding_failures(
                    cross_bound('native_process::management_tests::cross_file_helper_test',
                                "#1642 marker via the other module's helper"),
                    sources=cross_fixture) == [],
                    '#1642 fix-r3 a same-named helper in another module is '
                    'walked in its own scope, not skipped')
                # a2: recursion through the `crate::` branch terminates
                # (this assertion is reached at all) and fails closed.
                self.assertTrue(RUNNER.binding_failures(
                    cross_bound('native_process::management_tests::recursion_test',
                                '#1642 no such marker'),
                    sources=cross_fixture) == ['row'],
                    '#1642 fix-r3 recursion through the crate:: branch '
                    'terminates and reports stale')
            # a4: a rotted `#[path]` mapping resolves to no file, which is a
            # stale binding, not a FileNotFoundError traceback.
            with patch.object(RUNNER, 'CRATE_MODULE_FILES',
                              {'rotted_1642': Path(temporary) / 'missing-1642.rs'}), \
                 patch.object(RUNNER, '_MODULE_FUNCTIONS', {}):
                self.assertTrue(RUNNER.binding_failures(
                    cross_bound('native_process::management_tests::rotted_map_test',
                                '#1642 marker via helper'),
                    sources=cross_fixture) == ['row'],
                    '#1642 fix-r3 a rotted module mapping is stale, not a traceback')


if __name__ == '__main__':
    unittest.main(verbosity=2)
