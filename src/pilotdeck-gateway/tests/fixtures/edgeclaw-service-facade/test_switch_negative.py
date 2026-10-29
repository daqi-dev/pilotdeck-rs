#!/usr/bin/env python3
# anchor-selfcheck: unsupported tests of the boundary switch controls, no product source mutations
"""#996 r2 P2-4 failure-path tests; no cargo or product source writes.

The mutations run against a temporary copy of the committed mutants.rs bytes
(SNAPSHOT): only disposable text is changed, so the product HEAD guard is never
bypassed, and a concurrently running real control cannot race the snapshot.
"""
import contextlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import switch_negative as runner

# The real control mutates the on-disk mutants.rs while these failure-path
# tests may run in the same session (#996 rule 12), so a live read races the
# mutation windows. Snapshot the committed bytes instead; anchor freshness
# stays enforced by the real run's stale-anchor check.
SNAPSHOT = b'''//! #988: fixture-only switches; ordinary and release libraries cannot enable them.

#[cfg(all(test, feature = "pdrs-mutants"))]
pub fn on(name: &str) -> bool {
    !name.is_empty() && std::env::var("PDRS_MUTANT").is_ok_and(|selected| selected == name)
}

#[cfg(not(all(test, feature = "pdrs-mutants")))]
#[inline(always)]
pub const fn on(_name: &str) -> bool {
    false
}

#[cfg(all(test, not(feature = "pdrs-mutants")))]
mod tests {
    #[test]
    fn switches_are_disabled_without_feature() {
        const DISABLED: bool = super::on("default-root-dir");
        assert!(
            !std::hint::black_box(DISABLED),
            "#988 non-feature switch must be constant false"
        );
        assert!(!super::on(""), "#988 empty selection must be disabled");
    }
}

#[cfg(all(test, feature = "pdrs-mutants"))]
mod tests {
    #[test]
    fn selection_matches_only_the_requested_name() {
        let selected = std::env::var("PDRS_MUTANT").unwrap_or_default();
        assert_eq!(
            super::on(&selected),
            !selected.is_empty(),
            "#988 selected switch"
        );
        assert!(!super::on(&(selected + "-other")), "#988 unrelated switch");
        assert!(!super::on(""), "#988 empty switch");
    }
}
'''


def cargo_result(test, state='ok', anchor=''):
    return SimpleNamespace(returncode=0 if state == 'ok' else 101,
        stdout=f'test {test} ... {state}\n'
               + ('test result: ok. 1 passed;' if state == 'ok' else 'test result: FAILED'),
        # Real libtest header: the shared judge only reads the named test's own
        # `thread '<name>' panicked at` block (#818), never a bare line.
        stderr='' if state == 'ok' else f"thread '{test}' panicked at src/pilotdeck-gateway/src/mutants.rs:1:1:\n{anchor}")


