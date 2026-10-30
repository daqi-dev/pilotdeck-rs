#!/usr/bin/env python3
# anchor-selfcheck: unsupported M6 migration tests, in-memory modules and disposable JSON only
"""Regression checks for #935/#782/#835 and the two raw summary adapters."""
from dataclasses import replace
from contextlib import contextmanager
import importlib.util
import json
import re
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _negkit import contract as C, submit as A, compare_verdicts as V
import anchor_selfcheck as CHECK

FIXTURES = Path(__file__).resolve().parents[1]
ROOT = FIXTURES.parents[3]


def runner(name):
    spec = importlib.util.spec_from_file_location('migration_' + name.replace('-', '_'),
                                                FIXTURES / name / 'negative.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def observed(binding, payload=None, *, prelude=(), location='fixture.rs:1:1', code=101):
    raw = f'running 1 test\ntest {binding.test} ... FAILED\n'
    for text in (*prelude, payload if payload is not None else binding.assertion):
        raw += f"thread '{binding.test}' panicked at {location}:\n{text}\n"
    raw += 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
    return subprocess.CompletedProcess([], code, raw, '')


class MigrationTests(unittest.TestCase):
    longMessage = True

    def test_scope_fallback(self):
        for marker in ('assertion', 'assertion `left == right` failed', '"case": ',
                       '#813 timeout: specific', ''):
            binding = C.Case('bad', 'suite::test', marker, kind='message', payload_pattern=re.escape(marker))
            result = C.classify(observed(binding), binding)
            self.assertEqual(result[0], 'INVALID', '#935 shared scoped check rejects generic markers')

    def test_bare_step_scope(self):
        marker = 'snapshot-success: step'
        binding = C.Case('bad', 'suite::test', marker, kind='message', payload_pattern=re.escape(marker))
        self.assertEqual(C.classify(observed(binding), binding)[0], 'INVALID',
                         '#935 bare step prefix never kills')

    def test_first_line_ownership(self):
        binding = C.Case('legacy', 'suite::test', 'specific assertion', kind='message',
                         payload_pattern=re.escape('specific assertion'))
        self.assertEqual(C.classify(observed(binding), binding)[0], 'KILLED', '#1110 own legacy message kills')
        for result in (observed(binding, 'unrelated\nspecific assertion'),
                       observed(binding, code=-9),
                       observed(binding, prelude=('unrelated',)),
                       observed(binding, '#813 timeout: specific assertion')):
            self.assertEqual(C.classify(result, binding)[0], 'INVALID',
                             '#1110 only one owned first-line message counts')
        result = observed(binding)
        result.stdout = result.stdout.replace("thread 'suite::test'", "thread 'sibling'")
        self.assertEqual(C.classify(result, binding)[0], 'INVALID', '#1110 sibling panic is invalid')

    def test_real_legacy_payload_shapes(self):
        entries, _ = runner('turn-submit').build()
        for entry in entries:
            binding = entry.binding
            if binding.kind != 'message':
                continue
            spoof = 'fixture loading ' + binding.assertion + ' was unavailable'
            self.assertEqual(C.classify(observed(binding, spoof), binding)[0], 'INVALID',
                             '#935 incidental first-line substring cannot kill')
        binding = next(e.binding for e in entries if e.binding.mutation == 'utf16-mode-lossy')
        self.assertEqual(C.classify(observed(binding, 'assertion `left == right` failed: message'), binding)[0],
                         'KILLED', '#935 recorded UTF16 assertion still kills')

    def test_recorded_panic_logs(self):
        entries, _ = runner('turn-submit').build()
        bindings = {entry.binding.mutation: entry.binding for entry in entries}
        recorded = FIXTURES / '_negkit/testdata'
        rows = json.loads((recorded / 'turn-submit.json').read_text())
        count = 0
        for row in rows:
            path = recorded / 'turn-submit-logs' / (row['name'] + '.log')
            if not path.exists():
                continue
            binding = bindings[row['name']]
            if binding.kind == 'location':
                # The archived source had different line numbers. Keep the
                # actual raw log unchanged and use its recorded location.
                binding = replace(binding, assertion=row['anchor'])
            result = subprocess.CompletedProcess([], row['exit'], path.read_text(), '')
            self.assertEqual(C.classify(result, binding)[0], row['status'],
                             '#1110 archived legacy panic form retains its verdict')
            count += 1
        self.assertEqual(count, 13, '#1110 all thirteen legacy panic logs are exercised')

    def test_submit_oracle_binding(self):
        oracle = [{'name': 'c', 'steps': ['accept', 'end']}]
        self.assertEqual(A.submit_marker(oracle, 'c', 1, 'end'),
                         '#813 c: step 1 "end": submit scenario must match', '#935 valid oracle step')
        for rows, name, index, step in ((oracle, 'unknown', 0, 'accept'),
                                       (oracle * 2, 'c', 0, 'accept'),
                                       (oracle, 'c', -1, 'end'), (oracle, 'c', 2, 'end'),
                                       (oracle, 'c', True, 'end'), (oracle, 'c', 1, 'accept')):
            rejected = False
            try:
                A.submit_marker(rows, name, index, step)
            except ValueError:
                rejected = True
            self.assertTrue(rejected, '#935 wrong case index or step must be refused')

    def test_baseline_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            env = dict(CARGO_HOME=directory, CARGO_TARGET_DIR=directory, PDRS_BASELINE=directory)
            good = subprocess.CompletedProcess([], 0, A.BASELINE_COMMIT + '\n', '')
            with patch.object(A, 'run_owned', return_value=good):
                self.assertEqual(A.baseline_environment(env)['PDRS_BASELINE'], directory,
                                 '#782 valid baseline retained')
            for result in (subprocess.CompletedProcess([], 0, 'deadbeef\n', ''),
                           subprocess.CompletedProcess([], 1, A.BASELINE_COMMIT, 'not a repository')):
                with patch.object(A, 'run_owned', return_value=result):
                    rejected = False
                    try:
                        A.baseline_environment(env)
                    except ValueError:
                        rejected = True
                    self.assertTrue(rejected, '#782 wrong baseline commit is refused')

    def test_baseline_paths_and_override(self):
        with tempfile.TemporaryDirectory() as directory:
            env = dict(CARGO_HOME=directory, CARGO_TARGET_DIR=directory, PDRS_BASELINE=directory)
            good = subprocess.CompletedProcess([], 0, A.BASELINE_COMMIT, '')
            for changes in ({'PDRS_BASELINE': 'HEAD'}, {'PDRS_BASELINE': directory + '/missing'},
                            {'PDRS_NEGATIVE_BASELINE': 'some-git-object'}):
                with patch.object(A, 'run_owned', return_value=good):
                    rejected = False
                    try:
                        A.baseline_environment(dict(env, **changes))
                    except ValueError:
                        rejected = True
                    self.assertTrue(rejected, '#782 path and HEAD-only source constraints are enforced')

    def test_unwrap_invariants(self):
        before, after = 'unwrap_or_else(|p| p.into_inner())', 'unwrap()'
        workspace = Path('/tmp/workspace')
        target = workspace / 'host.rs'
        original = ('// shifted\n  ' + before).encode()
        self.assertEqual(A.unwrap_site(workspace, target, before, after, original), 'host.rs:2:3',
                         '#835 location follows the actual span')
        self.assertEqual(A.unwrap_site(workspace, target, before, after, ('é. ' + before).encode()),
                         'host.rs:1:4', '#835 Rust columns count Unicode code points')
        for old, new, data in (('prefix.' + before, after, ('prefix.' + before).encode()),
                               (before, '\n' + after, original), (before, ' ' + after, original),
                               ('', after, original), (before, after, b'missing'),
                               (before, after, original * 2)):
            rejected = False
            try:
                A.unwrap_site(workspace, target, old, new, data)
            except ValueError:
                rejected = True
            self.assertTrue(rejected, '#835 location derivation rejects shifted or ambiguous unwrap spans')

    def test_poison_binding(self):
        binding = C.Case('poison', 'suite::poison', 'host.rs:5:3', kind='location',
                         prelude=('poison the leases mutex mid-turn',))
        payload = 'called `Result::unwrap()` on an `Err` value: PoisonError { .. }'
        result = observed(binding, payload, prelude=binding.prelude, location=binding.assertion)
        self.assertEqual(C.classify(result, binding)[0], 'KILLED', '#835 exact poison pair kills')
        for result in (observed(binding, payload, location=binding.assertion),
                       observed(binding, payload, prelude=('other panic',), location=binding.assertion),
                       observed(binding, 'unrelated', prelude=binding.prelude, location=binding.assertion),
                       observed(binding, payload, prelude=binding.prelude, location='host.rs:6:3')):
            self.assertEqual(C.classify(result, binding)[0], 'INVALID', '#835 poison requires both payloads and exact site')

    def test_complete_inventory(self):
        for name, count in [('turn-submit', 58), ('failure-guard', 24)]:
            entries, oracle = runner(name).build()
            self.assertEqual(len(entries), count, '#1110 migration preserves every original control')
            sample = json.loads((FIXTURES / '_negkit/testdata' / (name + '.json')).read_text())
            self.assertEqual([e.binding.mutation for e in entries], [r['name'] for r in sample],
                             '#1110 original case identity and ordering are unchanged')
            self.assertEqual(A.check(entries, oracle), count, '#1110 every current anchor is valid')
            changed = list(entries)
            index = 1 if name == 'turn-submit' else 2
            changed[index] = replace(changed[index], before='missing anchor')
            rejected = False
            try:
                A.check(changed, oracle)
            except ValueError:
                rejected = True
            self.assertTrue(rejected, '#1110 stale mutation anchors refuse before execution')

    def test_real_raw_summary_adapters(self):
        for name, count in [('turn-submit', 58), ('failure-guard', 24)]:
            path = FIXTURES / '_negkit/testdata' / (name + '.json')
            raw = json.loads(path.read_text())
            table = V.load_table(path)
            self.assertEqual(len(table), count, '#1110 adapter retains baseline and every mutation')
            self.assertTrue(V.compare(table, table)['equivalent'], '#1110 raw summary is comparable')
            self.assertFalse(V.compare(table, table[:-1])['equivalent'], '#1110 adapter cannot hide case loss')
            altered = json.loads(json.dumps(raw))
            if name == 'turn-submit':
                altered[-1]['anchor'] += ' changed'
            else:
                altered[-1]['marker'] += ' changed'
            rows = [V.legacy_row(row, index) for index, row in enumerate(altered)]
            self.assertFalse(V.compare(table, rows)['equivalent'], '#1110 adapter preserves legacy binding markers')
            canonical = {'controls': [dict(mutation=row['case'], test=row['bindings'][0]['test'],
                                          assertion=row['bindings'][0]['assertion'], status=row['status'],
                                          hits=row['hits']) for row in table]}
            with tempfile.TemporaryDirectory() as directory:
                generated = Path(directory) / 'canonical.json'
                generated.write_text(json.dumps(canonical))
                self.assertTrue(V.compare(table, V.load_table(generated))['equivalent'],
                                '#1110 raw legacy shape compares to canonical schema')

    def test_summary_rejects_unknown_or_contradictory(self):
        raw = json.loads((FIXTURES / '_negkit/testdata/failure-guard.json').read_text())[2]
        for changes in ({'name': 'unknown'}, {'valid': 'true'}, {'case': 'unknown'},
                        {'reason': 'baseline passed'}, {'exit': 0}, {'timed_out': True},
                        {'reaped': False}, {'message': '#1110 unregistered alias'}):
            rejected = False
            try:
                V.legacy_row(dict(raw, **changes), 0)
            except V.InputError:
                rejected = True
            self.assertTrue(rejected, '#1110 unknown or contradictory legacy verdict is refused')
        raw = json.loads((FIXTURES / '_negkit/testdata/turn-submit.json').read_text())
        poison = next(row for row in raw if row['name'] == 'lease-poison-unwrap-finish')
        for source, changes in ((raw[0], {'exit': 101}), (raw[0], {'before': 'unexpected'}),
                                (raw[1], {'status': []}), (raw[1], {'exit': 0}),
                                (raw[1], {'before': None}), (poison, {'anchorKind': 'message'})):
            rejected = False
            try:
                V.legacy_row(dict(source, **changes), 0)
            except V.InputError:
                rejected = True
            self.assertTrue(rejected, '#1110 turn verdict types exits and anchor kinds are preserved')
        baseline = json.loads((FIXTURES / '_negkit/testdata/failure-guard.json').read_text())[0]
        rejected = False
        try:
            V.legacy_row(dict(baseline, exit=False), 0)
        except V.InputError:
            rejected = True
        self.assertTrue(rejected, '#1110 failure-guard exit code cannot be a boolean')

    def test_legacy_guard_unsuccessful_states(self):
        rows = json.loads((FIXTURES / '_negkit/testdata/failure-guard.json').read_text())
        survived = dict(rows[2], valid=False, exit=0, reason='survived (test passed)')
        self.assertEqual(V.legacy_row(survived, 0)['status'], 'SURVIVED',
                         '#1110 legacy passed mutant remains SURVIVED')
        for row in (dict(rows[0], valid=False, exit=101, reason='baseline failed'),
                    dict(rows[2], valid=False, reason='baseline failed'),
                    dict(rows[2], valid=False, exit=None, timed_out=True, reason='deadline'),
                    dict(rows[2], valid=False, reaped=False, reason='process not reaped')):
            self.assertEqual(V.legacy_row(row, 0)['status'], 'INVALID',
                             '#1110 failed legacy evidence remains INVALID')
        for changes in ({'exit': False}, {'exit': 101}, {'timed_out': True}, {'reaped': False}):
            rejected = False
            try:
                V.legacy_row(dict(survived, **changes), 0)
            except V.InputError:
                rejected = True
            self.assertTrue(rejected, '#1110 legacy survival requires an integer zero and owned completion')

    def exercise_adapter(self, failure):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'example.rs'
            source.write_text('before')
            binding = C.Case('mutant', 'suite::test', '#1110 mutation assertion')
            entries = [A.Mutation(C.Case('baseline', binding.test, '#1110 positive baseline',
                                       kind='positive', positive_tests=(binding.test,))),
                       A.Mutation(binding, source, 'before', 'after')]
            phases = []

            @contextmanager
            def guard(_root, _paths):
                try:
                    yield {source: b'before'}
                finally:
                    source.write_bytes(b'before')
                    self.assertFalse((root / 'out/summary.json').exists(), '#1110 adapter summary waits for restore')

            def process(command, **_kwargs):
                if '--no-run' in command:
                    phases.append('compile')
                    return subprocess.CompletedProcess(command, 1 if failure == 'compile' else 0, '', '')
                phase = 'baseline' if not phases else ('mutant' if source.read_text() == 'after'
                                                        else 'restored' if 'compile' in phases else 'positive')
                phases.append(phase)
                if phase == 'baseline' and failure == 'baseline-exception':
                    raise subprocess.TimeoutExpired(command, 1)
                if phase == 'mutant':
                    if failure == 'timeout':
                        raise subprocess.TimeoutExpired(command, 1)
                    if failure == 'cancel':
                        raise KeyboardInterrupt()
                    return observed(binding)
                if phase == 'restored' and failure == 'restored':
                    return observed(binding)
                raw = (f'running 1 test\ntest {binding.test} ... ok\n'
                       'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n')
                return subprocess.CompletedProcess(command, 0, raw, '')

            env = dict(CARGO_HOME=str(root), CARGO_TARGET_DIR=str(root), PDRS_BASELINE=str(root))
            with patch.object(A, 'baseline_environment', return_value=env), patch.object(A, 'mutating_sources', guard), \
                    patch.object(A, 'run_owned', process), patch.object(A, 'wait_for_slot',
                    return_value=subprocess.CompletedProcess([], 0, '', '')):
                code = A.run(entries, [{}], root, root / 'out', [], env)
            restored = source.read_text()
            document = json.loads((root / 'out/summary.json').read_text()) if (root / 'out/summary.json').exists() else None
            return code, phases, restored, document

    def test_executor_lifecycle(self):
        code, phases, restored, document = self.exercise_adapter(None)
        self.assertEqual((code, phases, restored, document['controls'][-1]['status']),
                         (0, ['baseline', 'positive', 'compile', 'mutant', 'restored'], 'before', 'KILLED'),
                         '#1110 adapter compiles then tests and restores every mutation')
        for failure in ('compile', 'timeout', 'restored'):
            code, phases, restored, document = self.exercise_adapter(failure)
            self.assertEqual((code, phases[-1], restored, document['controls'][-1]['status']),
                             (2, 'restored', 'before', 'INVALID'),
                             '#1110 failed compilation or execution still runs restored positive')
        code, phases, restored, document = self.exercise_adapter('cancel')
        self.assertEqual((code, restored, document), (2, 'before', None), '#1110 cancellation restores without publication')

    def test_baseline_exception_dominates(self):
        code, phases, restored, document = self.exercise_adapter('baseline-exception')
        self.assertEqual(document['controls'][-1]['status'], 'INVALID',
                         '#1110 baseline exception invalidates dependent mutations')
        self.assertIn('mutant', phases, '#1110 baseline failure still runs diagnostic mutation')
        self.assertEqual((code, restored), (2, 'before'), '#1110 failed baseline exits nonzero after restore')

    def test_positive_inventory(self):
        binding = C.Case('baseline', 'suite', '#1110 positive baseline', kind='positive',
                         positive_tests=('suite::one', 'suite::two'))
        raw = ('running 2 tests\ntest suite::one ... ok\ntest suite::two ... ok\n'
               'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n')
        self.assertEqual(C.classify(subprocess.CompletedProcess([], 0, raw, ''), binding)[0], 'PASS',
                         '#1110 complete baseline passes')
        for text in (raw.replace('suite::two', 'suite::other'), raw.replace('suite::two', 'suite::one'),
                     raw.replace('2 passed', '1 passed'), raw.replace('0 failed', '1 failed')):
            self.assertEqual(C.classify(subprocess.CompletedProcess([], 0, text, ''), binding)[0], 'INVALID',
                             '#1110 positive baseline checks complete exact inventory')

    def test_migrated_anchor_scanner(self):
        for name, expected in [('turn-submit', 57), ('failure-guard', 22)]:
            anchors, _, _ = CHECK.extract(FIXTURES / name / 'negative.py', ROOT)
            self.assertEqual(len(anchors), expected, '#1110 scanner retains migrated mutation inventory')

    def test_runtime_anchor_drift(self):
        original = CHECK.subprocess.run
        for drift in ('inventory', 'mutation', 'location'):
            def changed(command, **kwargs):
                result = original(command, **kwargs)
                if '-c' not in command or 'm6_anchor_audit' not in command[command.index('-c') + 1]:
                    return result
                document = json.loads(result.stdout)
                if drift == 'inventory':
                    document['rows'].pop()
                elif drift == 'mutation':
                    document['rows'][0][2] += 'changed'
                else:
                    row = next(row for row in document['rows'] if row[4] == 'location')
                    row[5] = 'wrong.rs:1:1'
                return subprocess.CompletedProcess(command, result.returncode, json.dumps(document), result.stderr)
            rejected = False
            with patch.object(CHECK.subprocess, 'run', changed):
                try:
                    CHECK.extract(FIXTURES / 'turn-submit/negative.py', ROOT)
                except CHECK.Unsupported:
                    rejected = True
            self.assertTrue(rejected, '#1110 scanner rejects executed case mutation or location drift')


if __name__ == '__main__':
    unittest.main(verbosity=2)
