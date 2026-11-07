#!/usr/bin/env python3
# anchor-selfcheck: unsupported static discovery-anchor scanner, no mutation definitions
"""#2133: a negative-control table must be findable by the literal it mutates.

``pdrs-ops/scripts/loop/neg-for-diff.sh`` derives the host's negative-control
list from the diff: for every changed file it computes the literal
``basename(dirname)/basename`` and runs
``git grep -l -F "dir/file" -- fixtures/**/*negative*.py``. Only tables whose
*text* carries that literal are scheduled; a table that spells the path as
``SMS = ROOT / '.../channel/sms'`` plus ``SMS / 'mod.rs'`` carries only
``mod.rs``, is never found, and the changed surface silently passes with no
mutation run at all — a ruler that reports PASS without measuring.

So every table must declare the literals it needs in a module-level ``DISCOVERY_ANCHORS``
tuple. What counts as *needed* is structural: any path built as
``<base> / '<bare file name>'`` takes its directory from ``<base>``, so
``dir/file`` can only reach the file text through an anchor.
``SOURCE / "runtime/memory/gateway.rs"`` already spells ``memory/gateway.rs``
inline and needs no anchor; ``SOURCE / "mod.rs"`` does. The rule covers every
file the table names, not only ``.rs``: the discovery loop keys on any changed
file under ``src/*.rs`` **or** ``tests/fixtures/**``, so a table that reads
``FIXTURE / 'oracle.json'`` is just as blind when that oracle changes.

The guard therefore asserts, per registered table:

1. ``set(DISCOVERY_ANCHORS)`` equals the recomputed set of bare-filename keys — a new row
   whose file has no anchor turns this red, and so does a stale or misspelled
   anchor;
2. every one of those keys occurs literally in the table's own text, which is
   exactly the condition ``git grep -F`` tests;
3. ``DISCOVERY_ANCHORS`` is a flat tuple of nonempty strings with no duplicates, declared
   exactly once (a second assignment would silently narrow the set);
4. every other table under this directory is either registered or listed in
   ``KNOWN_BLIND``, so a new blind table cannot appear unnoticed.

It is fail-closed on shapes it cannot resolve: a bare-filename concat whose base
is not a literal path this scanner can evaluate is reported as a problem, never
skipped. ``_segments`` understands module constants, an alias assigned exactly
once anywhere in the file, ``/`` joins, ``ROOT``, ``__file__``, ``Path(...)``,
``.parent``, ``.parents[n]``, ``.resolve()``, ``.joinpath()``, ``.with_name()``
and ``.with_suffix()``. Paths built by string ``+`` or f-strings are outside that
family and are reported as errors rather than guessed; so is anything else the
resolver cannot follow from the repository root. Silent skipping is what makes a
discovery gate *look* green while measuring nothing.

``test_removing_an_anchor_turns_the_guard_red`` re-runs the check with each
anchor dropped in turn, so the guard's discrimination is proven on every run
rather than only when somebody remembers to negate it by hand. Local negative
controls cover the other directions: a new bare row, an empty ``TABLES``, a
second ``DISCOVERY_ANCHORS`` assignment, a string-built path, and a new blind
table appearing outside both registers.

Scope: the four tables named by #2133 are the ones that carry anchors today;
adding a table to ``TABLES`` is the whole opt-in, and the rule then demands its
``DISCOVERY_ANCHORS`` match its own keys. Everything else that already builds
repository paths from bare file names is listed in ``KNOWN_BLIND`` — 96 tables,
273 keys, 122 of them on product files at the time of writing. That register is
a fix list, not an exemption: ``test_no_table_is_blind_outside_the_registers``
turns this red when a *new* table arrives blind, and the entries may only leave
it by being fixed and moved into ``TABLES``. Three of them have open PRs
(#2121, #2123, #2033), so they were left out of this change. ``schemata.py``
tables (``turn-gates`` / ``turn-prompt-hooks`` / ``turn-runner-tail`` /
``turn-start-notices``) do not match the discovery glob at all — that is a
limit of ``neg-for-diff.sh`` itself and no anchor can fix it.

Run from this directory, no cargo and no tree lock::

    python3 -B -m unittest -v test_negative_discovery_anchors
"""
import ast
from pathlib import Path
import unittest

