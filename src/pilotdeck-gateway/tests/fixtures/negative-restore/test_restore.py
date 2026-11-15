#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1037 temporary-file runner fault injection, no product mutations
"""Exercise four real suite entrypoints without cargo or product source writes.

Only the real guard tests write Git state, inside the existing temporary fixture.

--negative-controls compiles Python mutants in memory, runs every mutant, and
requires the exact regression tests to fail at their dedicated assertions.
The simulated source guard only owns TemporaryDirectory files; it is never a
replacement for the HEAD guard when running product mutations.
"""
import ast
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
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
            def guard(actual_root, requested, **_kwargs):
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

            # #1087 fix-r2: the runtime suites publish every guarded-source
            # write in the mutation window through `write_source_atomic`
            # (#1903 rule), so the mid-restore fault is injected on that
            # channel; the compile-only suite still truncates through
            # ``Path.write_bytes``, which keeps its own channel.
            def restore_target(path):
                target = Path(path)
                return root / target if not target.is_absolute() else target

            def maybe_inject(path, data):
                nonlocal injected
                target = restore_target(path)
                if (fault == 'mid-restore' and not injected and mutation_entries == 2
                        and target == paths[mutation_index] and data == snapshots[target]):
                    injected = True
                    raise OSError('#1037 injected mid restore failure')

            def restore_bytes(path, data):
                maybe_inject(path, data)
                return write_bytes(path, data)

            def restore_atomic(root_arg, path, data, *, mode=None):
                maybe_inject(path, data)
                return write_atomic(root_arg, path, data, mode=mode)

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
                    # #1087: node_test_killed() only accepts a marker inside
                    # the indented TAP block of the subtest's own
                    # `not ok N - <name>` line, never on top-level output.
                    log = (f'not ok 1 - {test}\n  ---\n  {row["anchors"][0]}\n  ...\n'
                           if stage == 'negative' else f'ok 1 - {test}\n')
                elif stage == 'negative':
                    # #1087: named_test_killed() only accepts a marker inside
                    # the panic payload of the thread libtest names after the
                    # bound test, so simulate real libtest output instead of a
                    # synthetic "panicked at" line that no strict judge sees.
                    log = (f'test {test} ... FAILED\n\n---- {test} stdout ----\n'
                           f"thread '{test}' panicked at src/fixture.rs:1:1:\n"
                           f'{row["anchors"][0]}\n'
                           f'note: run with `RUST_BACKTRACE=1` to display a backtrace\n\n'
                           f'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n')
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
            if compile_only:
                stack.enter_context(patch.object(Path, 'write_bytes', new=restore_bytes))
            else:
                # The runtime suites publish through write_source_atomic; the
                # compile-only suite still truncates through Path.write_bytes.
                write_atomic = module.write_source_atomic
                stack.enter_context(patch.object(module, 'write_source_atomic',
                                                 new=restore_atomic))
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
            raw_replace, raw_read, raw_head = os.replace, Path.read_bytes, PROCESS._head_bytes
            restoring = False
            injected = False

            # #1903: the shared restore publishes through `write_source_atomic`
            # (temp file + one `os.replace`), so the old `Path.write_bytes`
            # injection no longer runs. The failure is injected at the rename,
            # which is the step whose failure must abort the restore.
            def replace(source, destination, *args, **kwargs):
                nonlocal restoring, injected
                if Path(destination) == target:
                    restoring = True
                    if fault == 'write':
                        injected = True
                        raise OSError('#1037 real restore write failure')
                return raw_replace(source, destination, *args, **kwargs)

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
                with patch.object(os, 'replace', replace), patch.object(Path, 'read_bytes', read), \
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
                # Outside the patch: the restore is not the thing under test here.
                Path.write_bytes(target, original)

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
    def run_guard(self, negative_baseline=None):
        """Run the session entry once and report the baseline its guard saw.

        ``negative_baseline=None`` leaves ``PDRS_NEGATIVE_BASELINE`` unset.
        """
        with tempfile.TemporaryDirectory(prefix='pdrs-1037-session-') as directory:
            root = Path(directory).resolve()
            source = root / 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
            source.parent.mkdir(parents=True)
            source.write_text('dirty: false,')
            output = root / 'evidence'
            calls = []
            baselines = []
            environ = {key: value for key, value in os.environ.items()
                       if key != 'PDRS_NEGATIVE_BASELINE'}
            environ.update({'CARGO_TARGET_DIR': str(root / 'unused'),
                            'PDRS_BASELINE': str(root / 'unused-ts')})
            if negative_baseline is not None:
                environ['PDRS_NEGATIVE_BASELINE'] = negative_baseline

            @contextlib.contextmanager
            def guard(actual_root, paths, **kwargs):
                self.assertEqual(actual_root, root, '#1037 session guard must own temporary root')
                self.assertEqual(paths, [source], '#1037 session guard must own only temporary source')
                # The script funnels guard exceptions into an INVALID row, so
                # the observed value is recorded here and asserted by the caller.
                baselines.append(kwargs.get('baseline'))
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
                    patch.dict(os.environ, environ, clear=True), \
                    patch.object(sys, 'argv', ['negative_controls.py', '--compile', str(output), 'same-key-concurrent']), \
                    patch.object(sys, 'path', [str(FIXTURES / 'turn-runner'), *sys.path]), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit, msg='#1037 session entry must return a status') as caught:
                    exec(compile(SESSION_SOURCE, str(script), 'exec'), {'__file__': str(script), '__name__': '__main__'})
            rows = json.loads((output / 'summary.json').read_text())['controls']
            return caught, rows, calls, baselines

    def test_guard_exit_revokes_compiled(self):
        # #1281: the session runner now names its baseline explicitly so
        # an uncommitted candidate can be guarded. Pin a sentinel value
        # instead of the default: the assertion stays deterministic when
        # the ambient environment sets PDRS_NEGATIVE_BASELINE (as this
        # very meta suite does), and it proves the override reaches the
        # guard unchanged.
        caught, rows, calls, baselines = self.run_guard('sentinel-1281')
        self.assertEqual(baselines, ['sentinel-1281'],
                         '#1037 session guard must pass through PDRS_NEGATIVE_BASELINE')
        self.assertEqual(calls, ['dirty: true,'], f'#1037 session compile must observe its mutation: {rows}')
        self.assertEqual(caught.exception.code, 1, '#1037 session guard failure must exit nonzero')
        self.assertEqual(len(rows), 1, '#1037 session must report the selected row')
        self.assertEqual(rows[0]['status'], 'INVALID', '#1037 session guard must invalidate completed row')
        self.assertFalse(rows[0]['compiled'], '#1037 session guard must revoke compiled flag')
        self.assertIn('#1037 session guard exit failure', rows[0]['reason'], '#1037 session must preserve guard reason')

    def test_guard_defaults_to_head(self):
        # #1890-6: pinning a sentinel above left the default branch uncovered.
        # An unset variable and an empty one both have to reach the guard as
        # 'HEAD' — `get(key, 'HEAD')` passes '' straight through, which would
        # hand `mutating_sources` an unusable revision. Only the baseline is
        # asserted here; the row verdicts belong to the sibling test so this
        # walk-through stays independent of the guard-failure mutants.
        for value in (None, ''):
            _caught, _rows, _calls, baselines = self.run_guard(value)
            self.assertEqual(baselines, ['HEAD'],
                             f'#1037 an unset or empty baseline ({value!r}) must fall back to HEAD')


