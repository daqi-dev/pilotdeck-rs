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
and ``.with_suffix()``; ``_is_path_shape`` also reads the call spellings a table
may use instead of ``/`` (``Path(base, 'mod.rs')``, ``base.joinpath('mod.rs')``,
``'x.json'.format(...)``). Paths built by string ``+`` or by an f-string are
outside that family and are reported as errors rather than guessed; so is
anything else the resolver cannot follow from the repository root, including a
module-level name that a function rebinds and a comprehension over a non-literal
source. Silent skipping is what makes a discovery gate *look* green while
measuring nothing.

A table also depends on the shared fixtures it imports: the loop keys a changed
``fixtures/negative_suite.py`` as ``fixtures/negative_suite.py``, and the import
alone does not put that literal in the table's text, so ``violations`` requires
the same anchor for every imported ``negative_*`` helper.

``test_removing_an_anchor_turns_the_guard_red`` re-runs the check with each
anchor dropped in turn, so the guard's discrimination is proven on every run
rather than only when somebody remembers to negate it by hand. Local negative
controls cover the other directions: a new bare row, an empty ``TABLES``, a
second ``DISCOVERY_ANCHORS`` assignment, a string-built path, unsupported call
shapes, a shadowed constant, and new blind or unresolvable tables appearing
outside both registers.

Scope: the four tables named by #2133 are the ones that carry anchors today;
adding a table to ``TABLES`` is the whole opt-in, and the rule then demands its
``DISCOVERY_ANCHORS`` match its own keys. Everything else that already builds
repository paths from bare file names is listed in ``KNOWN_BLIND`` — 123 tables,
352 keys at the time of writing, 12 of those tables also holding shapes the
resolver cannot follow at all. That register is a fix list, not an exemption:
``test_no_table_is_blind_outside_the_registers`` turns this red when a *new*
table arrives blind or unresolvable, and ``test_known_blind_counts_match_the_scan``
turns it red when a registered table grows another key. Entries may only leave
the register by being fixed and moved into ``TABLES``; #2141 carries the
per-table list. ``schemata.py`` tables (``turn-gates`` / ``turn-prompt-hooks`` /
``turn-runner-tail`` / ``turn-start-notices``) do not match the discovery glob at
all — that is a limit of ``neg-for-diff.sh`` itself and no anchor can fix it.

Run from this directory, no cargo and no tree lock::

    python3 -B -m unittest -v test_negative_discovery_anchors
