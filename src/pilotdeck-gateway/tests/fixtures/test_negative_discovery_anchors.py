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
shapes, a shadowed, twice-assigned or parameter-bound constant, a runtime file
name behind ``/``, an f-string, a ``+`` or a ``joinpath`` / ``with_name`` /
``Path(base, name)`` call, a separator or file name behind a constant, a
comprehension over an unsupported
join, a display string that only looks like a path, a package initialiser whose
own imports must be keyed, and new blind, import-only or unresolvable tables
appearing outside both registers.

Scope: the four tables named by #2133 are the ones that carry anchors today;
adding a table to ``TABLES`` is the whole opt-in, and the rule then demands its
``DISCOVERY_ANCHORS`` match its own keys. Everything else that already builds
repository paths from bare file names, or imports a shared helper without
spelling its key, is listed in ``KNOWN_BLIND`` with a measured reason (the register's own
header carries the counts; every row is re-measured on each run), including
the tables that also hold shapes the resolver cannot follow at all. That register is a fix list,
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
# 253 files (the host glob can reach all of them), 190 blind only, 2 with unresolvable shapes only, 61 in both; 1284 keys: 358 bare-filename joins and 926 shared-helper import keys.
# Why a key a table needs shows up in no table text: the import line spells a
# module name, while the host keys the helper file it lives in.
IMPORTED_HELPER_REASON = 'imported shared fixture helper'

