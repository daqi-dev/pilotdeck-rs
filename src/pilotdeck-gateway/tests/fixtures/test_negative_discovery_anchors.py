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
may use instead of ``/`` (``Path(base, 'mod.rs')``, ``base.joinpath('mod.rs')``),
and a comprehension or ``for`` loop over a literal tuple is walked once per
element, so ``[SMS / name for name in ('mod.rs',)]`` keys ``sms/mod.rs``. Paths
built by string ``+`` (including a fragment that ends in ``/``, where the file
name arrives in the next operand), by an f-string, or with ``os.path.join`` /
``%`` / ``.format()`` / a bare ``join(...)`` are outside that family and are
reported as errors rather than guessed; so is anything else the resolver cannot
follow from the repository root, including a module-level name that a function
rebinds (by assignment, ``for`` target, parameter or any other binding form), a
comprehension over a non-literal source, and an enumeration of a repository
directory (``.glob`` / ``.rglob`` / ``.iterdir`` / ``.walk``) whose file names
only the runtime knows. Silent skipping is what makes a discovery gate *look*
green while measuring nothing.

A table also depends on the fixtures it imports: the loop keys a changed
``fixtures/negative_suite.py`` as ``fixtures/negative_suite.py`` and a changed
``fixtures/_shared/mutant_switch.py`` as ``_shared/mutant_switch.py``, and the
import alone does not put either literal in the table's text, so ``violations``
requires the same anchor for every imported local module — a top-level helper, a
library directory (``_shared``, ``_negkit``), a nested module path, or a
relative import resolved against the table's own directory. ``sweep`` applies
the same rule to every table, registered or not: an import-only table is blind
too, and turns the register check red until somebody decides.

``test_removing_an_anchor_turns_the_guard_red`` re-runs the check with each
anchor dropped in turn, so the guard's discrimination is proven on every run
rather than only when somebody remembers to negate it by hand. Local negative
controls cover the other directions: a new bare row, an empty ``TABLES``, a
second ``DISCOVERY_ANCHORS`` assignment, a string-built path, unsupported call
shapes, a shadowed or parameter-bound constant, a comprehension over an
unsupported join, a display string that only looks like a path, and new blind,
import-only or unresolvable tables appearing outside both registers.

Scope: the four tables named by #2133 are the ones that carry anchors today;
adding a table to ``TABLES`` is the whole opt-in, and the rule then demands its
``DISCOVERY_ANCHORS`` match its own keys. Everything else that already builds
repository paths from bare file names, or imports a shared helper without
spelling its key, is listed in ``KNOWN_BLIND`` — 243 tables, 670 keys at the
time of writing (329 of them shared-helper imports), 28 of those tables also
holding shapes the resolver cannot follow at all. That register is a fix list,
not an exemption: ``test_no_table_is_blind_outside_the_registers`` turns this
red when a *new* table arrives blind, import-only or unresolvable, and
``test_known_blind_counts_match_the_scan`` turns it red when a registered table
grows another key. Entries may only leave the register by being fixed and moved
into ``TABLES``; #2141 carries the per-table list. ``schemata.py`` tables
(``turn-gates`` / ``turn-prompt-hooks`` / ``turn-runner-tail`` /
``turn-start-notices``) do not match the discovery glob at all — that is a limit
of ``neg-for-diff.sh`` itself and no anchor can fix it.

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