"""
import ast
from pathlib import Path
import tempfile
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
# with a reason or fixed. Each reason states how many keys the table builds and
# how many path shapes the scanner cannot follow; both are re-measured by
# test_known_blind_counts_match_the_scan. #2141 carries the per-table fix list.
# 123 tables (116 blind, 12 with unresolvable shapes, 5 in both), 352 keys.
KNOWN_BLIND = {
    'agent-event-projector/negative.py': '2 keys',
    'anchor-marker/negative_controls.py': '2 keys',
    'api-web/negative_controls.py': '5 keys',
    'attach-followups-a/negative_controls.py': '2 keys',
    'background-persist-progress/negative_controls.py': '2 keys',
    'background-persist-restart/negative_controls.py': '2 keys',
    'background-subtick-gate/negative_controls.py': '4 keys',
    'channel-core/negative_controls.py': '3 keys',
    'channel-unmigrated/negative_controls.py': '1 key',
    'cli-parity-isolation/negative_controls.py': '1 key + 1 unresolvable',
    'cli-parity-lock-bounded/negative_controls.py': '1 key',
    'cli-parity-mock-gateway/negative_controls.py': '1 key + 1 unresolvable',
    'closure_guard_negative.py': '2 keys',
    'connection-context/negative.py': '4 keys',
    'cron-schedule/negative_controls.py': '2 keys',
    'cron-store/negative_controls.py': '2 keys',
    'edgeclaw-accessors/negative.py': '4 keys',
    'edgeclaw-dream/negative_shapes.py': '1 key',
    'edgeclaw-entries/negative_fix1.py': '1 unresolvable',
    'edgeclaw-entries/negative_round6.py': '3 keys',
    'edgeclaw-heartbeat-isolation/negative.py': '6 keys',
    'edgeclaw-native-default/negative.py': '6 keys',
    'edgeclaw-provider/negative.py': '6 keys',
    'edgeclaw-real-provider/negative.py': '1 key',
    'edgeclaw-same-root-generation/negative.py': '8 keys',
    'edgeclaw-service-orchestration/switch_negative.py': '2 keys',
    'edgeclaw-text/negative.py': '3 keys',
    'failure-guard/negative.py': '3 keys',
    'fe-baseline-report/negative_controls.py': '1 unresolvable',
    'file-history-cwd/negative_controls.py': '3 keys',
    'frozen-manifest/negative_controls.py': '1 key',
    'frozen-reinsert-guard/negative_controls.py': '2 keys',
    'gateway-batch/negative_controls.py': '2 keys',
    'interaction-values/negative_controls.py': '2 keys',
    'issue-297/negative_controls.py': '1 key',
    'ledger-diffs/negative_controls.py': '1 key',
    'live-e2e-tmpdir/negative_controls.py': '2 keys',
    'live-e2e/negative_controls.py': '3 keys',
    'live-recording/negative.py': '4 keys',
    'locale-adaptive/negative_controls.py': '2 keys',
    'manager-gateway/negative_controls.py': '1 key',
    'metadata-from-env/negative_controls.py': '2 keys',
    'model-catalog-cwd/negative_controls.py': '4 keys',
    'model-complete/negative-controls.py': '2 unresolvable',
    'module-host-approvals/negative_controls.py': '5 keys',
    'module-host/negative_controls.py': '5 keys',
    'native-archive-cwd/negative_controls.py': '1 key',
    'native-archive-cwd/negative_regeneration.py': '5 keys',
    'native-collation/negative_controls.py': '7 keys',
    'native-cron/negative_controls.py': '2 keys',
    'native-dispatch/negative_controls.py': '2 keys',
    'native-http-basic/negative_controls.py': '5 keys',
    'native-loop-stimulus/negative_controls.py': '4 keys',
    'native-management/negative_controls.py': '3 keys',
    'native-metadata-cwd/negative_controls.py': '1 key',
    'native-metadata-cwd/negative_regeneration.py': '5 keys',
    'native-process/negative_controls.py': '2 keys',
    'native-registry-switch/negative_controls.py': '3 keys',
    'native-session-e2e/negative_controls.py': '1 key',
    'native-stimulus-plan/negative_controls.py': '3 keys',
    'native-storage-stimulus/negative_controls.py': '3 keys',
    'native-topology/negative_signals.py': '2 keys',
    'native-witness/negative.py': '1 key',
    'native-witness/negative_authentication.py': '3 keys',
    'native-witness/negative_boundary.py': '1 key',
    'native-ws-caps/negative_controls.py': '3 keys',
    'native-ws-entry/negative_controls.py': '7 keys',
    'negative-adaptive/isolation_negative.py': '2 keys',
    'negative-adaptive/negative_controls.py': '2 keys',
    'negative-output-directory/negative.py': '3 keys',
    'negative_oracle_guard.py': '1 unresolvable',
    'output-schema/negative.py': '2 keys',
    'pending-reanchor/negative_controls.py': '3 keys',
    'projector-cwd/negative_controls.py': '2 keys',
    'proxy-listen-address/negative_controls.py': '3 keys',
    'recorder/fix3-negative.py': '7 keys + 1 unresolvable',
    'run-registry/count-negative.py': '7 keys + 1 unresolvable',
    'run-registry/timestamp-negative.py': '4 keys',
    'sdk-cli-lifecycle/negative_controls.py': '1 unresolvable',
    'sdk-format/negative_controls.py': '4 keys',
    'sdk-run/negative_controls.py': '5 keys',
    'sdk-sessions-list/negative_controls.py': '2 keys',
    'sdk-sessions-messages/negative_controls.py': '6 keys',
    'sdk-settings/negative_controls.py': '4 keys',
    'sdk-signals/negative_controls.py': '3 keys',
    'sdk-terminal/negative_controls.py': '4 keys',
    'session-admission/negative.py': '1 key',
    'session-adopt/switch_negative.py': '2 keys',
    'session-assembly-1316/negative_controls.py': '8 unresolvable',
    'session-domain-cleanup/negative_controls.py': '1 key',
    'session-domain-expectations/negative_controls.py': '1 key',
    'session-domain-lock-release/negative_controls.py': '1 key',
    'session-domain-native/negative_controls.py': '1 key',
    'session-domain/fix2-negative.py': '1 key + 1 unresolvable',
    'session-drop-release/negative_controls.py': '3 keys',
    'session-factory/append-delta/negative_controls.py': '2 keys',
    'session-factory/negative_controls.py': '11 keys',
    'session-factory/projection-cache/negative_controls.py': '5 keys',
    'session-lease/negative.py': '2 keys',
    'settings-gateway/negative_controls.py': '1 key',
    'shared-runner-failclosed/negative_controls.py': '1 key',
    'skills/negative_controls.py': '2 keys',
    'stats-cwd/negative_controls.py': '3 keys',
    'stress-followups/negative_controls.py': '1 key',
    'surrogate-collation-matrix/negative_controls.py': '6 keys',
    'treelock-budget/negative_controls.py': '5 keys',
    'turn-gateway/negative-fence.py': '5 keys',
    'turn-gateway/negative.py': '2 keys',
    'turn-publish/negative.py': '1 key',
    'turn-read/negative.py': '1 key',
    'turn-replay/negative.py': '4 keys',
    'turn-runner/forward_negative.py': '4 keys',
    'turn-runner/input_negative.py': '2 keys',
    'turn-runner/integrity_negative.py': '3 keys',
    'turn-runner/recording_negative.py': '1 key',
    'turn-submit/negative.py': '2 keys',
    'ui-server-parity-isolation/negative_controls.py': '2 unresolvable',
    'user-dialog-channel/negative.py': '5 keys',
    'web-conversion-lifecycle/negative.py': '2 keys',
    'webhook-channel/negative_controls.py': '7 keys',
    'wecom-callback/negative_controls.py': '5 keys',
    'weixin-channel/negative_controls.py': '7 keys',
    'workspace-pristine/negative_controls.py': '5 keys',
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
    the whole file has no ambiguity, so it is resolved too. A module-level name
    that a function rebinds is *not* substituted: the two sites would then mean
    different directories, and resolving both to the module-level value would
    demand an anchor for a key the runtime never uses while hiding the real one.
    """
    constants = {}
    for statement in tree.body:
        if isinstance(statement, ast.Assign):
            for target in statement.targets:
                if isinstance(target, ast.Name):
                    constants[target.id] = statement.value
    module_level = set(constants)
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
    for name in _shadowed_names(tree) & module_level:
        constants.pop(name, None)
    return constants