# #3001 item 3 (#3178): the migrated-table sweep. The MODULES harness above
# drives restore faults through three runners only; the other migrated family
# tables had no revert-to-truncating-write red until this scan.
# #3178 fix-r1 (boundary #7): the marker matches on a word boundary — a bare
# substring would let a future `#1087 fix-r10` note satisfy the fix-r1 probe.
MIGRATION_MARKER = re.compile(r'#1087 fix-r(?:1|2)\b')
MIGRATION_EXCLUDED_DIRS = ('_shared/', '_negkit/', 'negative-restore/')


def migration_files(root=FIXTURES):
    """Yield (relative path, text) for every migrated family table.

    The population is directory-derived the way ``_shared/test_compile_inputs.py``
    derives its compile sweep: a family table is a fixtures subdirectory runner
    carrying the #1087 fix-r1/fix-r2 atomic-write migration marker (#1903 rule).
    Unit tests quote those markers in assertions, root-level modules are the M6
    lane's shared machinery, and the ``_``-prefixed kit dirs are shared — none
    of them are family tables. Marker-less runners are the #1908 unmigrated
    backlog and deliberately stay out until they are migrated.
    """
    for path in sorted(root.rglob('*.py')):
        rel = path.relative_to(root)
        rel_posix = rel.as_posix()
        if path.name.startswith('test_') or '/' not in rel_posix:
            continue
        if rel_posix.startswith(MIGRATION_EXCLUDED_DIRS):
            continue
        try:
            text = path.read_text(encoding='utf-8', errors='surrogateescape')
        except OSError:
            continue
        if MIGRATION_MARKER.search(text):
            yield rel_posix, text


def _receiver_root(node):
    """The leftmost name of a receiver expression (``out`` in ``out / 'x.log'``,
    ``paths`` in ``paths[key]``)."""
    while True:
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
            node = node.left
        elif isinstance(node, ast.Subscript):
            node = node.value
        else:
            break
    return node.id if isinstance(node, ast.Name) else None


def _is_call_to(node, name):
    return (isinstance(node, ast.Call)
            and ((isinstance(node.func, ast.Name) and node.func.id == name)
                 or (isinstance(node.func, ast.Attribute) and node.func.attr == name)))


class _UnreadGuardShape(Exception):
    """A guard expression node ``_collect_names`` has no branch for.

    #3178 fix-r4 (DS boundary#2 + ds4 simplify#4 + MM sibling#7, the
    structural close-out): the r1→r2→r3 rounds each added the one node
    type the next review found missing (BinOp/Subscript, then Compare),
    and a type with no branch still contributed nothing *silently* —
    the guard stayed partially readable, the unread names went
    unguarded, and a revert to truncating writes through them went
    unseen. Guards are now collected in strict mode: meeting an
    unrecognized node aborts the collection and the window is reported
    as unreadable in full instead of half-resolved.
    """


