#!/usr/bin/env python3
# anchor-selfcheck: unsupported static discovery-anchor scanner, no mutation definitions
"""#2133: a negative-control table must be findable by the literal it mutates.

``pdrs-ops/scripts/loop/neg-for-diff.sh`` derives the host's negative-control
list from the diff: for every changed ``src/.../dir/file.rs`` it computes the
literal ``dir/file`` and runs ``git grep -l -F "dir/file" -- fixtures/**/*negative*.py``.
Only tables whose text carries that literal are scheduled; a table that spells
the path as ``SMS = ROOT / '.../channel/sms'`` plus ``SMS / 'mod.rs'`` carries
only ``mod.rs``, is never found, and the changed surface silently passes with no
mutation run at all — a ruler that reports PASS without measuring.

So every table must declare the literals it needs in a module-level ``ANCHORS``
tuple. What counts as *needed* is structural, not textual: a ``.rs`` path built
as ``<constant> / '<file>'`` where ``<file>`` is a bare file name (no ``/``)
takes its directory from the constant, so ``dir/file`` can only reach the file
text through an anchor. ``SOURCE / "runtime/memory/gateway.rs"`` already spells
``memory/gateway.rs`` inline and needs no anchor; ``SOURCE / "mod.rs"`` does.

The guard therefore asserts, per table:

1. ``set(ANCHORS)`` equals the recomputed set of bare-filename keys — a new
   mutation row whose file has no anchor turns this red, and so does a stale or
   misspelled anchor;
2. every one of those keys occurs literally in the table's own text, which is
   exactly the condition ``git grep -F`` tests;
3. ``ANCHORS`` is a flat tuple of nonempty strings with no duplicates.

``test_removing_an_anchor_turns_the_guard_red`` re-runs the same check with one
anchor dropped per table, so the guard's discrimination is proven on every run
rather than only when somebody remembers to negate it by hand.

Scope: the four tables named by #2133. A scan of every ``fixtures/**/*negative*.py``
with the same rule finds nine more blind tables (``channel-core``,
``channel-unmigrated``, ``edgeclaw-entries/negative_round6``, ``failure-guard``,
``turn-gateway/*``, ``turn-replay``, ``turn-runner/forward_negative``,
``wecom-callback``, ``weixin-channel``); three of them have open PRs, so they are
left to a follow-up and this registry is the one-line place to add them.

Run from this directory, no cargo and no tree lock::

    python3 -B -m unittest -v test_literal_anchors
"""
import ast
from pathlib import Path
import unittest

FIXTURES = Path(__file__).resolve().parent

# The tables #2133 found blind. Adding a table here is the whole opt-in: the
# rule below then demands its ANCHORS match its own bare-filename keys.
TABLES = (
    'channel-sms/negative_controls.py',
    'memory-gateway/negative_controls.py',
    'snapshot-projection/negative_controls.py',
    'metadata-process-cwd/negative_controls.py',
)

ROOT_NAMES = frozenset({'ROOT'})
COMPREHENSIONS = (ast.ListComp, ast.SetComp, ast.GeneratorExp)


def _constants(tree):
    """Module-level ``name = <expression>`` bindings, in source order."""
    constants = {}
    for statement in tree.body:
        if isinstance(statement, ast.Assign):
            for target in statement.targets:
                if isinstance(target, ast.Name):
                    constants[target.id] = statement.value
    return constants