def _shadowed_names(tree):
    """Names bound at module level and rebound inside a function or class.

    Such a name means different directories at different sites, so the scanner
    refuses to substitute it and reports the site instead of guessing.
    """
    module_level = set()
    for statement in tree.body:
        if isinstance(statement, ast.Assign):
            for target in statement.targets:
                if isinstance(target, ast.Name):
                    module_level.add(target.id)
    return _function_scoped_names(tree) & module_level


def _function_scoped_names(tree):
    """Names assigned anywhere below a function, lambda or class body."""
    names = set()

    def visit(node, scoped):
        for child in ast.iter_child_nodes(node):
            inner = scoped or isinstance(
                child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda, ast.ClassDef))
            if inner and isinstance(child, ast.Assign):
                for target in child.targets:
                    if isinstance(target, ast.Name):
                        names.add(target.id)
            visit(child, inner)

    visit(tree, False)
    return names


def _segments(node, constants, self_path, seen=()):
    """Path segments relative to the repository root, or None when unknown.

    An unresolvable expression returns None so the caller can report it; it is
    never guessed, because a false key would demand an anchor no table can
    satisfy.
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        if node.value.startswith('/'):
            # An absolute literal is not a path below the repository root; the
            # discovery loop never keys it, so it needs no anchor.
            return None
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
        if index.value < 0:
            # ``parents[-1]`` is the filesystem root, not the repository root.
            return None
        drop = index.value + 1
        return base[:-drop] if drop <= len(base) else None
    return None


def _call_segments(node, constants, self_path, seen):
    func = node.func
    if isinstance(func, ast.Name):
        if func.id != 'Path' or not node.args:
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


def _is_bare_component(value):
    """A single path component that could be a file name.

    ``''``, ``'.'``, ``'..'`` and anything with whitespace are not: the first
    three are navigation and the discovery loop only keys a *changed file*, while
    a space means the literal is an operator-shaped string (``' / '.join(...)``),
    not a path.
    """
    return (bool(value) and '/' not in value and value not in ('.', '..')
            and not any(character.isspace() for character in value))


def _bare_file(node):
    """A file name with no directory of its own: the shape that needs an anchor."""
    return (isinstance(node, ast.Constant) and isinstance(node.value, str)
            and _is_bare_component(node.value))


def _path_literal_ending_in_file(value):
    """Whether a string literal ends in a bare file name (``'/x/mod.rs'``).

    ``' / '.join(...)`` and ``f'{a} / {b}'`` carry a slash but no file name;
    treating them as paths would flood the guard with runtime string joins.
    """
    if '/' not in value:
        return False
    return _is_bare_component(value.rsplit('/', 1)[1])


def _joins_a_path_string(node):
    """Whether an ``Add`` / f-string carries a path-looking string literal.

    List concatenation (``[...] + [...]``) is common in tables and has nothing
    to do with paths, so the shape alone is not enough: one direct operand must
    be a string constant ending in a bare file name.
    """
    if isinstance(node, ast.JoinedStr):
        operands = node.values
    else:
        operands = (node.left, node.right)
    return any(isinstance(operand, ast.Constant) and isinstance(operand.value, str)
               and _path_literal_ending_in_file(operand.value) for operand in operands)


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


PATH_CALL_ATTRS = frozenset({
    'joinpath', 'with_name', 'with_suffix', 'resolve', 'absolute', 'expanduser',
})


def _bare_tail(node, constants, seen=()):
    """Whether the last path component comes from a bare file-name literal.

    ``SMS / 'mod.rs'`` and ``SOURCE.with_name('oracle.json')`` both end in a
    name the discovery loop cannot derive from the file text; ``SOURCE /
    'runtime/memory/gateway.rs'`` spells its directory inline and is already
    findable, so it needs no anchor.
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return _is_bare_component(node.value)
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return False
        return _bare_tail(constants[node.id], constants, seen + (node.id,))
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        return _bare_tail(node.right, constants, seen)
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name) and func.id == 'Path':
            return bool(node.args) and _bare_tail(node.args[-1], constants, seen)
        if isinstance(func, ast.Attribute):
            if func.attr in ('joinpath', 'with_name'):
                return bool(node.args) and _bare_tail(node.args[-1], constants, seen)
            if func.attr in PATH_CALL_ATTRS:
                return _bare_tail(func.value, constants, seen)
    return False