def _collect_names(node, names, *, strict=False):
    """Add every name an expression can draw from into ``names``.

    #3178 fix-r1 (sibling #3 / boundary #1 / concurrency #1): the first cut
    of the sweep only accepted ``Name`` and all-``Name`` literals, so every
    computed guard — ``[paths[key] for key in paths]`` (ListComp),
    ``list(dict.fromkeys(...))`` (Call), ``sorted({root / filename ...})``
    (SetComp + BinOp) — resolved to the empty set and the whole window was
    silently skipped by ``if not guarded: continue``. Names are collected
    recursively instead; comprehension targets stay out (they are bound, not
    drawn from), while the iterated collection does join (it is where the
    paths come from).

    #3178 fix-r2 (DS boundary#1, P0): "targets stay out" was a promise the
    first cut did not keep — ``[x for x in compute_paths()]`` recursed into
    ``node.elt`` and collected ``x`` anyway, so the bound loop variable
    polluted the guarded set, the unresolvable window looked resolvable,
    and the empty-window fail-closed never fired (a false PASS). The bound
    targets are now collected first and subtracted from what the element
    draws on; a chained ``for y in x`` iter is also a bound target and
    joins the subtraction.

    #3178 fix-r3 (DS3 boundary#1 + ds1 sibling#8): the subtraction is
    confined to the element/condition pool — the iters are evaluated in
    the *enclosing* scope, so ``[p for p in [p]]`` draws the outer ``p``
    through the iter and the old blanket ``drawn - bound`` wrongly dropped
    it (a same-name shadow hid the true guarded name and the window looked
    resolvable without it: a false PASS on truncated writes). And the
    ``ifs`` conditions are the third draw position of the same production:
    names appearing only there (``if p in CANDIDATES``) used to miss the
    guarded set entirely.

    #3178 fix-r4 (DS boundary#2 + ds4 simplify#4 + MM sibling#7): the
    round-by-round enumeration is over. ``BoolOp`` (``a or b``, ``p and
    q``) and ``UnaryOp`` (``not flag``) join the recognized shapes — an
    alias value or a comprehension filter built from them used to draw
    nothing, so the names appearing only there went unguarded and a
    truncating write through them went unseen (the three review shapes,
    demonstrated unseen on the pre-fix collector by
    _tmp/20261115-3178-fixr4/probe-alias-shapes.py). Every other
    unrecognized node now aborts a *strict* collection — the guard mode —
    instead of contributing nothing: a partially-read guard would leave
    its unread names unguarded (``_UnreadGuardShape``). ``Constant`` is a
    name-free leaf, not a blind spot, so literals inside guards keep
    them readable. The alias fixpoint keeps the lenient walk: assignment
    values in the real family tables are full of attribute receivers
    and dict literals, and a shape missed there can only under-collect
    (never invent a violation).
    """
    if isinstance(node, ast.Name):
        names.add(node.id)
    elif isinstance(node, ast.Constant):
        # #3178 fix-r4: a literal leaf can never draw a guarded name, so
        # meeting one is not an unread shape — guards hold constants
        # inside compares and set literals and must keep resolving.
        pass
    elif isinstance(node, (ast.List, ast.Tuple, ast.Set)):
        for item in node.elts:
            _collect_names(item, names, strict=strict)
    elif isinstance(node, ast.Starred):
        _collect_names(node.value, names, strict=strict)
    elif isinstance(node, ast.Call):
        for arg in node.args:
            _collect_names(arg, names, strict=strict)
        for kw in node.keywords:
            _collect_names(kw.value, names, strict=strict)
        # A method receiver carries names the plain argument walk cannot
        # reach (``originals.items()``); a plain function name (``sorted``)
        # is a builtin, not a path source, and stays out.
        if isinstance(node.func, ast.Attribute):
            _collect_names(node.func.value, names, strict=strict)
    elif isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
        # The loop variables a comprehension binds are local to it — they
        # are not names drawn from the enclosing scope. Collect what the
        # element, the filter conditions and the iters draw on. The bound
        # targets are subtracted from the element/condition pool only: the
        # iters are evaluated in the enclosing scope, so a name that
        # coincides with a bound target (``[p for p in [p]]``) is a real
        # outer draw and must survive the subtraction (fix-r3, DS3
        # boundary#1); the ``ifs`` conditions join the element pool because
        # they draw outer names of their own (fix-r3, ds1 sibling#8).
        bound = set(_assigned_names([generator.target
                                      for generator in node.generators]))
        drawn_elt = set()
        _collect_names(node.elt, drawn_elt, strict=strict)
        drawn_iters = set()
        for generator in node.generators:
            _collect_names(generator.iter, drawn_iters, strict=strict)
            for condition in generator.ifs:
                _collect_names(condition, drawn_elt, strict=strict)
        names |= (drawn_elt - bound) | drawn_iters
    elif isinstance(node, ast.DictComp):
        # Same rule for the dict shapes: key and value (and the ``ifs``
        # conditions) draw (recursively), the bound targets stay out of that
        # pool, and the iters keep their names whole.
        bound = set(_assigned_names([generator.target
                                      for generator in node.generators]))
        drawn_elt = set()
        _collect_names(node.key, drawn_elt, strict=strict)
        _collect_names(node.value, drawn_elt, strict=strict)
        drawn_iters = set()
        for generator in node.generators:
            _collect_names(generator.iter, drawn_iters, strict=strict)
            for condition in generator.ifs:
                _collect_names(condition, drawn_elt, strict=strict)
        names |= (drawn_elt - bound) | drawn_iters
    elif isinstance(node, ast.BinOp):
        _collect_names(node.left, names, strict=strict)
        _collect_names(node.right, names, strict=strict)
    elif isinstance(node, ast.BoolOp):
        # #3178 fix-r4 (MM sibling#7): ``a or b`` / ``p and q`` used to
        # draw nothing — an alias seeded from ``alias = src or out`` never
        # propagated, and a comprehension filter ``if p and q`` lost both
        # names (the truncating writes through them went unseen).
        for value in node.values:
            _collect_names(value, names, strict=strict)
    elif isinstance(node, ast.UnaryOp):
        # #3178 fix-r4 (MM sibling#7): ``not flag`` likewise — the operand
        # draws, the operator does not.
        _collect_names(node.operand, names, strict=strict)
    elif isinstance(node, ast.Subscript):
        _collect_names(node.value, names, strict=strict)
        _collect_names(node.slice, names, strict=strict)
    elif isinstance(node, ast.Compare):
        # #3178 fix-r3 (ds1 sibling#8): the comprehension ``ifs`` are filter
        # conditions, and the canonical shape is a Compare (``if p in
        # CANDIDATES``) — without this branch the ifs collection the same
        # round adds to the comprehension draw pools resolves nothing for it.
        _collect_names(node.left, names, strict=strict)
        for comparator in node.comparators:
            _collect_names(comparator, names, strict=strict)
    elif isinstance(node, ast.IfExp):
        _collect_names(node.body, names, strict=strict)
        _collect_names(node.test, names, strict=strict)
        _collect_names(node.orelse, names, strict=strict)
    elif isinstance(node, ast.JoinedStr):
        for value in node.values:
            _collect_names(value, names, strict=strict)
    elif isinstance(node, ast.FormattedValue):
        _collect_names(node.value, names, strict=strict)
    elif strict:
        # #3178 fix-r4: a guard expression node with no branch above.
        # Contributing nothing silently is how the partial-guard false
        # PASS shipped (control A2 of the fix-r4 probe); refuse instead.
        raise _UnreadGuardShape(type(node).__name__)


