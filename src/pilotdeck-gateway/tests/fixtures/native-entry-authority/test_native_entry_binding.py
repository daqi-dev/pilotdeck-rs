# anchor-selfcheck: unsupported static binding drift self-check, no source replacement table
"""Drift controls for the #1261 native-entry ``--check`` bindings (#1989).

Every item of #1989 was a *silent* unbinding: the source could be rewritten
(pin assertions replaced with ``assert!(true, …)``, the byte-bound assertion
deleted, the newline guard short-circuited) and both ``--check`` scripts still
exited 0. This file mutates the bound test source in memory, one drift at a
time, and requires ``native_entry_binding.binding_errors`` to name the binding
that noticed it. The unmutated source must be clean, so a needle that stops
matching the real tree fails here as well.

Run from this directory: ``python3 -m unittest test_native_entry_binding.py``.
Cargo is never launched; the renderers and the two ``--check`` subprocesses are
pure Python.
"""
import subprocess
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(FIXTURES))

import native_entry_binding as binding

ROOT = HERE.parents[4]
TEST_FILE = ROOT / 'src/pilotdeck-gateway/tests/native_process.rs'
NEGATIVE = HERE / 'negative_controls.py'
EARLY_EXIT = HERE / 'early_exit_invalid.py'

SHAPE_POSITIVE = (
    '        assert!(\n'
    '            advertises(line.as_bytes()),\n'
    '            "must count as an advertisement: {line:?}"\n'
    '        );'
)
SHAPE_NEGATIVE = (
    '        assert!(\n'
    '            !advertises(line.as_bytes()),\n'
    '            "must not count as an advertisement: {line:?}"\n'
    '        );'
)
FINISHED = '    assert!(advertised, "finished advertisement: {collected:?}");'
EOF_HALF_LINE = '    assert!(!advertised, "line cut by EOF: {collected:?}");'
BYTE_BOUND_HALF_LINE = (
    '    assert!(\n'
    '        !advertised,\n'
    '        "line cut by the byte bound: {} bytes",\n'
    '        collected.len()\n'
    '    );'
)
BYTE_BOUND_STOPS = (
    '    assert!(\n'
    '        collected.len() <= CHILD_STDERR_LIMIT,\n'
    '        "collected {} bytes",\n'
    '        collected.len()\n'
    '    );'
)
NEWLINE_GUARD = (
    '    if !line.ends_with(b"\\n") {\n'
    '        return false;\n'
    '    }'
)
RETURN_ACTION = (
    '        return false;\n'
    '    }\n'
    '    let text = String::from_utf8_lossy(line);'
)
READ_ERROR_CASE = (
    '    let (collected, advertised) = read_advertisement(\n'
    '        ErrorAfter::new(b"native listening 127.0.0.1:4321"),\n'
    '        silence,\n'
    '        budget,\n'
    '    )\n'
    '    .await;'
)
BUDGET_CASE = (
    '    let (collected, advertised) =\n'
    '        read_advertisement(TalkativeLines::lines(3), budget, silence).await;'
)
DELEGATION = (
    '    read_advertisement(stderr, CHILD_SILENCE_LIMIT, CHILD_OBSERVATION_BUDGET).await'
)
SWAPPED_DELEGATION = (
    '    read_advertisement(stderr, CHILD_OBSERVATION_BUDGET, CHILD_SILENCE_LIMIT).await'
)
SILENCE_ELAPSED = (
    '    assert!(\n'
    '        elapsed < std::time::Duration::from_secs(1),\n'
    '        "silence bound, not the total budget: {elapsed:?}"\n'
    '    );'
)
BUDGET_ELAPSED = (
    '    assert!(\n'
    '        elapsed < std::time::Duration::from_secs(1),\n'
    '        "the total budget ends a talkative child: {elapsed:?}"\n'
    '    );'
)
REJECTS_ASSERTION = (
    '        negative_assert!(!advertised, "#1261 native entry rejects inherited {key}");'
)