def _is_path_shape(node, constants):
    """Whether an expression is a path this scanner must evaluate or report.

    Only shapes that end in a bare file name qualify. A join whose last
    component is an opaque runtime value (``root / relative``, ``''.join(rows)``)
    cannot be a repository file key, so it is neither keyed nor reported; the
    alternative — treating every division and every ``.join`` as a path — makes
    the guard cry wolf on ordinary string handling.
    """
    if isinstance(node, ast.BinOp):
        if isinstance(node.op, ast.Div):
            return _bare_tail(node, constants)
        return _joins_a_path_string(node)      # ``+`` / ``%`` with a path literal
    if isinstance(node, ast.JoinedStr):
        return _joins_a_path_string(node)
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name):
            return func.id == 'Path' and bool(node.args) and _bare_tail(node, constants)
        if isinstance(func, ast.Attribute):
            if func.attr in PATH_CALL_ATTRS:
                return _bare_tail(node, constants)
            if func.attr == 'join':
                return bool(node.args) and _bare_tail(node.args[-1], constants)
            if func.attr in ('format', 'format_map'):
                return (isinstance(func.value, ast.Constant)
                        and isinstance(func.value.value, str)
                        and _path_literal_ending_in_file(func.value.value))
    return False


def _shared_module_keys(tree):
    """``fixtures/<helper>.py`` for every shared fixture helper the table imports.

    The discovery loop keys a changed ``fixtures/negative_suite.py`` as
    ``fixtures/negative_suite.py``; a table that imports that helper is not
    scheduled for the change unless the same literal occurs in its own text.
    Declaring it in ``DISCOVERY_ANCHORS`` is what puts it there.
    """
    keys = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            modules = [alias.name for alias in node.names]
        elif isinstance(node, ast.ImportFrom) and node.module and not node.level:
            modules = [node.module]
        else:
            continue
        for module in modules:
            name = module.split('.')[0]
            if name.startswith('negative_') and (FIXTURES / f'{name}.py').is_file():
                keys.add(f'fixtures/{name}.py')
    return keys