FIXTURES = Path(__file__).resolve().parent

# Where this directory sits below the repository root. Tables are addressed by
# their path relative to the root (``channel-sms/negative_controls.py``), so
# ``Path(__file__)`` needs the prefix to resolve ``.parents[n]`` / ``.parent``
# the way the interpreter does. It is the layout every table already assumes
# when it writes ``ROOT = Path(__file__).resolve().parents[5]``.
FIXTURES_FROM_ROOT = ('src', 'pilotdeck-gateway', 'tests', 'fixtures')

# The tables #2133 found blind. Adding a table here is the whole opt-in: the
# rule below then demands its DISCOVERY_ANCHORS match its own bare-filename keys.
TABLES = (
    'channel-sms/negative_controls.py',
    'memory-gateway/negative_controls.py',
    'snapshot-projection/negative_controls.py',
    'metadata-process-cwd/negative_controls.py',
)


# Tables that build repository paths from bare file names without declaring
# DISCOVERY_ANCHORS yet. This is a register, not an exemption: an entry may be
# deleted only by fixing the table (add the anchors, move it to TABLES), and a
# new table that arrives blind turns the guard red until it is registered here
# with a reason or fixed. Counts of the keys each one is missing are in #2133.
# 96 tables, product-table count 51
KNOWN_BLIND = {
        'agent-event-projector/negative.py': '2 product',
        'anchor-marker/negative_controls.py': '2 fixture',
        'api-web/negative_controls.py': '2 fixture',
        'attach-followups-a/negative_controls.py': '2 fixture',
        'background-persist-progress/negative_controls.py': '2 fixture',
        'background-persist-restart/negative_controls.py': '2 fixture',
        'background-subtick-gate/negative_controls.py': '2 fixture',
        'channel-core/negative_controls.py': '3 product',
        'channel-unmigrated/negative_controls.py': '1 product',
        'cli-parity-isolation/negative_controls.py': '1 product',
        'cli-parity-mock-gateway/negative_controls.py': '1 fixture',
        'closure_guard_negative.py': '2 fixture',
        'connection-context/negative.py': '3 product',
        'cron-schedule/negative_controls.py': '2 fixture',
        'cron-store/negative_controls.py': '2 fixture',
        'edgeclaw-accessors/negative.py': '4 product',
        'edgeclaw-dream/negative_shapes.py': '1 product',
        'edgeclaw-entries/negative_round6.py': '1 product',
        'edgeclaw-heartbeat-isolation/negative.py': '3 fixture',
        'edgeclaw-messages/negative.py': '1 product',
        'edgeclaw-native-default/negative.py': '2 fixture',
        'edgeclaw-provider/negative.py': '2 fixture',
        'edgeclaw-same-root-generation/negative.py': '3 fixture',
        'edgeclaw-service-orchestration/switch_negative.py': '2 fixture',
        'edgeclaw-text/negative.py': '5 product',
        'failure-guard/negative.py': '3 product',
        'file-history-cwd/negative_controls.py': '3 product',
        'gateway-batch/negative_controls.py': '2 fixture',
        'issue-297/negative_controls.py': '1 fixture',
        'live-e2e/negative_controls.py': '3 fixture',
        'live-e2e-tmpdir/negative_controls.py': '2 fixture',
        'locale-adaptive/negative_controls.py': '2 fixture',
        'manager-gateway/negative_controls.py': '1 fixture',
        'metadata-from-env/negative_controls.py': '2 product',
        'model-catalog-cwd/negative_controls.py': '4 product',
        'module-host/negative_controls.py': '2 fixture',
        'module-host-approvals/negative_controls.py': '2 fixture',
        'native-archive-cwd/negative_controls.py': '1 product',
        'native-archive-cwd/negative_regeneration.py': '1 product',
        'native-collation/negative_controls.py': '3 product',
        'native-cron/negative_controls.py': '2 fixture',
        'native-http-basic/negative_controls.py': '2 fixture',
        'native-loop-stimulus/negative_controls.py': '1 product',
        'native-management/negative_controls.py': '1 product',
        'native-metadata-cwd/negative_controls.py': '1 product',
        'native-metadata-cwd/negative_regeneration.py': '1 product',
        'native-process/negative_controls.py': '2 fixture',
        'native-registry-switch/negative_controls.py': '1 product',
        'native-storage-stimulus/negative_controls.py': '1 product',
        'native-topology/negative_signals.py': '2 product',
        'native-ws-caps/negative_controls.py': '2 fixture',
        'native-ws-entry/negative_controls.py': '4 product',
        'negative-adaptive/isolation_negative.py': '1 product',
        'negative-adaptive/negative_controls.py': '2 fixture',
        'negative-output-directory/negative.py': '3 fixture',
        'output-schema/negative.py': '1 product',
        'pending-reanchor/negative_controls.py': '3 fixture',
        'projector-cwd/negative_controls.py': '2 product',
        'proxy-listen-address/negative_controls.py': '1 product',
        'recorder/fix3-negative.py': '2 product',
        'run-registry/count-negative.py': '2 product',
        'run-registry/timestamp-negative.py': '4 fixture',
        'sdk-format/negative_controls.py': '2 product',
        'sdk-run/negative_controls.py': '3 product',
        'sdk-sessions-list/negative_controls.py': '2 fixture',
        'sdk-sessions-messages/negative_controls.py': '4 product',
        'sdk-settings/negative_controls.py': '2 product',
        'sdk-signals/negative_controls.py': '3 fixture',
        'sdk-terminal/negative_controls.py': '2 product',
        'session-admission/negative.py': '1 product',
        'session-adopt/switch_negative.py': '2 fixture',
        'session-domain/fix2-negative.py': '1 fixture',
        'session-drop-release/negative_controls.py': '1 fixture',
        'session-factory/append-delta/negative_controls.py': '2 product',
        'session-factory/negative_controls.py': '1 fixture',
        'session-factory/projection-cache/negative_controls.py': '7 product',
        'shared-runner-failclosed/negative_controls.py': '1 fixture',
        'skills/negative_controls.py': '2 product',
        'stats-cwd/negative_controls.py': '3 product',
        'stress-followups/negative_controls.py': '1 fixture',
        'surrogate-collation-matrix/negative_controls.py': '2 product',
        'treelock-budget/negative_controls.py': '5 fixture',
        'turn-gateway/negative-fence.py': '4 product',
        'turn-gateway/negative.py': '1 product',
        'turn-publish/negative.py': '1 fixture',
        'turn-read/negative.py': '1 fixture',
        'turn-replay/negative.py': '3 product',
        'turn-runner/forward_negative.py': '4 product',
        'turn-runner/integrity_negative.py': '1 fixture',
        'turn-submit/negative.py': '1 product',
        'user-dialog-channel/negative.py': '5 product',
        'web-conversion-lifecycle/negative.py': '2 product',
        'webhook-channel/negative_controls.py': '4 fixture',
        'wecom-callback/negative_controls.py': '5 product',
        'weixin-channel/negative_controls.py': '7 product',
        'workspace-pristine/negative_controls.py': '4 fixture',
}

