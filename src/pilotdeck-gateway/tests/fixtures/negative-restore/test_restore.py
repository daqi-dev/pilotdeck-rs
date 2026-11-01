#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1037 temporary-file runner fault injection, no product mutations
"""Exercise four real suite entrypoints without cargo or product source writes.

Only the real guard tests write Git state, inside the existing temporary fixture.

--negative-controls compiles Python mutants in memory, runs every mutant, and
requires the exact regression tests to fail at their dedicated assertions.
The simulated source guard only owns TemporaryDirectory files; it is never a
replacement for the HEAD guard when running product mutations.
"""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from collections import deque
from types import ModuleType
import unittest
from unittest.mock import patch

FIXTURES = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
sys.path.insert(0, str(FIXTURES))
import negative_process
import negative_suite
from test_negative_process import MutatingSources

PROCESS = negative_process


def load_suite(name):
    spec = importlib.util.spec_from_file_location(
        name.replace('-', '_'), FIXTURES / name / 'negative.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


MODULES = {name: load_suite(name) for name in
           ('mcp-http-encoding', 'regeneration-masks', 'node-provenance')}


class RestoreTests(unittest.TestCase):
    def exercise(self, fault):
        module = MODULES[self.suite_name]
        compile_only = self.suite_name == 'mcp-http-encoding'
        guard_failure = fault in ('enter', 'exit-restore', 'exit-verify', 'mid-restore')
        with tempfile.TemporaryDirectory(prefix='pdrs-1037-') as directory, \
                contextlib.ExitStack() as stack:
            root = Path(directory)
            output = root / 'evidence'
            snapshots = {}
            selected = []
            if compile_only:
                selected = [(name, root / f'source-{index}.rs', old, new, test, assertion)
                            for index, (name, _, old, new, test, assertion)
                            in enumerate(module.controls)]
                for _, path, old, _, _, _ in selected:
                    snapshots[path] = old.encode()
                stack.enter_context(patch.object(module, 'root', root))
                stack.enter_context(patch.object(module, 'controls', selected))
                names = [row[0] for row in selected]
                paths = [row[1] for row in selected]
            else:
                stack.enter_context(patch.object(module, 'ROOT', root))
                selected = module.ROWS
                names = [row['name'] for row in selected]
                paths = [root / row.get('path', getattr(module, 'MASKS', ''))
                         for row in selected]
                for row, path in zip(selected, paths):
                    snapshots[path] = snapshots.get(path, b'') + row['old'].encode() + b'\n'
            for path, data in snapshots.items():
                self.assertTrue(path.is_relative_to(root), '#1037 only temporary sources may be written')
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)

            calls = []
            completed = []
            injected = False
            mutation_index = None
            mutation_entries = 0
            write_bytes = Path.write_bytes
            invalidate = module.invalidate_completed

            def invalidate_and_capture(report_rows, reason):
                completed.extend(dict(row) for row in report_rows)
                invalidate(report_rows, reason)

            @contextlib.contextmanager
            def guard(actual_root, requested):
                self.assertEqual(actual_root, root, '#1037 guard must own the temporary root')
                self.assertEqual({root / path for path in requested}, set(snapshots),
                                 '#1037 guard must own exactly the temporary sources')
                if fault == 'enter':
                    raise RuntimeError('#1037 injected guard enter failure')
                try:
                    yield {Path(path): snapshots[root / path] for path in requested}
                finally:
                    for path, data in snapshots.items():
                        write_bytes(path, data)
                    if fault == 'exit-restore':
                        raise OSError('#1037 injected guard restore failure')
                    if fault == 'exit-verify':
                        raise RuntimeError('#1037 injected guard verify failure')

            def restore(path, data):
                nonlocal injected
                if (fault == 'mid-restore' and not injected and mutation_entries == 2
                        and path == paths[mutation_index] and data == snapshots[path]):
                    injected = True
                    raise OSError('#1037 injected mid restore failure')
                return write_bytes(path, data)

            def process(index, stage):
                nonlocal mutation_index, mutation_entries
                if stage == 'negative':
                    mutation_index = index
                    mutation_entries += 1
                row, path = selected[index], paths[index]
                old, new = (row[2], row[3]) if compile_only else (row['old'], row['new'])
                expected = snapshots[path]
                if stage == 'negative':
                    expected = expected.replace(old.encode(), new.encode(), 1)
                self.assertEqual(path.read_bytes(), expected,
                                 '#1037 simulated process must observe the actual mutation phase')
                calls.append((index, stage))
                if compile_only:
                    return subprocess.CompletedProcess(
                        [], 1 if fault == 'compile-failure' and index == 0 else 0,
                        'temporary compile result\n', '')
                test = row['test']
                if row.get('runner', 'cargo') == 'node':
                    log = (f"not ok - {test}\n{row['anchors'][0]}\n" if stage == 'negative'
                           else f'ok - {test}\n')
                elif stage == 'negative':
                    log = (f'test {test} ... FAILED\ntest result: FAILED\n'
                           f"panicked at fixture: {row['anchors'][0]}\n")
                else:
                    log = f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;\n'
                return subprocess.CompletedProcess([], 101 if stage == 'negative' else 0, log, '')

            if compile_only:
                stack.enter_context(patch.object(module, 'run_owned',
                                                 side_effect=lambda *_a, **_k: process(len(calls), 'negative')))
                stack.enter_context(patch.object(module, 'wait_for_slot',
                                                 return_value=subprocess.CompletedProcess([], 0, '', '')))
            elif self.suite_name == 'regeneration-masks':
                def run(test, _env, _output, label):
                    index = next(i for i, row in enumerate(selected) if label.startswith(row['name'] + '-'))
                    self.assertEqual(test, selected[index]['test'], '#1037 exact bound test must be preserved')
                    return process(index, label.rsplit('-', 1)[1])
                stack.enter_context(patch.object(module, 'run', side_effect=run))
            else:
                stack.enter_context(patch.object(module, 'run', side_effect=lambda row, _env, _out, stage:
                                                 process(selected.index(row), stage)))
            stack.enter_context(patch.object(module, 'mutating_sources', new=guard))
            stack.enter_context(patch.object(module, 'invalidate_completed', new=invalidate_and_capture))
            stack.enter_context(patch.object(Path, 'write_bytes', new=restore))
            stack.enter_context(patch.dict(os.environ, {
                'CARGO_TARGET_DIR': str(root / 'unused-target'), 'PDRS_BASELINE': str(root / 'unused-baseline')}))
            args = ['negative.py', '--compile-only', str(output)] if compile_only else ['negative.py', str(output)]
            stack.enter_context(patch.object(sys, 'argv', args))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            code = module.main()
            summary = output / ('compile-summary.json' if compile_only else 'summary.json')
            report = json.loads(summary.read_text())
            report_rows = report if compile_only else report['controls']
            self.assertEqual([row.get('mutation', row.get('name')) for row in report_rows], names,
                             '#1037 every selected row must be accounted for in order')
            expected_calls = (0 if fault == 'enter' else (2 if compile_only else 5)
                              if fault == 'mid-restore' else len(selected) * (1 if compile_only else 3))
            self.assertEqual(len(calls), expected_calls, '#1037 fault must occur at the requested execution point')
            if fault == 'mid-restore':
                self.assertTrue(injected, '#1037 second mutation restore fault must be injected')
            for path, data in snapshots.items():
                self.assertEqual(path.read_bytes(), data, '#1037 temporary sources must be restored')
            success = 'COMPILED' if compile_only else 'KILLED'
            expected_statuses = (['INVALID'] * len(selected) if guard_failure else
                                 ['INVALID'] + [success] * (len(selected) - 1)
                                 if fault == 'compile-failure' else [success] * len(selected))
            self.assertEqual([row['status'] for row in report_rows],
                             expected_statuses,
                             '#1037 row status must preserve only valid completed results')
            self.assertEqual(code, int(bool(fault)), '#1037 guard failure must return nonzero; healthy must pass')
            if compile_only:
                self.assertEqual([row['compiled'] for row in report_rows],
                                 [status == 'COMPILED' for status in expected_statuses],
                                 '#1037 guard failure must revoke every compiled flag')
            else:
                self.assertEqual(report['counts'], {
                    'KILLED': 0 if fault else len(selected), 'SURVIVED': 0,
                    'INVALID': len(selected) if fault else 0}, '#1037 counts must match the final rows')
            if guard_failure:
                expected_completed = 0 if fault == 'enter' else 1 if fault == 'mid-restore' else len(selected)
                self.assertEqual(len(completed), expected_completed, '#1037 completed rows must precede guard failure')
                self.assertTrue(all(row['status'] == success for row in completed),
                                '#1037 completed rows must be green before invalidation')
                detail = {'enter': 'guard enter', 'exit-restore': 'guard restore',
                          'exit-verify': 'guard verify', 'mid-restore': 'mid restore'}[fault]
                for row in report_rows:
                    self.assertIn(f'#1037 injected {detail} failure', row['reason'],
                                  '#1037 every row must retain the guard failure reason')
                for before, after in zip(completed, report_rows):
                    self.assertTrue(after['reason'].startswith(before['reason'] + '; source guard:'),
                                    '#1037 invalidation must preserve the prior diagnostic')

    def test_healthy(self):
        self.exercise(None)

    def test_guard_enter(self):
        self.exercise('enter')

    def test_guard_exit_restore(self):
        self.exercise('exit-restore')

    def test_guard_exit_verify(self):
        self.exercise('exit-verify')

    def test_mid_restore(self):
        self.exercise('mid-restore')