def _assigned_names(targets):
    """The names an assignment binds (tuple unpacking included)."""
    for target in targets:
        for sub in ast.walk(target):
            if isinstance(sub, ast.Name) and isinstance(sub.ctx, ast.Store):
                yield sub.id


def _with_target_names(item):
    """The names a with-item binds (``originals`` in ``as originals``).

    The snapshot mapping the guard yields is keyed by the guarded paths, so a
    name drawn from it (``for path, data in originals.items()``) is a guarded
    path under another spelling — it joins the alias seed (but never the
    resolvability decision: an opaque guard expression stays a violation).
    """
    names = set()
    if item.optional_vars is not None:
        for sub in ast.walk(item.optional_vars):
            if isinstance(sub, ast.Name):
                names.add(sub.id)
    return names


def _guarded_names(node):
    """The names a mutating_sources window's guard expressions draw from.

    ``node`` is the ``With`` statement; every ``mutating_sources`` item's
    guard expression is resolved recursively (#3178 fix-r1). The ``as``
    target is deliberately *not* included: it only aliases paths the guard
    already declared, so it cannot rescue an unresolvable guard.

    #3178 fix-r4: the collection is strict — a guard expression holding a
    node ``_collect_names`` has no branch for makes the whole window
    unreadable (``None``) instead of partially readable: the unread names
    would stay unguarded and a truncating write through them unseen.
    """
    names = set()
    for item in node.items:
        if not _is_call_to(item.context_expr, 'mutating_sources'):
            continue
        values = list(item.context_expr.args[1:2]) \
            + [kw.value for kw in item.context_expr.keywords
               if kw.arg in ('sources', 'paths')]
        try:
            for value in values:
                _collect_names(value, names, strict=True)
        except _UnreadGuardShape:
            return None
    return names


def scan_migration(text, rel):
    """Report every departure from the atomic-write migration in one table.

    Inside a ``mutating_sources`` window a truncating ``write_text`` or
    ``write_bytes`` may only ever target the evidence output — never a name
    guarded by the window (or an alias of one: ``path = paths[key]``,
    ``for path, data in originals.items()``), and never a
    ``write_source_atomic`` target (the guarded product source). A window
    whose guard set resolves to no names at all — or whose guard expression
    holds a node the collector cannot read in full (#3178 fix-r4) — is
    itself a violation: the scan refuses shapes it cannot read instead of
    silently skipping them (#3178 fix-r1). No ``write_source_atomic`` call may stat its target at
    call time for the ``mode=`` argument either: the helper already falls
    back to 0o644 for a deleted target, while a call-time stat raises
    FileNotFoundError and aborts the sequential restore loop, skipping every
    later file. Snapshot-recorded modes (``mode=modes[path]``) stay allowed.
    """
    violations = []
    tree = ast.parse(text)
    atomic_targets = set()
    atomic_roots = set()
    for node in ast.walk(tree):
        if not _is_call_to(node, 'write_source_atomic'):
            continue
        for arg in list(node.args[1:2]) + [kw.value for kw in node.keywords
                                           if kw.arg == 'path']:
            atomic_targets.add(ast.unparse(arg))
            root = _receiver_root(arg)
            if root is not None:
                atomic_roots.add(root)
        for kw in node.keywords:
            if kw.arg == 'mode':
                for sub in ast.walk(kw.value):
                    if isinstance(sub, ast.Call) and isinstance(sub.func, ast.Attribute) \
                            and sub.func.attr == 'stat':
                        violations.append(
                            f'{rel}: line {kw.lineno}: write_source_atomic passes a '
                            'call-time mode= stat (a deleted target would abort the '
                            'restore loop; record the mode in the snapshot instead)')
    for node in ast.walk(tree):
        if not isinstance(node, (ast.With, ast.AsyncWith)):
            continue
        if not any(_is_call_to(item.context_expr, 'mutating_sources')
                   for item in node.items):
            continue
        guarded = _guarded_names(node)
        if guarded is None:
            # #3178 fix-r4 (DS boundary#2 + MM sibling#7): a guard
            # expression holding a node the collector has no branch for
            # used to contribute its names silently, so the window stayed
            # half-readable — the unread names went unguarded and a
            # truncating write through them went unseen (the partial-guard
            # false PASS, probe control A2). Refuse the whole window
            # instead of enumerating one more node type per round.
            violations.append(
                f'{rel}: line {node.lineno}: the mutating_sources window guards '
                'on a shape the collector cannot read in full (a partially-read '
                'guard would leave a revert to truncating writes unseen on the '
                'unread names; rewrite the guard as a shape the scan resolves)')
            continue
        if not guarded:
            # #3178 fix-r1 (boundary #1): an unresolvable guard set is a
            # violation, never a silent skip — a computed shape the scan
            # cannot read would keep the whole window invisible to a revert
            # to truncating writes (the false PASS the first cut shipped).
            violations.append(
                f'{rel}: line {node.lineno}: the mutating_sources window declares '
                'no recognizable guarded names (a guard the scan cannot resolve '
                'must be rewritten as a resolvable shape, not skipped: a revert '
                'to truncating writes would go unseen here)')
            continue
        # A guarded name may be reborn under a local alias — ``path =
        # paths[key]``, ``for path, data in originals.items()`` — so writes on
        # the alias are caught too (#3178 fix-r1, boundary #1's second blind
        # spot: an exact-string ``receiver in atomic_targets`` match misses a
        # restore loop that renamed its variable). The ``as`` target joins the
        # seed because iterating the snapshot mapping yields the guarded
        # paths. Propagate to a fixpoint.
        aliases = set(guarded)
        for item in node.items:
            if _is_call_to(item.context_expr, 'mutating_sources'):
                aliases |= _with_target_names(item)
        changed = True
        while changed:
            changed = False
            for sub in ast.walk(node):
                targets, value = [], None
                if isinstance(sub, ast.Assign):
                    targets, value = sub.targets, sub.value
                elif isinstance(sub, (ast.AnnAssign, ast.AugAssign)):
                    if sub.value is not None:
                        targets, value = [sub.target], sub.value
                elif isinstance(sub, (ast.For, ast.AsyncFor)):
                    targets, value = [sub.target], sub.iter
                elif isinstance(sub, ast.comprehension):
                    targets, value = [sub.target], sub.iter
                if value is None:
                    continue
                drawn = set()
                _collect_names(value, drawn)
                if drawn & aliases:
                    for name in _assigned_names(targets):
                        if name not in aliases:
                            aliases.add(name)
                            changed = True
        for sub in ast.walk(node):
            if not (isinstance(sub, ast.Call) and isinstance(sub.func, ast.Attribute)
                    and sub.func.attr in ('write_text', 'write_bytes')):
                continue
            receiver = ast.unparse(sub.func.value)
            root = _receiver_root(sub.func.value)
            if root in aliases:
                violations.append(
                    f'{rel}: line {sub.lineno}: truncating {sub.func.attr} on the '
                    'guarded source itself (a kill mid-write leaves an empty or '
                    'half-written source; use write_source_atomic)')
            elif receiver in atomic_targets or root in atomic_roots:
                violations.append(
                    f'{rel}: line {sub.lineno}: truncating {sub.func.attr} on a '
                    'write_source_atomic target (the guarded source; use '
                    'write_source_atomic)')
    return violations