KNOWN_BLIND = {
    '_negkit/negative_compare_verdicts.py': '1 key',
    '_negkit/negative_contract.py': '4 keys',
    '_negkit/negative_submit_migration.py': '10 keys',
    '_shared/negative_control.py': '4 keys + 2 unresolvable',
    '_shared/negative_sqlite_header.py': '6 keys',
    '_shared/regeneration-negative.py': '6 keys',
    'agent-event-projector/negative.py': '5 keys',
    'anchor-marker/negative_controls.py': '3 keys',
    'api-server-channel/negative_controls.py': '6 keys + 1 unresolvable',
    'api-web/negative_controls.py': '10 keys',
    'attach-followups-a/negative_controls.py': '6 keys',
    'background-create-gate/negative_controls.py': '6 keys',
    'background-output/negative.py': '6 keys',
    'background-panic/negative_controls.py': '4 keys + 2 unresolvable',
    'background-persist-progress/negative_controls.py': '6 keys',
    'background-persist-restart/negative_controls.py': '6 keys',
    'background-subtick-gate/negative_controls.py': '8 keys',
    'baseline-provenance/negative_controls.py': '1 key',
    'channel-core/negative_controls.py': '11 keys + 2 unresolvable',
    'channel-feishu/negative_controls.py': '12 keys + 2 unresolvable',
    'channel-sms-head-timeout/negative_controls.py': '7 keys',
    'channel-unmigrated-startup/negative_controls.py': '6 keys',
    'channel-unmigrated/negative_controls.py': '7 keys + 1 unresolvable',
    'cli-parity-isolation/negative_controls.py': '1 key + 1 unresolvable',
    'cli-parity-lock-bounded/negative_controls.py': '7 keys',
    'cli-parity-mock-gateway/negative_controls.py': '1 key + 1 unresolvable',
    'closure_guard_negative.py': '3 keys',
    'commands-catalog/negative_controls.py': '4 keys',
    'config-changes/negative_coverage.py': '6 keys',
    'config-store/negative_controls.py': '6 keys',
    'connection-context/negative.py': '7 keys + 3 unresolvable',
    'cron-schedule/negative_controls.py': '6 keys',
    'cron-store/negative_controls.py': '6 keys',
    'diff-backfill/negative.py': '1 key + 1 unresolvable',
    'edgeclaw-accessors/negative.py': '5 keys',
    'edgeclaw-capture-turn/negative.py': '6 keys',
    'edgeclaw-dangling-links/negative.py': '3 keys',
    'edgeclaw-dream-run/negative.py': '1 key',
    'edgeclaw-dream-runner/negative.py': '1 key',
    'edgeclaw-dream/negative.py': '1 key',
    'edgeclaw-dream/negative_shapes.py': '2 keys',
    'edgeclaw-entries/negative.py': '1 key',
    'edgeclaw-entries/negative_fix1.py': '1 key + 1 unresolvable',
    'edgeclaw-entries/negative_ids.py': '1 key',
    'edgeclaw-entries/negative_round6.py': '6 keys',
    'edgeclaw-entries/negative_round7.py': '1 key',
    'edgeclaw-export/negative_locks.py': '2 keys',
    'edgeclaw-file-memory/negative.py': '3 keys',
    'edgeclaw-heartbeat-indexer/negative.py': '2 keys',
    'edgeclaw-heartbeat-isolation/negative.py': '10 keys',
    'edgeclaw-heartbeat/negative.py': '2 keys',
    'edgeclaw-llm-config/negative.py': '1 key',
    'edgeclaw-llm-dream/negative.py': '1 key',
    'edgeclaw-llm-envelope/negative.py': '1 key',
    'edgeclaw-llm-index/negative.py': '1 key',
    'edgeclaw-llm-recall/negative.py': '1 key',
    'edgeclaw-llm-telemetry/negative.py': '6 keys',
    'edgeclaw-llm/negative.py': '1 key',
    'edgeclaw-messages/negative.py': '2 keys',
    'edgeclaw-native-default/negative.py': '11 keys',
    'edgeclaw-provider/negative.py': '11 keys',
    'edgeclaw-real-provider/negative.py': '2 keys',
    'edgeclaw-retrieval-helpers/negative.py': '1 key',
    'edgeclaw-retriever/negative.py': '1 key',
    'edgeclaw-same-root-generation/negative.py': '12 keys',
    'edgeclaw-service-facade/negative.py': '6 keys',
    'edgeclaw-service-facade/switch_negative.py': '8 keys',
    'edgeclaw-service-orchestration/negative.py': '1 key',
    'edgeclaw-service-orchestration/switch_negative.py': '7 keys',
    'edgeclaw-text/negative.py': '4 keys',
    'exact-receipt-yaml/negative_controls.py': '1 key',
    'exact-rerun-receipt/negative_controls.py': '1 key',
    'failure-guard/negative.py': '7 keys',
    'fe-baseline-report/negative_controls.py': '2 unresolvable',
    'fe-coexist/negative_controls.py': '1 key',
    'file-history-cwd/negative_controls.py': '9 keys',
    'flake-progress/negative_controls.py': '6 keys',
    'frozen-manifest/negative_controls.py': '2 keys',
    'frozen-reinsert-guard/negative_controls.py': '8 keys',
    'gateway-batch/negative_controls.py': '2 keys',
    'grep-etxtbsy/negative_controls.py': '6 keys',
    'handle-path-macos/negative_controls.py': '6 keys',
    'hooks-output/negative.py': '1 key',
    'host-event-pump/negative_controls.py': '1 key',
    'html-timing/negative_controls.py': '2 keys + 1 unresolvable',
    'http-store-park/negative_controls.py': '6 keys + 1 unresolvable',
    'interaction-bus/negative_controls.py': '4 keys + 1 unresolvable',
    'interaction-values/negative_controls.py': '4 keys',
    'issue-1246/negative_controls.py': '3 keys',
    'issue-1437-methods/negative_controls.py': '6 keys',
    'issue-297/negative_controls.py': '7 keys',
    'js-math-zero/negative.py': '1 key',
    'ledger-diffs/negative_controls.py': '2 keys + 1 unresolvable',
    'live-e2e-tmpdir/negative_controls.py': '6 keys',
    'live-e2e/negative_controls.py': '7 keys + 7 unresolvable',
    'live-history/negative.py': '1 key',
    'live-recording/negative.py': '10 keys',
    'live-session/negative_controls.py': '6 keys',
    'locale-adaptive/negative_controls.py': '7 keys',
    'locale-contract/negative_controls.py': '6 keys',
    'locked-storage/negative_controls.py': '6 keys',
    'manager-gateway/negative_controls.py': '7 keys',
    'mcp-detached-dispatch/negative_controls.py': '6 keys',
    'mcp-gateway/negative_controls.py': '6 keys',
    'mcp-hooks-spawn/negative_controls.py': '1 key + 1 unresolvable',
    'mcp-http-corrupt-body/negative_controls.py': '6 keys',
    'mcp-http-encoding/negative.py': '6 keys',
    'memory-maintenance/negative_controls.py': '2 keys',
    'metadata-from-env/negative_controls.py': '8 keys',
    'model-catalog-cwd/negative_controls.py': '10 keys',
    'model-complete-timeout/negative_controls.py': '6 keys',
    'model-complete/negative-controls.py': '4 keys + 2 unresolvable',
    'model-factory/negative-controls.py': '1 key',
    'model-http-encoding/negative.py': '1 key',
    'module-host-approvals/negative_controls.py': '10 keys',
    'module-host/negative_controls.py': '10 keys',
    'native-archive-cwd/negative_controls.py': '7 keys',
    'native-archive-cwd/negative_proxy.py': '6 keys',
    'native-archive-cwd/negative_regeneration.py': '6 keys',
    'native-collation/negative_controls.py': '9 keys',
    'native-config/negative_controls.py': '6 keys + 1 unresolvable',
    'native-cron/negative_controls.py': '6 keys',
    'native-dispatch/negative_controls.py': '8 keys',
    'native-entry-authority/negative_controls.py': '8 keys',
    'native-http-basic/negative_controls.py': '10 keys',
    'native-loop-stimulus/negative_controls.py': '7 keys',
    'native-management/negative_controls.py': '8 keys',
    'native-metadata-cwd/negative_controls.py': '7 keys',
    'native-metadata-cwd/negative_regeneration.py': '6 keys',
    'native-process/negative_controls.py': '6 keys',
    'native-registry-switch/negative_controls.py': '7 keys',
    'native-session-e2e/negative_controls.py': '2 keys',
    'native-settings/negative/negative_controls.py': '4 keys + 1 unresolvable',
    'native-stimulus-plan/negative_controls.py': '9 keys + 1 unresolvable',
    'native-storage-stimulus/negative_controls.py': '7 keys',
    'native-topology/negative_signals.py': '3 keys',
    'native-witness/negative.py': '2 keys',
    'native-witness/negative_authentication.py': '4 keys + 1 unresolvable',
    'native-witness/negative_boundary.py': '2 keys',
    'native-ws-caps/negative_controls.py': '8 keys + 1 unresolvable',
    'native-ws-entry/negative_controls.py': '11 keys + 1 unresolvable',
    'native-ws-notify/negative_controls.py': '6 keys',
    'negative-adaptive/isolation_negative.py': '2 keys',
    'negative-adaptive/negative_controls.py': '2 keys + 1 unresolvable',
    'negative-output-directory/negative.py': '7 keys',
    'negative_oracle_guard.py': '1 key + 2 unresolvable',
    'network-timing/negative_controls.py': '6 keys',
    'network-timing/negative_private.py': '2 keys + 2 unresolvable',
    'node-io/negative_controls.py': '4 keys',
    'node-provenance/negative.py': '6 keys + 1 unresolvable',
    'operation-ledger/negative.py': '6 keys',
    'output-schema/negative.py': '3 keys',
    'p01-preflight/negative_controls.py': '6 keys',
    'parity-lock-release/negative_controls.py': '6 keys + 1 unresolvable',
    'pending-reanchor/negative_controls.py': '4 keys',
    'plugin-loader/negative-controls.py': '4 keys + 2 unresolvable',
    'plugin-runtime/negative-controls.py': '6 keys',
    'plugin-runtime/notify-window-negative.py': '4 keys',
    'plugin-runtime/panic-negative.py': '4 keys',
    'private-ca/negative_controls.py': '6 keys',
    'project-files-linux-dirent/negative_controls.py': '4 keys',
    'projector-cwd/negative_controls.py': '8 keys',
    'provider-http-boundaries/negative_controls.py': '6 keys',
    'proxy-listen-address/negative_controls.py': '7 keys',
    'pump-order-cases/negative_controls.py': '6 keys',
    'recorder/fix3-negative.py': '7 keys',
    'regeneration-masks/negative.py': '6 keys',
    'replacement/negative-518.py': '3 keys',
    'replacement/negative.py': '1 key',
    'retry-after-dialects/negative_controls.py': '6 keys',
    'run-registry/count-negative.py': '8 keys',
    'run-registry/timestamp-negative.py': '6 keys',
    'runtime-spawn-panic/helpers_negative.py': '3 keys',
    'runtime-spawn-panic/negative_controls.py': '4 keys + 2 unresolvable',
    'sdk-cli-lifecycle/negative_controls.py': '6 keys + 1 unresolvable',
    'sdk-format/negative_controls.py': '8 keys + 2 unresolvable',
    'sdk-mcp-p2/negative.py': '6 keys + 2 unresolvable',
    'sdk-run/negative_controls.py': '9 keys + 3 unresolvable',
    'sdk-sessions-list/negative_controls.py': '6 keys + 1 unresolvable',
    'sdk-sessions-messages/negative_controls.py': '10 keys + 1 unresolvable',
    'sdk-settings/negative_controls.py': '8 keys + 2 unresolvable',
    'sdk-signal-read-lease/negative_controls.py': '6 keys',
    'sdk-signals-close-order-pin/negative_controls.py': '6 keys + 1 unresolvable',
    'sdk-signals/negative_controls.py': '7 keys + 3 unresolvable',
    'sdk-terminal/negative_controls.py': '8 keys + 2 unresolvable',
    'session-abort-contract/negative.py': '6 keys + 3 unresolvable',
    'session-admission/negative.py': '4 keys',
    'session-adopt/switch_negative.py': '7 keys',
    'session-assembly-1316/negative_controls.py': '4 keys + 3 unresolvable',
    'session-domain-cleanup/negative_controls.py': '2 keys',
    'session-domain-expectations/negative_controls.py': '7 keys',
    'session-domain-lock-release/negative_controls.py': '7 keys',
    'session-domain-native/negative_controls.py': '7 keys',
    'session-domain/adapter-negative.py': '1 key + 2 unresolvable',
    'session-domain/fix1-negative.py': '1 key + 2 unresolvable',
    'session-domain/fix2-negative.py': '1 key + 2 unresolvable',
    'session-drop-release/negative_controls.py': '8 keys',
    'session-factory/append-delta/negative_controls.py': '6 keys + 8 unresolvable',
    'session-factory/negative_controls.py': '16 keys + 1 unresolvable',
    'session-factory/projection-cache/negative_controls.py': '8 keys + 6 unresolvable',
    'session-lease/negative.py': '3 keys',
    'session-owner-hooks/negative.py': '1 key',
    'session-owner/negative.py': '4 keys + 1 unresolvable',
    'session-owner/negative_close_shutdown.py': '6 keys',
    'settings-gateway/negative_controls.py': '5 keys + 1 unresolvable',
    'settings/negative_controls.py': '2 keys',
    'shared-runner-failclosed/negative_controls.py': '2 keys',
    'sidecar-payload/negative.py': '1 key + 1 unresolvable',
    'skills-external/negative_controls.py': '6 keys',
    'skills-external/negative_controls_949.py': '6 keys',
    'skills-tree-files/negative.py': '1 key + 2 unresolvable',
    'skills/negative_controls.py': '3 keys',
    'sms-stringify-depth/negative_controls.py': '4 keys + 2 unresolvable',
    'spill-isolation/negative_controls.py': '1 key',
    'stats-cwd/negative_controls.py': '9 keys',
    'stress-followups/negative_controls.py': '2 keys',
    'surrogate-collation-matrix/negative_controls.py': '7 keys',
    'task-budget-zero/negative.py': '1 key',
    'tool-budget/negative_project.py': '1 key + 1 unresolvable',
    'transcript-resolve/negative_controls.py': '1 key',
    'transcript-trim/negative_controls.py': '6 keys',
    'treelock-budget/negative_controls.py': '7 keys',
    'turn-gateway/negative-fence.py': '10 keys',
    'turn-gateway/negative.py': '6 keys',
    'turn-publish/negative.py': '5 keys',
    'turn-read/negative.py': '5 keys',
    'turn-replay/negative.py': '7 keys',
    'turn-runner/forward_negative.py': '11 keys',
    'turn-runner/input_negative.py': '9 keys',
    'turn-runner/integrity_negative.py': '10 keys',
    'turn-runner/live_negative.py': '7 keys',
    'turn-runner/negative_controls.py': '2 keys',
    'turn-runner/recording_negative.py': '3 keys',
    'turn-runner/runner_negative.py': '3 keys',
    'turn-submit-admission/negative_controls.py': '2 keys',
    'turn-submit/negative.py': '6 keys',
    'ui-server-parity-isolation/negative_controls.py': '3 unresolvable',
    'unsafe-config-boundary/negative_controls.py': '4 keys',
    'upload-gateway/negative_controls.py': '2 keys',
    'user-dialog-channel/negative.py': '8 keys + 1 unresolvable',
    'user-dialog-guard-order/negative_controls.py': '6 keys',
    'user-dialog-http-store-1555/negative_controls_1555.py': '6 keys',
    'user-dialog-http-store/negative_controls_1232.py': '6 keys',
    'user-dialog-http-store/negative_controls_1277.py': '6 keys',
    'user-dialog-lock-scope/negative_controls.py': '4 keys + 2 unresolvable',
    'web-conversion-lifecycle/negative.py': '7 keys',
    'webhook-channel/negative_controls.py': '10 keys',
    'wecom-callback/negative_controls.py': '12 keys + 2 unresolvable',
    'weixin-channel/negative_controls.py': '15 keys + 2 unresolvable',
    'workspace-pristine/negative_controls.py': '7 keys',
    'workspace-snapshot-gate/negative_controls.py': '1 key + 1 unresolvable',
    'workspace-snapshot-nonutf8/negative_controls.py': '1 key + 1 unresolvable',
    'zone-read-bound/negative_controls.py': '6 keys',
}