class McpRestoreTests(RestoreTests):
    suite_name = 'mcp-http-encoding'

    def test_compile_failure_continues(self):
        self.exercise('compile-failure')

    def test_run_cases_failure_reaches_main(self):
        module = MODULES[self.suite_name]
        with tempfile.TemporaryDirectory(prefix='pdrs-1037-main-') as directory:
            output = Path(directory) / 'evidence'
            with patch.object(module, 'audit'), \
                    patch.object(module, 'compile_controls', return_value=True) as compiled, \
                    patch.object(module, 'run_cases', return_value=1) as run, \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': str(Path(directory) / 'unused')}), \
                    patch.object(sys, 'argv', ['negative.py', str(output)]):
                code = module.main()
            self.assertEqual(compiled.call_count, 1, '#1037 MCP main must run compile controls')
            self.assertEqual(run.call_count, 1, '#1037 MCP main must reach run_cases')
            self.assertEqual(code, 1, '#1037 MCP main must propagate run_cases failure')


class MasksRestoreTests(RestoreTests):
    suite_name = 'regeneration-masks'


class NodeRestoreTests(RestoreTests):
    suite_name = 'node-provenance'


class RealGuardTests(unittest.TestCase):
    def exercise(self, fault):
        # The existing tiny git fixture has no Rust workspace/build directory.
        # Patch IO only; execute the actual guard's enter/finally/verification.
        with tempfile.TemporaryDirectory(prefix='pdrs-1037-real-') as directory:
            root, target = MutatingSources.repo(directory)
            original = target.read_bytes()
            raw_write, raw_read, raw_head = Path.write_bytes, Path.read_bytes, PROCESS._head_bytes
            restoring = False
            injected = False

            def write(path, data):
                nonlocal restoring, injected
                if path == target and data == original:
                    restoring = True
                    if fault == 'write':
                        injected = True
                        raise OSError('#1037 real restore write failure')
                return raw_write(path, data)

            def read(path):
                nonlocal injected
                if fault == 'readback' and restoring and path == target:
                    injected = True
                    return b'wrong readback'
                return raw_read(path)

            def head(actual_root, relative, baseline):
                nonlocal injected
                if fault == 'head' and restoring:
                    injected = True
                    return b'wrong HEAD'
                return raw_head(actual_root, relative, baseline)

            error = None
            try:
                with patch.object(Path, 'write_bytes', write), patch.object(Path, 'read_bytes', read), \
                        patch.object(PROCESS, '_head_bytes', head):
                    try:
                        with PROCESS.mutating_sources(root, [target], lock_timeout=0.1):
                            target.write_bytes(b'// mutant\n')
                    except (OSError, RuntimeError) as caught:
                        error = caught
                self.assertEqual(injected, fault is not None, '#1037 real guard fault injection must execute')
                if fault:
                    expected = {'write': (OSError, '#1037 real restore write failure'),
                                'readback': (RuntimeError, 'restore failed: source.rs'),
                                'head': (RuntimeError, 'restored source.rs does not match HEAD')}[fault]
                    self.assertIsInstance(error, expected[0], f'#1037 real guard must reject {fault} failure')
                    self.assertEqual(str(error), expected[1], '#1037 real guard must report the injected failure')
                else:
                    self.assertIsNone(error, '#1037 healthy real guard must not raise')
                if fault != 'write':
                    self.assertEqual(target.read_bytes(), original, '#1037 real guard restores actual bytes')
            finally:
                raw_write(target, original)

    def test_restore_write_failure(self):
        self.exercise('write')

    def test_restore_readback_mismatch(self):
        self.exercise('readback')

    def test_restore_head_mismatch(self):
        self.exercise('head')

    def test_healthy(self):
        self.exercise(None)