ROOT_NAMES = frozenset({'ROOT'})

# Shared executors and helpers every table imports; they are not tables
# themselves, and ``neg-for-diff.sh`` filters them out of the list it emits.
# Modules under a leading-underscore directory (``_shared``, ``_negkit``) are
# libraries, not discovery targets.
SHARED_HELPERS = frozenset({
    'anchor_selfcheck.py', 'negative_panic.py', 'negative_process.py',
    'negative_selection.py', 'negative_suite.py', 'pending_reanchor.py',
})


def _constants(tree):
    """``name = <expression>`` bindings this scanner is willing to substitute.

    Module-level bindings are authoritative (last one wins, as at runtime).
    Tables also build paths from local aliases (``source = ROOT / '...'`` inside
    a function, then ``source.parent / 'x.rs'``); a name assigned exactly once in
    the whole file has no ambiguity, so it is resolved too. A name assigned more
    than once outside the module level could mean different things at different
    sites, so it stays unresolved and the caller fails closed.
    """
    constants = {}
    for statement in tree.body:
        if isinstance(statement, ast.Assign):
            for target in statement.targets:
                if isinstance(target, ast.Name):
                    constants[target.id] = statement.value
    counts = {}
    assignments = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name):
                    counts[target.id] = counts.get(target.id, 0) + 1
                    assignments.append((target.id, node.value))
    for name, value in assignments:
        if name not in constants and counts[name] == 1:
            constants[name] = value
    return constants