def bare_concat_keys(tree, table):
    """``({key: [source expression, ...]}, problems)`` for one table.

    A key is the discovery key ``dir/file`` of a path the table builds from a
    bare file name, however the join is spelled: ``/``, ``Path(a, b)``,
    ``joinpath``, ``with_name``, ``with_suffix``, or a comprehension over a
    literal tuple. Every path shape the scanner cannot resolve is reported, so a
    table that hides its directory behind an unsupported expression fails the
    guard instead of quietly passing it.
    """
    self_path = FIXTURES_FROM_ROOT + tuple(table.split('/'))
    constants = _constants(tree)
    shadowed = _shadowed_names(tree)
    keys = {}
    problems = []

    def record(segments, expression):
        keys.setdefault('/'.join(segments[-2:]), []).append(expression)

    def resolve(node, expression):
        segments = _segments(node, constants, self_path)
        if segments is None:
            ambiguous = sorted({child.id for child in ast.walk(node)
                                if isinstance(child, ast.Name) and child.id in shadowed})
            if ambiguous:
                problems.append('ambiguous module-level name rebound inside a function: '
                                f'{ambiguous[0]}: {expression}')
            elif _is_repo_rooted(node, constants):
                problems.append('unresolvable repository path (this scanner cannot '
                                f'follow it): {expression}')
            return
        if not _bare_tail(node, constants):
            return
        if len(segments) < 2:
            problems.append(f'no discovery key (needs a directory and a file name): {expression}')
            return
        record(segments, expression)

    class Visitor(ast.NodeVisitor):
        def visit_BinOp(self, node):
            if (isinstance(node.op, ast.Add) and _joins_a_path_string(node)
                    and _is_repo_rooted(node, constants)):
                # ``str(ROOT) + '/src/x/mod.rs'`` is statically resolvable in
                # principle but not by this scanner; keeping it silent would be
                # the same fail-open shape the gate exists to catch.
                problems.append('unsupported string concatenation of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            elif _is_path_shape(node, constants):
                resolve(node, ast.unparse(node))
            self.generic_visit(node)

        def visit_JoinedStr(self, node):
            if _joins_a_path_string(node) and _is_repo_rooted(node, constants):
                problems.append('unsupported f-string path of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            self.generic_visit(node)

        def visit_Call(self, node):
            if _is_path_shape(node, constants):
                resolve(node, ast.unparse(node))
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
                    return
            # A comprehension element that is a path shape but not a literal
            # tuple walk is still a path: evaluate or report it like any other.
            if isinstance(element, (ast.BinOp, ast.Call, ast.JoinedStr)) and _is_path_shape(element, constants):
                resolve(element, ast.unparse(element))

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
    for key in sorted(_shared_module_keys(tree)):
        required.setdefault(key, ['imported shared fixture helper'])
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


def sweep(root=FIXTURES):
    """``(blind, unresolvable)`` for every negative-control table under ``root``.

    ``blind`` maps a table to the discovery keys it builds from bare file names;
    ``unresolvable`` maps one to the repository path shapes this scanner cannot
    follow. Both must be registered, so a new table cannot appear without a
    decision — including a table whose every path is written in a shape the
    scanner does not resolve. A path with no two-part discovery key (``ROOT /
    'rust-toolchain.toml'``) is not reported: the loop never keys it either.
    """
    blind, unresolvable = {}, {}
    for path in sorted(root.rglob('*.py')):
        rel = path.relative_to(root).as_posix()
        if 'negative' not in path.name or rel in TABLES:
            continue
        if path.name in SHARED_HELPERS or path.name.startswith('test_'):
            continue
        if rel.startswith('_'):
            continue
        keys, problems = bare_concat_keys(ast.parse(path.read_text(encoding='utf-8')), rel)
        if keys:
            blind[rel] = keys
        hard = [problem for problem in problems
                if not problem.startswith('no discovery key')]
        if hard:
            unresolvable[rel] = hard
    return blind, unresolvable


class DiscoveryAnchorTests(unittest.TestCase):
    def test_the_registry_is_not_empty(self):
        """An empty or gutted TABLES list must not turn every check vacuous."""
        self.assertTrue(TABLES, 'TABLES is empty: the guard would measure nothing')

    def test_no_table_is_blind_outside_the_registers(self):
        """A new blind table cannot appear silently.

        Every ``*negative*.py`` table under this directory either declares its
        anchors (so it is in ``TABLES``), or is listed in ``KNOWN_BLIND`` with a
        reason, or needs none. Adding a table that builds repository paths from
        bare file names turns this red until somebody decides which it is — and
        so does a table whose paths are all written in a shape this scanner
        cannot follow.
        """
        blind, unresolvable = sweep()
        for rel in sorted(blind):
            with self.subTest(table=rel):
                self.assertIn(
                    rel, KNOWN_BLIND,
                    'builds repository paths from bare file names: declare '
                    'DISCOVERY_ANCHORS and add the table to TABLES, or list it in '
                    'KNOWN_BLIND with a reason')
        for rel, problems in sorted(unresolvable.items()):
            with self.subTest(table=rel):
                self.assertIn(
                    rel, KNOWN_BLIND,
                    'builds repository paths this scanner cannot follow '
                    f'({problems[0][:120]}): declare DISCOVERY_ANCHORS and add the '
                    'table to TABLES, or list it in KNOWN_BLIND with a reason')
        found = set(blind) | set(unresolvable)
        self.assertEqual(sorted(set(KNOWN_BLIND) - found), [],
                         'stale KNOWN_BLIND entries: fixed or deleted tables must be removed')

    def test_a_new_blind_table_turns_the_guard_red(self):
        """The register's discrimination, proven on a table that is not in it."""
        with tempfile.TemporaryDirectory() as workspace:
            table = Path(workspace) / 'newfeat' / 'negative_controls.py'
            table.parent.mkdir()
            table.write_text(
                "from pathlib import Path\n"
                "ROOT = Path(__file__).resolve().parents[5]\n"
                "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
                "rows = [('a', SMS / 'mod.rs', 'x')]\n",
                encoding='utf-8')
            blind, unresolvable = sweep(Path(workspace))
        self.assertEqual(sorted(blind), ['newfeat/negative_controls.py'])
        self.assertEqual(unresolvable, {})
        self.assertNotIn('newfeat/negative_controls.py', KNOWN_BLIND)

    def test_a_new_table_of_unresolvable_shapes_turns_the_guard_red(self):
        """A table whose paths are all unsupported cannot hide behind "no keys"."""
        with tempfile.TemporaryDirectory() as workspace:
            table = Path(workspace) / 'newfeat' / 'negative_controls.py'
            table.parent.mkdir()
            table.write_text(
                "from pathlib import Path\n"
                "ROOT = Path(__file__).resolve().parents[5]\n"
                "PICK = {'dir': 'src/pilotdeck-gateway/src/channel/sms'}\n"
                "rows = [('a', ROOT / PICK['dir'] / 'mod.rs', 'x')]\n",
                encoding='utf-8')
            blind, unresolvable = sweep(Path(workspace))
        self.assertEqual(blind, {})
        self.assertEqual(sorted(unresolvable), ['newfeat/negative_controls.py'])
        self.assertTrue(any('unresolvable' in problem
                            for problem in unresolvable['newfeat/negative_controls.py']))

    def test_known_blind_entries_name_a_real_table(self):
        for rel, reason in KNOWN_BLIND.items():
            with self.subTest(table=rel):
                self.assertTrue((FIXTURES / rel).is_file(),
                                f'{rel} is gone; drop its KNOWN_BLIND entry')
                self.assertNotIn(rel, TABLES, f'{rel} is registered; drop its KNOWN_BLIND entry')
                self.assertGreater(len(reason), 4, f'{rel}: KNOWN_BLIND needs a reason')

    def test_known_blind_counts_match_the_scan(self):
        """The register's numbers are part of it: a stale count hides new keys.

        Each reason states how many discovery keys the table builds and how many
        path shapes the scanner could not follow. Both are recomputed here, so a
        row added to a registered table — or a shape that stops resolving — turns
        this red instead of quietly widening the blind spot behind an old number.
        """
        blind, unresolvable = sweep()
        def key_count(count):
            return f'{count} key' if count == 1 else f'{count} keys'
        for rel, reason in KNOWN_BLIND.items():
            with self.subTest(table=rel):
                keys = blind.get(rel, {})
                problems = unresolvable.get(rel, [])
                expected = (
                    f'{key_count(len(keys))} + {len(problems)} unresolvable' if keys and problems
                    else key_count(len(keys)) if keys
                    else f'{len(problems)} unresolvable')
                self.assertEqual(reason, expected,
                                 f'{rel}: KNOWN_BLIND reason is stale; re-measure the table')

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

    def test_call_shapes_carry_their_keys(self):
        """``with_name`` / ``joinpath`` / ``Path(a, b)`` hide a directory too."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
            "FIXTURE = Path(__file__).resolve().parent\n"
            "DISCOVERY_ANCHORS = ('sms/mod.rs', 'sms/sender.rs', 'example/oracle.json')\n"
            "rows = [Path(SMS, 'mod.rs'), SMS.joinpath('sender.rs')]\n"
            "oracle = Path(__file__).with_name('oracle.json')\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])
        dropped = synthetic.replace(
            'DISCOVERY_ANCHORS = (\'sms/mod.rs\', \'sms/sender.rs\', \'example/oracle.json\')',
            "DISCOVERY_ANCHORS = ('sms/mod.rs',)")
        problems = violations(dropped, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: sms/sender.rs' in problem for problem in problems),
                        problems)
        self.assertTrue(any('missing anchor: example/oracle.json' in problem for problem in problems),
                        problems)

    def test_unsupported_call_shapes_fail_closed(self):
        """``os.path.join`` / ``%`` / ``.format()`` are errors, not silent passes."""
        shapes = {
            'os.path.join': "path = os.path.join(SMS, 'sender.rs')\n",
            'percent': "path = '%s/sender.rs' % SMS\n",
            'format': "path = '{}/sender.rs'.format(SMS)\n",
        }
        for label, line in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + line
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(any('unresolvable' in problem or 'unsupported' in problem
                                    for problem in problems), problems)

    def test_imported_shared_helpers_need_an_anchor(self):
        """A shared fixture helper is discovered by its own path literal."""
        synthetic = (
            "from negative_suite import matrix_main\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [('a', ROOT / 'src/example/named.rs', 'x')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: fixtures/negative_suite.py' in problem
                            for problem in problems), problems)
        declared = synthetic.replace("DISCOVERY_ANCHORS = ()",
                                     "DISCOVERY_ANCHORS = ('fixtures/negative_suite.py',)")
        self.assertEqual(violations(declared, 'example/negative_controls.py'), [])

    def test_a_shadowed_module_constant_is_not_substituted(self):
        """A name rebound inside a function means two directories; report it."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example/a'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def main():\n"
            "    BASE = ROOT / 'src/example/b'\n"
            "    return [BASE / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous' in problem for problem in problems), problems)

    def test_a_negative_parent_index_is_not_the_repository_root(self):
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[-1]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'src/example/named.rs']\n"
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