class SharedInvalidationTests(unittest.TestCase):
    def test_optional_compiled_key(self):
        rows = [dict(status='KILLED', reason='prior'),
                dict(status='COMPILED', reason='', compiled=True),
                dict(status='INVALID', reason='compile failed', compiled=False)]
        negative_suite.invalidate_completed(rows, 'source guard: failure')
        self.assertNotIn('compiled', rows[0], '#1037 invalidation must not add absent compiled keys')
        self.assertEqual([row['compiled'] for row in rows[1:]], [False, False],
                         '#1037 shared invalidation must revoke compiled flags')
        self.assertEqual([row['status'] for row in rows], ['INVALID'] * 3,
                         '#1037 shared invalidation must invalidate every row')
        self.assertEqual([row['reason'] for row in rows],
                         ['prior; source guard: failure', '; source guard: failure',
                          'compile failed; source guard: failure'],
                         '#1037 shared invalidation must preserve prior reasons')


SESSION_SOURCE = (FIXTURES / 'session-factory/negative_controls.py').read_text()
SESSION_RUNNER = 'src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py'
SESSION_OWNER = 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
SESSION_FACTORY_FILE = 'src/pilotdeck-gateway/src/runtime/session/factory.rs'
SESSION_LIVE = 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
SESSION_PREFIX = 'runtime::session::factory::tests::'