# Tables the discovery loop cannot schedule from a change they depend on. This is
# a register, not an exemption: an entry may be deleted only by fixing the table
# (add the anchors, move it to TABLES), and a new table that arrives blind turns
# the guard red until it is registered here with a reason or fixed. Two axes are
# recorded, because ``git grep -F`` fails on both the same way:
#   * a path the table builds from a bare file name (``SMS / 'mod.rs'``), and
#   * a shared fixture helper the table imports (``from negative_suite import
#     ...``): a change to ``fixtures/negative_suite.py`` keys
#     ``fixtures/negative_suite.py``, which the import line never spells out.
# Each reason states how many discovery keys the table needs and how many path
# shapes the scanner could not follow; both are re-measured by
# test_known_blind_counts_match_the_scan. #2141 carries the per-table fix list.
# 243 tables (215 blind, 2 with unresolvable shapes, 26 in both), 670 keys: 341
# bare-filename joins and 329 shared-helper imports.
KNOWN_BLIND = {
    'agent-event-projector/negative.py': '3 keys',
    'anchor-marker/negative_controls.py': '3 keys',
    'api-server-channel/negative_controls.py': '2 keys',
    'api-web/negative_controls.py': '5 keys',
    'attach-followups-a/negative_controls.py': '2 keys',
    'background-create-gate/negative_controls.py': '2 keys',
    'background-output/negative.py': '2 keys',
    'background-panic/negative_controls.py': '3 keys',
    'background-persist-progress/negative_controls.py': '2 keys',
    'background-persist-restart/negative_controls.py': '2 keys',
    'background-subtick-gate/negative_controls.py': '4 keys',
    'baseline-provenance/negative_controls.py': '1 key',
    'channel-core/negative_controls.py': '6 keys + 3 unresolvable',
    'channel-unmigrated-startup/negative_controls.py': '1 key',
    'channel-unmigrated/negative_controls.py': '2 keys + 1 unresolvable',
    'cli-parity-isolation/negative_controls.py': '1 key + 1 unresolvable',
    'cli-parity-lock-bounded/negative_controls.py': '3 keys',
    'cli-parity-mock-gateway/negative_controls.py': '1 key + 1 unresolvable',
    'closure_guard_negative.py': '3 keys',
    'commands-catalog/negative_controls.py': '3 keys',
    'config-changes/negative_coverage.py': '2 keys',
    'config-store/negative_controls.py': '1 key',
    'connection-context/negative.py': '6 keys + 1 unresolvable',
    'cron-schedule/negative_controls.py': '2 keys',
    'cron-store/negative_controls.py': '2 keys',
    'diff-backfill/negative.py': '1 key',
    'edgeclaw-accessors/negative.py': '5 keys',
    'edgeclaw-capture-turn/negative.py': '3 keys',
    'edgeclaw-dangling-links/negative.py': '2 keys',
    'edgeclaw-dream-run/negative.py': '1 key',
    'edgeclaw-dream-runner/negative.py': '1 key',
    'edgeclaw-dream/negative.py': '1 key',
    'edgeclaw-dream/negative_shapes.py': '2 keys',
    'edgeclaw-entries/negative.py': '1 key',
    'edgeclaw-entries/negative_fix1.py': '1 key + 1 unresolvable',
    'edgeclaw-entries/negative_ids.py': '1 key',
    'edgeclaw-entries/negative_round6.py': '3 keys',
    'edgeclaw-entries/negative_round7.py': '1 key',
    'edgeclaw-export/negative_locks.py': '1 key',
    'edgeclaw-file-memory/negative.py': '2 keys',
    'edgeclaw-heartbeat-indexer/negative.py': '2 keys',
    'edgeclaw-heartbeat-isolation/negative.py': '6 keys',
    'edgeclaw-heartbeat/negative.py': '1 key',
    'edgeclaw-llm-config/negative.py': '1 key',
    'edgeclaw-llm-dream/negative.py': '1 key',
    'edgeclaw-llm-envelope/negative.py': '1 key',
    'edgeclaw-llm-index/negative.py': '1 key',
    'edgeclaw-llm-recall/negative.py': '1 key',
    'edgeclaw-llm-telemetry/negative.py': '2 keys',
    'edgeclaw-llm/negative.py': '1 key',
    'edgeclaw-messages/negative.py': '2 keys',
    'edgeclaw-native-default/negative.py': '6 keys',
    'edgeclaw-provider/negative.py': '6 keys',
    'edgeclaw-real-provider/negative.py': '2 keys',
    'edgeclaw-retrieval-helpers/negative.py': '1 key',
    'edgeclaw-retriever/negative.py': '1 key',
    'edgeclaw-same-root-generation/negative.py': '8 keys',
    'edgeclaw-service-facade/negative.py': '2 keys',
    'edgeclaw-service-facade/switch_negative.py': '4 keys',
    'edgeclaw-service-orchestration/negative.py': '1 key',
    'edgeclaw-service-orchestration/switch_negative.py': '2 keys',
    'edgeclaw-text/negative.py': '4 keys',
    'exact-receipt-yaml/negative_controls.py': '1 key',
    'exact-rerun-receipt/negative_controls.py': '1 key',
    'failure-guard/negative.py': '5 keys',
    'fe-baseline-report/negative_controls.py': '1 unresolvable',
    'fe-coexist/negative_controls.py': '1 key',
    'file-history-cwd/negative_controls.py': '4 keys',
    'flake-progress/negative_controls.py': '2 keys',
    'frozen-manifest/negative_controls.py': '2 keys',
    'frozen-reinsert-guard/negative_controls.py': '4 keys',
    'gateway-batch/negative_controls.py': '2 keys',
    'grep-etxtbsy/negative_controls.py': '2 keys',
    'hooks-output/negative.py': '1 key',
    'host-event-pump/negative_controls.py': '1 key',
    'html-timing/negative_controls.py': '2 keys',
    'http-store-park/negative_controls.py': '3 keys',
    'interaction-bus/negative_controls.py': '3 keys',
    'interaction-values/negative_controls.py': '4 keys',
    'issue-1246/negative_controls.py': '2 keys',
    'issue-1437-methods/negative_controls.py': '1 key',
    'issue-297/negative_controls.py': '3 keys',
    'js-math-zero/negative.py': '1 key',
    'ledger-diffs/negative_controls.py': '2 keys',
    'live-e2e-tmpdir/negative_controls.py': '2 keys',
    'live-e2e/negative_controls.py': '3 keys + 2 unresolvable',
    'live-history/negative.py': '1 key',
    'live-recording/negative.py': '6 keys',
    'live-session/negative_controls.py': '2 keys',
    'locale-adaptive/negative_controls.py': '2 keys',
    'locale-contract/negative_controls.py': '2 keys',
    'locked-storage/negative_controls.py': '2 keys',
    'manager-gateway/negative_controls.py': '3 keys',
    'mcp-detached-dispatch/negative_controls.py': '1 key',
    'mcp-gateway/negative_controls.py': '2 keys',
    'mcp-hooks-spawn/negative_controls.py': '1 key',
    'mcp-http-corrupt-body/negative_controls.py': '2 keys',
    'mcp-http-encoding/negative.py': '2 keys',
    'memory-maintenance/negative_controls.py': '2 keys',
    'metadata-from-env/negative_controls.py': '3 keys',
    'model-catalog-cwd/negative_controls.py': '5 keys',
    'model-complete-timeout/negative_controls.py': '2 keys',
    'model-complete/negative-controls.py': '3 keys + 2 unresolvable',
    'model-factory/negative-controls.py': '1 key',
    'model-http-encoding/negative.py': '1 key',
    'module-host-approvals/negative_controls.py': '5 keys',
    'module-host/negative_controls.py': '5 keys',
    'native-archive-cwd/negative_controls.py': '3 keys',
    'native-archive-cwd/negative_proxy.py': '2 keys',
    'native-archive-cwd/negative_regeneration.py': '4 keys',
    'native-collation/negative_controls.py': '7 keys',
    'native-config/negative_controls.py': '2 keys',
    'native-cron/negative_controls.py': '2 keys',
    'native-dispatch/negative_controls.py': '4 keys',
    'native-entry-authority/negative_controls.py': '3 keys',
    'native-http-basic/negative_controls.py': '5 keys',
    'native-loop-stimulus/negative_controls.py': '4 keys',
    'native-management/negative_controls.py': '4 keys',
    'native-metadata-cwd/negative_controls.py': '3 keys',
    'native-metadata-cwd/negative_regeneration.py': '4 keys',
    'native-process/negative_controls.py': '2 keys',
    'native-registry-switch/negative_controls.py': '3 keys',
    'native-session-e2e/negative_controls.py': '2 keys',
    'native-settings/negative/negative_controls.py': '3 keys',
    'native-stimulus-plan/negative_controls.py': '5 keys',
    'native-storage-stimulus/negative_controls.py': '3 keys',
    'native-topology/negative_signals.py': '3 keys',
    'native-witness/negative.py': '2 keys',
    'native-witness/negative_authentication.py': '4 keys',
    'native-witness/negative_boundary.py': '2 keys',
    'native-ws-caps/negative_controls.py': '3 keys + 1 unresolvable',
    'native-ws-entry/negative_controls.py': '6 keys + 1 unresolvable',
    'native-ws-notify/negative_controls.py': '2 keys',
    'negative-adaptive/isolation_negative.py': '2 keys',
    'negative-adaptive/negative_controls.py': '2 keys',
    'negative-output-directory/negative.py': '3 keys',
    'negative_oracle_guard.py': '1 key + 1 unresolvable',
    'network-timing/negative_controls.py': '4 keys',
    'network-timing/negative_private.py': '2 keys',
    'node-io/negative_controls.py': '3 keys',
    'node-provenance/negative.py': '2 keys',
    'operation-ledger/negative.py': '2 keys',
    'output-schema/negative.py': '3 keys',
    'p01-preflight/negative_controls.py': '2 keys',
    'parity-lock-release/negative_controls.py': '2 keys',
    'pending-reanchor/negative_controls.py': '4 keys',
    'plugin-loader/negative-controls.py': '3 keys',
    'plugin-runtime/negative-controls.py': '4 keys',
    'plugin-runtime/notify-window-negative.py': '3 keys',
    'plugin-runtime/panic-negative.py': '3 keys',
    'private-ca/negative_controls.py': '1 key',
    'project-files-linux-dirent/negative_controls.py': '3 keys',
    'projector-cwd/negative_controls.py': '3 keys',
    'provider-http-boundaries/negative_controls.py': '2 keys',
    'proxy-listen-address/negative_controls.py': '3 keys',
    'pump-order-cases/negative_controls.py': '2 keys',
    'recorder/fix3-negative.py': '5 keys',
    'regeneration-masks/negative.py': '2 keys',
    'replacement/negative-518.py': '2 keys',
    'replacement/negative.py': '1 key',
    'retry-after-dialects/negative_controls.py': '2 keys',
    'run-registry/count-negative.py': '6 keys',
    'run-registry/timestamp-negative.py': '4 keys',
    'runtime-spawn-panic/helpers_negative.py': '2 keys',
    'runtime-spawn-panic/negative_controls.py': '3 keys',
    'sdk-cli-lifecycle/negative_controls.py': '2 keys + 1 unresolvable',
    'sdk-format/negative_controls.py': '4 keys + 2 unresolvable',
    'sdk-mcp-p2/negative.py': '2 keys',
    'sdk-run/negative_controls.py': '5 keys + 3 unresolvable',
    'sdk-sessions-list/negative_controls.py': '2 keys + 1 unresolvable',
    'sdk-sessions-messages/negative_controls.py': '6 keys + 1 unresolvable',
    'sdk-settings/negative_controls.py': '5 keys + 2 unresolvable',
    'sdk-signal-read-lease/negative_controls.py': '3 keys',
    'sdk-signals-close-order-pin/negative_controls.py': '2 keys',
    'sdk-signals/negative_controls.py': '3 keys + 3 unresolvable',
    'sdk-terminal/negative_controls.py': '4 keys + 2 unresolvable',
    'session-abort-contract/negative.py': '3 keys',
    'session-admission/negative.py': '3 keys',
    'session-adopt/switch_negative.py': '2 keys',
    'session-assembly-1316/negative_controls.py': '3 keys + 8 unresolvable',
    'session-domain-cleanup/negative_controls.py': '2 keys',
    'session-domain-expectations/negative_controls.py': '3 keys',
    'session-domain-lock-release/negative_controls.py': '3 keys',
    'session-domain-native/negative_controls.py': '3 keys',
    'session-domain/adapter-negative.py': '1 key',
    'session-domain/fix1-negative.py': '1 key',
    'session-domain/fix2-negative.py': '1 key + 1 unresolvable',
    'session-drop-release/negative_controls.py': '4 keys',
    'session-factory/append-delta/negative_controls.py': '3 keys + 7 unresolvable',
    'session-factory/negative_controls.py': '12 keys',
    'session-factory/projection-cache/negative_controls.py': '5 keys + 4 unresolvable',
    'session-lease/negative.py': '3 keys',
    'session-owner-hooks/negative.py': '1 key',
    'session-owner/negative.py': '3 keys',
    'session-owner/negative_close_shutdown.py': '2 keys',
    'settings-gateway/negative_controls.py': '4 keys',
    'settings/negative_controls.py': '2 keys',
    'shared-runner-failclosed/negative_controls.py': '2 keys',
    'sidecar-payload/negative.py': '1 key',
    'skills-external/negative_controls.py': '2 keys',
    'skills-external/negative_controls_949.py': '1 key',
    'skills-tree-files/negative.py': '1 key',
    'skills/negative_controls.py': '3 keys',
    'spill-isolation/negative_controls.py': '1 key',
    'stats-cwd/negative_controls.py': '4 keys',
    'stress-followups/negative_controls.py': '2 keys',
    'surrogate-collation-matrix/negative_controls.py': '5 keys',
    'task-budget-zero/negative.py': '1 key',
    'tool-budget/negative_project.py': '1 key',
    'transcript-resolve/negative_controls.py': '1 key',
    'transcript-trim/negative_controls.py': '2 keys',
    'treelock-budget/negative_controls.py': '5 keys',
    'turn-gateway/negative-fence.py': '9 keys',
    'turn-gateway/negative.py': '5 keys',
    'turn-publish/negative.py': '4 keys',
    'turn-read/negative.py': '4 keys',
    'turn-replay/negative.py': '6 keys',
    'turn-runner/forward_negative.py': '5 keys',
    'turn-runner/input_negative.py': '4 keys',
    'turn-runner/integrity_negative.py': '4 keys',
    'turn-runner/live_negative.py': '2 keys',
    'turn-runner/negative_controls.py': '1 key',
    'turn-runner/recording_negative.py': '2 keys',
    'turn-runner/runner_negative.py': '2 keys',
    'turn-submit-admission/negative_controls.py': '2 keys',
    'turn-submit/negative.py': '4 keys',
    'ui-server-parity-isolation/negative_controls.py': '2 unresolvable',
    'unsafe-config-boundary/negative_controls.py': '3 keys',
    'upload-gateway/negative_controls.py': '2 keys',
    'user-dialog-channel/negative.py': '7 keys + 1 unresolvable',
    'user-dialog-guard-order/negative_controls.py': '2 keys',
    'user-dialog-http-store-1555/negative_controls_1555.py': '2 keys',
    'user-dialog-http-store/negative_controls_1232.py': '2 keys',
    'user-dialog-http-store/negative_controls_1277.py': '2 keys',
    'user-dialog-lock-scope/negative_controls.py': '3 keys',
    'web-conversion-lifecycle/negative.py': '3 keys',
    'webhook-channel/negative_controls.py': '7 keys',
    'wecom-callback/negative_controls.py': '7 keys + 2 unresolvable',
    'weixin-channel/negative_controls.py': '9 keys + 2 unresolvable',
    'workspace-pristine/negative_controls.py': '5 keys',
    'workspace-snapshot-gate/negative_controls.py': '1 key',
    'workspace-snapshot-nonutf8/negative_controls.py': '1 key',
    'zone-read-bound/negative_controls.py': '2 keys',

}

