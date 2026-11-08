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
import ast
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(FIXTURES))

import early_exit_invalid as early
import native_entry_binding as binding
import negative_controls as negative

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
READ_ERROR_EVIDENCE = (
    '    assert!(\n'
    '        collected.ends_with(b"127.0.0.1:4321"),\n'
    '        "read error keeps the bytes read before it: {collected:?}"\n'
    '    );'
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
SILENCE_INPUT = '    let silence = std::time::Duration::from_millis(50);'
BUDGET_INPUT = '    let budget = std::time::Duration::from_secs(5);'
REJECTS_ASSERTION = (
    '        negative_assert!(!advertised, "#1261 native entry rejects inherited {key}");'
)
# The scrub test's "cannot judge" branch and the rejects test's own gate: both
# were fragment needles before #1989 r2 review, so each gets a drift control.
SCRUB_BRANCH = (
    '    if !advertised {\n'
    '        panic!(\n'
    '            "#1261 native entry scrub child neither advertised listening nor '
    'exited, cannot judge"\n'
    '        );\n'
    '    }'
)
REJECTION_GATE = (
    '        if !stderr.contains(&rejection) {\n'
    '            panic!(\n'
    '                "#1261 native entry child exited early; no rejection and no '
    'listening advertisement for {key}, cannot judge"\n'
    '            );\n'
    '        }'
)


def assigned_names(text):
    """Every name a module assigns, wherever it sits on the line.

    A regex over `^NAME\\s*=` missed indented copies, annotated ones and
    `X = 1; NAME = ...`; walking the AST sees the assignment itself, so the
    "no private copy" rule has no layout to hide behind (#1989 r2 review).
    """
    names = set()
    for node in ast.walk(ast.parse(text)):
        if isinstance(node, ast.Assign):
            targets = node.targets
        elif isinstance(node, (ast.AnnAssign, ast.AugAssign, ast.For)):
            targets = [node.target]
        elif isinstance(node, (ast.comprehension,)):
            targets = [node.target]
        elif isinstance(node, ast.With):
            targets = [item.optional_vars for item in node.items
                       if item.optional_vars is not None]
        elif isinstance(node, ast.alias):
            # `import x as MARKERS` binds the shared name just as an
            # assignment would; a plain `from … import MARKERS` has no asname.
            if node.asname:
                names.add(node.asname)
            continue
        else:
            continue
        for target in targets:
            for leaf in ast.walk(target):
                if isinstance(leaf, ast.Name):
                    names.add(leaf.id)
    return names


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

    def assertGateDrift(self, old, new, expected):
        # The rejects test's own gate lives in `early_exit_invalid`; driving it
        # here proves the fragment needle no longer accepts a rewritten guard.
        source = TEST_FILE.read_text()
        self.assertEqual(source.count(old), 1,
                         f'#1989 gate drift anchor is not unique: {old!r}')
        errors = early.gate_errors(source.replace(old, new, 1))
        joined = '; '.join(errors)
        self.assertIn(expected, joined,
                      f'#1989 the gate drift was not noticed as {expected!r}: {joined}')

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

    def test_a_missing_read_error_evidence_is_unbound(self):
        # The verdict (`!advertised`) is a different needle: without this one
        # the error case would still pass with its evidence assertion deleted.
        self.assertDrift(READ_ERROR_EVIDENCE, '',
                         'read error keeps the bytes read before it')

    def test_a_swapped_delegation_is_unbound(self):
        self.assertDrift(DELEGATION, SWAPPED_DELEGATION, 'bounded delegation')

    def test_a_missing_silence_elapsed_bound_is_unbound(self):
        self.assertDrift(SILENCE_ELAPSED, '', 'silence elapsed bound')

    def test_a_missing_budget_elapsed_bound_is_unbound(self):
        self.assertDrift(BUDGET_ELAPSED, '', 'total budget elapsed bound')

    # #2102 P2-2: the two duration inputs. Shrunk to 500 ms, both keep every
    # elapsed bound green at runtime (500 ms is still under 1 s), and from
    # then on a `timeout(left, ...)` that lost `silence.min(left)` only ever
    # costs the budget -- only the input needles notice (#1989 item 5).
    def test_a_shortened_budget_input_is_unbound(self):
        self.assertDrift(BUDGET_INPUT,
                         '    let budget = std::time::Duration::from_millis(500);',
                         'total budget input')

    def test_a_widened_silence_input_is_unbound(self):
        self.assertDrift(SILENCE_INPUT,
                         '    let silence = std::time::Duration::from_millis(500);',
                         'silence bound input')

    # The case inputs and the rejects test's observation, kept from the old
    # per-script tables: a verdict assertion alone cannot tell which bytes it
    # judged.
    def test_a_trimmed_finished_input_is_unbound(self):
        self.assertDrift(
            'read_advertisement(&b"native listening 127.0.0.1:4321\\n"[..], '
            'silence, budget).await;',
            'read_advertisement(&b"native listening 127.0.0.1:4321"[..], '
            'silence, budget).await;',
            'finished advertisement input')

    def test_a_shortened_byte_bound_input_is_unbound(self):
        self.assertDrift('CHILD_STDERR_LIMIT * 2', 'CHILD_STDERR_LIMIT',
                         'unterminated line longer than the byte bound')

    def test_a_missing_rejects_observation_is_unbound(self):
        self.assertDrift(
            '        let (collected, advertised) = observe_child_stderr(stderr).await;',
            '',
            'listening advertisement observation')

    # The scrub "cannot judge" branch and the rejects test's gate were the two
    # needles that had already drifted between the runners; both are whole
    # guards now, so a polarity flip or a demoted panic must go red (#1989 r2
    # review).
    def test_a_polarity_flipped_scrub_branch_is_unbound(self):
        self.assertDrift(SCRUB_BRANCH,
                         SCRUB_BRANCH.replace('if !advertised', 'if advertised'),
                         'scrub unjudgeable branch')

    def test_a_rewritten_scrub_branch_message_is_unbound(self):
        self.assertDrift(SCRUB_BRANCH,
                         SCRUB_BRANCH.replace('cannot judge', 'cannot tell'),
                         'scrub unjudgeable branch')

    def test_a_short_circuited_rejection_gate_is_unbound(self):
        self.assertGateDrift(
            REJECTION_GATE,
            REJECTION_GATE.replace('contains(&rejection) {',
                                   'contains(&rejection) && false {'),
            'rejection gate')

    def test_a_demoted_rejection_gate_panic_is_unbound(self):
        self.assertGateDrift(
            REJECTION_GATE,
            '        if !stderr.contains(&rejection) {\n'
            '            let _ = "#1261 native entry child exited early; no '
            'rejection and no listening advertisement for {key}, cannot judge";\n'
            '        }',
            'rejection gate')


class SharedTables(unittest.TestCase):
    """Item 6: one copy of the marker constants and the shared needles."""

    def test_the_real_source_satisfies_every_binding(self):
        self.assertEqual(binding.binding_errors(TEST_FILE.read_text()), [])

    def test_neither_runner_keeps_a_private_copy(self):
        # Every constant the shared table owns, not a hand-picked three: a
        # re-forked copy of any of them would be invisible to the old list.
        shared = sorted(name for name in vars(binding)
                        if name.isupper() and not name.startswith('_'))
        for required in ('KEYS', 'MARKERS', 'MARKER_TEMPLATE', 'SCRUB_MARKER',
                         'SCRUB_EARLY_EXIT', 'TEST', 'SCRUB_TEST'):
            self.assertIn(required, shared)
        for script in (NEGATIVE, EARLY_EXIT):
            text = script.read_text()
            self.assertIn('from native_entry_binding import', text, script.name)
            self.assertEqual(sorted(assigned_names(text) & set(shared)), [],
                             f'{script.name} keeps a private copy of a shared name')

    def test_both_runners_consume_the_shared_table(self):
        # Importing the table is not consuming it: a runner could keep a
        # renamed private copy and still pass every check above (#1989 r3
        # review, P1). Driving each runner's own `--check` body with a mutated
        # source proves the shared needles are what it reports.
        drifted = TEST_FILE.read_text().replace(
            FINISHED, '    assert!(true, "finished advertisement: {collected:?}");', 1)
        self.assertNotEqual(drifted, TEST_FILE.read_text())
        for module in (negative, early):
            joined = '; '.join(module.check_errors(drifted))
            self.assertIn('finished advertisement case', joined, module.__name__)

    def test_a_commented_out_row_payload_is_unbound(self):
        # The early-exit rows must be judged on the same comment-stripped text
        # as the needles, or a payload left in a comment still "proves" the
        # row can reach its verdict (#1989 r3 review, second lineage).
        source = TEST_FILE.read_text()
        self.assertEqual(source.count(SCRUB_BRANCH), 1)
        commented = SCRUB_BRANCH.replace(
            '            "#1261 native entry scrub child neither advertised '
            'listening nor exited, cannot judge"',
            '            // "#1261 native entry scrub child neither advertised '
            'listening nor exited, cannot judge"')
        self.assertNotEqual(commented, SCRUB_BRANCH)
        joined = '; '.join(early.check_errors(source.replace(SCRUB_BRANCH, commented, 1)))
        self.assertIn('payload', joined)

    def test_neither_runner_carries_its_own_needle_literals(self):
        # The other half of the same rule: no needle-shaped literal may live
        # in a runner, whatever table it is pasted into.
        for script in (NEGATIVE, EARLY_EXIT):
            text = script.read_text()
            for shape in ('negative_assert!(', 'assert!(', 'if !advertised',
                          'line.ends_with', 'read_advertisement(stderr,'):
                self.assertNotIn(shape, text,
                                 f'{script.name} carries a needle literal: {shape}')

    def test_the_private_copy_rule_sees_every_assignment_form(self):
        # The forms a `^NAME\s*=` regex missed; all three are copies.
        for text in ('if x:\n    MARKERS = ["x"]\n',
                     'for MARKERS in (["x"],):\n    pass\n',
                     'with open("x") as MARKERS:\n    pass\n',
                     'import other as MARKERS\n',
                     '[MARKERS for MARKERS in [["x"]]]\n',
                     'MARKERS: list = ["x"]\n',
                     'OTHER = 1; MARKERS = ["x"]\n',
                     'def f():\n    MARKERS = ["x"]\n'):
            self.assertIn('MARKERS', assigned_names(text), text)
        # ... and it is not a substring match: uses and imports are not copies.
        for text in ('from native_entry_binding import MARKERS\n',
                     'print(MARKERS)\n',
                     'MARKERS_LIKE = 1\n'):
            self.assertNotIn('MARKERS', assigned_names(text), text)

    def test_both_runners_check_passes_on_the_real_tree(self):
        for script in (NEGATIVE, EARLY_EXIT):
            result = subprocess.run(
                [sys.executable, '-I', str(script), '--check'],
                cwd=HERE, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0,
                             script.name + ': ' + result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()