class SessionRunnerTests(unittest.TestCase):
    """#1210/#1055 session-factory runner behavior over a mocked temp tree.

    No cargo runs and no product source outside the temporary root is read or
    written; the exec'ed runner sees only mock guards and mock results, exactly
    like SessionFactoryTests above.
    """

    def run_session_runner(self, source_text, names, files, bound, *,
                           fault=None, on_negative=None, classify=None):
        """Exec the runner source against a temp tree; return (code, rows, ...).

        ``bound`` orders the (test, assertion) pairs the way the table orders
        the selected rows: source-group rows first, then switch-group rows.
        """
        with tempfile.TemporaryDirectory(prefix='pdrs-1218-runner-') as directory:
            root = Path(directory).resolve()
            script = root / SESSION_RUNNER
            snapshots = {}
            for relative, content in files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
                snapshots[path] = content.encode()
            output = root / 'evidence'
            guard_calls = []

            @contextlib.contextmanager
            def guard(actual_root, requested, **_kwargs):
                self.assertEqual(actual_root, root, '#1218 runner guard must own the temp root')
                guard_calls.append(tuple(requested))
                if fault == 'switch-exit' and not requested:
                    yield {}
                    raise RuntimeError('#1210 injected switch guard exit failure')
                try:
                    yield {path: snapshots[path] for path in requested}
                finally:
                    for path in requested:
                        path.write_bytes(snapshots[path])

            stages = deque((test, assertion, stage) for test, assertion in bound
                           for stage in ('positive', 'negative', 'restored'))

            def run_owned(command, **_kwargs):
                if '--no-run' in command:
                    return subprocess.CompletedProcess(command, 0, 'compiling\n', '')
                test, assertion, stage = stages.popleft()
                if stage == 'negative':
                    if on_negative is not None:
                        on_negative(root)
                    return subprocess.CompletedProcess(command, 101,
                        f'test {test} ... FAILED\n'
                        f"thread '{test}' panicked at owner.rs:1:1:\n{assertion}\n"
                        'test result: FAILED. 0 passed; 1 failed;\n', '')
                return subprocess.CompletedProcess(command, 0,
                    f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;\n', '')

            stack = contextlib.ExitStack()
            with stack:
                stack.enter_context(patch.object(negative_process, 'mutating_sources', guard))
                stack.enter_context(patch.object(negative_process, 'run_owned', run_owned))
                stack.enter_context(patch.object(negative_process, 'wait_for_slot',
                                                  return_value=subprocess.CompletedProcess([], 0, '', '')))
                if classify is not None:
                    stack.enter_context(patch.object(negative_suite, 'classify', classify))
                stack.enter_context(patch.dict(os.environ, {
                    'CARGO_TARGET_DIR': str(root / 'unused-target'),
                    'PDRS_BASELINE': str(root / 'unused-baseline')}))
                stack.enter_context(patch.object(sys, 'argv',
                                                 ['negative_controls.py', str(output), *names]))
                stack.enter_context(patch.object(sys, 'path',
                                                 [str(FIXTURES / 'turn-runner'), *sys.path]))
                stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                with self.assertRaises(SystemExit, msg='#1218 runner must exit with a status') as caught:
                    exec(compile(source_text, str(script), 'exec'),
                         {'__file__': str(script), '__name__': '__main__'})
            rows = json.loads((output / 'summary.json').read_text())['controls']
            restored_files = {relative: (root / relative).read_bytes() == snapshots[root / relative]
                             for relative in files}
            return caught.exception.code, rows, guard_calls, restored_files

    def test_same_file_changes_accumulate(self):
        source = changed_text(
            SESSION_SOURCE,
            "    ('same-key-concurrent', owner, 'dirty: false,', 'dirty: true,', None, prefix+'same_key_concurrent', '#745 same-key-concurrent conclusion'),",
            "    dict(name='same-key-concurrent',\n"
            "         changes=[(owner, 'dirty: false,', 'dirty: true,'),\n"
            "                  (owner, 'second_anchor_1218', 'second_anchor_1218_mutated')],\n"
            "         test=prefix+'same_key_concurrent', assertion='#745 same-key-concurrent conclusion'),")
        captured = {}
        code, rows, _guard_calls, restored_files = self.run_session_runner(
            source, ['same-key-concurrent'],
            {SESSION_OWNER: 'dirty: false,\nsecond_anchor_1218\n'},
            [(SESSION_PREFIX + 'same_key_concurrent', '#745 same-key-concurrent conclusion')],
            on_negative=lambda root: captured.update(
                owner=(root / SESSION_OWNER).read_text()))
        self.assertTrue(rows[0]['status'] == 'KILLED',
                        '#1210 same-file changes must compose into one mutant')
        self.assertTrue('dirty: true,' in captured['owner'] and
                        'second_anchor_1218_mutated' in captured['owner'],
                        '#1210 same-file changes must compose into one mutant')
        self.assertEqual(code, 0, '#1210 accumulated mutant run must succeed')
        self.assertTrue(all(restored_files.values()), '#1210 temp sources must be restored')

    def test_switch_rows_accept_dict_changes_and_pristine_check(self):
        source = changed_text(
            SESSION_SOURCE,
            "    ('r3-owner-replays-drained-busy', owner, 'option_env!(\"PDRS_1022_R3_REPLAY_DRAINED_BUSY\").is_none()', 'false', None, prefix+'recreation_cancelled_timeout_retries_after_drain', '#1022 r3 completed drain must discard abandoned BUSY'),",
            "    dict(name='r3-owner-replays-drained-busy',\n"
            "         changes=[(owner, 'option_env!(\"PDRS_1022_R3_REPLAY_DRAINED_BUSY\").is_none()', 'false'),\n"
            "                  (factory, 'fn can_recreate(&self) -> bool {', 'fn can_recreate_mutated(&self) -> bool {')],\n"
            "         test=prefix+'recreation_cancelled_timeout_retries_after_drain',\n"
            "         assertion='#1022 r3 completed drain must discard abandoned BUSY'),")
        code, rows, guard_calls, restored_files = self.run_session_runner(
            source, ['r3-owner-replays-drained-busy'],
            {SESSION_OWNER: 'option_env!("PDRS_1022_R3_REPLAY_DRAINED_BUSY").is_none()\n',
             SESSION_FACTORY_FILE: 'fn can_recreate(&self) -> bool {\n'},
            [(SESSION_PREFIX + 'recreation_cancelled_timeout_retries_after_drain',
              '#1022 r3 completed drain must discard abandoned BUSY')])
        self.assertTrue(rows[0]['status'] == 'KILLED',
                        '#1210 switch group must accept dict change rows')
        self.assertTrue(any(call == () for call in guard_calls),
                        '#1055 switch-only run must pass the pristine guard')
        self.assertEqual(code, 0, '#1210 dict switch row must run clean')
        self.assertTrue(all(restored_files.values()), '#1210 temp sources must be restored')

    def test_switch_guard_failure_keeps_source_verdicts(self):
        code, rows, _guard_calls, restored_files = self.run_session_runner(
            SESSION_SOURCE,
            ['same-key-concurrent', 'r3-owner-replays-drained-busy'],
            {SESSION_OWNER: 'dirty: false,\noption_env!("PDRS_1022_R3_REPLAY_DRAINED_BUSY").is_none()\n'},
            [(SESSION_PREFIX + 'same_key_concurrent', '#745 same-key-concurrent conclusion'),
             (SESSION_PREFIX + 'recreation_cancelled_timeout_retries_after_drain',
              '#1022 r3 completed drain must discard abandoned BUSY')],
            fault='switch-exit')
        by_name = {row['mutation']: row for row in rows}
        self.assertTrue(by_name['same-key-concurrent']['status'] == 'KILLED',
                        '#1210 switch guard failure must not revoke source verdicts')
        self.assertTrue(by_name['r3-owner-replays-drained-busy']['status'] == 'INVALID' and
                        'source guard: #1210 injected switch guard exit failure'
                        in by_name['r3-owner-replays-drained-busy']['reason'],
                        '#1210 switch guard failure must still void its own rows')
        self.assertEqual(code, 1, '#1210 switch guard failure must exit nonzero')
        self.assertTrue(all(restored_files.values()), '#1210 temp sources must be restored')

    def test_two_rows_without_guard_fault_are_both_killed(self):
        # fix-r1 (#1218 P2-4): every row consumes its own positive, negative
        # and restored stages in table order, so in a multi-row no-fault run
        # each bound positive test actually executes. A leftover stage answers
        # the next row's positive run with the PREVIOUS row's test name (exit
        # 0); passed() only accepts the exact named "... ok" line, so that row
        # fails as "bound positive test failed" — the shift never silently
        # reaches a later stage (pinned by session-restored-stage-skipped).
        code, rows, _guard_calls, restored_files = self.run_session_runner(
            SESSION_SOURCE,
            ['same-key-concurrent', 'recreate-disposed'],
            {SESSION_OWNER: 'dirty: false,\n',
             SESSION_LIVE: 'if self.disposed {\n',
             SESSION_FACTORY_FILE: 'if self.disposed.load(Ordering::SeqCst) {\n'},
            [(SESSION_PREFIX + 'same_key_concurrent', '#745 same-key-concurrent conclusion'),
             (SESSION_PREFIX + 'recreate_disposed', '#745 recreate expected failure')])
        by_name = {row['mutation']: row for row in rows}
        self.assertTrue(by_name['same-key-concurrent']['status'] == 'KILLED',
                        '#1210 the first row must actually run its bound positive test')
        self.assertTrue(by_name['recreate-disposed']['status'] == 'KILLED',
                        '#1210 the second row must actually run its bound positive test')
        self.assertEqual(code, 0, '#1210 a two-row no-fault run must exit clean')
        self.assertTrue(all(restored_files.values()), '#1210 temp sources must be restored')

    def test_classifier_receives_every_change(self):
        captured = []
        real_classify = negative_suite.classify

        def classify(result, case):
            captured.append(case)
            return real_classify(result, case)

        code, rows, _guard_calls, restored_files = self.run_session_runner(
            SESSION_SOURCE, ['recreate-disposed'],
            {SESSION_LIVE: 'if self.disposed {\n',
             SESSION_FACTORY_FILE: 'if self.disposed.load(Ordering::SeqCst) {\n'},
            [(SESSION_PREFIX + 'recreate_disposed', '#745 recreate expected failure')],
            classify=classify)
        self.assertTrue(rows[0]['status'] == 'KILLED',
                        '#1210 multi-file row must still classify')
        self.assertTrue(captured and captured[0].changes is not None and
                        len(captured[0].changes) == 2,
                        '#1210 the runner case record must carry every change of the row')
        self.assertEqual(code, 0, '#1210 multi-file row must run clean')
        self.assertTrue(all(restored_files.values()), '#1210 temp sources must be restored')


class SessionFactoryTests(unittest.TestCase):
    def test_guard_exit_revokes_compiled(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1037-session-') as directory:
            root = Path(directory).resolve()
            source = root / 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
            source.parent.mkdir(parents=True)
            source.write_text('dirty: false,')
            output = root / 'evidence'
            calls = []

            @contextlib.contextmanager
            def guard(actual_root, paths):
                self.assertEqual(actual_root, root, '#1037 session guard must own temporary root')
                self.assertEqual(paths, [source], '#1037 session guard must own only temporary source')
                yield {source: b'dirty: false,'}
                self.assertEqual(source.read_text(), 'dirty: false,', '#1037 session restores before guard exit')
                raise RuntimeError('#1037 session guard exit failure')

            def run(*_args, **_kwargs):
                calls.append(source.read_text())
                return subprocess.CompletedProcess([], 0, '', '')

            script = root / 'src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py'
            with patch.object(negative_process, 'mutating_sources', guard), \
                    patch.object(negative_process, 'run_owned', run), \
                    patch.object(negative_process, 'wait_for_slot', return_value=subprocess.CompletedProcess([], 0, '', '')), \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': str(root / 'unused'), 'PDRS_BASELINE': str(root / 'unused-ts')}), \
                    patch.object(sys, 'argv', ['negative_controls.py', '--compile', str(output), 'same-key-concurrent']), \
                    patch.object(sys, 'path', [str(FIXTURES / 'turn-runner'), *sys.path]), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit, msg='#1037 session entry must return a status') as caught:
                    exec(compile(SESSION_SOURCE, str(script), 'exec'), {'__file__': str(script), '__name__': '__main__'})
            rows = json.loads((output / 'summary.json').read_text())['controls']
            self.assertEqual(calls, ['dirty: true,'], f'#1037 session compile must observe its mutation: {rows}')
            self.assertEqual(caught.exception.code, 1, '#1037 session guard failure must exit nonzero')
            self.assertEqual(len(rows), 1, '#1037 session must report the selected row')
            self.assertEqual(rows[0]['status'], 'INVALID', '#1037 session guard must invalidate completed row')
            self.assertFalse(rows[0]['compiled'], '#1037 session guard must revoke compiled flag')
            self.assertIn('#1037 session guard exit failure', rows[0]['reason'], '#1037 session must preserve guard reason')


TEST_CLASSES = (McpRestoreTests, MasksRestoreTests, NodeRestoreTests, RealGuardTests,
                SharedInvalidationTests, SessionFactoryTests, SessionRunnerTests)


def tests(test_class):
    return unittest.defaultTestLoader.loadTestsFromTestCase(test_class)


def changed_source(path, old, new):
    source = path.read_text()
    if source.count(old) != 1:
        raise ValueError(f'#1037 stale Python anchor; expected exactly one: {old!r}')
    source = source.replace(old, new, 1)
    compile(source, str(path), 'exec')
    return source


def changed_text(source, old, new):
    """#1218: same single-anchor replacement, but from an in-memory source."""
    if source.count(old) != 1:
        raise ValueError(f'#1210 stale Python anchor; expected exactly one: {old!r}')
    source = source.replace(old, new, 1)
    compile(source, '<session-runner>', 'exec')
    return source


def mutated_module(path, old, new):
    module = ModuleType(path.stem)
    module.__file__ = str(path)
    exec(compile(changed_source(path, old, new), str(path), 'exec'), module.__dict__)
    return module


@contextlib.contextmanager
def shared_mutation(old, new):
    mutant = mutated_module(FIXTURES / 'negative_suite.py', old, new)
    with patch.object(negative_suite, 'invalidate_completed', mutant.invalidate_completed), \
            patch.object(MODULES['mcp-http-encoding'], 'invalidate_completed', mutant.invalidate_completed):
        yield


def negative_controls():
    row_message = '#1037 row status must preserve only valid completed results'
    exit_tests = ('test_guard_exit_restore', 'test_guard_exit_verify', 'test_mid_restore')
    variants = []
    for cls in (McpRestoreTests, MasksRestoreTests, NodeRestoreTests):
        variants.append((cls.suite_name + '-invalidation-noop', (cls,),
                         {(cls, test): row_message for test in exit_tests},
                         lambda cls=cls: patch.object(MODULES[cls.suite_name], 'invalidate_completed', lambda *_: None)))
    variants.extend([
        ('mcp-compile-status-always-green', (McpRestoreTests,),
         {(McpRestoreTests, 'test_compile_failure_continues'): row_message},
         lambda: patch.dict(MODULES, {'mcp-http-encoding': mutated_module(
             FIXTURES / 'mcp-http-encoding/negative.py',
             "status='COMPILED' if run.returncode == 0 else 'INVALID'", "status='COMPILED'")})),
        ('mcp-run-cases-status-swallowed', (McpRestoreTests,),
         {(McpRestoreTests, 'test_run_cases_failure_reaches_main'): '#1037 MCP main must propagate run_cases failure'},
         lambda: patch.dict(MODULES, {'mcp-http-encoding': mutated_module(
             FIXTURES / 'mcp-http-encoding/negative.py',
             'return int(not compiled or status != 0)', 'return int(not compiled)')})),
        ('shared-compiled-not-revoked', (McpRestoreTests, SharedInvalidationTests, SessionFactoryTests),
         {(McpRestoreTests, test): '#1037 guard failure must revoke every compiled flag' for test in exit_tests}
         | {(SharedInvalidationTests, 'test_optional_compiled_key'): '#1037 shared invalidation must revoke compiled flags',
            (SessionFactoryTests, 'test_guard_exit_revokes_compiled'): '#1037 session guard must revoke compiled flag'},
         lambda: shared_mutation("row['compiled'] = False", "row['compiled'] = row['compiled']")),
        ('shared-adds-absent-compiled-key', (SharedInvalidationTests,),
         {(SharedInvalidationTests, 'test_optional_compiled_key'): '#1037 invalidation must not add absent compiled keys'},
         lambda: shared_mutation("if 'compiled' in row:", 'if True:')),
        ('session-invalidation-noop', (SessionFactoryTests,),
         {(SessionFactoryTests, 'test_guard_exit_revokes_compiled'): '#1037 session guard failure must exit nonzero'},
         lambda: patch.dict(globals(), {'SESSION_SOURCE': changed_source(
             FIXTURES / 'session-factory/negative_controls.py',
             "    scope = {row_name(case) for case in group}\n"
             "    invalidate_completed([row for row in results if row['mutation'] in scope],\n"
             "                          f'source guard: {error}')",
             '    pass')})),
        # #1210/#1055: reverse negatives for the session runner behavior the
        # new SessionRunnerTests pin (accumulation, dict switch rows, switch
        # pristine check, scoped guard failure, classifier change list).
        ('session-accumulate-removed', (SessionRunnerTests,),
         {(SessionRunnerTests, 'test_same_file_changes_accumulate'): '#1210 same-file changes must compose into one mutant'},
         lambda: patch.dict(globals(), {'SESSION_SOURCE': changed_source(
             FIXTURES / 'session-factory/negative_controls.py',
             '                        current=texts.get(path, originals[path].decode())',
             '                        current=originals[path].decode()')})),
        ('session-switch-unpack-reverted', (SessionRunnerTests,),
         {(SessionRunnerTests, 'test_switch_rows_accept_dict_changes_and_pristine_check'): '#1210 switch group must accept dict change rows'},
         lambda: patch.dict(globals(), {'SESSION_SOURCE': changed_source(
             FIXTURES / 'session-factory/negative_controls.py',
             '            for case in switch_cases:\n'
             '                name,changes,scope,test,assertion = row_parts(case)',
             '            for name,path,old,new,scope,test,assertion in switch_cases:\n'
             '                changes = [(path,old,new)]')})),
        # Bypassing the empty-set guard window also un-faults the switch group
        # of test_switch_guard_failure_keeps_source_verdicts (its r3 row is a
        # legacy 7-tuple that still runs), so both tests must fail.
        ('session-switch-pristine-removed', (SessionRunnerTests,),
         {(SessionRunnerTests, 'test_switch_rows_accept_dict_changes_and_pristine_check'): '#1055 switch-only run must pass the pristine guard',
          (SessionRunnerTests, 'test_switch_guard_failure_keeps_source_verdicts'): '#1210 switch guard failure must still void its own rows'},
         lambda: patch.dict(globals(), {'SESSION_SOURCE': changed_source(
             FIXTURES / 'session-factory/negative_controls.py',
             '        with mutating_sources(root, []):',
             '        for _bypassed_pristine_1218 in [0]:')})),
        ('session-guard-scope-reverted', (SessionRunnerTests,),
         {(SessionRunnerTests, 'test_switch_guard_failure_keeps_source_verdicts'): '#1210 switch guard failure must not revoke source verdicts'},
         lambda: patch.dict(globals(), {'SESSION_SOURCE': changed_source(
             FIXTURES / 'session-factory/negative_controls.py',
             "    scope = {row_name(case) for case in group}\n"
             "    invalidate_completed([row for row in results if row['mutation'] in scope],\n"
             "                          f'source guard: {error}')",
             "    invalidate_completed(results, f'source guard: {error}')")})),
        ('session-case-first-change-only', (SessionRunnerTests,),
         {(SessionRunnerTests, 'test_classifier_receives_every_change'): '#1210 the runner case record must carry every change of the row'},
         lambda: patch.dict(globals(), {'SESSION_SOURCE': changed_source(
             FIXTURES / 'session-factory/negative_controls.py',
             "        status,reason=classify(run(name+'-negative',test),\n"
             "                               RunnerCase(name, changes, test, assertion))",
             "        status,reason=classify(run(name+'-negative',test),\n"
             "                               RunnerCase(name, changes[:1], test, assertion))")})),
        # fix-r1 (#1218 P2-4): skipping the restored run leaves its stage
        # unconsumed, so the next row's positive run pops it. The mock answers
        # a leftover stage with the previous row's test name (exit 0), and
        # passed() only accepts the exact named "... ok" line, so the next row
        # fails as a bound positive (INVALID, "bound positive test failed")
        # instead of actually running — the misalignment the review feared. The
        # no-fault two-row test catches exactly that.
        ('session-restored-stage-skipped', (SessionRunnerTests,),
         {(SessionRunnerTests, 'test_two_rows_without_guard_fault_are_both_killed'): '#1210 the second row must actually run its bound positive test'},
         lambda: patch.dict(globals(), {'SESSION_SOURCE': changed_source(
             FIXTURES / 'session-factory/negative_controls.py',
             "def restored(row, name, test):\n    if not compile_only:",
             "def restored(row, name, test):\n    if False:")})),
    ])
    for label, test, old, new in [
        ('write', 'test_restore_write_failure',
         '                    path.write_bytes(snapshot[path])',
         '                    try:\n                        path.write_bytes(snapshot[path])\n                    except OSError:\n                        pass'),
        ('readback', 'test_restore_readback_mismatch',
         'raise RuntimeError(f"restore failed: {relative}")', 'pass'),
        ('head', 'test_restore_head_mismatch',
         'raise RuntimeError(\n                            f"restored {relative} does not match {baseline}")', 'pass'),
    ]:
        variants.append(('real-guard-' + label + '-failure-swallowed', (RealGuardTests,),
                         {(RealGuardTests, test): f'#1037 real guard must reject {label} failure'},
                         lambda old=old, new=new: patch.dict(globals(), {'PROCESS': mutated_module(
                             FIXTURES / 'negative_process.py', old, new)})))

    results = []
    for mutation, classes, failures, prepare in variants:
        expected = {cls(method).id(): message for (cls, method), message in failures.items()}
        try:
            # Every replacement is compiled before execution. No source file is
            # changed; arbitrary errors, timeouts, and wrong assertions are INVALID.
            stream = io.StringIO()
            runner = unittest.TextTestRunner(stream=stream, verbosity=2)
            baseline = runner.run(unittest.TestSuite(tests(cls) for cls in classes))
            with prepare():
                result = runner.run(unittest.TestSuite(tests(cls) for cls in classes))
            restored = runner.run(unittest.TestSuite(tests(cls) for cls in classes))
            print(stream.getvalue(), end='')
            observed = {test.id(): trace for test, trace in result.failures}
            healthy = baseline.wasSuccessful() and restored.wasSuccessful()
            killed = (healthy and result.testsRun == sum(tests(cls).countTestCases() for cls in classes)
                      and not result.errors and observed.keys() == expected.keys()
                      and all(message in observed[test] for test, message in expected.items()))
            results.append({'mutation': mutation,
                            'status': 'KILLED' if killed else 'SURVIVED' if healthy and result.wasSuccessful() else 'INVALID',
                            'baseline': baseline.wasSuccessful(), 'restored': restored.wasSuccessful(),
                            'tests': expected, 'observed': sorted(observed),
                            'reason': 'exact bound assertions failed' if killed else 'bound failure mismatch'})
        except Exception as error:
            results.append({'mutation': mutation, 'status': 'INVALID', 'tests': expected,
                            'reason': f'{type(error).__name__}: {error}'})
    print(json.dumps(results, indent=2))
    print('summary: ' + ', '.join(f'{status}={sum(row["status"] == status for row in results)}'
                                 for status in ('KILLED', 'SURVIVED', 'INVALID')))
    return int(any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--negative-controls']:
        raise SystemExit(negative_controls())
    if sys.argv[1:]:
        raise SystemExit('usage: test_restore.py [--negative-controls]')
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.TestSuite(tests(test_class) for test_class in TEST_CLASSES))
    raise SystemExit(not result.wasSuccessful())