ROOT_NAMES = frozenset({'ROOT'})

# Exactly the basenames ``neg-for-diff.sh`` drops from its own output list
# (``grep -vE '/(_negative_common|negative_suite|negative_process|negative_panic|
# negative_selection|negative_source|negative_shared|test_[^/]*)\.py$'``). The
# sweep must skip no more than this, or a file the host is willing to run would
# never be measured here — and no less, or the register would carry files the
# host never emits. Shared helpers that the host *does* emit stay in scope: they
# are measured like any other file, and a table that imports one is owed that
# helper's key by the import scan.
HOST_FILTERED = frozenset({
    '_negative_common.py', 'negative_panic.py', 'negative_process.py',
    'negative_selection.py', 'negative_shared.py', 'negative_source.py',
    'negative_suite.py',
})


def _bind_names(target, names):
    """Add every name bound by an assignment target (including ``a, b`` / ``*a``)."""
    if isinstance(target, ast.Name):
        names.add(target.id)
    elif isinstance(target, (ast.Tuple, ast.List)):
        for element in target.elts:
            _bind_names(element, names)
    elif isinstance(target, ast.Starred):
        _bind_names(target.value, names)


# Pattern-matching bindings exist from 3.10 on; keep the names lookup total so a
# parser without them still imports (it can never produce the nodes either).
MATCH_BINDINGS = tuple(
    node for node in (getattr(ast, 'MatchAs', None), getattr(ast, 'MatchStar', None))
    if node is not None)


def _bound_names(node):
    """Every name one AST node binds, whatever the binding form.

    One dispatcher for both ``_rebound_elsewhere`` and ``_function_scoped_names``
    so the two cannot drift: a form added here is seen by both. The plain
    ``name = ...`` case is included; the caller decides whether it counts.
    """
    names = set()
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
    elif isinstance(node, ast.Assign):
        for target in node.targets:
            _bind_names(target, names)
    elif isinstance(node, ast.match_case):
        for child in ast.walk(node.pattern):
            if MATCH_BINDINGS and isinstance(child, MATCH_BINDINGS) and child.name:
                names.add(child.name)
            elif isinstance(child, getattr(ast, 'MatchMapping', ())) and child.rest:
                names.add(child.rest)
    return names


def _rebound_elsewhere(tree):
    """Names bound by any form other than a plain ``name = ...`` statement.

    A parameter, a ``for`` target, a ``with ... as``, an ``except ... as``, a
    comprehension target, a ``+=``, a walrus or a ``match`` pattern binds the
    name just as firmly as an assignment. When the same name is also assigned
    once, the *other* binding decides its value at that site, so substituting the
    assignment there keys a directory the runtime never uses — a wrong key, or a
    spurious failure.
    """
    names = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.Assign):
            names |= _bound_names(node)
    return names


def _module_level_assignments(tree):
    """The values every ``name = ...`` statement gives a module-level name.

    ``if flag: A = ...`` at module level runs at import time and rebinds the
    global just like a top-level line, so it counts. Function, lambda and class
    bodies do not: those names are local to their scope, and the module-level
    value they shadow is already handled by ``_function_scoped_names``.
    """
    values = {}

    def visit(node):
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.Lambda, ast.ClassDef)):
                continue
            if isinstance(child, ast.Assign):
                names = set()
                for target in child.targets:
                    _bind_names(target, names)
                written = ast.dump(child.value)
                for name in names:
                    values.setdefault(name, []).append(written)
            visit(child)

    visit(tree)
    return values


def _constants(tree):
    """``name = <expression>`` bindings this scanner is willing to substitute.

    Module-level bindings are authoritative (last one wins, as at runtime), but
    only when there is exactly one of them: a second ``A = ...`` — at the top
    level or inside an ``if`` — means an earlier use site saw the first value,
    so substituting either one keys a directory the code may never build and
    hides the one it does. Such a name is left out and reported at its uses.
    Tables also build paths from local aliases (``source = ROOT / '...'`` inside
    a function, then ``source.parent / 'x.rs'``); a name assigned exactly once in
    the whole file has no ambiguity, so it is resolved too. A module-level name
    that a function rebinds is *not* substituted either: the two sites would
    then mean different directories, and resolving both to the module-level
    value would demand an anchor for a key the runtime never uses while hiding
    the real one. Neither is a name bound by some other form (parameter, loop
    target, ...).
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
    for name in (set(_shadowed_names(tree)) | _duplicated_module_names(tree)) & module_level:
        constants.pop(name, None)
    return constants


def _duplicated_module_names(tree):
    """Module-level names whose ``name = ...`` statements disagree.

    Assigning the same expression twice is harmless — whichever ran, the value
    is the same — so only genuinely different values are ambiguous.
    """
    return {name for name, values in _module_level_assignments(tree).items()
            if len(set(values)) > 1}


def _shadowed_names(tree):
    """Module-level names that some other binding form also binds.

    Such a name means different directories at different sites, so the scanner
    refuses to substitute it and reports the site instead of guessing. Every
    binding form counts, not only an assignment in a nested block: a function
    parameter (``def run(BASE): ...``) hides the module constant just as well,
    and following the module value there would key a directory the runtime never
    uses while hiding the real one. ``_rebound_elsewhere`` already enumerates
    those forms at every depth, so it carries the decision. A plain second
    assignment counts too: ``A = ROOT / 'x'`` followed by ``A = ROOT / 'y'``
    makes the value at a given site depend on the order the module ran in, and
    "last one wins" would key ``y/…`` for code that built ``x/…``.
    """
    module_level = set()
    for statement in tree.body:
        if isinstance(statement, ast.Assign):
            for target in statement.targets:
                if isinstance(target, ast.Name):
                    module_level.add(target.id)
    return ((_function_scoped_names(tree) | _rebound_elsewhere(tree)
             | _duplicated_module_names(tree))
            & module_level)


def _function_scoped_names(tree):
    """Names bound anywhere below a function, lambda or class body.

    Every binding form counts, not just ``name = ...``: a ``for`` target, a
    ``with ... as``, an ``except ... as``, a comprehension target or a walrus
    rebinds the name just as well, and a module-level constant of the same name
    then means two different things depending on the call site.
    """
    names = set()

    def visit(node, scoped):
        for child in ast.iter_child_nodes(node):
            inner = scoped or isinstance(
                child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda, ast.ClassDef))
            if inner:
                names.update(_bound_names(child))
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
    return _path_literal_ending_in_file(value)


def _string_value(node, constants, seen=()):
    """The string a fragment names, following module constants to a literal.

    ``str(SMS) + SEP + 'mod.rs'`` hides the separator behind a name; the join
    is the same path build as spelling ``'/'`` inline, so the fragment test has
    to look through the constant table rather than at direct operands only.
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.Name) and node.id not in seen and node.id in constants:
        return _string_value(constants[node.id], constants, seen + (node.id,))
    return None


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


def _adds_a_runtime_file_name(node, constants, self_path):
    """Whether an ``Add`` chain appends an unknowable name to a known directory.

    ``str(ROOT) + name`` builds the same unfollowable path as ``ROOT / name``:
    the directory resolves, the file name does not, so the loop can never key
    it. Only a *directly adjacent* opaque name counts — ``str(ROOT) + '/x' +
    label`` is a label, not a path — and the left operand must really be a
    repository path, not merely a name a caller happened to pass one to, so
    ordinary string assembly (``name + ': ' + ' | '.join(lines)``) stays quiet.
    """
    operands = _add_operands(node)
    for left, right in zip(operands, operands[1:]):
        base = left
        if (isinstance(base, ast.Call) and isinstance(base.func, ast.Name)
                and base.func.id == 'str' and len(base.args) == 1):
            base = base.args[0]
        if not (_segments(base, constants, self_path) is not None
                and _is_repo_rooted(base, constants)):
            continue
        if (isinstance(right, ast.Name) and right.id not in ROOT_NAMES
                and right.id != '__file__' and right.id not in constants):
            return True
    return False


def _iterated_elements(node, constants, seen=()):
    """The literal elements a ``for name in (...)`` loop walks, or None.

    A starred element is only literal if what it unpacks is: ``(*('a.rs',),)``
    walks ``'a.rs'``, while ``(*runtime_names,)`` is unknowable and returns None
    so the caller reports the shape instead of guessing. ``seen`` stops a
    self-referential binding (``NAMES = (*NAMES,)``, dead code at runtime but
    parsed all the same) from recursing forever.
    """
    target = node
    if isinstance(node, ast.Name) and node.id in constants:
        if node.id in seen:
            return None
        target = constants[node.id]
        seen = seen + (node.id,)
    if isinstance(target, (ast.Tuple, ast.List)):
        elements = []
        for element in target.elts:
            if isinstance(element, ast.Starred):
                unpacked = _iterated_elements(element.value, constants, seen)
                if unpacked is None:
                    return None
                elements.extend(unpacked)
            else:
                elements.append(element)
        return elements
    return None