ROOT_NAMES = frozenset({'ROOT'})

# Shared executors and helpers every table imports; they are not tables
# themselves. ``neg-for-diff.sh`` filters these out of the list it emits
# (``grep -vE '/(_negative_common|negative_suite|negative_process|negative_panic|
# negative_selection|negative_source|negative_shared)\.py$'``), and this list is
# that filter plus two helpers the host does not name but which define no
# mutation table either (``anchor_selfcheck.py``, ``pending_reanchor.py``, and
# ``negative_scope.py``). ``test_*.py`` is dropped by the same host filter and is
# skipped alongside these by ``sweep``. A helper still owed an anchor is found by
# the import scan: the list says what is not a table, not what a table may
# depend on for free.
SHARED_HELPERS = frozenset({
    '_negative_common.py', 'anchor_selfcheck.py', 'negative_panic.py',
    'negative_process.py', 'negative_scope.py', 'negative_selection.py',
    'negative_shared.py', 'negative_source.py', 'negative_suite.py',
    'pending_reanchor.py',
})

# Directories holding shared libraries rather than discovery targets. A changed
# ``fixtures/_shared/mutant_switch.py`` is keyed ``_shared/mutant_switch.py`` by
# the loop, so importing from one of these is the same dependency as importing a
# ``SHARED_HELPERS`` module one level up.
LIBRARY_DIRS = frozenset({'_shared', '_negkit'})