def _segments(node, constants, seen=()):
    """Path segments relative to the repository root, or None when unknown.

    Resolves the shapes the tables actually use: string constants, module-level
    names, ``/`` joins, ``Path('x')`` and ``Path(__file__)...parents[n]``. An
    unresolvable expression is skipped rather than guessed: a false key would
    demand an anchor no table can satisfy.
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return tuple(part for part in node.value.split('/') if part not in ('', '.'))
    if isinstance(node, ast.Name):
        if node.id in ROOT_NAMES:
            return ()
        if node.id in seen or node.id not in constants:
            return None
        return _segments(constants[node.id], constants, seen + (node.id,))
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        left = _segments(node.left, constants, seen)
        right = _segments(node.right, constants, seen)
        if left is None or right is None:
            return None
        return left + right
    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
            and node.func.id == 'Path' and node.args):
        return _segments(node.args[0], constants, seen)
    if (isinstance(node, ast.Subscript) and isinstance(node.value, ast.Attribute)
            and node.value.attr == 'parents'):
        return ()
    return None


def _rust_file(node):
    return (isinstance(node, ast.Constant) and isinstance(node.value, str)
            and node.value.endswith('.rs'))


def _bare_file(node):
    """A ``.rs`` file name with no directory of its own: the blind shape."""
    return _rust_file(node) and '/' not in node.value


def _iterated_files(node, constants):
    """The literal elements a ``for name in (...)`` loop walks, or None."""
    target = node
    if isinstance(node, ast.Name) and node.id in constants:
        target = constants[node.id]
    if isinstance(target, (ast.Tuple, ast.List)):
        return list(target.elts)
    return None


def bare_concat_keys(tree):
    """Every ``dir/file`` key reachable only through a bare-filename concat.

    Returns ``{key: [source expression, ...]}`` so a failure can name the rows
    that need the anchor instead of only the missing string.
    """
    constants = _constants(tree)
    keys = {}

    def record(segments, expression):
        if len(segments) < 2:
            return
        keys.setdefault('/'.join(segments[-2:]), []).append(expression)

    class Visitor(ast.NodeVisitor):
        def visit_BinOp(self, node):
            if isinstance(node.op, ast.Div) and _bare_file(node.right):
                segments = _segments(node, constants)
                if segments is not None:
                    record(segments, ast.unparse(node))
            self.generic_visit(node)

        def visit_ListComp(self, node):
            self._comprehension(node)

        def visit_SetComp(self, node):
            self._comprehension(node)

        def visit_GeneratorExp(self, node):
            self._comprehension(node)

        def _comprehension(self, node):
            element, generators = node.elt, node.generators
            if (len(generators) == 1
                    and isinstance(element, ast.BinOp)
                    and isinstance(element.op, ast.Div)
                    and isinstance(element.right, ast.Name)
                    and isinstance(generators[0].target, ast.Name)
                    and element.right.id == generators[0].target.id):
                base = _segments(element.left, constants)
                elements = _iterated_files(generators[0].iter, constants)
                if base is not None and elements is not None:
                    for candidate in elements:
                        if _bare_file(candidate):
                            record(base + (candidate.value,),
                                   f'{ast.unparse(element)} for {candidate.value}')
            self.generic_visit(node)

    Visitor().visit(tree)
    return keys


def declared_anchors(tree):
    """The module-level ``ANCHORS`` tuple, or None when it is not declared."""
    for statement in tree.body:
        if not isinstance(statement, ast.Assign):
            continue
        if not any(isinstance(target, ast.Name) and target.id == 'ANCHORS'
                   for target in statement.targets):
            continue
        if not isinstance(statement.value, (ast.Tuple, ast.List)):
            return None
        anchors = []
        for element in statement.value.elts:
            if not isinstance(element, ast.Constant) or not isinstance(element.value, str):
                return None
            anchors.append(element.value)
        return anchors
    return None


def violations(text, anchors=None):
    """Everything wrong with one table's discovery anchors; empty means green.

    ``anchors`` overrides the declared tuple so the negative control can drop an
    entry without rewriting the file.
    """
    tree = ast.parse(text)
    problems = []
    required = bare_concat_keys(tree)
    declared = declared_anchors(tree) if anchors is None else anchors
    if declared is None:
        problems.append('no module-level ANCHORS tuple of strings')
        declared = []
    duplicates = sorted({anchor for anchor in declared if declared.count(anchor) > 1})
    for anchor in duplicates:
        problems.append(f'duplicate anchor: {anchor}')
    for anchor in sorted(set(declared) - set(required)):
        problems.append(f'stale anchor (no bare-filename concat uses it): {anchor}')
    for key in sorted(set(required) - set(declared)):
        rows = ', '.join(sorted(set(required[key])))[:200]
        problems.append(f'missing anchor: {key} (needed by {rows})')
    for key in sorted(required):
        if key not in text:
            problems.append(f'anchor is not a literal in this file: {key}')
    return problems


class LiteralAnchorTests(unittest.TestCase):
    def test_tables_declare_their_discovery_anchors(self):
        for name in TABLES:
            with self.subTest(table=name):
                text = (FIXTURES / name).read_text(encoding='utf-8')
                self.assertEqual(violations(text), [], f'{name} discovery anchors')

    def test_removing_an_anchor_turns_the_guard_red(self):
        """Every declared anchor is load-bearing: drop one, the guard must fail."""
        for name in TABLES:
            text = (FIXTURES / name).read_text(encoding='utf-8')
            anchors = declared_anchors(ast.parse(text))
            self.assertIsNotNone(anchors, f'{name}: no ANCHORS to negate')
            if not anchors:
                continue  # covered by the injected-row probe below
            with self.subTest(table=name, dropped=anchors[0]):
                self.assertTrue(violations(text, anchors=anchors[1:]),
                                f'{name}: dropping {anchors[0]} stayed green')

    def test_a_new_bare_row_without_an_anchor_turns_the_guard_red(self):
        """The forward direction, on the real tables: a future row cannot slip by.

        The probe appends one bare-filename concat to each table's own source,
        so it holds for a table with anchors and for the empty one alike.
        """
        probe = ("\n# probe: a new mutation row whose file has no anchor\n"
                 "ANCHOR_PROBE = ROOT / 'src/anchor-probe' / 'unlisted.rs'\n")
        for name in TABLES:
            text = (FIXTURES / name).read_text(encoding='utf-8')
            with self.subTest(table=name):
                problems = violations(text + probe)
                self.assertTrue(any('anchor-probe/unlisted.rs' in problem
                                    for problem in problems), problems)

    def test_the_scanner_reports_a_synthetic_new_row(self):
        """The reporting path itself, on a two-row module built for the test."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example'\n"
            "ANCHORS = ('example/listed.rs',)\n"
            "rows = [('a', BASE / 'listed.rs', 'x'),\n"
            "        ('b', BASE / 'unlisted.rs', 'x')]\n"
        )
        problems = violations(synthetic)
        self.assertTrue(any('example/unlisted.rs' in problem for problem in problems),
                        problems)
        self.assertFalse(any('example/listed.rs' in problem for problem in problems),
                         problems)

    def test_an_inline_directory_needs_no_anchor(self):
        """``SOURCE / "runtime/x.rs"`` already spells the discovery key."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SOURCE = ROOT / 'src/pilotdeck-gateway/src'\n"
            "ANCHORS = ()\n"
            "rows = [('a', SOURCE / 'runtime/memory/gateway.rs', 'x')]\n"
        )
        self.assertEqual(violations(synthetic), [])


if __name__ == '__main__':
    unittest.main(verbosity=2)