SWEEP_FAMILY = '''\
import sys
from pathlib import Path
from negative_process import mutating_sources, write_source_atomic

out = Path(sys.argv[1]).resolve()
root = Path(__file__).resolve().parents[5]
src = root / 'src/thing.rs'

# #1087 fix-r2: every guarded-source write in the window is one
# atomic rename (#1903 rule, shared with ``mutating_sources``).
with mutating_sources(root, [src]) as originals:
    mutant = originals[src].replace(b'left', b'right', 1)
    write_source_atomic(root, src, mutant)
{body}'''

# #3178 fix-r1 (sibling #3 / boundary #1): the computed-list guard shapes the
# migrated tables actually use (heartbeat's ListComp, live-history's and
# gateway-batch's Call) — the red side for the recursive name collection.
COMPUTED_GUARD_FAMILY = '''\
import sys
from pathlib import Path
from negative_process import mutating_sources, write_source_atomic

root = Path(__file__).resolve().parents[5]
paths = {{'src': root / 'src/thing.rs'}}

# #1087 fix-r2: every guarded-source write in the window is one
# atomic rename (#1903 rule, shared with ``mutating_sources``).
with mutating_sources(root, {guard}) as originals:
{body}'''


class MigrationWindowSweepTests(unittest.TestCase):
    """#3001 item 3: every migrated family table keeps the #1903 shape."""

    def test_family_windows_never_truncate_guarded_sources(self):
        population = dict(migration_files())
        self.assertTrue(len(population) >= 20,
                        '#3001 the migrated family must stay discoverable by its marker')
        for required in ('fe-coexist/negative_controls.py',
                         'live-history/negative.py',
                         'edgeclaw-heartbeat/negative.py',
                         'edgeclaw-heartbeat-indexer/negative.py',
                         'gateway-batch/negative_controls.py',
                         'ledger-diffs/negative_controls.py'):
            self.assertIn(required, population,
                          f'#3001 the sweep must cover the migrated table {required}')
        self.assertNotIn('negative_suite.py', population,
                         '#3001 the M6 shared harness stays outside this sweep; its '
                         'remaining call-time mode= sites belong to the M6 lane '
                         '(registered in '
                         'test_the_m6_shared_harness_mode_sites_stay_registered)')
        for rel, text in sorted(population.items()):
            with self.subTest(table=rel):
                self.assertEqual(scan_migration(text, rel), [],
                                 '#3001 a migrated table left the atomic-write shape')

    def test_the_m6_shared_harness_mode_sites_stay_registered(self):
        """#3178 fix-r2 (MM sibling#4): the sweep's one blind spot, pinned.

        ``negative_suite.py`` is the M6 lane's shared harness (#1099; the
        standing constraint keeps this round from touching its body), so it
        stays outside the sweep population by design — which left its four
        ``mode=...stat().st_mode`` call-time sites as the only ones in the
        tree that no scan ever sees (a rule that stays green while scanning
        nothing, the review's "规则假绿"). They are the registered exception
        instead: any drift — a fifth site, one removed, a line moved — must
        red this registration rather than pass silently, and retiring them
        is the M6 lane's deliberate act (update this pin then).
        """
        text = (FIXTURES / 'negative_suite.py').read_text(
            encoding='utf-8', errors='surrogateescape')
        findings = [violation for violation
                    in scan_migration(text, 'negative_suite.py')
                    if 'call-time mode=' in violation]
        lines = sorted(int(violation.split(' line ')[1].split(':')[0])
                       for violation in findings)
        self.assertEqual(
            lines, [401, 412, 807, 831],
            '#3178 negative_suite.py (the M6 lane\'s shared harness, #1099) '
            'must carry exactly the four registered call-time mode= sites '
            '(lines 401/412/807/831): a fifth site must red here, not pass '
            'unseen outside the sweep population')

    def _family(self, body):
        return SWEEP_FAMILY.format(body=body)

    def test_scan_flags_a_reverted_guarded_write(self):
        """The #3001 red side: a table reverted to write_text must go red."""
        source = self._family("    src.write_text('mutated')")
        self.assertTrue(any('truncating write_text on the guarded source' in v
                            for v in scan_migration(source, 'probe/table.py')),
                        '#3001 a guarded write reverted to write_text must red')

    def test_scan_flags_a_write_targeting_an_atomic_target(self):
        """The loop-variable shape (``for target, data in ...``) must red too.

        #3178 fix-r1: with alias tracking the loop variable over ``[(src,
        b'x')]`` is a guarded alias, so this shape now reds on the
        guarded-source message; the plain atomic-target message is still
        the one for an unaliased receiver, so both spellings count.
        """
        source = self._family(
            "    for target, data in [(src, b'x')]:\n"
            "        write_source_atomic(root, target, data)\n"
            "        target.write_text('mutated')")
        self.assertTrue(any('write_source_atomic target' in v
                            or 'guarded source itself' in v
                            for v in scan_migration(source, 'probe/table.py')),
                        '#3001 a truncating write on a write_source_atomic target must red')

    def test_scan_flags_call_time_mode_and_allows_snapshot_mode(self):
        source = self._family('')
        reverted = source.replace('write_source_atomic(root, src, mutant)',
                                  'write_source_atomic(root, src, mutant,\n'
                                  '                    mode=src.stat().st_mode & 0o7777)')
        self.assertTrue(any('call-time mode=' in v
                            for v in scan_migration(reverted, 'probe/table.py')),
                        '#3001 a call-time mode= stat must red')
        snapshotted = source.replace('write_source_atomic(root, src, mutant)',
                                    'write_source_atomic(root, src, mutant, mode=modes[src])')
        self.assertEqual(scan_migration(snapshotted, 'probe/table.py'), [],
                         '#3001 a snapshot-recorded mode stays allowed')

    def test_scan_flags_a_reverted_computed_guard_window(self):
        """#3178 fix-r1 (sibling #3 / boundary #1 / concurrency #1): a
        computed-list guard window reverted to a truncating write must red.

        The first cut of ``_guarded_names`` only accepted ``Name`` and
        all-``Name`` literals, so ``[paths[key] for key in paths]`` resolved
        to the empty set and ``if not guarded: continue`` silently skipped
        the window — the exact false PASS the review demonstrated by
        reverting ``edgeclaw-heartbeat/negative.py`` (evidence in
        _tmp/20261026-issue-3178/fix-r1/). Both spellings are pinned: the
        bare ListComp and the ``list(dict.fromkeys(...))`` Call form the
        live-history / gateway-batch tables use.
        """
        for guard in ('[paths[key] for key in paths]',
                      'list(dict.fromkeys([paths[key] for key in paths]))'):
            with self.subTest(guard=guard):
                atomic = COMPUTED_GUARD_FAMILY.format(
                    guard=guard,
                    body="    for path, data in originals.items():\n"
                         "        write_source_atomic(root, path, data)")
                self.assertEqual(scan_migration(atomic, 'probe/table.py'), [],
                                 '#3178 a computed guard must resolve and stay '
                                 'green in its atomic shape')
                reverted = COMPUTED_GUARD_FAMILY.format(
                    guard=guard,
                    body="    for path, data in originals.items():\n"
                         "        path.write_bytes(data)")
                self.assertTrue(
                    any('guarded source itself' in v
                        for v in scan_migration(reverted, 'probe/table.py')),
                    '#3178 a computed-guard window reverted to a truncating '
                    'write must red (the first cut skipped it silently)')

    def test_scan_flags_an_unresolvable_guard_window(self):
        """#3178 fix-r1 (boundary #1): a guard the scan cannot resolve is a
        violation, never a silent skip — an opaque shape would keep the whole
        window invisible to a revert to truncating writes."""
        source = self._family("    src.write_text('mutated')").replace(
            'mutating_sources(root, [src])',
            'mutating_sources(root, compute_paths())')
        self.assertTrue(any('no recognizable guarded names' in v
                            for v in scan_migration(source, 'probe/table.py')),
                        '#3178 an unresolvable guard window must be a '
                        'violation, not a silent skip')

    def test_scan_flags_a_comprehension_target_name_pollution(self):
        """#3178 fix-r2 (DS boundary#1, P0): the red side.

        ``[x for x in compute_paths()]`` binds its own ``x`` — a loop
        variable is not a name drawn from the enclosing scope. The first cut
        of the recursive collector added it to the guarded set anyway, so the
        unresolvable window looked resolvable, the fail-closed never fired,
        and the truncating write below went unseen (the false PASS the
        coordinator's probe demonstrated). The guard must resolve to no name
        and be reported as unresolvable; and the subtraction must not blind
        the scan to the collection actually iterated.
        """
        source = COMPUTED_GUARD_FAMILY.format(
            guard='[x for x in compute_paths()]',
            body="    src.write_text('mutated')")
        violations = scan_migration(source, 'probe/table.py')
        self.assertTrue(any('no recognizable guarded names' in v
                            for v in violations),
                        '#3178 a comprehension target name must not rescue an '
                        'unresolvable guard (fail-closed, never a false PASS)')
        resolvable = COMPUTED_GUARD_FAMILY.format(
            guard='[p for p in paths.values()]',
            body="    paths['src'].write_text('mutated')")
        self.assertTrue(
            any('truncating write_text on the guarded source' in v
                for v in scan_migration(resolvable, 'probe/table.py')),
            '#3178 dropping the comprehension target must not blind the scan '
            'to the collection it iterates (paths.values() is still a '
            'guarded-name draw, and the truncating write through it must red)')

    def test_scan_keeps_shadowed_iter_names_and_collects_ifs(self):
        """#3178 fix-r3 (DS3 boundary#1 + ds1 sibling#8): the red side.

        Two draw positions the fix-r2 blanket ``drawn - bound`` got wrong or
        missed. The iters are evaluated in the *enclosing* scope, so
        ``[p for p in [p]] + [q]`` draws the outer ``p`` through the iter;
        subtracting the bound target from it dropped the true guarded name,
        the window looked resolvable without it, and the truncated write on
        ``p`` went unseen (a false PASS the pre-fix-r2 code — which never
        collected the target at all — did flag). And the ``ifs`` conditions
        draw names of their own: a name appearing only in the filter clause
        (``if p in CANDIDATES``) used to miss the guarded set entirely. The
        DictComp branch carries the same two probes.
        """
        for guard in ('[p for p in [p]] + [q]',
                      '{p: p for p in [p] if p in [p]}'):
            with self.subTest(shadow=guard):
                source = COMPUTED_GUARD_FAMILY.format(
                    guard=guard, body="    p.write_text('mutated')")
                self.assertTrue(
                    any('truncating write_text on the guarded source' in v
                        for v in scan_migration(source, 'probe/table.py')),
                    '#3178 a name shadowed by the comprehension target must '
                    'stay guarded through its iter draw (the enclosing-scope '
                    'name the subtraction used to drop)')
        for guard in ('[p for p in [q] if r in [q]]',
                      '{k: v for k, v in [(q, q)] if r in paths}'):
            with self.subTest(ifs=guard):
                source = COMPUTED_GUARD_FAMILY.format(
                    guard=guard, body="    r.write_text('mutated')")
                self.assertTrue(
                    any('truncating write_text on the guarded source' in v
                        for v in scan_migration(source, 'probe/table.py')),
                    '#3178 a name drawn only by a comprehension ``ifs`` '
                    'condition must stay guarded (the third draw position the '
                    'collector used to skip entirely)')
        # The positive side: an ifs-carrying guard still resolves, stays
        # green in its atomic shape, and a truncating write through the
        # iterated collection still reds (no false red from the fix).
        atomic = COMPUTED_GUARD_FAMILY.format(
            guard='[p for p in [q] if q in paths]',
            body="    for path, data in originals.items():\n"
                 "        write_source_atomic(root, path, data)")
        self.assertEqual(scan_migration(atomic, 'probe/table.py'), [],
                         '#3178 an ifs-carrying computed guard must resolve '
                         'and stay green in its atomic shape')
        reverted = COMPUTED_GUARD_FAMILY.format(
            guard='[p for p in paths.values() if p in paths.values()]',
            body="    paths['src'].write_text('mutated')")
        self.assertTrue(
            any('truncating write_text on the guarded source' in v
                for v in scan_migration(reverted, 'probe/table.py')),
            '#3178 the iterated collection behind an ifs filter is still a '
            'guarded-name draw, and the truncating write through it must red')

    def test_scan_reads_boolop_and_unaryop_guards_and_aliases(self):
        """#3178 fix-r4 (MM sibling#7 + DS boundary#2): the red side.

        Three shapes the r1-r3 collector read as blank, each demonstrated
        unseen pre-fix by _tmp/20261115-3178-fixr4/probe-alias-shapes.py:
        an alias seeded from ``alias = src or out`` drew nothing, so the
        truncating write through the alias went unseen (the false PASS
        the review inferred); a comprehension filter ``if p and q`` drew
        neither name; a filter ``if not flag and p in paths`` lost both
        the negated flag and the compare side. The BoolOp/UnaryOp
        branches close all three, and the same guards stay green in
        their atomic shape (no false red from the fix).
        """
        # 1) alias = a or b: the alias must propagate to the write.
        source = COMPUTED_GUARD_FAMILY.format(
            guard='[src]',
            body="    alias = src or out\n"
                 "    alias.write_text('mutated')")
        self.assertTrue(
            any('truncating write_text on the guarded source' in v
                for v in scan_migration(source, 'probe/table.py')),
            '#3178 a BoolOp alias value must propagate the guarded name to '
            'the alias (the truncating write through it used to go unseen)')
        # 2) [p for p in Y if p and q]: the filter names must stay guarded.
        source = COMPUTED_GUARD_FAMILY.format(
            guard='[p for p in paths.values() if p and q]',
            body="    q.write_text('mutated')")
        self.assertTrue(
            any('truncating write_text on the guarded source' in v
                for v in scan_migration(source, 'probe/table.py')),
            '#3178 a name drawn only by a BoolOp comprehension filter must '
            'stay guarded (the filter used to be read as blank)')
        # 3) if not flag and p in X: UnaryOp+BoolOp+Compare together.
        source = COMPUTED_GUARD_FAMILY.format(
            guard='[p for p in paths.values() if not flag and p in paths]',
            body="    flag.write_text('mutated')")
        self.assertTrue(
            any('truncating write_text on the guarded source' in v
                for v in scan_migration(source, 'probe/table.py')),
            '#3178 a name drawn only by a not/and/in filter chain must stay '
            'guarded (both the negated flag and the compare side used to '
            'be read as blank)')
        # The positive side: the same guards stay green in their atomic
        # shape, and a benign BoolOp alias joins no violation either.
        atomic = COMPUTED_GUARD_FAMILY.format(
            guard='[p for p in paths.values() if not flag and p in paths]',
            body="    for path, data in originals.items():\n"
                 "        write_source_atomic(root, path, data)")
        self.assertEqual(scan_migration(atomic, 'probe/table.py'), [],
                         '#3178 a not/and/in filter guard must resolve and '
                         'stay green in its atomic shape')
        benign = COMPUTED_GUARD_FAMILY.format(
            guard='[src]',
            body="    alias = src or out\n"
                 "    write_source_atomic(root, alias, b'x')")
        self.assertEqual(scan_migration(benign, 'probe/table.py'), [],
                         '#3178 a BoolOp alias value feeding the atomic '
                         'restore helper is not a truncating write')

    def test_scan_fail_closes_on_guard_shapes_it_cannot_read(self):
        """#3178 fix-r4 (DS boundary#2 + ds4 simplify#4): the structural
        close-out.

        The collector used to answer every node it had no branch for with
        silence, so each review round found one more missing type (r2
        BinOp/Subscript, r3 Compare) and a partially-read guard kept the
        window looking resolved while its unread names stayed unguarded
        (probe control A2: ``[src, env.THING]`` with the write on
        ``env`` went unseen). An unrecognized node inside a guard now
        refuses the whole window instead of waiting for the next round
        to enumerate it. ``Constant`` leaves are name-free, so literal
        elements keep a guard readable — the reverse control.
        """
        for guard in ('[src, env.THING]', '{k: src}', 'src.attr'):
            with self.subTest(guard=guard):
                source = COMPUTED_GUARD_FAMILY.format(
                    guard=guard, body="    src.write_text('mutated')")
                self.assertTrue(
                    any('cannot read in full' in v
                        for v in scan_migration(source, 'probe/table.py')),
                    '#3178 a guard expression holding a node the collector '
                    'has no branch for must refuse the whole window (the '
                    'partially-read guard false PASS), not half-resolve it')
        # The reverse control: name-free literal leaves stay readable,
        # resolve to the readable side, and stay green in atomic shape.
        readable = COMPUTED_GUARD_FAMILY.format(
            guard="[src, 'literal']",
            body="    for path, data in originals.items():\n"
                 "        write_source_atomic(root, path, data)")
        self.assertEqual(scan_migration(readable, 'probe/table.py'), [],
                         '#3178 a name-free literal leaf must not refuse '
                         'the guard (constants can never draw a guarded '
                         'name)')

    def test_scan_allows_evidence_writes_and_population_follows_the_marker(self):
        self.assertEqual(
            scan_migration(self._family("    (out / 'row.log').write_text('log')"),
                           'probe/table.py'),
            [], '#3001 evidence output is never a guarded source')
        with tempfile.TemporaryDirectory(prefix='pdrs-3001-sweep-') as temp:
            root = Path(temp)
            marked = root / 'marked' / 'negative.py'
            marked.parent.mkdir()
            marked.write_text(self._family("    src.write_text('mutated')"))
            # The #1908 backlog shape: same violating write, no migration
            # marker — it must stay outside the sweep until it is migrated.
            # #3178 fix-r1 (boundary #7): the future-round spelling
            # `#1087 fix-r10` must not satisfy the fix-r1/r2 marker either.
            unmarked = ('backlog/negative_controls.py', 'future/negative_controls.py')
            for rel in unmarked:
                probe = root / rel
                probe.parent.mkdir(parents=True, exist_ok=True)
                probe.write_text(self._family("    src.write_text('mutated')")
                                 .replace('#1087 fix-r2', '#1908 backlog'))
            (root / 'future' / 'negative_controls.py').write_text(
                self._family("    src.write_text('mutated')")
                .replace('#1087 fix-r2', '#1087 fix-r10'))
            # #3178 fix-r1 (boundary #5): the marker alone only drives the
            # marker rule. Each exclusion surface is now probed with a file
            # that *carries* the marker, so deleting the directory rule, the
            # test_ prefix rule, or the top-level rule re-admits its probe.
            # #3178 fix-r2 (DS boundary#2): the prefix-rule probe lives in a
            # subdirectory — at the temp root the top-level rule ('/' not in
            # rel) fired first, so deleting the test_ prefix rule re-admitted
            # nothing and the rule was never actually probed (the comment
            # claimed a causality the probe could not deliver). In
            # family/test_marked.py only the prefix rule excludes it;
            # top_level.py owns the top-level rule alone.
            for rel in ('_shared/kit.py', 'negative-restore/own.py',
                        'family/test_marked.py', 'top_level.py'):
                probe = root / rel
                probe.parent.mkdir(parents=True, exist_ok=True)
                probe.write_text(self._family("    src.write_text('mutated')"))
            self.assertEqual([rel for rel, _ in migration_files(root)],
                             ['marked/negative.py'],
                             '#3001 only marked family tables enter the sweep '
                             '(the marked dir/prefix/top-level probes must stay '
                             'excluded by their own rule, not by the marker)')