def _literal_combinations(generators, constants):
    """Every binding a comprehension's literal sources can produce, or None.

    ``[SMS / name for name in ('a.rs', 'b.rs')]`` really runs twice, once per
    element, so the key is known for each. A comprehension with several
    ``for`` clauses is the same loop nested, and each clause contributes its own
    literal elements: the bindings multiply. As soon as one source is not a
    literal tuple the scanner cannot enumerate it and returns None, so the caller
    reports the shape instead of keying a guess.
    """
    combos = [{}]
    for generator in generators:
        if not isinstance(generator.target, ast.Name):
            return None
        elements = _iterated_elements(generator.iter, constants)
        if elements is None:
            return None
        expanded = []
        for combo in combos:
            for element in elements:
                bound = dict(combo)
                bound[generator.target.id] = element
                expanded.append(bound)
        combos = expanded
    return combos


def _add_operands(node):
    """Every operand of a left-associated ``a + b + c`` chain."""
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return _add_operands(node.left) + [node.right]
    return [node]


def _is_repo_rooted(node, constants, seen=(), shadowed=()):
    """Whether an expression is built from the repository root or ``__file__``.

    Tables also concatenate bare file names onto *runtime* directories (the
    evidence directory, a temp dir). Those are not repository paths and the
    discovery loop can never key on them, so they are not required to carry an
    anchor. An expression that does start at ``ROOT`` / ``__file__`` but that
    this scanner cannot evaluate is a real hole and is reported.

    ``shadowed`` names are not substituted: a module-level constant rebound
    somewhere else (in a function, a loop target, a ``with``/``except`` binding,
    a parameter, a walrus or a match pattern) means different values at
    different sites, and following it would report a directory the code never
    uses.
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


def _repo_returning_functions(tree, constants):
    """In-file functions that can hand back a repository path.

    ``def source_dir(): return ROOT / 'src/x'`` then ``source_dir() / 'mod.rs'``
    keeps the directory one call away. The join itself carries a bare file name
    that the discovery loop needs, so hiding the base behind such a call must
    not read as "not a repository path" (that shape was silent: no key and no
    problem, so an unregistered table passed).
    """
    names = set()
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        for child in ast.walk(node):
            if (isinstance(child, ast.Return) and child.value is not None
                    and _is_repo_rooted(child.value, constants)):
                names.add(node.name)
                break
    return names


def _repository_parameters(tree, constants, returns=()):
    """Parameter names of in-file functions called with a repository path.

    ``def build(base): return base / 'mod.rs'`` loses the directory at the
    parameter, but the call site (``build(SMS)``) still names it. Only parameters
    of functions that are really called that way are tainted, so the ordinary
    ``def dump(out_dir)`` over an evidence directory stays silent.
    """
    returns = set(returns)
    definitions = {}
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            definitions.setdefault(node.name, []).append(node)

    def from_repository(argument):
        return _is_repo_rooted(argument, constants) or (
            isinstance(argument, ast.Call) and isinstance(argument.func, ast.Name)
            and argument.func.id in returns)

    # Match argument to parameter position: tainting every parameter of a callee
    # just because *one* argument is repository-rooted would also taint ``out``
    # in ``save(out, name, restored)`` and turn every ``out / 'summary.json'``
    # in the tree red.
    tainted = set()
    for node in ast.walk(tree):
        if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)):
            continue
        for definition in definitions.get(node.func.id, ()):
            parameters = (list(definition.args.posonlyargs)
                          + list(definition.args.args))
            for parameter, argument in zip(parameters, node.args):
                if from_repository(argument):
                    tainted.add(parameter.arg)
            for keyword in node.keywords:
                if keyword.arg is not None and from_repository(keyword.value):
                    tainted.add(keyword.arg)
    return tainted


def _carries_repository_path(node, tainted, returns):
    """Whether ``node`` mentions a tainted parameter or a repo-returning call."""
    for child in ast.walk(node):
        if isinstance(child, ast.Name) and child.id in tainted:
            return True
        if (isinstance(child, ast.Call) and isinstance(child.func, ast.Name)
                and child.func.id in returns):
            return True
    return False


PATH_CALL_ATTRS = frozenset({
    'joinpath', 'with_name', 'with_suffix', 'resolve', 'absolute', 'expanduser',
})

# Calls that list a directory's contents: what they yield is a runtime value, so
# a repository path passed to one is a key the loop can never derive from text.
ENUMERATION_ATTRS = frozenset({
    'glob', 'rglob', 'iterdir', 'walk', 'listdir', 'scandir',
})
ENUMERATION_NAMES = frozenset({'walk', 'listdir', 'scandir'})
# Stdlib modules whose enumeration helpers take the *directory* as an argument
# (``os.walk(SMS)``) rather than as the receiver (``SMS.glob('*.rs')``). Only
# these get the argument test: ``ast.walk(ast.parse(source))`` is also an
# ``.walk`` whose receiver is not a path, and its argument is file *content*
# that merely happens to be built from a repository constant.
FS_MODULES = frozenset({'os', 'os.path', 'glob', 'shutil', 'pathlib'})


def _module_aliases(tree):
    """Map every name an import statement binds to the module path it names."""
    aliases = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                bound = alias.asname or alias.name.split('.')[0]
                aliases[bound] = alias.name
        elif isinstance(node, ast.ImportFrom) and node.module and not node.level:
            for alias in node.names:
                if alias.name != '*':
                    aliases[alias.asname or alias.name] = f'{node.module}.{alias.name}'
    return aliases


def _enumerates_a_directory(func, aliases):
    """Whether a bare call name is one of the directory-listing functions.

    ``from os import walk`` reaches it by plain name, and ``from os import walk
    as w`` reaches the same stdlib function through the import alias — the
    alias map carries the qualified target, so both spellings are one check.
    """
    if not isinstance(func, ast.Name):
        return False
    if func.id in ENUMERATION_NAMES:
        return True
    qualified = aliases.get(func.id)
    if not qualified:
        return False
    module, _, name = qualified.rpartition('.')
    return module in FS_MODULES and name in ENUMERATION_NAMES


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


def _is_path_expression(node):
    """Whether ``node`` is *spelled* as a path build, resolvable or not.

    ``_is_path_shape`` answers a narrower question — "does this end in a bare
    file name, so it must be keyed" — and therefore says no to ``BASE / name``
    while ``name`` is still an opaque loop variable. For a comprehension over a
    non-literal source that is exactly the shape that must be reported: the loop
    variable is runtime data, so neither the key nor its absence is decidable.
    Container elements are searched so ``(BASE / name, label)`` is not missed;
    a tuple that merely *contains* a repository constant (``(n, source, s, r)``
    fed to a printer) is not a path build and stays silent.
    """
    if isinstance(node, (ast.Tuple, ast.List, ast.Set)):
        return any(_is_path_expression(child) for child in node.elts)
    if isinstance(node, ast.Dict):
        return any(_is_path_expression(child)
                   for child in list(node.keys) + list(node.values)
                   if child is not None)
    if isinstance(node, ast.BinOp):
        return isinstance(node.op, ast.Div) or _joins_a_path_string(node)
    if isinstance(node, ast.JoinedStr):
        return True
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name):
            return func.id in ('Path', 'join')
        if isinstance(func, ast.Attribute):
            return func.attr in PATH_CALL_ATTRS or func.attr in (
                'join', 'format', 'format_map')
    return False


def _module_file(parts, root):
    """The file one module path names under ``root``, or None if there is none."""
    if not parts:
        return None
    path = root.joinpath(*parts).with_suffix('.py')
    return path if path.is_file() else None


def _module_key(parts, root):
    """Discovery key for one imported module path, or nothing if it is not here.

    The loop keys a changed file as ``basename(dirname)/basename`` relative to
    the repository root, so a top-level fixture keys as ``fixtures/x.py`` and a
    library module as ``_shared/x.py``. Resolving the module against ``root``
    keeps an unrelated import (``os``, ``json``) from demanding an anchor for a
    file this repository does not have, and covers every local helper rather
    than a hand-maintained list.
    """
    path = _module_file(parts, root)
    if path is None:
        return set()
    rel = path.relative_to(root).as_posix()
    if len(parts) == 1:
        return {'fixtures/' + rel}
    return {'/'.join(parts[-2:]) + '.py'}


def _module_candidates(tree, search_dirs):
    """Every module path an import statement can name, with its search order.

    ``search_dirs`` are the directories Python would try, in order: the running
    script's directory comes first (the host runs ``python3 <table>``), then the
    fixtures root. Relative imports resolve against the importing file's own
    directory, which is what the caller passes for a module it recursed into.

    ``importlib.import_module('x')`` / ``__import__('x')`` count too when the
    argument is a string literal: the dependency is exactly as real as an
    ``import`` line. A *computed* module name stays out of reach — no static
    scanner can know it — and is listed as a known boundary in the README.
    """
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                parts = tuple(alias.name.split('.'))
                for directory in search_dirs:
                    yield tuple(directory) + parts
        elif isinstance(node, ast.Call):
            func = node.func
            dynamic = (
                (isinstance(func, ast.Attribute) and func.attr == 'import_module')
                or (isinstance(func, ast.Name) and func.id == '__import__'))
            if not dynamic or not node.args:
                continue
            argument = node.args[0]
            if not (isinstance(argument, ast.Constant)
                    and isinstance(argument.value, str)):
                continue
            parts = tuple(argument.value.split('.'))
            for directory in search_dirs:
                yield tuple(directory) + parts
        elif isinstance(node, ast.ImportFrom):
            if node.level:
                base = list(search_dirs[0])
                if node.level > 1:
                    base = base[:max(0, len(base) - (node.level - 1))]
                module = tuple(base) + (tuple(node.module.split('.')) if node.module else ())
                yield module
                for alias in node.names:
                    yield module + (alias.name,)
            elif node.module:
                parts = tuple(node.module.split('.'))
                # An absolute import is resolved against the running script's
                # directory first: ``from helper import x`` finds
                # ``<table dir>/helper.py`` before ``<fixtures>/helper.py``.
                for directory in search_dirs:
                    candidate = tuple(directory) + parts
                    yield candidate
                    for alias in node.names:
                        yield candidate + (alias.name,)


def _package_prefixes(candidate, search_dirs, root):
    """The parent packages a dotted import runs before the module itself.

    ``import a.b.c`` executes ``a/__init__.py`` and ``a/b/__init__.py`` first, so
    a helper either of them imports is a real dependency of the table — and the
    loop keys a changed ``a/__init__.py`` on its own. Only prefixes that really
    are packages (a directory with ``__init__.py``) count: yielding every prefix
    would parse a same-named plain module that the import never touches.
    """
    for directory in search_dirs:
        directory = tuple(directory)
        if candidate[:len(directory)] != directory:
            continue
        parts = candidate[len(directory):]
        for depth in range(1, len(parts)):
            prefix = directory + parts[:depth]
            if _module_file(prefix + ('__init__',), root) is not None:
                yield prefix
        return


def _shared_module_keys(tree, root=FIXTURES, table_dir=()):
    """Discovery keys for every local module the table imports, transitively.

    The discovery loop keys a changed ``fixtures/negative_suite.py`` as
    ``fixtures/negative_suite.py`` and a changed ``fixtures/_shared/x.py`` as
    ``_shared/x.py``; a table that imports the module is not scheduled for the
    change unless the same literal occurs in its own text. Declaring it in
    ``DISCOVERY_ANCHORS`` is what puts it there. ``from _shared import x`` names
    no module path at all, so each imported name is expanded as well; relative
    imports resolve against the table's own directory.

    The walk is transitive: ``negative_suite`` imports ``negative_panic``, which
    imports ``negative_scope``, and the loop keys each changed helper on its own
    — a table is scheduled for any of them only if the literal is in its text.
    Stopping at the direct imports left that whole second level silently
    unscheduled, the same false pass one hop further out. Each file is visited
    once, so import cycles terminate.
    """
    keys = set()
    seen = set()
    queue = [(tree, (tuple(table_dir), ()))]
    while queue:
        current, search_dirs = queue.pop()
        for candidate in _module_candidates(current, search_dirs):
            chain = list(_package_prefixes(candidate, search_dirs, root))
            chain.append(candidate)
            for parts in chain:
                # Importing a package also runs its ``__init__.py``, and the loop
                # keys a changed ``pkg/__init__.py`` as ``pkg/__init__.py``. The
                # file is parsed like any other module: what *it* imports is a
                # dependency of the table too, and skipping that hop left the
                # imports made from a package initialiser invisible.
                for keyed in (parts, parts + ('__init__',)):
                    keys.update(_module_key(keyed, root))
                path = _module_file(parts, root)
                if path is None:
                    path = _module_file(parts + ('__init__',), root)
                if path is None or path in seen:
                    continue
                seen.add(path)
                module_dir = path.relative_to(root).parts[:-1]
                queue.append((ast.parse(path.read_text(encoding='utf-8')),
                              (module_dir, tuple(table_dir), ())))
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
    aliases = _module_aliases(tree)
    repository_returns = _repo_returning_functions(tree, constants)
    tainted = _repository_parameters(tree, constants, repository_returns)
    keys = {}
    problems = []

    def repo_rooted(node):
        # A directory that only reaches the join through a parameter or a call
        # return is still a repository path: the loop cannot key it from the
        # text either, so it has to be reported like any other unsupported base.
        return (_is_repo_rooted(node, constants, shadowed=shadowed)
                or _carries_repository_path(node, tainted, repository_returns))

    def record(segments, expression):
        # A single component is not a discovery key: the loop always keys
        # ``dir/file``. Every caller reports it here rather than at its own
        # call site, so a comprehension and a ``/`` join agree.
        if len(segments) < 2:
            problems.append(f'no discovery key (needs a directory and a file name): {expression}')
            return
        keys.setdefault('/'.join(segments[-2:]), []).append(expression)

    reported_ambiguous = set()

    def ambiguous_names(node):
        return sorted({child.id for child in ast.walk(node)
                       if isinstance(child, ast.Name) and child.id in shadowed})

    def report_ambiguous(name, expression):
        # One report per name: the same constant used in ten rows is one
        # decision, and a wall of identical lines buries the real finding.
        if name in reported_ambiguous:
            return
        reported_ambiguous.add(name)
        problems.append('ambiguous module-level name rebound elsewhere: '
                        f'{name}: {expression}')

    def report_unresolvable(expression):
        problems.append('unresolvable repository path (this scanner cannot '
                        f'follow it): {expression}')

    def resolve(node, expression):
        segments = _segments(node, constants, self_path)
        if segments is None:
            ambiguous = ambiguous_names(node)
            if ambiguous:
                report_ambiguous(ambiguous[0], expression)
            elif repo_rooted(node):
                report_unresolvable(expression)
            return
        if not _bare_tail(node, constants):
            return
        record(segments, expression)

    def report_runtime_tail(node):
        """Report a join whose directory is known but whose file name is not.

        ``SMS / name`` — with ``name`` a parameter, a runtime loop variable or a
        comprehension source this scanner cannot enumerate — is the same hole as
        ``SMS.glob('*.rs')``: the loop needs a ``dir/file`` literal the text does
        not contain. Only the outermost join of a chain reports (an inner opaque
        prefix is consumed by the join above it), and only when the directory
        half resolves, so ``ROOT / PICK['dir'] / 'x.rs'`` still reports once, at
        the join that is actually opaque.
        """
        if _segments(node.left, constants, self_path) is None:
            return
        if _segments(node.right, constants, self_path) is not None:
            return
        if not repo_rooted(node):
            return
        ambiguous = ambiguous_names(node)
        if ambiguous:
            report_ambiguous(ambiguous[0], ast.unparse(node))
        else:
            report_unresolvable(ast.unparse(node))

    def report_runtime_call_tail(node):
        """Report the call spellings of ``SMS / name``.

        ``SMS.joinpath(name)``, ``SMS.with_name(name)`` and ``Path(SMS, name)``
        join a repository directory to a file name only the runtime knows, just
        like the ``/`` form that has reported since r9. Left silent, a table
        written in this style is neither keyed nor flagged, so a new table using
        it would never be registered — the same false pass this guard exists to
        prevent. Only the outermost call reports, and only when the directory
        resolves and the file-name argument does not.
        """
        func = node.func
        if isinstance(func, ast.Attribute) and func.attr in ('joinpath', 'with_name'):
            base = func.value
        elif (isinstance(func, ast.Name) and func.id == 'Path'
                and len(node.args) >= 2 and not node.keywords):
            base = node.args[0]
        else:
            return
        if not node.args:
            return
        tail = node.args[-1]
        if _segments(tail, constants, self_path) is not None:
            return
        if not (_is_repo_rooted(base, constants, shadowed=shadowed)
                or _carries_repository_path(node, tainted, repository_returns)):
            return
        ambiguous = ambiguous_names(node)
        if ambiguous:
            report_ambiguous(ambiguous[0], ast.unparse(node))
        else:
            report_unresolvable(ast.unparse(node))

    class Visitor(ast.NodeVisitor):
        def visit_BinOp(self, node):
            if isinstance(node.op, ast.Add) and repo_rooted(node):
                separators = [
                    operand for operand in _add_operands(node)
                    if (value := _string_value(operand, constants)) is not None
                    and '/' in value]
                if (_joins_a_path_string(node) or separators
                        or _adds_a_runtime_file_name(node, constants, self_path)):
                    # ``str(ROOT) + '/src/x/mod.rs'`` is statically resolvable in
                    # principle but not by this scanner; keeping it silent would
                    # be the same fail-open shape the gate exists to catch. The
                    # same goes for a chain that only ever adds separators and
                    # bare components (``str(ROOT) + '/' + 'src' + …``): each
                    # fragment looks harmless on its own. And ``str(ROOT) + name``
                    # hides the file name behind a runtime value, so the join is
                    # no more followable than ``SMS / name``.
                    problems.append('unsupported string concatenation of a repository '
                                    f'path (use ``/`` joins): {ast.unparse(node)}')
                    # Report the whole chain once. Recursing would report the
                    # same concatenation again for every prefix of it.
                    for operand in _add_operands(node):
                        self.visit(operand)
                    return
            elif isinstance(node.op, ast.Div):
                if _is_path_shape(node, constants):
                    resolve(node, ast.unparse(node))
                else:
                    report_runtime_tail(node)
            elif _is_path_shape(node, constants):
                resolve(node, ast.unparse(node))
            self.generic_visit(node)

        def visit_JoinedStr(self, node):
            joined = _joins_a_path_string(node)
            if not joined:
                # ``f'{SMS}/{name}'`` names its directory but not its file, the
                # same unfollowable join as ``SMS / name``: a repository
                # directory interpolated immediately before a ``/``. A display
                # counter (``f'{killed}/{total} killed'``) interpolates no
                # repository path, so it is not a path build.
                for current, following in zip(node.values, node.values[1:]):
                    if not isinstance(current, ast.FormattedValue):
                        continue
                    target = current.value
                    if not (_segments(target, constants, self_path) is not None
                            and _is_repo_rooted(target, constants)):
                        continue
                    if (isinstance(following, ast.Constant)
                            and isinstance(following.value, str)
                            and '/' in following.value):
                        joined = True
                        break
            if joined and repo_rooted(node):
                problems.append('unsupported f-string path of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            self.generic_visit(node)

        def visit_Call(self, node):
            func = node.func
            enumerates_repository = (
                (func.attr in ENUMERATION_ATTRS
                 and (repo_rooted(func.value)
                      or (isinstance(func.value, ast.Name)
                          and aliases.get(func.value.id) in FS_MODULES
                          and any(repo_rooted(argument) for argument in node.args))))
                if isinstance(func, ast.Attribute) else False)
            if enumerates_repository:
                # Enumerating a repository directory yields file names only the
                # runtime knows; the loop can never key them from the text. Both
                # spellings count: ``SMS.glob('*.rs')`` on the directory itself
                # and ``os.walk(SMS)`` / ``os.listdir(SMS)`` taking it as an
                # argument — the stdlib ones are the more common shape.
                problems.append('unsupported enumeration of a repository path '
                                f'(this scanner cannot follow it): {ast.unparse(node)}')
            elif (_enumerates_a_directory(func, aliases)
                    and any(repo_rooted(argument) for argument in node.args)):
                # ``from os import walk`` / ``from os import listdir`` reaches the
                # same unmodelled call, and ``from os import walk as w`` reaches
                # it through the alias.
                problems.append('unsupported enumeration of a repository path '
                                f'(this scanner cannot follow it): {ast.unparse(node)}')
            elif (isinstance(func, ast.Name) and func.id == 'join'
                    and any(repo_rooted(argument) for argument in node.args)):
                # ``from os.path import join`` reaches the same unmodelled call.
                problems.append('unsupported path join of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            if _is_path_shape(node, constants):
                resolve(node, ast.unparse(node))
            else:
                # ``SMS.joinpath(name)`` / ``Path(SMS, name)``: the directory
                # half resolves and the file name does not — the call spelling
                # of ``SMS / name``.
                report_runtime_call_tail(node)
            self.generic_visit(node)

        def visit_For(self, node):
            """Walk ``for name in ('mod.rs',):`` once per literal element.

            The loop really runs the body with each literal bound, so a path
            built from the target (``SMS / name``) is statically known. Walking
            it once with an opaque name would leave the key invisible. The
            iterable itself is walked as well: ``for p in (BASE / 'new.rs',):``
            builds a real path before the body ever runs.
            """
            self.visit(node.iter)
            elements = None
            if isinstance(node.target, ast.Name):
                elements = _iterated_elements(node.iter, constants)
            if elements is None:
                # The target really is bound to a runtime value here, so a stale
                # constant of the same name (for instance an outer literal loop
                # that bound it) must not be substituted inside the body: the
                # key it would produce is one the code never builds.
                # Every name the target binds, not just a bare ``Name``: a
                # tuple target binds its pieces the same way, and a piece that
                # shares a name with a module constant must not be substituted
                # with that constant inside the body.
                bound = set()
                _bind_names(node.target, bound)
                missing = object()
                stale = {name: constants.pop(name, missing) for name in bound}
                try:
                    for statement in node.body:
                        self.visit(statement)
                    for statement in node.orelse:
                        self.visit(statement)
                finally:
                    for name, value in stale.items():
                        if value is not missing:
                            constants[name] = value
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

        visit_AsyncFor = visit_For

        def visit_ListComp(self, node):
            self._comprehension(node, [node.elt])

        def visit_SetComp(self, node):
            self._comprehension(node, [node.elt])

        def visit_GeneratorExp(self, node):
            self._comprehension(node, [node.elt])

        def visit_DictComp(self, node):
            self._comprehension(node, [node.key, node.value])

        def _comprehension(self, node, parts):
            """Bind the loop variables and visit the element in each binding.

            A comprehension is a nested loop whose elements are evaluated with
            the targets bound; walking the element under every literal binding
            keys ``[SMS / name for name in ('a.rs',)]`` exactly like the plain
            loop. That also covers a tuple element
            (``[(BASE / 'new.rs', x) for x in ('x',)]``), a dict key/value, and
            several ``for`` clauses, all of which used to be skipped whole. When
            a source is not literal the bindings are unknown: if an element
            still builds a repository path and no key came out of it, the shape
            is reported.
            """
            generators = node.generators
            combos = _literal_combinations(generators, constants)
            if combos is None:
                for part in parts:
                    recorded = len(keys)
                    diagnosed = len(problems)
                    self.visit(part)
                    if (len(keys) == recorded and len(problems) == diagnosed
                            and repo_rooted(part) and _is_path_expression(part)):
                        problems.append(
                            f'unresolvable comprehension source: {ast.unparse(node)}')
            else:
                missing = object()
                for combo in combos:
                    saved = {name: constants.get(name, missing) for name in combo}
                    constants.update(combo)
                    try:
                        for part in parts:
                            self.visit(part)
                    finally:
                        for name, value in saved.items():
                            if value is missing:
                                constants.pop(name, None)
                            else:
                                constants[name] = value
            # Never drop the iteration sources or the ``if`` clauses: a shape
            # this scanner cannot follow can hide there just as well as in the
            # element, and skipping them was a silent hole.
            for generator in generators:
                self.visit(generator.iter)
                for condition in generator.ifs:
                    self.visit(condition)

    Visitor().visit(tree)
    return keys, list(dict.fromkeys(problems))


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
        required.setdefault(key, [IMPORTED_HELPER_REASON])
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
        if path.name in HOST_FILTERED or path.name.startswith('test_'):
            # The two exclusions the host applies to its own output list, no
            # more: a file skipped here is never emitted as a table there
            # either, so the sweep never has to measure something the host
            # cannot run.
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
            keys.setdefault(key, [IMPORTED_HELPER_REASON])
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

    def test_the_register_summary_line_matches_the_scan(self):
        """The one-line tally above KNOWN_BLIND is a claim like any other row.

        Per-table counts are checked above; the summary sentence is the number
        reviewers and follow-up issues quote, and it drifted once already (a
        regeneration rewrote the block but not the sentence). Re-measure it.
        """
        blind, unresolvable = sweep()
        measured = set(blind) | set(unresolvable)
        total_keys = sum(len(keys) for keys in blind.values())
        import_keys = sum(1 for keys in blind.values() for rows in keys.values()
                          if rows == [IMPORTED_HELPER_REASON])
        expected = (
            f'# {len(measured)} files (the host glob can reach all of them), '
            f'{sum(1 for rel in measured if blind.get(rel) and not unresolvable.get(rel))} blind only, '
            f'{sum(1 for rel in measured if unresolvable.get(rel) and not blind.get(rel))} '
            f'with unresolvable shapes only, '
            f'{sum(1 for rel in measured if blind.get(rel) and unresolvable.get(rel))} in both; '
            f'{total_keys} keys: {total_keys - import_keys} bare-filename joins and '
            f'{import_keys} shared-helper import keys.')
        source = (FIXTURES / 'test_negative_discovery_anchors.py').read_text(encoding='utf-8')
        line = next((row for row in source.splitlines()
                     if row.startswith('# ') and 'files (the host glob can reach all of them)' in row),
                    None)
        self.assertEqual(line, expected,
                         'the summary line above KNOWN_BLIND is stale; re-run the sweep and '
                         'rewrite it (see the r6 regeneration script in _tmp)')

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

    def test_a_repository_path_passed_through_a_parameter_fails_closed(self):
        """``def build(base): base / 'mod.rs'`` is still a repository path.

        The directory only reaches the join as an argument, so the scanner
        cannot name the key — that must be reported, not skipped: a table that
        hides its base this way used to be green while the discovery loop could
        never schedule it.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def build(base):\n"
            "    return [('a', base / 'mod.rs', 'x')]\n"
            "rows = build(SMS)\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('unresolvable repository path' in problem
                            for problem in problems), problems)

    def test_a_repository_path_returned_from_a_function_fails_closed(self):
        """``source_dir() / 'mod.rs'`` hides the directory one call deep."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def source_dir():\n"
            "    return ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
            "rows = [('a', source_dir() / 'mod.rs', 'x')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('unresolvable repository path' in problem
                            for problem in problems), problems)

    def test_an_evidence_directory_parameter_stays_silent(self):
        """A helper over the output directory is not a repository path.

        Only parameters of functions actually called with a repository path are
        tainted; otherwise every ``def dump(out_dir)`` in the tree would turn
        red and the register would lose its signal.
        """
        synthetic = (
            "import sys\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "out = Path(sys.argv[1])\n"
            "def dump(out_dir):\n"
            "    return (out_dir / 'summary.json').write_text('{}')\n"
            "dump(out)\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

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
        self.assertTrue(any('unresolvable repository path' in problem
                            for problem in problems), problems)

    def test_a_comprehension_element_is_walked_whatever_its_shape(self):
        """The element sits inside a tuple, and several ``for`` clauses nest."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [(BASE / 'new.rs', x) for x in ('x',)]\n"
            "more = [BASE / name for x in ('x',) for name in ('new.rs',)]\n"
            "table = {BASE / name: 1 for name in ('new.rs',)}\n"
            "gen = (BASE / name for name in ('new.rs',))\n"
            "sett = {BASE / name for name in ('new.rs',)}\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: example/new.rs' in problem
                            for problem in problems), problems)

    def test_a_comprehension_over_a_runtime_source_fails_closed(self):
        """The file name comes from a call, so no key is derivable from text."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def names():\n"
            "    return ('new.rs',)\n"
            "rows = [BASE / name for name in names()]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        # The join itself is now named (``BASE / name``); the source-level
        # message only stands in when the element yields no diagnosis of its own.
        self.assertTrue(any('unresolvable' in problem for problem in problems), problems)

    def test_a_comprehension_element_that_is_not_a_path_stays_silent(self):
        """A tuple fed to a printer is not a path build, even if it holds one."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SOURCE = ROOT / 'src/example/mod.rs'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def show(mutations):\n"
            "    print([(n, SOURCE) for n, s in mutations])\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_starred_or_constant_comprehension_source_is_expanded(self):
        """``(*('new.rs',),)`` and ``(NAME,)`` are both literal enough."""
        for source in ("(*('new.rs',),)", "(NAME,)"):
            with self.subTest(source=source):
                synthetic = (
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "BASE = ROOT / 'src/example'\n"
                    "NAME = 'new.rs'\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    f"rows = [BASE / name for name in {source}]\n")
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(any('missing anchor: example/new.rs' in problem
                                    for problem in problems), problems)

    def test_a_for_loop_walks_its_iterable_too(self):
        """``for p in (BASE / 'new.rs',):`` builds the path before the body."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def main():\n"
            "    for path in (BASE / 'new.rs',):\n"
            "        print(path)\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: example/new.rs' in problem
                            for problem in problems), problems)

    def test_a_separator_only_string_chain_fails_closed(self):
        """``str(ROOT) + '/' + 'src' + …`` is a path split beyond recognition."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "path = str(ROOT) + '/' + 'src' + '/' + 'example' + '/' + 'new.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        reports = [problem for problem in problems
                   if 'unsupported string concatenation' in problem]
        self.assertEqual(len(reports), 1, problems)

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

    def test_stdlib_enumeration_taking_the_directory_as_an_argument(self):
        """``os.walk(SMS)`` and friends enumerate a repository directory too.

        The directory is the argument here, not the receiver, so the receiver
        test alone missed the most common spelling.
        """
        shapes = {
            'os.walk': "import os\npaths = os.walk(SMS)\n",
            'os.listdir': "import os\nnames = os.listdir(SMS)\n",
            'os.scandir': "import os\nentries = list(os.scandir(SMS))\n",
            'aliased': "import os as filesystem\npaths = filesystem.listdir(SMS)\n",
            'bare-walk': "from os import walk\npaths = walk(SMS)\n",
            'bare-listdir': "from os import listdir\nnames = listdir(SMS)\n",
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
                self.assertTrue(any('unsupported enumeration' in problem
                                    for problem in problems), problems)

    def test_a_non_path_walk_over_file_contents_stays_silent(self):
        """``ast.walk(ast.parse(source))`` is not a directory enumeration.

        ``source`` is file *content* that happens to be built from a repository
        constant. Treating every ``.walk`` whose argument touches ``ROOT`` as a
        directory scan would report tables that have no hole at all.
        """
        synthetic = (
            "import ast\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def main():\n"
            "    source = (ROOT / 'src/example').read_text()\n"
            "    return [node for node in ast.walk(ast.parse(source))]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertFalse(any('enumeration' in problem for problem in problems), problems)

    def test_a_runtime_loop_does_not_reuse_a_stale_literal_binding(self):
        """An inner ``for name in names()`` must not key the outer literal name."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/example/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def main():\n"
            "    out = []\n"
            "    for name in ('outer.rs',):\n"
            "        for name in names():\n"
            "            out.append(SMS / name)\n"
            "    return out\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertFalse(any('sms/outer.rs' in problem for problem in problems), problems)

    def test_a_match_pattern_binds_a_name_like_an_assignment(self):
        """``case {'dir': BASE}`` rebinds ``BASE`` at that site, as ``=`` does."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example/a'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def main(value):\n"
            "    match value:\n"
            "        case {'dir': BASE}:\n"
            "            return [BASE / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous' in problem for problem in problems), problems)

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

    def test_a_sibling_module_of_the_table_keys_like_the_script_dir(self):
        """``from helper import x`` finds ``<table dir>/helper.py`` at runtime.

        The host runs ``python3 <table>``, so the script's directory is on
        ``sys.path`` before the fixtures root. Keying only ``fixtures/helper.py``
        missed a change to ``example/helper.py`` — the file the table really
        imports — and demanded an anchor for one it does not.
        """
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'example').mkdir()
            (root / 'example' / 'helper.py').write_text('', encoding='utf-8')
            (root / 'example' / 'pkg').mkdir()
            (root / 'example' / 'pkg' / '__init__.py').write_text('', encoding='utf-8')
            tree = ast.parse(
                "from helper import run\n"
                "import pkg\n")
            keys = _shared_module_keys(tree, root, ('example',))
        self.assertEqual(sorted(keys),
                         ['example/helper.py', 'pkg/__init__.py'])

    def test_imports_are_followed_transitively(self):
        """A helper's own imports are keys of the table that imports it.

        ``negative_suite`` imports ``negative_panic``, which imports
        ``negative_scope``; a change to either keys a helper the table's text
        never spells, so stopping at the direct imports left it unscheduled.
        """
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'suite.py').write_text(
                'from panic import run\n', encoding='utf-8')
            (root / 'panic.py').write_text(
                'from _shared import scope\n', encoding='utf-8')
            (root / '_shared').mkdir()
            (root / '_shared' / 'scope.py').write_text('', encoding='utf-8')
            tree = ast.parse('from suite import run\n')
            keys = _shared_module_keys(tree, root, ())
        self.assertEqual(sorted(keys), ['_shared/scope.py', 'fixtures/panic.py',
                                        'fixtures/suite.py'])

    def test_a_literal_dynamic_import_is_followed(self):
        """``import_module('x')`` names the same helper as ``import x``.

        Tables reach for it to import a helper by name at runtime; the module
        path is still a literal, so the key is known.
        """
        shapes = {
            'importlib': "import importlib\nhelper = importlib.import_module('_shared.scope')\n",
            'dunder': "helper = __import__('_shared.scope')\n",
        }
        for label, line in shapes.items():
            with self.subTest(shape=label):
                with tempfile.TemporaryDirectory() as workspace:
                    root = Path(workspace)
                    (root / '_shared').mkdir()
                    (root / '_shared' / 'scope.py').write_text('', encoding='utf-8')
                    keys = _shared_module_keys(ast.parse(line), root, ())
                self.assertEqual(sorted(keys), ['_shared/scope.py'])

    def test_an_import_cycle_terminates(self):
        """Two helpers importing each other must not loop the walk."""
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'a.py').write_text('import b\n', encoding='utf-8')
            (root / 'b.py').write_text('import a\n', encoding='utf-8')
            tree = ast.parse('import a\n')
            keys = _shared_module_keys(tree, root, ())
        self.assertEqual(sorted(keys), ['fixtures/a.py', 'fixtures/b.py'])

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
        """A shared fixture helper is discovered by its own path literal.

        The walk is transitive, so importing ``negative_suite`` also owes the
        helpers *it* imports (``negative_panic`` → ``negative_scope`` …): the
        loop keys each changed helper on its own and the table text spells out
        none of them.
        """
        synthetic = (
            "from negative_suite import matrix_main\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [('a', ROOT / 'src/example/named.rs', 'x')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        for key in ('fixtures/negative_suite.py', 'fixtures/negative_panic.py',
                    'fixtures/negative_scope.py', 'fixtures/pending_reanchor.py'):
            self.assertIn(f'missing anchor: {key} ', '\n'.join(problems), problems)
        direct_only = synthetic.replace(
            "DISCOVERY_ANCHORS = ()",
            "DISCOVERY_ANCHORS = ('fixtures/negative_suite.py',)")
        self.assertTrue(any('missing anchor: fixtures/negative_panic.py' in problem
                            for problem in violations(direct_only,
                                                      'example/negative_controls.py')),
                        'the second import level must still be owed')
        transitive = synthetic.replace(
            "DISCOVERY_ANCHORS = ()",
            "DISCOVERY_ANCHORS = ('fixtures/negative_suite.py',\n"
            "                     'fixtures/negative_panic.py',\n"
            "                     'fixtures/negative_process.py',\n"
            "                     'fixtures/negative_scope.py',\n"
            "                     'fixtures/negative_selection.py',\n"
            "                     'fixtures/pending_reanchor.py')")
        self.assertEqual(violations(transitive, 'example/negative_controls.py'), [])

    def test_a_parameter_shadows_a_module_constant_of_the_same_name(self):
        """``def run(BASE)`` hides the module constant at that call site.

        The parameter is bound at every depth by ``_rebound_elsewhere``, so the
        module value is not substituted there; the site is reported instead of
        keying the directory the module constant happens to name.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example/a'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def run(BASE):\n"
            "    return BASE / 'new.rs'\n"
            "run(ROOT / 'src/example/b')\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous module-level name rebound elsewhere: BASE'
                            in problem for problem in problems), problems)
        self.assertFalse(any('a/new.rs' in problem for problem in problems), problems)

    def test_a_negative_parent_index_is_not_the_repository_root(self):
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[-1]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'src/example/named.rs']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_runtime_file_name_position_fails_closed(self):
        """``SMS / name`` knows the directory and not the file: report it.

        The three spellings a table reaches for are one hole — the loop needs a
        ``dir/file`` literal the text does not contain — and a table that uses
        them was silently skipped, so it registered no anchor and ran no mutant.
        """
        shapes = {
            'division': "def row(name):\n    return SMS / name\n",
            'f-string': "def row(name):\n    return f'{SMS}/{name}'\n",
            'string concat': "def row(name):\n    return str(SMS) + name\n",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "SMS = ROOT / 'src/example/sms'\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + body
                    + "rows = [row('mod.rs')]\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(any('unresolvable' in problem or 'unsupported' in problem
                                    for problem in problems), problems)

    def test_a_runtime_file_name_in_a_call_fails_closed(self):
        """``SMS.joinpath(name)`` is the call spelling of ``SMS / name``.

        The ``/``, f-string and ``+`` forms reported since r9; the call forms
        were silent, so a table written that way produced neither a key nor a
        report and never had to register an anchor.
        """
        shapes = {
            'joinpath': "def row(name):\n    return SMS.joinpath(name)\n",
            'with_name': "def row(name):\n    return SMS.with_name(name)\n",
            'Path base arg': "def row(name):\n    return Path(SMS, name)\n",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "SMS = ROOT / 'src/example/sms'\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + body
                    + "rows = [row('mod.rs')]\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(any('unresolvable' in problem for problem in problems), problems)

    def test_a_literal_call_tail_still_keys_the_file(self):
        """``SMS.joinpath('mod.rs')`` names its file: key it, do not report it."""
        synthetic = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/example/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [SMS.joinpath('mod.rs'), Path(SMS, 'gateway.rs')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: sms/mod.rs' in problem for problem in problems), problems)
        self.assertTrue(any('missing anchor: sms/gateway.rs' in problem for problem in problems), problems)

    def test_a_separator_behind_a_constant_still_fails_closed(self):
        """``str(SMS) + SEP + 'mod.rs'`` hides the separator behind a name."""
        shapes = {
            'separator constant': "SEP = '/'\nrows = [str(SMS) + SEP + 'mod.rs']\n",
            'fragment constant': "MOD = '/mod.rs'\nrows = [str(SMS) + MOD]\n",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "SMS = ROOT / 'src/example/sms'\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + body
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(any('unsupported string concatenation' in problem
                                    for problem in problems), problems)

    def test_a_display_string_stays_silent(self):
        """A counter or a label joins no repository directory."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/example/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def report(killed, total, name, lines):\n"
            "    print(f'{killed}/{total} killed; probe={SMS}')\n"
            "    return name + ': ' + ' | '.join(lines)\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_second_plain_assignment_of_a_module_constant_is_ambiguous(self):
        """Last-wins would key ``y/mod.rs`` for the row that built ``x/mod.rs``."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example/x'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [(BASE / 'mod.rs', 'x')]\n"
            "BASE = ROOT / 'src/example/y'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous' in problem for problem in problems), problems)
        self.assertFalse(any('y/mod.rs' in problem for problem in problems), problems)

    def test_a_conditional_second_assignment_is_ambiguous_too(self):
        """``if flag: BASE = ...`` rebinds at import time like a top-level line."""
        synthetic = (
            "import os\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example/x'\n"
            "if os.environ.get('PDRS_X'):\n"
            "    BASE = ROOT / 'src/example/y'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [(BASE / 'mod.rs', 'x')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous' in problem for problem in problems), problems)
        self.assertFalse(any('x/mod.rs' in problem for problem in problems), problems)

    def test_the_same_value_assigned_twice_is_not_ambiguous(self):
        """Assigning one expression twice leaves the value unambiguous."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example/x'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [(BASE / 'mod.rs', 'x')]\n"
            "BASE = ROOT / 'src/example/x'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertFalse(any('ambiguous' in problem for problem in problems), problems)
        self.assertTrue(any('missing anchor: x/mod.rs' in problem
                            for problem in problems), problems)

    def test_a_tuple_loop_target_does_not_reuse_a_stale_constant(self):
        """``for name, kind in rows:`` binds ``name`` as firmly as a bare name."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/example/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def main(rows):\n"
            "    out = []\n"
            "    for name in ('outer.rs',):\n"
            "        for name, kind in rows:\n"
            "            out.append(SMS / name)\n"
            "    return out\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertFalse(any('sms/outer.rs' in problem for problem in problems), problems)

    def test_a_from_import_alias_of_a_directory_walk_fails_closed(self):
        """``from os import walk as w`` reaches the same unmodelled enumeration."""
        synthetic = (
            "from os import walk as w\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/example/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "for _root, _dirs, _files in w(SMS):\n"
            "    pass\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('enumeration' in problem for problem in problems), problems)

    def test_a_self_referential_starred_constant_terminates(self):
        """``NAMES = (*NAMES,)`` is dead code at runtime but still parsed."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "NAMES = (*NAMES,)\n"
            "DISCOVERY_ANCHORS = ()\n"
            "for name in NAMES:\n"
            "    (ROOT / 'src/example' / name, 'x')\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('unresolvable' in problem for problem in problems), problems)

    def test_a_package_initialiser_is_parsed_for_its_own_imports(self):
        """``import pkg`` runs ``pkg/__init__.py``; what it imports is a key too."""
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'pkg').mkdir()
            (root / 'pkg' / '__init__.py').write_text(
                'from helper import run\n', encoding='utf-8')
            (root / 'helper.py').write_text('', encoding='utf-8')
            keys = _shared_module_keys(ast.parse('import pkg\n'), root, ())
        self.assertEqual(sorted(keys), ['fixtures/helper.py', 'pkg/__init__.py'])

    def test_a_dotted_import_keys_every_package_it_runs(self):
        """``import a.b.c`` executes ``a/__init__.py`` and ``a/b/__init__.py``."""
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'a' / 'b').mkdir(parents=True)
            (root / 'a' / '__init__.py').write_text('', encoding='utf-8')
            (root / 'a' / 'b' / '__init__.py').write_text('', encoding='utf-8')
            (root / 'a' / 'b' / 'c.py').write_text('', encoding='utf-8')
            keys = _shared_module_keys(ast.parse('import a.b.c\n'), root, ())
        # The loop keys a nested file by its last two components, so the
        # ``a/b/__init__.py`` it runs keys as ``b/__init__.py``.
        self.assertEqual(sorted(keys), ['a/__init__.py', 'b/__init__.py', 'b/c.py'])

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
