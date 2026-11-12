#!/usr/bin/env python3
# anchor-selfcheck: unsupported structural census of shared-assertion rows; no product source mutation table
"""#2692 fix-r13 (DS + MM sibling/boundary, both families): the family census.

The one-table-at-a-time fixes (r11 blankkey, r12 turn-submit roster, this
round's ROW_PINS) kept closing the same hole after a review tripped over it,
so this census machine-checks the whole family instead: in every fixture
mutation table this parser can read (the two 6-tuple shapes below), a mutant
row whose assertion string another row also carries must appear in that
file's ROW_PINS. Otherwise a both-tables drop of the row survives
`registered()` while the surviving sibling keeps the shared anchor bound --
the exact P1 of this round, which no per-table fix can promise to be the last
of.

Tables this parser cannot fully read (the older tuple arities and
non-literal columns) and family-debt tables with known unpinned shared rows
are registered below instead of being silently invisible: a new gap must be
added to the registry on purpose, and a stale or duplicate entry goes red
(the #2133 family batch owns clearing the debt). Run from anywhere:

    python3 -I -B src/pilotdeck-gateway/tests/fixtures/test_shared_assertion_pins.py
"""
import ast
import unittest
from collections import defaultdict
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent
TABLE_NAMES = ('mutations', 'mutants')
NO_TABLE = 'no mutation table'


def module_assigns(tree):
    """The module-level `Name -> value` assignments (the census ignores locals).

    A local `mutations = ...` helper variable is not a table; walking the
    whole tree would count it and flag the file it lives in.
    """
    assigns = {}
    for node in tree.body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1 \
                and isinstance(node.targets[0], ast.Name):
            assigns[node.targets[0].id] = node.value
    return assigns