class BindingDrift(unittest.TestCase):
    """One drift per test: the binding that owns it must report the miss."""

    maxDiff = None

    def assertDrift(self, old, new, expected):
        source = TEST_FILE.read_text()
        self.assertEqual(source.count(old), 1,
                         f'#1989 drift anchor is not unique: {old!r}')
        errors = binding.binding_errors(source.replace(old, new, 1))
        joined = '; '.join(errors)
        self.assertIn(expected, joined,
                      f'#1989 the drift was not noticed as {expected!r}: {joined}')

    # Item 1: pin assertions bound as whole calls, head + polarity + message.
    def test_a_constant_shape_acceptance_is_unbound(self):
        self.assertDrift(
            SHAPE_POSITIVE,
            '        assert!(true, "must count as an advertisement: {line:?}");',
            'accepted advertisement assertion')

    def test_a_constant_shape_rejection_is_unbound(self):
        self.assertDrift(
            SHAPE_NEGATIVE,
            '        assert!(true, "must not count as an advertisement: {line:?}");',
            'rejected advertisement assertion')

    def test_a_constant_finished_advertisement_is_unbound(self):
        self.assertDrift(
            FINISHED,
            '    assert!(true, "finished advertisement: {collected:?}");',
            'finished advertisement case')

    def test_a_constant_eof_verdict_is_unbound(self):
        self.assertDrift(
            EOF_HALF_LINE,
            '    assert!(true, "line cut by EOF: {collected:?}");',
            'EOF half-line case')

    def test_a_constant_byte_bound_verdict_is_unbound(self):
        self.assertDrift(
            BYTE_BOUND_HALF_LINE,
            '    assert!(true, "line cut by the byte bound: {} bytes", collected.len());',
            'byte bound half-line case')

    # Item 2: deleting the "buffer stops growing" assertion.
    def test_a_missing_byte_bound_is_unbound(self):
        self.assertDrift(BYTE_BOUND_STOPS, '', 'byte bound stops the buffer')

    # Item 3: the newline guard, by short circuit and by return action.
    def test_a_short_circuited_newline_guard_is_unbound(self):
        self.assertDrift(
            NEWLINE_GUARD,
            '    if !line.ends_with(b"\\n") && false {\n'
            '        return false;\n'
            '    }',
            'newline guard')

    def test_a_rewritten_newline_guard_return_is_unbound(self):
        self.assertDrift(
            RETURN_ACTION,
            '        return true;\n'
            '    }\n'
            '    let text = String::from_utf8_lossy(line);',
            'newline guard')

    # Item 4: a character literal must not shift literal boundaries, or a
    # two-space message would satisfy the one-space needle again.
    def test_a_character_literal_does_not_hide_a_drifted_message(self):
        self.assertDrift(
            REJECTS_ASSERTION,
            '        let c = \'"\';\n'
            '        negative_assert!(!advertised, "#1261  native entry rejects '
            'inherited {key}");',
            'listening advertisement assertion')

    # Item 5: the read-error and total-budget cases, the two elapsed bounds and
    # the bounded delegation must all stay bound.
    def test_a_missing_read_error_case_is_unbound(self):
        self.assertDrift(READ_ERROR_CASE, '', 'read error injection')

    def test_a_missing_budget_case_is_unbound(self):
        self.assertDrift(BUDGET_CASE, '', 'talkative child total budget')

    def test_a_swapped_delegation_is_unbound(self):
        self.assertDrift(DELEGATION, SWAPPED_DELEGATION, 'bounded delegation')

    def test_a_missing_silence_elapsed_bound_is_unbound(self):
        self.assertDrift(SILENCE_ELAPSED, '', 'silence elapsed bound')

    def test_a_missing_budget_elapsed_bound_is_unbound(self):
        self.assertDrift(BUDGET_ELAPSED, '', 'total budget elapsed bound')


class SharedTables(unittest.TestCase):
    """Item 6: one copy of the marker constants and the shared needles."""

    def test_the_real_source_satisfies_every_binding(self):
        self.assertEqual(binding.binding_errors(TEST_FILE.read_text()), [])

    def test_neither_runner_keeps_a_private_copy(self):
        for script in (NEGATIVE, EARLY_EXIT):
            text = script.read_text()
            self.assertIn('from native_entry_binding import', text, script.name)
            for own in ('MARKERS = [', 'MARKER_TEMPLATE =', 'SCRUB_MARKER ='):
                self.assertNotIn(own, text,
                                 f'{script.name} keeps a private copy of {own}')

    def test_both_runners_check_passes_on_the_real_tree(self):
        for script in (NEGATIVE, EARLY_EXIT):
            result = subprocess.run(
                [sys.executable, '-I', str(script), '--check'],
                cwd=HERE, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0,
                             script.name + ': ' + result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()