def _bind_names(target, names):
    """Add every name bound by an assignment target (including ``a, b`` / ``*a``)."""
    if isinstance(target, ast.Name):
        names.add(target.id)
    elif isinstance(target, (ast.Tuple, ast.List)):
        for element in target.elts:
            _bind_names(element, names)
    elif isinstance(target, ast.Starred):
        _bind_names(target.value, names)


def _rebound_elsewhere(tree):
    """Names bound by any form other than a plain ``name = ...`` statement.

    A parameter, a ``for`` target, a ``with ... as``, an ``except ... as``, a
    comprehension target, a ``+=`` or a walrus binds the name just as firmly as
    an assignment. When the same name is also assigned once, the *other* binding
    decides its value at that site, so substituting the assignment there keys a
    directory the runtime never uses — a wrong key, or a spurious failure.
    """
    names = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            arguments = node.args
            for argument in (list(arguments.posonlyargs) + list(arguments.args)
                             + list(arguments.kwonlyargs)
                             + [arguments.vararg, arguments.kwarg]):
                if argument is not None:
                    names.add(argument.arg)
        elif isinstance(node, (ast.For, ast.AsyncFor)):
            _bind_names(node.target, names)
        elif isinstance(node, (ast.With, ast.AsyncWith)):
            for item in node.items:
                if item.optional_vars is not None:
                    _bind_names(item.optional_vars, names)
        elif isinstance(node, ast.ExceptHandler) and node.name:
            names.add(node.name)
        elif isinstance(node, (ast.ListComp, ast.SetComp, ast.DictComp, ast.GeneratorExp)):
            for generator in node.generators:
                _bind_names(generator.target, names)
        elif isinstance(node, (ast.AugAssign, ast.AnnAssign)):
            _bind_names(node.target, names)
        elif isinstance(node, ast.NamedExpr):
            _bind_names(node.target, names)
    return names