# Family debt: a readable table whose shared-assertion rows are not pinned.
# The reason column keeps the batch owner explicit; liveness is structural
# (the entry goes red the day the table is pinned or its rows stop sharing).
ROW_PIN_GAPS = (
    ('config-store/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('cron-scheduler/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('edgeclaw-real-provider/negative.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('fe-coexist/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('frozen-manifest/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('issue-1246/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('ledger-diffs/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('locale-contract/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('mcp-hooks-spawn/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('memory-gateway/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('mode-routing/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('native-collation/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('native-dispatch/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('native-management/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('native-process/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('native-registry-switch/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('pending-reanchor/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('proxy-listen-address/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('sdk-format/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('sdk-run/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('sdk-sessions-list/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('sdk-sessions-messages/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('sdk-settings/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('sdk-signals/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('sdk-terminal/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('session-assembly-1316/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('session-factory/append-delta/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('session-factory/projection-cache/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('turn-gateway-clock-edge/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
    ('upload-gateway/negative_controls.py', 'family debt: unpinned shared-assertion rows (#2133)'),
)

# A table exists but this census cannot read it (older arity / non-literal
# columns). Unreadable is not approved: the entry only makes the blind spot
# visible and nameable, and goes red when the table becomes readable.
SHAPE_GAPS = (
    ('agent-event-projector/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('attach-twopass-1460/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('background-output/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('background-panic/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('background-persist-restart/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('cli-parity-lock-bounded/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('cli-parity-mock-gateway/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('commands-catalog/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('connection-context/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('cron-schedule/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('cron-store/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('edgeclaw-dream-run/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('edgeclaw-dream-runner/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('edgeclaw-entries/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('edgeclaw-entries/negative_ids.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('edgeclaw-entries/negative_round6.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('edgeclaw-entries/negative_round7.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('edgeclaw-llm-dream/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('failure-guard/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('hooks-output/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('host-event-pump/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('html-timing/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('interaction-bus/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('interaction-values/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('memory-maintenance/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('native-entry-authority/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('native-settings/negative/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('native-topology/negative_signals.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('node-io/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('parity-subprocess-hardening/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('project-files-linux-dirent/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('pump-order-cases/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('retirement-negctl-roster/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('runtime-spawn-panic/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('sdk-cli-lifecycle/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('sdk-signals-close-order-pin/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('settings/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('settings-gateway/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('sidecar-payload/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('snapshot-projection/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('surrogate-collation-matrix/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-gateway/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-publish/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-read/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-replay/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-runner/input_negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-runner/live_negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-runner/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-runner/recording_negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-runner/runner_negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-submit-admission/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('turn-submit-panic-close/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('user-dialog-channel/negative.py', 'unreadable table shape; extend the parser or pin (#2133)'),
    ('user-dialog-lock-scope/negative_controls.py', 'unreadable table shape; extend the parser or pin (#2133)'),
)

ROW_PIN_GAPS_BY_PATH = {path: reason for path, reason in ROW_PIN_GAPS}
SHAPE_GAPS_BY_PATH = {path: reason for path, reason in SHAPE_GAPS}


def const_str(node):
    return node.value if isinstance(node, ast.Constant) and isinstance(node.value, str) else None


def const_strs(node):
    """A literal string or a literal list/tuple of strings; None anchor -> []."""
    if isinstance(node, ast.Constant):
        if isinstance(node.value, str):
            return [node.value]
        if node.value is None:
            return []
        return None
    if isinstance(node, (ast.List, ast.Tuple)):
        out = []
        for item in node.elts:
            value = const_str(item)
            if value is None:
                return None
            out.append(value)
        return out
    return None


def anchor_overrides(tree):
    """The literal `mutant_anchor` dict ({} when absent), or None when not literal.

    An absent dict is fine (the marker column carries every anchor); a dict
    this parser cannot read leaves the covered rows' effective anchors unknown.
    """
    value = module_assigns(tree).get('mutant_anchor')
    if value is None:
        return {}
    if not isinstance(value, ast.Dict):
        return None
    overrides = {}
    for key, item in zip(value.keys, value.values):
        name = const_str(key)
        if name is None or not isinstance(item, ast.Tuple) or len(item.elts) != 2:
            return None
        kind = const_str(item.elts[0])
        anchor = const_strs(item.elts[1])
        if kind is None or anchor is None:
            return None
        overrides[name] = (kind, anchor)
    return overrides


def table_rows(tree, name, wanted):
    """Rows of `name` table; per element the literal string(s) or None.

    Only the positions in `wanted` must be literal (0 = the row name; the
    assertion column): the test / path columns are often `CFG + '...'`
    concatenations or module constants, which the census does not need.
    """
    value = module_assigns(tree).get(name)
    if not isinstance(value, (ast.List, ast.Tuple)):
        return None
    rows = []
    for row in value.elts:
        if isinstance(row, (ast.Tuple, ast.List)):
            parts = [const_strs(item) if position in wanted else 'SKIP'
                     for position, item in enumerate(row.elts)]
        elif wanted == {0} and isinstance(row, ast.Constant) \
                and isinstance(row.value, str):
            parts = [[row.value]]
        else:
            return None
        rows.append(parts)
    return rows


def effective_assertions(tree):
    """(name -> [assertion strings], shape) for one parsed file, or (None, reason)."""
    # Local holders, deliberately not named `mutants` / `mutations`: the
    # anchor_selfcheck exemption scan reads any Store of those table names as
    # a declared table, and this census is a checker, not a mutation entry.
    turn_rows = table_rows(tree, 'mutants', {0, 4})
    case_rows = table_rows(tree, 'mutations', {0, 5})
    if turn_rows is not None:
        if any(len(row) != 6 for row in turn_rows):
            return None, f'mutants table with non-6-tuple rows: {[len(r) for r in turn_rows]}'
        overrides = anchor_overrides(tree)
        if overrides is None:
            return None, 'mutants table: mutant_anchor not fully literal'
        out = {}
        for row in turn_rows:
            name, marker = row[0], row[4]
            if not isinstance(name, list) or not isinstance(marker, list):
                return None, 'mutants table: non-literal name or marker row'
            name = name[0]
            marker = marker[0] if marker else None
            if name in overrides:
                kind, anchor = overrides[name]
                out[name] = [] if kind == 'location' else anchor
            else:
                out[name] = [] if marker is None else [marker]
        return out, 'mutants (turn shape)'
    if case_rows is not None:
        if any(len(row) != 6 for row in case_rows):
            return None, f'mutations table with non-6-tuple rows: {[len(r) for r in case_rows]}'
        overrides = anchor_overrides(tree)
        out = {}
        for row in case_rows:
            if not isinstance(row[0], list):
                return None, 'mutations table: non-literal name row'
            name, assertion = row[0][0], row[5]
            if overrides is not None and name in overrides:
                kind, anchor = overrides[name]
                out[name] = [] if kind == 'location' else anchor
            elif not isinstance(assertion, list):
                # Without an override the assertion column is the only anchor
                # this census could read; a non-literal one stays unreadable.
                return None, 'mutations table: non-literal assertion row'
            else:
                out[name] = assertion
        return out, 'mutations (Case shape)'
    if any(name in module_assigns(tree) for name in TABLE_NAMES):
        return None, 'mutation table with a shape this census cannot read'
    return None, NO_TABLE


def row_pins(tree):
    """The literal ROW_PINS names, or None when absent or unreadable."""
    rows = table_rows(tree, 'ROW_PINS', {0})
    if rows is None:
        return None
    return [row[0][0] for row in rows]


def classify(text):
    """One of 'pinned' / 'gap' / 'shared-free' / 'shape-gap' / 'no-table'."""
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return 'shape-gap'
    rows, shape = effective_assertions(tree)
    if rows is None:
        return 'no-table' if shape == NO_TABLE else 'shape-gap'
    by_assertion = defaultdict(set)
    for name, assertions in rows.items():
        for assertion in assertions:
            by_assertion[assertion].add(name)
    shared = {name for names in by_assertion.values()
              if len(names) >= 2 for name in names}
    if not shared:
        return 'shared-free'
    pins = row_pins(tree)
    if pins and shared <= set(pins):
        return 'pinned'
    return 'gap'


def census_refusal(relpath, text):
    """The census verdict for one file, or None when it is clean.

    The registries are honored: a registered gap is accepted as registered
    debt, an unregistered gap is refused, and a stale registration (the table
    was pinned, rewritten or deleted) is refused so the registry cannot
    outlive what it describes.
    """
    state = classify(text)
    if state == 'no-table':
        if relpath in ROW_PIN_GAPS_BY_PATH or relpath in SHAPE_GAPS_BY_PATH:
            return f'{relpath}: registered but holds no mutation table anymore'
        return None
    if state == 'shape-gap':
        if relpath in ROW_PIN_GAPS_BY_PATH:
            return f'{relpath}: registered in ROW_PIN_GAPS but the table is unreadable'
        if relpath not in SHAPE_GAPS_BY_PATH:
            return f'{relpath}: a mutation table this census cannot read is ' \
                   f'unregistered -- extend the parser or add a SHAPE_GAPS entry'
        return None
    if state in ('pinned', 'shared-free'):
        if relpath in ROW_PIN_GAPS_BY_PATH:
            return f'{relpath}: stale ROW_PIN_GAPS entry -- the table is ' \
                   f'{"pinned" if state == "pinned" else "without shared rows"} now'
        if relpath in SHAPE_GAPS_BY_PATH:
            return f'{relpath}: stale SHAPE_GAPS entry -- the table is readable now'
        return None
    # state == 'gap'
    if relpath in SHAPE_GAPS_BY_PATH:
        return f'{relpath}: registered in SHAPE_GAPS but the table reads fine'
    if relpath not in ROW_PIN_GAPS_BY_PATH:
        return f'{relpath}: shared-assertion rows are not pinned -- a ' \
               f'both-tables drop of one row survives the roster guard ' \
               f'(add ROW_PINS, or register the debt in ROW_PIN_GAPS)'
    return None


def walk_fixtures():
    for path in sorted(FIXTURES.rglob('*.py')):
        if '__pycache__' in path.parts:
            continue
        yield path.relative_to(FIXTURES).as_posix(), path.read_text(encoding='utf-8')


class SharedAssertionPinCensus(unittest.TestCase):
    def assertCensusClean(self, relpath, text):
        refusal = census_refusal(relpath, text)
        self.assertIsNone(refusal, f'#2692 fix-r13: {refusal}')

    def test_every_table_in_the_tree_is_pinned_or_registered(self):
        # The structural leg: no fixture mutation table may carry an
        # unpinned shared-assertion row without a registered debt entry.
        seen = 0
        for relpath, text in walk_fixtures():
            if classify(text) != 'no-table':
                seen += 1
            self.assertCensusClean(relpath, text)
        self.assertGreater(seen, 50,
                           '#2692 fix-r13: the census found suspiciously few '
                           'mutation tables; the walk is broken')

    def test_the_registries_are_live_unique_and_disjoint(self):
        # A registry entry that no longer matches its file hides exactly the
        # drift the census exists to catch, so both stale and duplicate
        # entries go red (the TWIN_EXEMPTIONS shape).
        texts = dict(walk_fixtures())
        for registry, kind in ((ROW_PIN_GAPS, 'gap'), (SHAPE_GAPS, 'shape-gap')):
            self.assertEqual(len(set(path for path, _ in registry)), len(registry),
                             f'#2692 fix-r13: duplicate {kind} registry entry')
            for path, reason in registry:
                self.assertTrue(reason.strip(),
                                f'#2692 fix-r13: {path} carries an empty reason')
                self.assertIn(path, texts,
                              f'#2692 fix-r13: {path} is registered but missing')
                self.assertEqual(classify(texts[path]), kind,
                                 f'#2692 fix-r13: {path} is registered as {kind} '
                                 f'but the live table classifies differently')
        self.assertFalse(set(ROW_PIN_GAPS_BY_PATH) & set(SHAPE_GAPS_BY_PATH),
                         '#2692 fix-r13: a table is registered in both registries')

    # The checker's own red side: the cases below parse inline source, so a
    # census that has drifted into always-green fails here without anyone
    # having to corrupt a real fixture.

    def test_the_checker_flags_an_unpinned_shared_assertion(self):
        source = (
            'mutations = [\n'
            "    ('fold-a', 'x.rs', 'old', 'new', 'test_a', '#1 shared anchor'),\n"
            "    ('fold-b', 'x.rs', 'old2', 'new2', 'test_b', '#1 shared anchor'),\n"
            ']\n'
            'EXPECTED_NAMES = ("fold-a", "fold-b")\n')
        refusal = census_refusal('synthetic/negative_controls.py', source)
        self.assertIsNotNone(refusal, '#2692 fix-r13: an unpinned shared '
                                      'assertion must refuse')
        self.assertIn('not pinned', refusal)

    def test_the_checker_accepts_a_pinned_shared_table(self):
        source = (
            'mutations = [\n'
            "    ('fold-a', 'x.rs', 'old', 'new', 'test_a', '#1 shared anchor'),\n"
            "    ('fold-b', 'x.rs', 'old2', 'new2', 'test_b', '#1 shared anchor'),\n"
            ']\n'
            'ROW_PINS = (\n'
            "    'fold-a',\n"
            "    'fold-b',\n"
            ')\n')
        self.assertCensusClean('synthetic/negative_controls.py', source)

    def test_the_checker_ignores_a_table_without_shared_rows(self):
        source = (
            'mutations = [\n'
            "    ('fold-a', 'x.rs', 'old', 'new', 'test_a', '#1 anchor a'),\n"
            "    ('fold-b', 'x.rs', 'old2', 'new2', 'test_b', '#1 anchor b'),\n"
            ']\n')
        self.assertCensusClean('synthetic/negative_controls.py', source)

    def test_the_checker_ignores_a_file_without_a_table(self):
        self.assertCensusClean('synthetic/helpers.py', 'TOTAL = 3\n')

    def test_the_checker_ignores_an_unregistered_gap_only_when_registered(self):
        # A gap may carry debt only through the registry: the same source
        # without the registry entry refused above, so prove the honored
        # path by pointing the checker at a real registered debt file.
        for relpath in ROW_PIN_GAPS_BY_PATH:
            if relpath == 'config-store/negative_controls.py':
                self.assertCensusClean(relpath, dict(walk_fixtures())[relpath])
                return
        self.fail('#2692 fix-r13: config-store left the family-debt registry')


if __name__ == '__main__':
    unittest.main()