class SwitchNegativeTests(unittest.TestCase):
    def prepare(self, temp):
        """Point every mutation at a temporary copy of the snapshot bytes."""
        source = Path(temp) / 'mutants.rs'
        source.write_bytes(SNAPSHOT)
        rows = [(name, source, old, new, test, selector, args)
                for name, _path, old, new, test, selector, args in runner.MUTATIONS]
        original = source.read_bytes()

        @contextlib.contextmanager
        def guard(_root, paths):
            assert paths == [source]
            try:
                yield {source: original}
            finally:
                source.write_bytes(original)

        return source, rows, guard, original

    def run_main(self, temp, cargo, rows=None):
        """Drive main() against the temporary source; capture every command."""
        source, prepared, guard, original = self.prepare(temp)
        rows = prepared if rows is None else rows
        out = Path(temp) / 'out'
        calls = []

        def wrapped(command, **kwargs):
            calls.append((command, kwargs.get('env', {}).get('PDRS_MUTANT')))
            return cargo(command, source=source, rows=rows,
                         original=original.decode(), **kwargs)

        with patch.object(runner, 'MUTATIONS', rows), patch.object(runner, 'SOURCE', source), \
             patch.object(runner, 'mutating_sources', guard), \
             patch.object(runner, 'run_owned', side_effect=wrapped), \
             patch.object(runner, 'wait_for_slot',
                          return_value=SimpleNamespace(stdout='', stderr='')), \
             patch.object(runner.time, 'sleep'), \
             patch.object(sys, 'argv', ['switch_negative.py', str(out)]), \
             patch.dict(os.environ, {'CARGO_TARGET_DIR': temp}):
            code = runner.main()
        return (code, json.loads((out / 'summary.json').read_text()), calls,
                source.read_bytes(), original)

    def killing_cargo(self, command, *, source, rows, original, **_):
        """The source copy decides the outcome: mutated text fails the bound test."""
        if Path(command[0]).name != 'cargo' or '--no-run' in command:
            return SimpleNamespace(returncode=0, stdout='compiled', stderr='')
        text = source.read_text()
        active = next((row for row in rows if row[2] in original and row[2] not in text), None)
        test = command[command.index('--') - 1]
        if active is not None:
            return cargo_result(active[4], 'FAILED', runner.MESSAGES[active[0]])
        return cargo_result(test, 'ok')

    def test_all_controls_kill_with_per_row_commands(self):
        with tempfile.TemporaryDirectory() as temp:
            code, document, calls, source_bytes, original = self.run_main(temp, self.killing_cargo)
        self.assertEqual((code, source_bytes), (0, original),
                         '#818 every boundary control must kill and restore the source')
        self.assertEqual(document['counts'],
                         {'KILLED': len(runner.MUTATIONS), 'SURVIVED': 0, 'INVALID': 0},
                         '#818 every boundary control must be KILLED')
        self.assertEqual([row['status'] for row in document['controls']],
                         ['KILLED'] * len(runner.MUTATIONS),
                         '#818 every boundary row must publish KILLED')
        # Each row drives its own cargo shape and selector environment.
        by_test = {}
        for row in runner.MUTATIONS:
            by_test.setdefault(row[4], []).append(row)
        for command, selector in calls:
            if command[-1] != '--nocapture' or '--no-run' in command:
                continue
            test = command[command.index('--') - 1]
            rows = by_test.get(test)
            if rows is None:
                continue
            self.assertIn(selector, {row[5] for row in rows},
                          '#818 each row must run under its own selector')
            self.assertTrue(all(set(row[6]) <= set(command) for row in rows),
                            '#818 each row must carry its declared cargo arguments')
        gates = [command for command, _ in calls if '--test' in command]
        self.assertTrue(gates and all('mutants_boundary' in command for command in gates),
                        '#818 the boundary build gate must run')

    def test_bare_panic_line_is_not_a_kill(self):
        # #818 negative control: the same marker without the bound test's own
        # libtest thread header must stay INVALID, never a synthetic kill.
        def bare_cargo(command, *, source, rows, original, **_):
            if Path(command[0]).name != 'cargo' or '--no-run' in command:
                return SimpleNamespace(returncode=0, stdout='compiled', stderr='')
            text = source.read_text()
            active = next((row for row in rows if row[2] in original and row[2] not in text), None)
            test = command[command.index('--') - 1]
            if active is not None:
                answer = cargo_result(active[4], 'FAILED', runner.MESSAGES[active[0]])
                answer.stderr = answer.stderr.replace(f"thread '{active[4]}' ", '')
                return answer
            return cargo_result(test, 'ok')
        with tempfile.TemporaryDirectory() as temp:
            code, document, _calls, source_bytes, original = self.run_main(temp, bare_cargo)
        self.assertEqual((code, source_bytes), (1, original),
                         '#818 a headerless panic must fail the whole boundary suite')
        self.assertEqual(document['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(runner.MUTATIONS)},
                         '#818 every headerless synthetic kill must be INVALID')

    def test_survivor_keeps_the_summary_nonzero(self):
        def surviving_cargo(command, **_):
            if Path(command[0]).name != 'cargo' or '--no-run' in command:
                return SimpleNamespace(returncode=0, stdout='compiled', stderr='')
            return cargo_result(command[command.index('--') - 1], 'ok')
        with tempfile.TemporaryDirectory() as temp:
            code, document, _calls, _source_bytes, _original = self.run_main(temp, surviving_cargo)
        self.assertEqual(code, 1)
        self.assertEqual(document['counts'],
                         {'KILLED': 0, 'SURVIVED': len(runner.MUTATIONS), 'INVALID': 0})
        self.assertTrue(all('bound test still passed' in row['reason']
                            for row in document['controls']))

    def test_stale_anchor_times_out_but_restores_and_continues(self):
        def timeout_cargo(command, *, source, rows, original, **_):
            if Path(command[0]).name != 'cargo' or '--no-run' in command:
                return SimpleNamespace(returncode=0, stdout='compiled', stderr='')
            text = source.read_text()
            active = next((row for row in rows if row[2] in original and row[2] not in text), None)
            if active is not None:
                raise subprocess.TimeoutExpired(command, 1)
            return cargo_result(command[command.index('--') - 1], 'ok')
        with tempfile.TemporaryDirectory() as temp:
            source, rows, guard, original = self.prepare(temp)
            # Break one anchor: that mutation text is absent from the copy.
            stale = rows[0][0]
            rows[0] = (stale, source, 'absent anchor', 'never applied', *rows[0][4:])
            out = Path(temp) / 'out'

            def wrapped(command, **kwargs):
                return timeout_cargo(command, source=source, rows=rows,
                                     original=original.decode(), **kwargs)

            with patch.object(runner, 'MUTATIONS', rows), patch.object(runner, 'SOURCE', source), \
                 patch.object(runner, 'mutating_sources', guard), \
                 patch.object(runner, 'run_owned', side_effect=wrapped), \
                 patch.object(runner, 'wait_for_slot',
                              return_value=SimpleNamespace(stdout='', stderr='')), \
                 patch.object(runner.time, 'sleep'), \
                 patch.object(sys, 'argv', ['switch_negative.py', str(out)]), \
                 patch.dict(os.environ, {'CARGO_TARGET_DIR': temp}):
                self.assertEqual(runner.main(), 1)
            document = json.loads((out / 'summary.json').read_text())
            by_name = {row['name']: row for row in document['controls']}
            self.assertEqual(by_name[stale]['status'], 'INVALID')
            self.assertIn('stale anchor', by_name[stale]['reason'])
            self.assertEqual(source.read_bytes(), original)

    def test_runtime_shrink_fails_the_whole_suite(self):
        # #996 fix-r5: this entry's MUTATIONS table is its own declaration, so
        # a main() that drops a row at runtime used to publish the shorter
        # suite as an all-KILLED success. The shared post-execution check must
        # reconcile the executed names with the script's static table.
        with tempfile.TemporaryDirectory() as temp:
            _source, prepared, _guard, _original = self.prepare(temp)
            dropped = prepared[0][0]
            code, document, _calls, source_bytes, original = self.run_main(
                temp, self.killing_cargo, rows=prepared[1:])
        self.assertEqual(code, 1, '#996 r5 a dropped declared row must fail '
                                  'the whole boundary suite')
        self.assertEqual(source_bytes, original,
                         '#996 r5 the drifted run must still restore the source')
        self.assertEqual(document['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(prepared) - 1},
                         '#996 r5 the drifted run must count every row INVALID')
        reasons = [row['reason'] for row in document['controls']]
        self.assertTrue(all('switch denominator drift' in reason for reason in reasons),
                        '#996 r5 every drifted row must name the denominator check')
        self.assertTrue(any(f'missing=[{dropped!r}]' in reason for reason in reasons),
                        f'#996 r5 the drift must list the missing {dropped}')

    def test_runtime_empty_table_exits_nonzero(self):
        # #996 fix-r6 review P0: r5 only reconciled a shrunken table; a main()
        # that empties MUTATIONS whole at runtime left results=[], the final
        # any() read False, and main() returned 0 with an all-zero summary
        # (the review's in-memory repro). The shared check now keeps a
        # suite-level INVALID row, which also decides this entry's exit code.
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / 'out'

            @contextlib.contextmanager
            def emptied_table(_root, _paths):
                yield {}

            with patch.object(runner, 'MUTATIONS', []), \
                    patch.object(runner, 'mutating_sources', emptied_table), \
                    patch.object(sys, 'argv', ['switch_negative.py', str(out)]), \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': temp}), \
                    patch('sys.stderr') as quiet:
                quiet.write = lambda *_: None
                code = runner.main()
            document = json.loads((out / 'summary.json').read_text())
        declared = sorted(name for name, *_ in runner.MUTATIONS)
        self.assertNotEqual(code, 0, '#996 r6 a runtime table emptied whole must '
                                      'exit nonzero, not publish an empty success')
        self.assertEqual(document['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#996 r6 the emptied run must keep a suite-level INVALID row')
        reason = document['controls'][0]['reason']
        self.assertIn('switch denominator drift', reason,
                      '#996 r6 the suite-level row must name the denominator check')
        for name in declared:
            self.assertIn(name, reason,
                          f'#996 r6 the drift must list the missing {name}')

    def test_source_guard_failure_fails_every_row_closed(self):
        @contextlib.contextmanager
        def guard(_root, _paths):
            raise RuntimeError('tree dirty')
            yield {}

        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / 'out'
            with patch.object(runner, 'mutating_sources', guard), \
                 patch.object(sys, 'argv', ['switch_negative.py', str(out)]), \
                 patch.dict(os.environ, {'CARGO_TARGET_DIR': temp}):
                self.assertEqual(runner.main(), 1)
            document = json.loads((out / 'summary.json').read_text())
            self.assertEqual([row['status'] for row in document['controls']],
                             ['INVALID'] * len(runner.MUTATIONS))
            self.assertTrue(all('source guard' in row['reason'] for row in document['controls']))


if __name__ == '__main__':
    unittest.main()