def _segments(node, constants, self_path, seen=()):
    """Path segments relative to the repository root, or None when unknown.

    An unresolvable expression returns None so the caller can report it; it is
    never guessed, because a false key would demand an anchor no table can
    satisfy.
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return tuple(part for part in node.value.split('/') if part not in ('', '.'))
    if isinstance(node, ast.Name):
        if node.id == '__file__':
            return self_path
        if node.id in ROOT_NAMES:
            return ()
        if node.id in seen or node.id not in constants:
            return None
        return _segments(constants[node.id], constants, self_path, seen + (node.id,))
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        left = _segments(node.left, constants, self_path, seen)
        right = _segments(node.right, constants, self_path, seen)
        if left is None or right is None:
            return None
        return left + right
    if isinstance(node, ast.Call):
        return _call_segments(node, constants, self_path, seen)
    if isinstance(node, ast.Attribute) and node.attr == 'parent':
        base = _segments(node.value, constants, self_path, seen)
        if not base:
            return None
        return base[:-1]
    if (isinstance(node, ast.Subscript) and isinstance(node.value, ast.Attribute)
            and node.value.attr == 'parents'):
        base = _segments(node.value.value, constants, self_path, seen)
        index = node.slice
        if base is None or not isinstance(index, ast.Constant) or not isinstance(index.value, int):
            return None
        drop = index.value + 1
        return base[:-drop] if drop <= len(base) else None
    return None


def _call_segments(node, constants, self_path, seen):
    func = node.func
    if isinstance(func, ast.Name):
        if func.id != 'Path':
            return None
        base = ()
        args = node.args
    elif isinstance(func, ast.Attribute):
        base = _segments(func.value, constants, self_path, seen)
        if base is None:
            return None
        if func.attr in ('resolve', 'absolute', 'expanduser') and not node.args:
            return base
        if func.attr == 'joinpath':
            args = node.args
        elif func.attr == 'with_name':
            if len(node.args) != 1 or not base:
                return None
            name = _segments(node.args[0], constants, self_path, seen)
            if name is None or len(name) != 1:
                return None
            return base[:-1] + name
        elif func.attr == 'with_suffix':
            if len(node.args) != 1 or not base:
                return None
            suffix = node.args[0]
            if not isinstance(suffix, ast.Constant) or not isinstance(suffix.value, str):
                return None
            name = base[-1]
            dot = name.rfind('.')
            stem = name[:dot] if dot > 0 else name
            return base[:-1] + ((stem + suffix.value) if suffix.value else (stem,))
        else:
            return None
    else:
        return None
    out = base
    for argument in args:
        part = _segments(argument, constants, self_path, seen)
        if part is None:
            return None
        out += part
    return out


def _bare_file(node):
    """A file name with no directory of its own: the shape that needs an anchor."""
    return (isinstance(node, ast.Constant) and isinstance(node.value, str)
            and bool(node.value) and '/' not in node.value)


def _joins_a_path_string(node):
    """Whether an ``Add`` / f-string carries a path-looking string literal.

    List concatenation (``[...] + [...]``) is common in tables and has nothing
    to do with paths, so the shape alone is not enough: one direct operand must
    be a string constant with a separator in it.
    """
    if isinstance(node, ast.JoinedStr):
        operands = node.values
    else:
        operands = (node.left, node.right)
    return any(isinstance(operand, ast.Constant) and isinstance(operand.value, str)
               and '/' in operand.value for operand in operands)


def _iterated_elements(node, constants):
    """The literal elements a ``for name in (...)`` loop walks, or None."""
    target = node
    if isinstance(node, ast.Name) and node.id in constants:
        target = constants[node.id]
    if isinstance(target, (ast.Tuple, ast.List)):
        return list(target.elts)
    return None


def _is_repo_rooted(node, constants, seen=()):
    """Whether an expression is built from the repository root or ``__file__``.

    Tables also concatenate bare file names onto *runtime* directories (the
    evidence directory, a temp dir). Those are not repository paths and the
    discovery loop can never key on them, so they are not required to carry an
    anchor. An expression that does start at ``ROOT`` / ``__file__`` but that
    this scanner cannot evaluate is a real hole and is reported.
    """
    if isinstance(node, ast.Name):
        if node.id in ROOT_NAMES or node.id == '__file__':
            return True
        if node.id in seen or node.id not in constants:
            return False
        return _is_repo_rooted(constants[node.id], constants, seen + (node.id,))
    for child in ast.iter_child_nodes(node):
        if _is_repo_rooted(child, constants, seen):
            return True
    return False


def bare_concat_keys(tree, table):
    """``{key: [source expression, ...]}`` for every anchor a table needs.

    A key is the discovery key ``dir/file`` of a path the table builds from a
    bare file name. Unresolvable bases and keyless paths are reported separately
    so the guard fails closed instead of quietly requiring nothing.
    """
    self_path = FIXTURES_FROM_ROOT + tuple(table.split('/'))
    constants = _constants(tree)
    keys = {}
    problems = []

    def record(segments, expression):
        if len(segments) < 2:
            problems.append(f'no discovery key (needs a directory and a file name): {expression}')
            return
        keys.setdefault('/'.join(segments[-2:]), []).append(expression)

    def resolve(node, expression, shape, base):
        segments = _segments(node, constants, self_path)
        if segments is None:
            if _is_repo_rooted(base, constants):
                problems.append(f'unresolvable {shape}: {expression}')
            return
        record(segments, expression)

    class Visitor(ast.NodeVisitor):
        def visit_BinOp(self, node):
            if isinstance(node.op, ast.Div) and _bare_file(node.right):
                resolve(node, ast.unparse(node), 'bare-filename concat', node.left)
            elif (isinstance(node.op, ast.Add) and _joins_a_path_string(node)
                    and _is_repo_rooted(node, constants)):
                # ``str(ROOT) + '/src/x/mod.rs'`` is statically resolvable in
                # principle but not by this scanner; keeping it silent would be
                # the same fail-open shape the gate exists to catch.
                problems.append(f'unsupported string concatenation of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            self.generic_visit(node)

        def visit_JoinedStr(self, node):
            if _joins_a_path_string(node) and _is_repo_rooted(node, constants):
                problems.append(f'unsupported f-string path of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
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
                base = _segments(element.left, constants, self_path)
                elements = _iterated_elements(generators[0].iter, constants)
                if elements is None:
                    if _is_repo_rooted(generators[0].iter, constants):
                        problems.append(f'unresolvable comprehension source: {ast.unparse(node)}')
                elif base is None:
                    if _is_repo_rooted(element.left, constants):
                        problems.append(f'unresolvable comprehension base: {ast.unparse(node)}')
                else:
                    for candidate in elements:
                        if _bare_file(candidate):
                            record(base + (candidate.value,),
                                   f'{ast.unparse(element)} for {candidate.value}')
            self.generic_visit(node)

    Visitor().visit(tree)
    return keys, problems


def declared_anchors(tree):
    """``(anchors, problems)`` for the module-level ``DISCOVERY_ANCHORS`` tuple.

    ``anchors`` is None when the table declares none, or when the declaration is
    not a flat tuple of strings. A second ``DISCOVERY_ANCHORS`` assignment is a problem:
    ``_constants`` and Python itself would honour the last one while a reader
    (and the first version of this guard) would see the first.
    """
    found = []
    for statement in tree.body:
        if not isinstance(statement, ast.Assign):
            continue
        if not any(isinstance(target, ast.Name) and target.id == 'DISCOVERY_ANCHORS'
                   for target in statement.targets):
            continue
        found.append(statement)
    if not found:
        return None, []
    problems = []
    if len(found) > 1:
        problems.append(f'DISCOVERY_ANCHORS assigned {len(found)} times; keep exactly one declaration')
    value = found[0].value
    if not isinstance(value, (ast.Tuple, ast.List)):
        return None, problems
    anchors = []
    for element in value.elts:
        if not isinstance(element, ast.Constant) or not isinstance(element.value, str):
            return None, problems
        anchors.append(element.value)
    return anchors, problems


def violations(text, table, anchors=None):
    """Everything wrong with one table's discovery anchors; empty means green.

    ``anchors`` overrides the declared tuple so the negative control can drop an
    entry without rewriting the file.
    """
    tree = ast.parse(text)
    problems = []
    required, scan_problems = bare_concat_keys(tree, table)
    problems.extend(scan_problems)
    declared, anchor_problems = declared_anchors(tree)
    problems.extend(anchor_problems)
    if anchors is not None:
        declared = list(anchors)
    if declared is None:
        problems.append('no module-level DISCOVERY_ANCHORS tuple of strings')
        declared = []
    for anchor in declared:
        if not anchor:
            problems.append('empty anchor')
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


class DiscoveryAnchorTests(unittest.TestCase):
    def test_the_registry_is_not_empty(self):
        """An empty or gutted TABLES list must not turn every check vacuous."""
        self.assertTrue(TABLES, 'TABLES is empty: the guard would measure nothing')

    def test_no_table_is_blind_outside_the_registers(self):
        """A new blind table cannot appear silently.

        Every ``*negative*.py`` table under this directory either declares its
        anchors (so it is in ``TABLES``), or is listed in ``KNOWN_BLIND`` with a
        reason, or needs none. Adding a table that builds repository paths from
        bare file names turns this red until somebody decides which it is.
        """
        found = set()
        for path in sorted(FIXTURES.rglob('*.py')):
            rel = path.relative_to(FIXTURES).as_posix()
            if 'negative' not in path.name or rel in TABLES:
                continue
            if path.name in SHARED_HELPERS or path.name.startswith('test_'):
                continue
            if rel.startswith('_'):
                continue
            with self.subTest(table=rel):
                keys, _ = bare_concat_keys(ast.parse(path.read_text(encoding='utf-8')), rel)
                if not keys:
                    continue
                found.add(rel)
                self.assertIn(
                    rel, KNOWN_BLIND,
                    'builds repository paths from bare file names: declare '
                    'DISCOVERY_ANCHORS and add the table to TABLES, or list it in '
                    'KNOWN_BLIND with a reason')
        self.assertEqual(sorted(set(KNOWN_BLIND) - found), [],
                         'stale KNOWN_BLIND entries: fixed or deleted tables must be removed')

    def test_known_blind_entries_name_a_real_table(self):
        for rel, reason in KNOWN_BLIND.items():
            with self.subTest(table=rel):
                self.assertTrue((FIXTURES / rel).is_file(),
                                f'{rel} is gone; drop its KNOWN_BLIND entry')
                self.assertNotIn(rel, TABLES, f'{rel} is registered; drop its KNOWN_BLIND entry')
                self.assertGreater(len(reason), 4, f'{rel}: KNOWN_BLIND needs a reason')

    def test_tables_declare_their_discovery_anchors(self):
        for name in TABLES:
            with self.subTest(table=name):
                text = (FIXTURES / name).read_text(encoding='utf-8')
                self.assertEqual(violations(text, name), [], f'{name} discovery anchors')

    def test_removing_an_anchor_turns_the_guard_red(self):
        """Every declared anchor is load-bearing: drop it, the guard must fail."""
        for name in TABLES:
            text = (FIXTURES / name).read_text(encoding='utf-8')
            anchors, problems = declared_anchors(ast.parse(text))
            self.assertEqual(problems, [], name)
            self.assertIsNotNone(anchors, f'{name}: no DISCOVERY_ANCHORS to negate')
            if not anchors:
                continue  # covered by the injected-row probe below
            for dropped in anchors:
                with self.subTest(table=name, dropped=dropped):
                    remaining = [anchor for anchor in anchors if anchor != dropped]
                    problems = violations(text, name, anchors=remaining)
                    self.assertTrue(
                        any(problem.startswith(f'missing anchor: {dropped} ')
                            for problem in problems),
                        f'{name}: dropping {dropped} did not report a missing anchor: {problems}')

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
                problems = violations(text + probe, name)
                self.assertTrue(any('anchor-probe/unlisted.rs' in problem
                                    for problem in problems), problems)

    def test_the_scanner_reports_a_synthetic_new_row(self):
        """The reporting path itself, on a two-row module built for the test."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example'\n"
            "DISCOVERY_ANCHORS = ('example/listed.rs',)\n"
            "rows = [('a', BASE / 'listed.rs', 'x'),\n"
            "        ('b', BASE / 'unlisted.rs', 'x')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('example/unlisted.rs' in problem for problem in problems),
                        problems)
        self.assertFalse(any('example/listed.rs' in problem for problem in problems),
                         problems)

    def test_an_inline_directory_needs_no_anchor(self):
        """``SOURCE / "runtime/x.rs"`` already spells the discovery key."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SOURCE = ROOT / 'src/pilotdeck-gateway/src'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [('a', SOURCE / 'runtime/memory/gateway.rs', 'x')]\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_unresolvable_shapes_fail_closed(self):
        """A repo path the scanner cannot evaluate must be an error, not a skip."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [('a', ROOT / PICK['dir'] / 'unlisted.rs', 'x')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('unresolvable' in problem for problem in problems), problems)

    def test_string_built_paths_fail_closed(self):
        """Shapes outside the resolver's family are errors, not silent passes."""
        shapes = {
            'concatenation': "path = str(ROOT) + '/src/example/unlisted.rs'\n",
            'f-string': "path = f'{ROOT}/src/example/unlisted.rs'\n",
        }
        for label, line in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + line
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(any('unsupported' in problem for problem in problems), problems)

    def test_local_aliases_are_resolved(self):
        """``source = ROOT / '...'`` inside a function still hides the directory.

        Tables write ``storage = source.parent / 'storage.rs'`` off a local alias;
        the alias is assigned once, so the scanner can follow it and the key must
        be anchored like any other.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ('edgeclaw/storage.rs',)\n"
            "def main():\n"
            "    source = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory'\n"
            "    storage = source.parent / 'storage.rs'\n"
            "    return [storage]\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])
        dropped = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def main():\n"
            "    source = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory'\n"
            "    storage = source.parent / 'storage.rs'\n"
            "    return [storage]\n"
        )
        problems = violations(dropped, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: edgeclaw/storage.rs' in p for p in problems),
                        problems)

    def test_runtime_directories_need_no_anchor(self):
        """Evidence-dir joins are not repository paths, so they carry no key."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [('a', ROOT / 'src/example/named.rs', 'x')]\n"
            "evidence = [out / 'summary.json', out / 'run.out']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_the_resolver_understands_the_shapes_the_tables_use(self):
        """``__file__`` chains resolve against the table's own path."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "FIXTURE = Path(__file__).resolve().parent\n"
            "MEMORY = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/file_memory.rs'\n"
            "DISCOVERY_ANCHORS = ('example/oracle.json', 'example/generate.mts', 'file_memory/tests.rs')\n"
            "rows = [FIXTURE / 'oracle.json', FIXTURE / 'generate.mts',\n"
            "        MEMORY.with_suffix('') / 'tests.rs']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_second_anchors_assignment_turns_the_guard_red(self):
        synthetic = (
            "DISCOVERY_ANCHORS = ('example/listed.rs',)\n"
            "BASE = Path(__file__).resolve().parents[5] / 'src/example'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [BASE / 'listed.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('DISCOVERY_ANCHORS assigned 2 times' in problem for problem in problems),
                        problems)


if __name__ == '__main__':
    unittest.main(verbosity=2)