TEST_CLASSES = (McpRestoreTests, MasksRestoreTests, NodeRestoreTests, RealGuardTests,
                SharedInvalidationTests, SessionFactoryTests, SessionRunnerTests,
                MigrationWindowSweepTests)


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
        # #1890-6: `get(key, 'HEAD')` hands an empty PDRS_NEGATIVE_BASELINE to
        # the guard; the default walk-through must catch that.
        ('session-baseline-empty-string', (SessionFactoryTests,),
         {(SessionFactoryTests, 'test_guard_defaults_to_head'):
          '#1037 an unset or empty baseline'},
         lambda: patch.dict(globals(), {'SESSION_SOURCE': changed_source(
             FIXTURES / 'session-factory/negative_controls.py',
             "                baseline=os.environ.get('PDRS_NEGATIVE_BASELINE') or 'HEAD') as originals:",
             "                baseline=os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')) as originals:")})),
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
             # #1281: the guard call gained an explicit baseline= argument (it
             # has to follow PDRS_NEGATIVE_BASELINE for uncommitted candidates),
             # so the anchor is the whole current line, not the old two-argument
             # form; an expired anchor here makes this reverse control INVALID.
             "        with mutating_sources(root, [], baseline=os.environ.get('PDRS_NEGATIVE_BASELINE') or 'HEAD'):",
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
    # r22：restore 循环包进 `with restore_critical():`（MiniMax r17 P1-1）后整体缩进 +4，
    # 锚点跟着源码走；write 锚点若停在旧的 20 空格，会作为 24 空格行的子串错位替换
    # （CI 实测 IndentationError），head 锚点则 0 命中（ValueError stale anchor）。
    # #1903：还原改走 `write_source_atomic(root, path, …)`（临时文件 + 一次 os.replace），
    # write 锚点跟着换成新行；吞掉 OSError 后目标仍是变异体，verify 循环照样抛
    # `restore failed: source.rs`，所以这条负控仍然只钉在 write 故障上。
    for label, test, old, new in [
        ('write', 'test_restore_write_failure',
         '                        write_source_atomic(root, path, snapshot[path],\n'
         '                                            mode=snapshot_modes[path])',
         '                        try:\n'
         '                            write_source_atomic(root, path, snapshot[path],\n'
         '                                                mode=snapshot_modes[path])\n'
         '                        except OSError:\n'
         '                            pass'),
        ('readback', 'test_restore_readback_mismatch',
         'raise RuntimeError(f"restore failed: {relative}")', 'pass'),
        ('head', 'test_restore_head_mismatch',
         'raise RuntimeError(\n                                f"restored {relative} does not match {baseline}")', 'pass'),
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