def _constants(tree):
    """``name = <expression>`` bindings this scanner is willing to substitute.

    Module-level bindings are authoritative (last one wins, as at runtime).
    Tables also build paths from local aliases (``source = ROOT / '...'`` inside
    a function, then ``source.parent / 'x.rs'``); a name assigned exactly once in
    the whole file has no ambiguity, so it is resolved too. A module-level name
    that a function rebinds is *not* substituted: the two sites would then mean
    different directories, and resolving both to the module-level value would
    demand an anchor for a key the runtime never uses while hiding the real one.
    Neither is a name bound by some other form (parameter, loop target, ...).
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
    rebound = _rebound_elsewhere(tree)
    for name, value in assignments:
        if name not in constants and counts[name] == 1 and name not in rebound:
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
    """Names bound anywhere below a function, lambda or class body.

    Every binding form counts, not just ``name = ...``: a ``for`` target, a
    ``with ... as``, an ``except ... as``, a comprehension target or a walrus
    rebinds the name just as well, and a module-level constant of the same name
    then means two different things depending on the call site.
    """
    names = set()

    def bind(target):
        if isinstance(target, ast.Name):
            names.add(target.id)
        elif isinstance(target, (ast.Tuple, ast.List)):
            for element in target.elts:
                bind(element)
        elif isinstance(target, ast.Starred):
            bind(target.value)

    def visit(node, scoped):
        for child in ast.iter_child_nodes(node):
            inner = scoped or isinstance(
                child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda, ast.ClassDef))
            if inner:
                if isinstance(child, ast.Assign):
                    for target in child.targets:
                        bind(target)
                elif isinstance(child, (ast.AnnAssign, ast.AugAssign)):
                    bind(child.target)
                elif isinstance(child, ast.For):
                    bind(child.target)
                elif isinstance(child, (ast.With, ast.AsyncWith)):
                    for item in child.items:
                        if item.optional_vars is not None:
                            bind(item.optional_vars)
                elif isinstance(child, ast.ExceptHandler) and child.name:
                    names.add(child.name)
                elif isinstance(child, (ast.ListComp, ast.SetComp, ast.DictComp,
                                        ast.GeneratorExp)):
                    for generator in child.generators:
                        bind(generator.target)
                elif isinstance(child, ast.NamedExpr):
                    bind(child.target)
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


def _path_literal_operand(value):
    """Whether a string literal is one fragment of a ``/``-joined path.

    Either it ends in a bare file name (``'/src/x/mod.rs'``) or it ends with a
    separator, so the file name arrives in the next operand:
    ``str(ROOT) + '/src/x/' + 'mod.rs'``. Looking only at the final component of
    each *literal* misses that second shape — the literal's last component is
    empty — and the whole path then stays invisible to the discovery loop, which
    is exactly the false pass this guard exists to prevent. A bare separator
    (``f'{killed}/{total} killed'``) names no component and stays a plain string.
    """
    if not isinstance(value, str) or '/' not in value:
        return False
    if value.endswith('/'):
        return any(_is_bare_component(part) for part in value.split('/'))
    return _is_bare_component(value.rsplit('/', 1)[1])


def _joins_a_path_string(node):
    """Whether an ``Add`` / f-string carries a path-looking string literal.

    List concatenation (``[...] + [...]``) is common in tables and has nothing
    to do with paths, so the shape alone is not enough: one direct operand must
    be a string constant that is a path fragment (see ``_path_literal_operand``).
    """
    if isinstance(node, ast.JoinedStr):
        operands = node.values
    else:
        operands = (node.left, node.right)
    return any(isinstance(operand, ast.Constant)
               and _path_literal_operand(operand.value) for operand in operands)


def _iterated_elements(node, constants):
    """The literal elements a ``for name in (...)`` loop walks, or None."""
    target = node
    if isinstance(node, ast.Name) and node.id in constants:
        target = constants[node.id]
    if isinstance(target, (ast.Tuple, ast.List)):
        return list(target.elts)
    return None


def _join_base(element, target_name):
    """The part of a comprehension element that precedes the loop variable.

    ``[SMS / name for name in ('mod.rs',)]`` and ``[Path(SMS, name) for ...]``
    and ``[SMS.joinpath(name) for ...]`` all append the loop variable to a
    directory, so the directory is what decides the key. Returning the base lets
    the caller substitute each literal element and key the result; returning
    None means the element is not of that shape. Anything else — including a
    join spelling the resolver does not model, such as ``os.path.join`` — keeps
    its base here too, so the caller reports it instead of skipping it.
    """
    if (isinstance(element, ast.BinOp) and isinstance(element.op, ast.Div)
            and isinstance(element.right, ast.Name) and element.right.id == target_name):
        return element.left
    if not isinstance(element, ast.Call) or not element.args:
        return None
    last = element.args[-1]
    if not (isinstance(last, ast.Name) and last.id == target_name):
        return None
    func = element.func
    if isinstance(func, ast.Name) and func.id == 'Path':
        parts = list(element.args[:-1])
    elif isinstance(func, ast.Attribute) and func.attr in ('joinpath', 'join'):
        parts = [func.value] + list(element.args[:-1])
    else:
        return None
    if not parts:
        return None
    base = parts[0]
    for part in parts[1:]:
        # ``Path(a, b)`` means ``a / b``; folding keeps one path model.
        base = ast.BinOp(left=base, op=ast.Div(), right=part)
    return base


def _is_repo_rooted(node, constants, seen=(), shadowed=()):
    """Whether an expression is built from the repository root or ``__file__``.

    Tables also concatenate bare file names onto *runtime* directories (the
    evidence directory, a temp dir). Those are not repository paths and the
    discovery loop can never key on them, so they are not required to carry an
    anchor. An expression that does start at ``ROOT`` / ``__file__`` but that
    this scanner cannot evaluate is a real hole and is reported.

    ``shadowed`` names are not substituted: a module-level constant rebound in a
    function means different values at different sites, and following it would
    report a directory the code never uses.
    """
    if isinstance(node, ast.Name):
        if node.id in ROOT_NAMES or node.id == '__file__':
            return True
        if node.id in seen or node.id in shadowed or node.id not in constants:
            return False
        return _is_repo_rooted(constants[node.id], constants, seen + (node.id,), shadowed)
    for child in ast.iter_child_nodes(node):
        if _is_repo_rooted(child, constants, seen, shadowed):
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


def _module_key(parts, root):
    """Discovery key for one imported module path, or nothing if it is not here.

    The loop keys a changed file as ``basename(dirname)/basename`` relative to
    the repository root, so a top-level fixture keys as ``fixtures/x.py`` and a
    library module as ``_shared/x.py``. Resolving the module against ``root``
    keeps an unrelated import (``os``, ``json``) from demanding an anchor for a
    file this repository does not have, and covers every local helper rather
    than a hand-maintained list.
    """
    if not parts:
        return set()
    rel = '/'.join(list(parts[:-1]) + [parts[-1] + '.py'])
    if not (root / rel).is_file():
        return set()
    if len(parts) == 1:
        return {'fixtures/' + rel}
    return {'/'.join(rel.split('/')[-2:])}


def _shared_module_keys(tree, root=FIXTURES, table_dir=()):
    """Discovery keys for every local module the table imports.

    The discovery loop keys a changed ``fixtures/negative_suite.py`` as
    ``fixtures/negative_suite.py`` and a changed ``fixtures/_shared/x.py`` as
    ``_shared/x.py``; a table that imports the module is not scheduled for the
    change unless the same literal occurs in its own text. Declaring it in
    ``DISCOVERY_ANCHORS`` is what puts it there. ``from _shared import x`` names
    no module path at all, so each imported name is expanded as well; relative
    imports resolve against the table's own directory.
    """
    keys = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                keys |= _module_key(alias.name.split('.'), root)
        elif isinstance(node, ast.ImportFrom):
            if node.level:
                base = list(table_dir)
                if node.level > 1:
                    base = base[:max(0, len(base) - (node.level - 1))]
                module = base + (node.module.split('.') if node.module else [])
                keys |= _module_key(module, root)
                for alias in node.names:
                    keys |= _module_key(module + [alias.name], root)
            elif node.module:
                module = node.module.split('.')
                keys |= _module_key(module, root)
                for alias in node.names:
                    keys |= _module_key(module + [alias.name], root)
    return keys


def bare_concat_keys(tree, table, root=FIXTURES):
    """``({key: [source expression, ...]}, problems)`` for one table.

    A key is the discovery key ``dir/file`` of a path the table builds from a
    bare file name, however the join is spelled: ``/``, ``Path(a, b)``,
    ``joinpath``, ``with_name``, ``with_suffix``, or a comprehension over a
    literal tuple. Every path shape the scanner cannot resolve is reported, so a
    table that hides its directory behind an unsupported expression fails the
    guard instead of quietly passing it.
    """
    self_path = (FIXTURES_FROM_ROOT + tuple(table.split('/')) if root == FIXTURES
                 else tuple(table.split('/')))
    constants = _constants(tree)
    shadowed = _shadowed_names(tree)
    keys = {}
    problems = []

    def repo_rooted(node):
        return _is_repo_rooted(node, constants, shadowed=shadowed)

    def record(segments, expression):
        # A single component is not a discovery key: the loop always keys
        # ``dir/file``. Every caller reports it here rather than at its own
        # call site, so a comprehension and a ``/`` join agree.
        if len(segments) < 2:
            problems.append(f'no discovery key (needs a directory and a file name): {expression}')
            return
        keys.setdefault('/'.join(segments[-2:]), []).append(expression)

    def resolve(node, expression):
        segments = _segments(node, constants, self_path)
        if segments is None:
            ambiguous = sorted({child.id for child in ast.walk(node)
                                if isinstance(child, ast.Name) and child.id in shadowed})
            if ambiguous:
                problems.append('ambiguous module-level name rebound inside a function: '
                                f'{ambiguous[0]}: {expression}')
            elif repo_rooted(node):
                problems.append('unresolvable repository path (this scanner cannot '
                                f'follow it): {expression}')
            return
        if not _bare_tail(node, constants):
            return
        record(segments, expression)

    class Visitor(ast.NodeVisitor):
        def visit_BinOp(self, node):
            if (isinstance(node.op, ast.Add) and _joins_a_path_string(node)
                    and repo_rooted(node)):
                # ``str(ROOT) + '/src/x/mod.rs'`` is statically resolvable in
                # principle but not by this scanner; keeping it silent would be
                # the same fail-open shape the gate exists to catch.
                problems.append('unsupported string concatenation of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            elif _is_path_shape(node, constants):
                resolve(node, ast.unparse(node))
            self.generic_visit(node)

        def visit_JoinedStr(self, node):
            if _joins_a_path_string(node) and repo_rooted(node):
                problems.append('unsupported f-string path of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            self.generic_visit(node)

        def visit_Call(self, node):
            func = node.func
            if (isinstance(func, ast.Attribute)
                    and func.attr in ('glob', 'rglob', 'iterdir', 'walk')
                    and repo_rooted(func.value)):
                # Enumerating a repository directory yields file names only the
                # runtime knows; the loop can never key them from the text.
                problems.append('unsupported enumeration of a repository path '
                                f'(this scanner cannot follow it): {ast.unparse(node)}')
            elif (isinstance(func, ast.Name) and func.id == 'join'
                    and any(repo_rooted(argument) for argument in node.args)):
                # ``from os.path import join`` reaches the same unmodelled call.
                problems.append('unsupported path join of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            if _is_path_shape(node, constants):
                resolve(node, ast.unparse(node))
            self.generic_visit(node)

        def visit_For(self, node):
            """Walk ``for name in ('mod.rs',):`` once per literal element.

            The loop really runs the body with each literal bound, so a path
            built from the target (``SMS / name``) is statically known. Walking
            it once with an opaque name would leave the key invisible.
            """
            elements = None
            if isinstance(node.target, ast.Name):
                elements = _iterated_elements(node.iter, constants)
            if elements is None:
                self.generic_visit(node)
                return
            missing = object()
            saved = constants.get(node.target.id, missing)
            try:
                for element in elements:
                    constants[node.target.id] = element
                    for statement in node.body:
                        self.visit(statement)
            finally:
                if saved is missing:
                    constants.pop(node.target.id, None)
                else:
                    constants[node.target.id] = saved
            for statement in node.orelse:
                self.visit(statement)

        def visit_ListComp(self, node):
            self._comprehension(node)

        def visit_SetComp(self, node):
            self._comprehension(node)

        def visit_GeneratorExp(self, node):
            self._comprehension(node)

        def _comprehension(self, node):
            element, generators = node.elt, node.generators
            base = None
            if len(generators) == 1 and isinstance(generators[0].target, ast.Name):
                base = _join_base(element, generators[0].target.id)
            if base is not None:
                segments = _segments(base, constants, self_path)
                elements = _iterated_elements(generators[0].iter, constants)
                if elements is None:
                    if repo_rooted(generators[0].iter):
                        problems.append(f'unresolvable comprehension source: {ast.unparse(node)}')
                elif segments is None:
                    if repo_rooted(base):
                        problems.append(f'unresolvable comprehension base: {ast.unparse(node)}')
                else:
                    for candidate in elements:
                        if _bare_file(candidate):
                            record(segments + (candidate.value,),
                                   f'{ast.unparse(element)} for {candidate.value}')
            elif (isinstance(element, (ast.BinOp, ast.Call, ast.JoinedStr))
                    and _is_path_shape(element, constants)):
                # A comprehension element that is a path shape but not a literal
                # tuple walk is still a path: evaluate or report it like any other.
                resolve(element, ast.unparse(element))
            # Never drop the iteration sources or the ``if`` clauses: a shape
            # this scanner cannot follow can hide there just as well as in the
            # element, and skipping them was a silent hole.
            for generator in generators:
                self.visit(generator.iter)
                for condition in generator.ifs:
                    self.visit(condition)

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
    for key in sorted(_shared_module_keys(tree, FIXTURES,
                                         tuple(table.split('/')[:-1]))):
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
            # Same two exclusions the host applies to its own output list
            # (``grep -vE '/(...|test_[^/]*)\.py$'`` in neg-for-diff.sh), so a
            # file skipped here is never emitted as a table there either.
            continue
        if rel.split('/')[0] in LIBRARY_DIRS:
            # Library directories hold shared modules, not runnable tables; the
            # host chain imports them. Anywhere else a leading underscore is a
            # normal directory name and the host still discovers the file.
            continue
        text = path.read_text(encoding='utf-8')
        tree = ast.parse(text)
        keys, problems = bare_concat_keys(tree, rel, root=root)
        for key in _shared_module_keys(tree, root, tuple(rel.split('/')[:-1])):
            # A table that imports a shared helper is scheduled for changes to
            # that helper only if the helper's key occurs in its own text; the
            # import line spells the module name, not the key. This is the same
            # blind spot as a bare file name, one level up, and it applies to
            # every table — not just the four that declare anchors.
            keys.setdefault(key, ['imported shared fixture helper'])
        # "Blind" is exactly the loop's condition: the literal key does not
        # occur anywhere in the table, so ``git grep -F`` cannot find it. A key
        # already spelled out in the text needs no entry.
        keys = {key: rows for key, rows in keys.items() if key not in text}
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

    def test_a_table_that_only_imports_a_helper_turns_the_guard_red(self):
        """An import-only table builds no bare key, yet a change to the helper
        still decides whether it runs. Registering it is a decision, not a pass."""
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'negative_suite.py').write_text('', encoding='utf-8')
            table = root / 'newfeat' / 'negative_controls.py'
            table.parent.mkdir()
            table.write_text(
                "from negative_suite import matrix_main\n"
                "ROOT = Path(__file__).resolve().parents[5]\n"
                "rows = [('a', ROOT / 'src/example/named.rs', 'x')]\n",
                encoding='utf-8')
            blind, unresolvable = sweep(root)
        self.assertEqual(sorted(blind), ['newfeat/negative_controls.py'])
        self.assertEqual(sorted(blind['newfeat/negative_controls.py']),
                         ['fixtures/negative_suite.py'])
        self.assertEqual(unresolvable, {})
        self.assertNotIn('newfeat/negative_controls.py', KNOWN_BLIND)

    def test_a_leading_underscore_directory_is_still_a_table(self):
        """Only ``_shared`` / ``_negkit`` are libraries; ``_private/`` is scanned."""
        with tempfile.TemporaryDirectory() as workspace:
            table = Path(workspace) / '_private' / 'negative_controls.py'
            table.parent.mkdir()
            table.write_text(
                "ROOT = Path(__file__).resolve().parents[5]\n"
                "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
                "rows = [('a', SMS / 'mod.rs', 'x')]\n",
                encoding='utf-8')
            blind, _ = sweep(Path(workspace))
        self.assertEqual(sorted(blind), ['_private/negative_controls.py'])

    def test_known_blind_entries_name_a_real_table(self):
        for rel, reason in KNOWN_BLIND.items():
            with self.subTest(table=rel):
                self.assertTrue((FIXTURES / rel).is_file(),
                                f'{rel} is gone; drop its KNOWN_BLIND entry')
                self.assertNotIn(rel, TABLES, f'{rel} is registered; drop its KNOWN_BLIND entry')
                self.assertRegex(reason, r'^(?:\d+ keys?|\d+ unresolvable)'
                                         r'(?: \+ \d+ unresolvable)?$',
                                 f'{rel}: KNOWN_BLIND needs the measured counts')

    def test_every_blind_table_really_hides_a_key(self):
        """A blind entry must name a table that actually lacks one of its keys.

        ``neg-for-diff.sh`` schedules a table by grepping for the literal key, so
        a table whose every computed key already occurs in its own text is not
        blind at all: the entry is either stale (the table was fixed — register
        it in ``TABLES``) or the register is drifting.
        """
        blind, _ = sweep()
        for rel, keys in sorted(blind.items()):
            text = (FIXTURES / rel).read_text(encoding='utf-8')
            with self.subTest(table=rel):
                self.assertTrue(
                    [key for key in keys if key not in text],
                    f'{rel}: every computed key already appears in the table text; '
                    'drop the KNOWN_BLIND entry or register anchors in TABLES')

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
            'fragment': "path = str(ROOT) + '/src/example/'\nrest = path + 'sender.rs'\n",
            'fragment-fstring': "label = 'src'\npath = f'{ROOT}/src/{label}/'\n",
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

    def test_a_display_slash_is_not_a_path(self):
        """``f'{killed}/{total} killed'`` shares the shape but builds no path."""
        synthetic = (
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [f'{killed}/{total} killed', ' / '.join(parts)]\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_comprehension_join_spellings_carry_their_keys(self):
        """``Path(SMS, name)`` / ``SMS.joinpath(name)`` key like ``SMS / name``."""
        for spelling in ('Path(SMS, name)', 'SMS.joinpath(name)', 'SMS / name'):
            with self.subTest(spelling=spelling):
                synthetic = (
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    f"rows = [{spelling} for name in ('new.rs',)]\n")
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(any('missing anchor: sms/new.rs' in problem
                                    for problem in problems), problems)

    def test_a_comprehension_over_an_unsupported_join_fails_closed(self):
        """A join spelling the resolver does not model is reported, not skipped.

        The loop variable arrives in the file name, so the directory — and the
        key — is exactly what ``os.path.join`` hides.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.path.join(SMS, name) for name in ('new.rs',)]\n")
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('unresolvable comprehension base' in problem
                            for problem in problems), problems)

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

    def test_an_annotated_rebinding_also_shadows_the_constant(self):
        """``BASE: Path = ...`` rebinds like ``BASE = ...``, not like a new name."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example/a'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def main():\n"
            "    BASE: Path = ROOT / 'src/example/b'\n"
            "    return [BASE / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous' in problem for problem in problems), problems)

    def test_an_enumeration_of_a_repository_directory_fails_closed(self):
        """``glob`` / ``rglob`` / ``iterdir`` / bare ``join`` name files the
        scanner cannot see, so they are errors rather than silent passes."""
        shapes = {
            'glob': "paths = SMS.glob('*.rs')\n",
            'rglob': "paths = (ROOT / 'src/example').rglob('*.rs')\n",
            'iterdir': "paths = SMS.iterdir()\n",
            'bare-join': "path = join(SMS, 'sender.rs')\n",
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
                self.assertTrue(any('unsupported' in problem for problem in problems),
                                problems)

    def test_a_for_loop_over_a_literal_tuple_carries_its_keys(self):
        """``for name in ('mod.rs',):`` runs with a literal, so the key is known."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def main():\n"
            "    out = []\n"
            "    for name in ('new.rs',):\n"
            "        out.append(SMS / name)\n"
            "    return out\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: sms/new.rs' in problem for problem in problems),
                        problems)

    def test_relative_and_nested_imports_resolve_to_their_files(self):
        """The import scan follows the module path the loop keys."""
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'pkg' / 'deep').mkdir(parents=True)
            (root / 'pkg' / 'deep' / 'helper.py').write_text('', encoding='utf-8')
            (root / 'pkg' / 'sibling.py').write_text('', encoding='utf-8')
            (root / '_shared').mkdir()
            (root / '_shared' / 'switch.py').write_text('', encoding='utf-8')
            tree = ast.parse(
                "from . import sibling\n"
                "from .deep.helper import run\n"
                "from .._shared import switch\n")
            keys = _shared_module_keys(tree, root, ('pkg',))
        self.assertEqual(sorted(keys),
                         ['_shared/switch.py', 'deep/helper.py', 'pkg/sibling.py'])

    def test_a_parameter_is_not_substituted_for_a_single_assignment(self):
        """A name that is also a parameter is not a module constant.

        Following the one assignment into the f-string would mark a display
        label as a repository path and fail the table for a key it never builds.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def load(name):\n"
            "    module = ROOT / 'src/example/legacy'\n"
            "    return module\n"
            "def render(module):\n"
            "    return f'imposter/{module}-package'\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

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
