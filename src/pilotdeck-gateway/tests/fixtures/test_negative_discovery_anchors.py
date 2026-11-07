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
# 254 files (the host glob reaches the ones under tests/fixtures/, not the top-level fixtures/ tables; #2141), 179 blind only, 3 with unresolvable shapes only, 72 in both; 1284 keys: 358 bare-filename joins and 926 shared-helper import keys.
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
    'closure_guard_negative.py': '3 keys + 2 unresolvable',
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
    'issue-297/negative_controls.py': '7 keys + 1 unresolvable',
    'js-math-zero/negative.py': '1 key',
    'ledger-diffs/negative_controls.py': '2 keys + 1 unresolvable',
    'live-e2e-tmpdir/negative_controls.py': '6 keys',
    'live-e2e/negative_controls.py': '7 keys + 7 unresolvable',
    'live-history/negative.py': '1 key',
    'live-recording/negative.py': '10 keys',
    'live-session/negative_controls.py': '6 keys + 1 unresolvable',
    'locale-adaptive/negative_controls.py': '7 keys',
    'locale-contract/negative_controls.py': '6 keys',
    'locked-storage/negative_controls.py': '6 keys',
    'manager-gateway/negative_controls.py': '7 keys + 1 unresolvable',
    'mcp-detached-dispatch/negative_controls.py': '6 keys',
    'mcp-gateway/negative_controls.py': '6 keys',
    'mcp-hooks-spawn/negative_controls.py': '1 key + 2 unresolvable',
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
    'negative_oracle_guard.py': '1 key + 3 unresolvable',
    'negative_scope.py': '1 unresolvable',
    'network-timing/negative_controls.py': '6 keys',
    'network-timing/negative_private.py': '2 keys + 2 unresolvable',
    'node-io/negative_controls.py': '4 keys',
    'node-provenance/negative.py': '6 keys + 1 unresolvable',
    'operation-ledger/negative.py': '6 keys',
    'output-schema/negative.py': '3 keys',
    'p01-preflight/negative_controls.py': '6 keys + 1 unresolvable',
    'parity-lock-release/negative_controls.py': '6 keys + 1 unresolvable',
    'pending-reanchor/negative_controls.py': '4 keys',
    'plugin-loader/negative-controls.py': '4 keys + 2 unresolvable',
    'plugin-runtime/negative-controls.py': '6 keys',
    'plugin-runtime/notify-window-negative.py': '4 keys',
    'plugin-runtime/panic-negative.py': '4 keys',
    'private-ca/negative_controls.py': '6 keys + 1 unresolvable',
    'project-files-linux-dirent/negative_controls.py': '4 keys',
    'projector-cwd/negative_controls.py': '8 keys',
    'provider-http-boundaries/negative_controls.py': '6 keys',
    'proxy-listen-address/negative_controls.py': '7 keys',
    'pump-order-cases/negative_controls.py': '6 keys',
    'recorder/fix3-negative.py': '7 keys + 1 unresolvable',
    'regeneration-masks/negative.py': '6 keys',
    'replacement/negative-518.py': '3 keys',
    'replacement/negative.py': '1 key',
    'retry-after-dialects/negative_controls.py': '6 keys',
    'run-registry/count-negative.py': '8 keys + 1 unresolvable',
    'run-registry/timestamp-negative.py': '6 keys',
    'runtime-spawn-panic/helpers_negative.py': '3 keys',
    'runtime-spawn-panic/negative_controls.py': '4 keys + 2 unresolvable',
    'sdk-cli-lifecycle/negative_controls.py': '6 keys + 2 unresolvable',
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
    'session-domain/fix2-negative.py': '1 key + 3 unresolvable',
    'session-drop-release/negative_controls.py': '8 keys + 1 unresolvable',
    'session-factory/append-delta/negative_controls.py': '6 keys + 8 unresolvable',
    'session-factory/negative_controls.py': '16 keys + 2 unresolvable',
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
    'stress-followups/negative_controls.py': '2 keys + 1 unresolvable',
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
    'turn-runner/integrity_negative.py': '10 keys + 1 unresolvable',
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

    ``REPO: Path = ROOT / 'x'`` is not one of those forms: the annotation binds
    the name at import time exactly like ``REPO = ROOT / 'x'``, so it is resolved
    like one. An annotated binding *inside a function* still shadows a module
    constant of the same name through ``_function_scoped_names``.
    """
    names = set()
    for node in ast.walk(tree):
        if not isinstance(node, (ast.Assign, ast.AnnAssign)):
            names |= _bound_names(node)
    return names


def _imported_names(tree):
    """Names an ``import`` statement binds.

    ``from external_config import base`` gives ``base`` a value the file text
    does not contain. When the same name is also assigned once inside a
    function, that assignment used to be promoted to a file-wide constant, so
    every other site resolved to that one directory: a table importing such a
    name and also aliasing it locally keyed the wrong directory and reported
    nothing at the site it could not follow. An import is a binding like any
    other — the scanner must not look past it.
    """
    names = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                names.add(alias.asname or alias.name.split('.')[0])
        elif isinstance(node, ast.ImportFrom):
            for alias in node.names:
                if alias.name != '*':
                    names.add(alias.asname or alias.name)
    return names


def _unpacked_bindings(target, value):
    """``(name, value)`` pairs for one assignment target and its expression.

    ``A, B = ROOT / 'sms', ROOT / 'mms'`` binds each name to its own expression,
    which a table listing several directories on one line relies on. Returns
    None when the pairing is not literal and element-wise (a starred target, a
    length mismatch, a non-tuple value): the caller then reports the names
    instead of guessing a directory.
    """
    if isinstance(target, ast.Name):
        return [(target.id, value)]
    if not isinstance(target, (ast.Tuple, ast.List)):
        return None
    if (not isinstance(value, (ast.Tuple, ast.List))
            or len(value.elts) != len(target.elts)):
        return None
    pairs = []
    for element_target, element_value in zip(target.elts, value.elts):
        if isinstance(element_target, ast.Starred):
            return None
        nested = _unpacked_bindings(element_target, element_value)
        if nested is None:
            return None
        pairs.extend(nested)
    return pairs


def _assignment_pairs(statement):
    """``(pairs, unfollowable)`` for one assignment statement.

    ``REPO: Path = ROOT / 'x'`` binds at import time exactly like
    ``REPO = ROOT / 'x'``; leaving the annotated form out of the constant table
    made its uses neither keyed nor reported. ``unfollowable`` carries the
    ``(name, value)`` pairs whose target/value shapes do not pair up, so the
    caller can report their uses rather than drop them.
    """
    if isinstance(statement, ast.Assign):
        targets, value = statement.targets, statement.value
    elif isinstance(statement, ast.AnnAssign) and statement.value is not None:
        targets, value = [statement.target], statement.value
    else:
        return [], []
    pairs, unfollowable = [], []
    for target in targets:
        mapped = _unpacked_bindings(target, value)
        if mapped is None:
            names = set()
            _bind_names(target, names)
            unfollowable.extend((name, value) for name in sorted(names))
        else:
            pairs.extend(mapped)
    return pairs, unfollowable


def _module_level_pairs(tree):
    """``(pairs, unfollowable)`` for every module-level assignment.

    Conditional blocks count, function and class bodies do not: ``if flag: A =
    ...`` runs at import time and rebinds the global just like a top-level line,
    while a name inside a function is local to that scope.

    Loop bodies do not count either. ``for row in cases: count = ...`` rebinds
    the name on every iteration with a value that depends on the row, so it is
    no more a constant than a function local — substituting it globally keys
    (or reports) a directory the code may never build, and hides the one it
    does. The same walk feeds ``_duplicated_module_names``, so a loop body also
    cannot make an unrelated top-level constant look ambiguous.
    """
    pairs, unfollowable = [], []

    def visit(node):
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.Lambda, ast.ClassDef, ast.For,
                                  ast.AsyncFor, ast.While)):
                continue
            if isinstance(child, (ast.Assign, ast.AnnAssign)):
                child_pairs, child_unfollowable = _assignment_pairs(child)
                pairs.extend(child_pairs)
                unfollowable.extend(child_unfollowable)
            visit(child)

    visit(tree)
    return pairs, unfollowable


def _module_level_assignments(tree):
    """The values every ``name = ...`` statement gives a module-level name.

    ``if flag: A = ...`` at module level runs at import time and rebinds the
    global just like a top-level line, so it counts. Function, lambda and class
    bodies do not: those names are local to their scope, and the module-level
    value they shadow is already handled by ``_function_scoped_names``.
    """
    values = {}
    pairs, _ = _module_level_pairs(tree)
    for name, value in pairs:
        values.setdefault(name, []).append(ast.dump(value))
    return values


class _Constants(dict):
    """The constant table, plus the container names some statement mutates.

    ``CH['k'] = ...``, ``del CH['k']`` and ``CH.append(...)`` leave the literal
    the scanner reads out of date: the value the runtime indexes is no longer
    the one written in the source, so an index of that container has to be
    reported instead of keyed. Carrying the set on the table keeps every
    resolver signature unchanged.
    """

    mutated = frozenset()


CHDIR_CALLS = frozenset({'chdir', 'fchdir'})
# Modules whose ``chdir`` moves the process directory: ``os`` and ``posix``
# (the C-level spelling), ``os.path`` (kept fail-closed: ``import os.path as
# osp; osp.chdir(...)`` is an AttributeError at runtime, and reporting it is the
# harmless direction) and ``contextlib`` (the 3.11+ context manager).
CHDIR_MODULES = frozenset({'os', 'os.path', 'posix', 'contextlib'})


def _chdir_bindings(tree):
    """Names through which the table can reach a ``chdir`` call.

    The import map keeps one target per name (last import wins), which is right
    for resolving ``mod.func`` spellings but wrong here: ``from os import chdir
    as cd`` followed later by ``from os.path import join as cd`` still leaves a
    real ``cd('…')`` call earlier in the file, and the map no longer knew the
    name. The reading is fail-closed — if a name is *ever* bound to a module
    that has ``chdir`` or to ``chdir`` itself, a call through it counts.
    Assignment aliases (``cd = os.chdir``) are the same edge.
    """
    modules = set()
    names = set()
    assigned = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.asname:
                    if alias.name in CHDIR_MODULES:
                        modules.add(alias.asname)
                elif alias.name in CHDIR_MODULES:
                    # ``import os.path`` binds the package name ``os``.
                    modules.add(alias.name.split('.')[0])
        elif isinstance(node, ast.ImportFrom) and node.module and not node.level:
            if node.module in CHDIR_MODULES:
                for alias in node.names:
                    if alias.name in CHDIR_CALLS:
                        names.add(alias.asname or alias.name)
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            pairs, _ = _assignment_pairs(node)
            assigned.extend(pairs)
    grew = True
    while grew:
        grew = False
        for name, value in assigned:
            if name in names:
                continue
            spelling = _dotted_spelling(value)
            if spelling is None:
                continue
            module_part, _, last = spelling.rpartition('.')
            if ((module_part and last in CHDIR_CALLS and module_part in modules)
                    or (not module_part and last in names)):
                names.add(name)
                grew = True
    return modules, names


def _changes_directory(tree):
    """Whether the table can move the process working directory at runtime.

    ``Path.cwd()`` is read as the repository root because the host starts a
    table there. ``os.chdir(...)`` — at module level or inside a ``with`` —
    breaks that premise after the fact: a later join names a file under
    whatever directory the table moved to, and the single-component result used
    to be dropped silently. Any chdir call therefore makes the working
    directory unknowable, which is reported at its uses. Every spelling that
    can reach the call counts: an aliased module, ``from os import chdir as
    cd``, and a plain assignment alias (see ``_chdir_bindings``).
    """
    modules, names = _chdir_bindings(tree)
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if isinstance(func, ast.Attribute):
            if func.attr not in CHDIR_CALLS:
                continue
            receiver = _dotted_spelling(func.value)
            if (isinstance(func.value, ast.Name) and func.value.id in modules) \
                    or receiver in CHDIR_MODULES:
                return True
        elif isinstance(func, ast.Name) and func.id in names:
            return True
    return False


def _container_root_name(node):
    """The name a container read or write ultimately starts from.

    ``CH['a']['b'] = ...`` and ``CH['a'].update(...)`` both reach the very same
    dictionary ``CH`` names; only the last step is an attribute or an index.
    """
    while True:
        if isinstance(node, ast.Name):
            return node.id
        if isinstance(node, ast.Subscript):
            node = node.value
        elif isinstance(node, ast.Attribute):
            node = node.value
        else:
            return None


MUTATING_METHODS = frozenset({'append', 'extend', 'insert', 'pop', 'remove',
                              'clear', 'update', 'setdefault', 'sort',
                              'reverse', '__setitem__', '__delitem__'})

# ``operator.setitem(CH, 'k', v)`` writes through a function call the method
# spelling above never sees; the bare import lands here as a plain name.
SETITEM_CALLS = frozenset({'operator.setitem', 'setitem'})


def _mutated_containers(tree):
    """Names whose literal container some statement changes after it is built.

    A name bound to a mutated container refers to the *same* object, so the set
    is closed over the alias graph in **both** directions: a write through the
    alias leaves the literal behind ``CH`` just as stale as a write through
    ``CH`` itself.

    Every spelling that can carry the reference counts as an edge:
    ``ALIAS = CH``, ``SUB = CH['a']`` (the *same* nested object, so
    ``SUB['b'] = ...`` rewrites what ``CH['a']['b']`` reads), and
    ``CP = CH.copy()`` — the copy is shallow, so a write *through it* into a
    nested container still rewrites the literal behind ``CH``. Following the
    graph transitively is what closes ``A = CH; B = A['a']; B['b'] = ...``;
    the earlier reading only paired a subscript whose root was itself bound to
    a container literal, so a plain alias in between hid the write and the
    stale key was accepted.

    Deep writes (``CH['a']['b'] = ...``) and mutations of a nested container
    (``CH['a'].update(...)``) are attributed to the outermost name. The point
    is to report the shape rather than guess which element the runtime sees.

    Depth is tracked because a shallow copy separates the two cases: replacing
    ``{**CH}['k']`` writes a key of the *copy* and leaves ``CH['k']`` alone,
    while ``{**CH}['a']['b'] = ...`` rewrites the nested dict ``CH['a']`` names.
    Depths saturate at two — nothing below the first level changes a verdict.
    """
    NESTED = 1
    depths = {}  # name -> non-empty set of depths at which its container is hit

    def record(container):
        name = _container_root_name(container)
        depth = _container_depth(container)
        if name is None or depth is None:
            return
        depths.setdefault(name, set()).add(min(depth, 2))

    def record_target(target):
        for child in ast.walk(target):
            if isinstance(child, ast.Subscript):
                # ``CH['k'] = ...`` mutates the container *holding* the key,
                # so the depth is that of ``child.value``, not of the target.
                record(child.value)

    for node in ast.walk(tree):
        if isinstance(node, ast.Assign):
            for target in node.targets:
                record_target(target)
        elif isinstance(node, (ast.AnnAssign, ast.AugAssign)):
            record_target(node.target)
            if isinstance(node, ast.AugAssign):
                if isinstance(node.target, ast.Name):
                    # ``CH |= {...}`` / ``CH += [...]`` rebinds through the
                    # same name and leaves the literal out of date.
                    record(node.target)
                elif isinstance(node.target, ast.Subscript):
                    # ``CP['a'] |= {...}`` / ``CP['a'] += [...]`` mutate the
                    # *nested object itself* in place (``dict.__ior__`` /
                    # ``list.__iadd__``), so the write reaches one level below
                    # the container the subscript names. Recording the bare
                    # container said depth 0, and a shallow-copy edge only
                    # carries depth ≥ 1 — the original never saw the write.
                    record(node.target)
        elif isinstance(node, ast.Delete):
            for target in node.targets:
                record_target(target)
        elif isinstance(node, ast.Call) and isinstance(node.func, (ast.Attribute, ast.Name)):
            func = node.func
            if isinstance(func, ast.Attribute) and func.attr in MUTATING_METHODS:
                record(func.value)
            elif node.args and _dotted_spelling(func) in SETITEM_CALLS:
                record(node.args[0])
    # (name, root, levels between them); ``copy_of`` marks shallow copies,
    # whose top level is a separate object while every nested one is shared.
    aliases = _module_aliases(tree)
    same_object = []
    copy_of = []
    for node in ast.walk(tree):
        if not isinstance(node, (ast.Assign, ast.AnnAssign)):
            continue
        pairs, _ = _assignment_pairs(node)
        for name, value in pairs:
            if isinstance(value, ast.Name):
                # ``ALIAS = CH``: the same object under a second name.
                same_object.append((name, value.id, 0))
            elif isinstance(value, ast.Subscript):
                root = _container_root_name(value)
                depth = _container_depth(value)
                if isinstance(value.slice, ast.Slice):
                    # ``CP = CH[:]`` copies the container: the copy's own
                    # top level is a separate object, only its nested
                    # containers are shared. Reading the slice as the same
                    # object made a top-level write on the copy pollute the
                    # original. The edge is only drawn for a plain container
                    # name: ``SAVED = sys.path[:]`` snapshots an *attribute*
                    # chain, and rooting it at ``sys`` made every later
                    # ``sys.path[:] = SAVED`` restore read as a mutation of
                    # the snapshot.
                    if root is not None and isinstance(value.value, ast.Name):
                        copy_of.append((name, root))
                    continue
                # ``SUB = CH['a']`` names the *same* nested object one level
                # down; the closure below follows any number of aliases.
                if root is not None and depth is not None:
                    same_object.append((name, root, min(depth, 2)))
            else:
                for root in _shallow_copy_roots(value, aliases):
                    copy_of.append((name, root))
    grew = True
    while grew:
        grew = False
        for name, root, levels in same_object:
            # A write through ``name`` at depth d is a write into ``root`` at
            # depth d + levels; the reverse is d - levels, and a write that
            # does not reach that far leaves ``name``'s container alone.
            for source, target, shift in ((name, root, levels),
                                          (root, name, -levels)):
                for depth in tuple(depths.get(source, ())):
                    reached = depth + shift
                    if reached < 0:
                        continue
                    reached = min(reached, 2)
                    if reached not in depths.setdefault(target, set()):
                        depths[target].add(reached)
                        grew = True
        for name, root in copy_of:
            # Only the nested containers survive the copy; a write into the
            # copy's own top level never reaches the original.
            for source, target in ((name, root), (root, name)):
                nested = {depth for depth in depths.get(source, ())
                          if depth >= NESTED}
                if not nested <= depths.setdefault(target, set()):
                    depths[target] |= nested
                    grew = True
    return frozenset(name for name, hits in depths.items() if hits)


# Copy constructors that keep the elements themselves: a new top-level object
# whose nested containers are the very same ones the source holds.
SHALLOW_COPY_CALLS = frozenset({'dict', 'list', 'tuple', 'set', 'frozenset',
                                'sorted', 'copy.copy', 'copy.deepcopy'})


def _shallow_copy_roots(value, aliases=None):
    """Names whose container ``value`` shallow-copies.

    ``CH.copy()``, ``copy.copy(CH)``, ``dict(CH)``, ``list(CH)``, ``tuple(CH)``,
    ``sorted(CH)``, ``{**CH}``, ``[*CH, extra]`` and ``CH + [x]`` all build a
    new top-level object that still holds the very same nested containers, so a
    write through any of them *below its own top level* rewrites what
    ``CH['a']`` reads. The import alias is resolved for the bare spellings
    (``from copy import copy as cp2``), ``dict(CH, extra=1)`` keeps the source
    in its first positional argument, and every ``*`` expansion counts —
    requiring exactly one element left ``[None, *CH]`` as an edge to nothing. A
    copy's own top-level key is rebound on the copy alone — replacing
    ``{**CH}['k']`` leaves ``CH['k']`` untouched — so the edge only carries
    writes that reach a nested container; see ``_mutated_containers``.
    """
    aliases = aliases or {}
    if isinstance(value, ast.Call):
        func = value.func
        if isinstance(func, ast.Name):
            qualified = aliases.get(func.id, func.id)
            if qualified in SHALLOW_COPY_CALLS:
                sources = list(value.args)
                if not sources and qualified == 'dict':
                    # ``dict(**CH)``: the unpacking keyword is the source.
                    sources = [keyword.value for keyword in value.keywords
                               if keyword.arg is None]
                if sources:
                    root = _container_root_name(sources[0])
                    return [root] if root is not None else []
            return []
        if isinstance(func, ast.Attribute):
            spelling = _dotted_spelling(func)
            if spelling is None:
                # ``CH['a'].copy()`` / ``f().copy()``: the receiver is not a
                # plain name, so no copy spelling can be resolved from it.
                return []
            head, _, rest = spelling.rpartition('.')
            if head:
                spelling = f'{aliases.get(head, head)}.{rest}'
            if spelling in SHALLOW_COPY_CALLS or rest == 'copy':
                if value.args and spelling in SHALLOW_COPY_CALLS:
                    # ``copy.copy(CH)``: the module receiver is not a container.
                    root = _container_root_name(value.args[0])
                else:
                    # ``CH.copy()``: a bound-method call on the container.
                    root = _container_root_name(func.value)
                return [root] if root is not None else []
    if isinstance(value, ast.Dict):
        return [root for key, element in zip(value.keys, value.values)
                if key is None and (root := _container_root_name(element)) is not None]
    if isinstance(value, (ast.List, ast.Tuple, ast.Set)):
        return [root for element in value.elts
                if isinstance(element, ast.Starred)
                and (root := _container_root_name(element.value)) is not None]
    if isinstance(value, ast.BinOp) and isinstance(value.op, ast.Add):
        # ``CH + [x]`` / ``[x] + CH``: list concatenation copies the top level
        # and keeps every element object. An operand that is itself a plain
        # container (``CH``) is not a copy expression, so it has no roots of its
        # own — read it as the container it names.
        roots = []
        for operand in (value.left, value.right):
            found = _shallow_copy_roots(operand, aliases)
            if found:
                roots.extend(found)
                continue
            root = _container_root_name(operand)
            if root is not None:
                roots.append(root)
        return roots
    return []


def _container_depth(node):
    """How many steps below the root name a container expression reaches.

    ``CH`` is the root itself (0); ``CH['a']`` and ``CH.a`` name a container
    held *inside* it (1); ``CH['a']['b']`` two levels down (2). Only nested
    containers survive a shallow copy, so the depth decides whether a write
    through the copy reaches the original.
    """
    depth = 0
    while isinstance(node, (ast.Subscript, ast.Attribute)):
        depth += 1
        node = node.value
    return depth if isinstance(node, ast.Name) else None


def _constants(tree):
    """``name = <expression>`` bindings this scanner is willing to substitute.

    Module-level bindings are authoritative (last one wins, as at runtime), but
    only when there is exactly one of them: a second ``A = ...`` — at the top
    level or inside an ``if`` — means an earlier use site saw the first value,
    so substituting either one keys a directory the code may never build and
    hides the one it does. Such a name is left out and reported at its uses.
    Tables also build paths from local aliases (``source = ROOT / '...'`` inside
    a function, then ``source.parent / 'x.rs'``); a name whose assignments all
    give the same expression has no ambiguity, so it is resolved too — assignments
    that disagree are reported by ``_ambiguous_local_names``. A module-level name
    that a function rebinds is *not* substituted either: the two sites would
    then mean different directories, and resolving both to the module-level
    value would demand an anchor for a key the runtime never uses while hiding
    the real one. Neither is a name bound by some other form (parameter, loop
    target, ...).
    """
    constants = {}
    module_pairs, _ = _module_level_pairs(tree)
    for name, value in module_pairs:
        constants[name] = value        # last one wins, exactly as at runtime
    module_level = {name for name, _ in module_pairs}
    values = {}
    assignments = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            pairs, _ = _assignment_pairs(node)
            for name, value in pairs:
                values.setdefault(name, []).append(ast.dump(value))
                assignments.append((name, value))
    rebound = _rebound_elsewhere(tree)
    imported = _imported_names(tree)
    unpairable = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            _, bad = _assignment_pairs(node)
            unpairable.update(name for name, _ in bad)
    bound_anywhere = set(values) | unpairable | rebound
    for name, value in assignments:
        if name in constants or name in rebound or name in imported:
            continue
        if len(values[name]) == 1:
            constants[name] = value
    # A name assigned several times is still a constant when every assignment
    # gives the same expression — but only when that expression means the same
    # thing at every site. ``def a(): base = ROOT / 'x'; src = base`` and
    # ``def b(): base = ROOT / 'y'; src = base`` give ``src`` two directories
    # although the expression ``base`` is byte-identical: merging on the AST
    # alone resolved ``src`` to a name the scanner could not follow and reported
    # nothing at all. Expressions that read a name some other binding also owns
    # are therefore left out and reported at their uses, like any other
    # disagreement. Assignments that disagree are reported by
    # ``_ambiguous_local_names`` — leaving them out *and* silent was the hole
    # (two functions, two directories, no key, no report).
    for name, value in assignments:
        if name in constants or name in rebound or name in imported:
            continue
        if len(set(values[name])) == 1 and _scope_free(value, constants, bound_anywhere):
            constants[name] = value
    # Substitution is refused for every module binding some other form can
    # rebind — including a binding inside a top-level ``if`` / ``with`` / ``try``
    # body, which runs at import time exactly like a top-level line. That is the
    # ``module_level`` set above, and the "other form" set is the one
    # ``_shadowed_names`` uses. ``_shadowed_names`` itself stays narrower (it
    # intersects with the plain top-level statements): it feeds the *reporting*
    # net, and a temporary directory bound in a ``with`` body must not be
    # reported as a repository path it never was.
    shadowing = (_function_scoped_names(tree) | _rebound_elsewhere(tree)
                 | _duplicated_module_names(tree))
    for name in shadowing & module_level:
        constants.pop(name, None)
    table = _Constants(constants)
    table.mutated = _mutated_containers(tree)
    table.cwd_unknown = _changes_directory(tree)
    return table


def _builds_from_root(value, constants, seen=()):
    """Whether ``value`` is a path whose directory spine starts at the repo root.

    ``_is_repo_rooted`` answers "does this expression mention a repository path
    anywhere", which is the right question for a join that must be reported —
    but too loose to decide whether a *name* holds a repository directory:
    ``command = ['node', f'{baseline}/loader.mjs', ...]`` mentions one inside a
    container that is really an output directory, and every join on ``command``
    then looked like a repository path.
    """
    if isinstance(value, ast.Name):
        if value.id in ROOT_NAMES or value.id == '__file__':
            return True
        if value.id in seen or value.id not in constants:
            return False
        return _builds_from_root(constants[value.id], constants, seen + (value.id,))
    if isinstance(value, ast.BinOp) and isinstance(value.op, ast.Div):
        return _builds_from_root(value.left, constants, seen)
    if isinstance(value, ast.Attribute) and value.attr in ('parent', 'resolve'):
        return _builds_from_root(value.value, constants, seen)
    if (isinstance(value, ast.Subscript) and isinstance(value.value, ast.Attribute)
            and value.value.attr == 'parents'):
        # ``(ROOT / 'x/gateway.rs').parents[0]`` is the directory of a
        # repository file: still rooted at the repository, so a local alias
        # built this way has to seed the ambiguity net like ``.parent`` does.
        return _builds_from_root(value.value.value, constants, seen)
    if isinstance(value, ast.JoinedStr):
        return any(isinstance(part, ast.FormattedValue)
                   and _builds_from_root(part.value, constants, seen)
                   for part in value.values)
    if isinstance(value, ast.Call):
        func = value.func
        if isinstance(func, ast.Name) and func.id in ('str', 'Path'):
            return bool(value.args) and _builds_from_root(value.args[0], constants, seen)
        if isinstance(func, ast.Attribute) and func.attr in ('resolve', 'joinpath',
                                                             'with_name'):
            return _builds_from_root(func.value, constants, seen)
    return False


def _carries_name_as_path(value, names):
    """Whether ``value`` builds a path *from* one of ``names``.

    Used to follow a repository directory through a local alias
    (``base = ROOT / 'x'`` then ``src = base``) without treating every
    expression that merely *mentions* such a name as a path. ``command =
    ['node', f'{baseline}/loader.mjs']`` holds a repository path in a container
    that is later used as an output directory; the name is not a path, and
    reporting its joins would be crying wolf on evidence paths.
    """
    if isinstance(value, ast.Name):
        return value.id in names
    if isinstance(value, ast.BinOp) and isinstance(value.op, ast.Div):
        return _carries_name_as_path(value.left, names)
    if isinstance(value, ast.Attribute) and value.attr in ('parent', 'resolve'):
        return _carries_name_as_path(value.value, names)
    if (isinstance(value, ast.Subscript) and isinstance(value.value, ast.Attribute)
            and value.value.attr == 'parents'):
        return _carries_name_as_path(value.value.value, names)
    if isinstance(value, ast.JoinedStr):
        # ``src = f'{base}'`` builds the path *from* ``base`` just as ``src =
        # base`` does; the interpolation is not a display string when the whole
        # value is one formatted name. Without this step the taint stopped at
        # the f-string and every join on ``src`` stayed silent.
        return any(isinstance(part, ast.FormattedValue)
                   and _carries_name_as_path(part.value, names)
                   for part in value.values)
    if isinstance(value, ast.Call):
        func = value.func
        if isinstance(func, ast.Name) and func.id in ('str', 'Path'):
            return bool(value.args) and _carries_name_as_path(value.args[0], names)
        if isinstance(func, ast.Attribute) and func.attr in ('resolve', 'joinpath',
                                                             'with_name'):
            return _carries_name_as_path(func.value, names)
    return False


def _carries_name_as_path_in_unpacking(value, names):
    """Whether an unpairable right-hand side passes a path name into a target.

    ``src, *rest = (base, 1)`` binds ``src`` to ``base``, and ``base`` may be a
    repository directory that only another scope assigns; the element-wise
    spine test does not look inside the container, so the ambiguity of ``base``
    never reached ``src`` and its joins stayed silent. Only unpairable
    assignments use this: an ordinary ``command = ['node', p]`` is a container
    that merely *holds* a path and must stay silent (it is an output directory).
    """
    if isinstance(value, (ast.Tuple, ast.List, ast.Set)):
        return any(_carries_name_as_path(element, names)
                   or _carries_name_as_path_in_unpacking(element, names)
                   for element in value.elts)
    if isinstance(value, ast.Dict):
        return any(_carries_name_as_path(element, names)
                   or _carries_name_as_path_in_unpacking(element, names)
                   for element in value.values)
    return _carries_name_as_path(value, names)


def _scope_free(value, constants, bound_anywhere):
    """Whether an expression means the same thing at every site that uses it.

    A literal, ``ROOT``, ``__file__`` or a module constant resolves identically
    wherever it appears, and so does a name nothing in the file binds (an import
    or a builtin). A name that *is* bound somewhere in the file — a parameter, a
    loop target, a local alias — can carry a different value per scope, so two
    byte-identical ``src = base`` lines are not proof of one directory.
    """
    for node in ast.walk(value):
        if not isinstance(node, ast.Name) or not isinstance(node.ctx, ast.Load):
            continue
        if node.id in ROOT_NAMES or node.id == '__file__' or node.id in constants:
            continue
        if node.id in bound_anywhere:
            return False
    return True


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
        if isinstance(statement, (ast.Assign, ast.AnnAssign)):
            pairs, _ = _assignment_pairs(statement)
            module_level.update(name for name, _ in pairs)
    return ((_function_scoped_names(tree) | _rebound_elsewhere(tree)
             | _duplicated_module_names(tree))
            & module_level)


def _unfollowable_module_names(tree, constants):
    """Module-level names bound by a shape this scanner cannot pair up.

    ``A, *rest = ROOT / 'a', ROOT / 'b'`` binds names the scanner cannot follow.
    Dropping them silently is the hole this guard exists to close, so their uses
    are reported like any other ambiguous module-level name — but only when the
    value really mentions the repository root, so an ordinary ``a, b = 1, 2``
    stays quiet.
    """
    _, unfollowable = _module_level_pairs(tree)
    return {name for name, value in unfollowable
            if _is_repo_rooted(value, constants)}


def _ambiguous_local_names(tree, constants):
    """Non-constant names bound in more than one way, at least one a repo path.

    ``def a(): src = ROOT / 'x'`` and ``def b(): src = ROOT / 'y'`` give one name
    two directories. It is not a constant (substituting either value would key a
    directory the other site never builds) and it is not module level either, so
    the module-level report net never saw it: both joins produced no key and no
    problem, and the table looked clean while the discovery loop never scheduled
    it.

    Every binding form counts, not only a second assignment. A name assigned a
    repository path in one function and used as a ``for`` target anywhere in the
    file — in the same function or in a third one — is just as ambiguous, and so
    is a name whose assignments pair up badly (``src, *rest = ROOT / 'a', ...``).
    Both used to be dropped silently: the whole-file rebinding set excluded the
    name from every net, and the pairing helper only reported at module level.
    """
    values = {}
    unpairable = {}
    for node in ast.walk(tree):
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            pairs, bad = _assignment_pairs(node)
            for name, value in pairs:
                values.setdefault(name, []).append(value)
            for name, value in bad:
                unpairable.setdefault(name, []).append(value)
    rebound = _rebound_elsewhere(tree)
    bound_anywhere = set(values) | set(unpairable) | rebound
    # Which names can carry a repository path, directly or through another
    # name: ``def a(): base = ROOT / 'x'; src = base`` makes ``src`` a repo path
    # even though the expression that binds it is a bare name. Without the
    # transitive step the ambiguity of ``base`` never reached ``src``, and the
    # join on ``src`` stayed silent.
    repo_bound = {name for name, bound in values.items()
                  if any(_builds_from_root(value, constants) for value in bound)}
    # An unpairable binding carries the whole right-hand side (``src, *rest =
    # (ROOT / 'a', ROOT / 'b')``), so the container itself has to be inspected:
    # any repository path inside it is enough to make the join unanswerable.
    repo_bound |= {name for name, bad in unpairable.items()
                   if any(_is_repo_rooted(value, constants) for value in bad)}
    grew = True
    while grew:
        grew = False
        for name in set(values) | set(unpairable):
            if name in repo_bound:
                continue
            # An unpairable binding hands the whole container to its targets
            # (``src, *rest = (base, 1)``), so its *elements* have to be
            # inspected too; a plain assignment only follows the path spine.
            if (any(_carries_name_as_path(value, repo_bound) for value in values.get(name, []))
                    or any(_carries_name_as_path_in_unpacking(value, repo_bound)
                           for value in unpairable.get(name, []))):
                repo_bound.add(name)
                grew = True
    names = set()
    # "Module level" here means exactly the set ``_shadowed_names`` owns: the
    # top-level statements. A name assigned only inside a module-level ``if``
    # body is not covered by that net, so it is judged here rather than skipped
    # — that conditional shape plus a rebinding elsewhere used to fall between
    # the two nets and be keyed from the ``if`` value.
    module_level = set()
    for statement in tree.body:
        if isinstance(statement, (ast.Assign, ast.AnnAssign)):
            pairs, _ = _assignment_pairs(statement)
            module_level.update(name for name, _ in pairs)
    for name in set(values) | set(unpairable):
        if name in module_level:
            # Module-level names have their own net (``_shadowed_names``);
            # reporting them here too would only relabel the same finding.
            continue
        bound = values.get(name, [])
        bad = unpairable.get(name, [])
        if name not in repo_bound:
            continue
        if bad or name in rebound or len({ast.dump(value) for value in bound}) > 1:
            names.add(name)
            continue
        if len(bound) > 1 and not all(_scope_free(value, constants, bound_anywhere)
                                     for value in bound):
            names.add(name)
    # A name an ``import`` binds *and* some function assigns a repository path
    # means two directories at two sites. ``_constants`` refuses to substitute
    # it, which is right, but leaving it at that made the join that reads it
    # silent: no key, no report, so the table looked clean. Reporting is the
    # fail-closed side of the same decision.
    for name in _imported_names(tree):
        if name in repo_bound and (values.get(name) or unpairable.get(name)):
            names.add(name)
    # An alias of an ambiguous name is ambiguous too: ``src = base`` only ever
    # means the value ``base`` has at that site.
    grew = True
    while grew:
        grew = False
        for name, value in constants.items():
            if name in names:
                continue
            if any(isinstance(child, ast.Name) and child.id in names
                   for child in ast.walk(value)):
                names.add(name)
                grew = True
    return names


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
        if (node.value.startswith('/') or node.value.startswith('\\')
                or _is_drive_prefixed(node.value)):
            # An absolute literal is not a path below the repository root; the
            # discovery loop never keys it, so it needs no anchor. A
            # drive-prefixed or backslash-rooted literal is absolute on
            # Windows and relative on POSIX, so the key would depend on the
            # platform: nothing may be keyed and the caller reports it.
            return None
        # Internal ``..`` collapses here as it does at runtime
        # (``Path('sms/../mod.rs')`` is ``mod.rs``), while a *leading* ``..``
        # is kept: which directory it climbs from is only known once the join
        # above it supplies one (``SMS / '../x.rs'``).
        return _collapsed_segments(
            tuple(part for part in node.value.split('/') if part not in ('', '.')))
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
        return _normalised_segments(left + right)
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
    if isinstance(node, ast.Subscript):
        marker = ('@container', id(node))
        if marker in seen:
            # ``CH = {'k': CH['k']}`` indexes the container it builds; the
            # value is unknowable and following the cycle never terminates.
            return None
        return _container_segments(node, constants, self_path, seen + (marker,))
    return None


def _collapsed_segments(segments):
    """Resolve ``.`` and internal ``..`` in one literal, keeping a leading ``..``.

    ``'sms/../mod.rs'`` is ``mod.rs`` at runtime, so keying ``../mod.rs`` picked
    a file the table never builds. A leading ``..`` is left in place because the
    directory it climbs from comes from the join above it.
    """
    stack = []
    for part in segments:
        if part == '..' and stack and stack[-1] != '..':
            stack.pop()
        elif part not in ('', '.'):
            stack.append(part)
    return tuple(stack)


def _normalised_segments(segments):
    """Resolve ``.`` and ``..`` the way the filesystem does, or None.

    ``SMS / 'a/..' / 'x.rs'`` is ``sms/x.rs`` at runtime; keeping the ``..``
    segment emitted the key ``../x.rs``, which the host never fires on — the
    anchor look-able but useless. Walking above the repository root is not a
    repository path at all and stays unknowable.
    """
    stack = []
    for part in segments:
        if part == '..':
            if not stack:
                return None
            stack.pop()
        elif part not in ('', '.'):
            stack.append(part)
    return tuple(stack)


def _container_element(container, index, constants, seen=()):
    """The AST element a literal index selects, or None when it is unknowable.

    Names are followed through the constant table and subscripts through one
    another, so ``CH['a']['b']`` reaches the inner dictionary instead of
    stopping at the first index. A duplicate dictionary key keeps the *last*
    value, exactly as Python does at runtime: the first-wins reading keyed a
    directory the code never builds, and an anchor declared for the real one
    then looked stale.
    """
    if isinstance(container, ast.Name):
        if container.id in seen or container.id not in constants:
            return None
        if container.id in getattr(constants, 'mutated', frozenset()):
            # ``CH['k'] = ...`` / ``CH.update(...)`` made the literal stale;
            # the value the runtime indexes is not the one written here.
            return None
        return _container_element(constants[container.id], index, constants,
                                  seen + (container.id,))
    if isinstance(container, ast.Subscript):
        marker = ('@container', id(container))
        if marker in seen:
            return None
        inner = _container_element(container.value, container.slice,
                                   constants, seen + (marker,))
        if inner is None:
            return None
        return _container_element(inner, index, constants, seen + (marker,))
    if not isinstance(index, ast.Constant):
        return None
    if isinstance(container, ast.Dict) and isinstance(index.value, str):
        found = None
        for key, value in zip(container.keys, container.values):
            if key is None:
                # ``{'k': 'a', **OTHER}``: the unpacking may overwrite this
                # key at runtime, and only the runtime knows with what.
                return None
            if isinstance(key, ast.Constant) and isinstance(key.value, str):
                if key.value == index.value:
                    found = value
                continue
            spelled = _string_value(key, constants)
            if spelled is None:
                # A key this scanner cannot evaluate — ``{'k': x, K: y}`` with
                # K computed at runtime — may equal the index and overwrite the
                # literal one; only the runtime knows which value wins.
                return None
            if spelled == index.value:
                found = value
        return found
    if isinstance(container, (ast.List, ast.Tuple)) and isinstance(index.value, int):
        elements = container.elts
        if -len(elements) <= index.value < len(elements):
            return elements[index.value]
    return None


def _container_segments(node, constants, self_path, seen):
    """The path a literal container index names, or None.

    ``CH = {'feishu': 'src/pilotdeck-gateway/src/channel/feishu'}`` then
    ``Path(CH['feishu']) / 'mod.rs'`` names a directory the file text spells
    out in full; every other join spelling would be keyed, so this one must be
    too. A key the container does not hold literally (an opaque index, a
    missing key, a container some statement mutates) is left to the callers to
    report.
    """
    element = _container_element(node.value, node.slice, constants, seen)
    if element is None:
        return None
    return _segments(element, constants, self_path, seen)


def _call_segments(node, constants, self_path, seen):
    func = node.func
    if (isinstance(func, ast.Attribute) and func.attr == 'cwd'
            and isinstance(func.value, ast.Name) and func.value.id == 'Path'
            and not node.args):
        if getattr(constants, 'cwd_unknown', False):
            # The table may have chdir'ed away from the repository root; see
            # ``_changes_directory``. Returning None makes the join report.
            return None
        # Hosts run a table from the repository root (the same premise
        # ``_is_relative_path_literal`` rests on), so ``Path.cwd()`` names the
        # root: ``Path.cwd() / 'sms' / 'mod.rs'`` builds the very path
        # ``Path('sms') / 'mod.rs'`` does, and used to build no key at all.
        return ()
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
        out = _normalised_segments(out + part)
        if out is None:
            return None
    return out


def _is_bare_component(value):
    """A single path component that could be a file name.

    ``''``, ``'.'`` and ``'..'`` are not: they are navigation, and the discovery
    loop only keys a *changed file*. Whitespace inside a name is ordinary
    (``'msg 1.json'``); whitespace *around* it is not — ``' / '.join(...)``
    splits into two padded pieces, which are operator-shaped strings rather than
    paths. Rejecting every space used to hide real file names like ``'my file.rs'``
    from the guard entirely: no key, no report, and the table dropped out of the
    discovery list.
    """
    return (bool(value) and value == value.strip() and '/' not in value
            and value not in ('.', '..'))



def _is_fragment_component(value):
    """A component inside a string *fragment* (``+``, ``%``, f-string, ``join``).

    Whitespace anywhere means the fragment is operator- or prose-shaped
    (``' / '.join(...)``, ``'; compilation/zero-test failure is not evidence'``)
    rather than a path piece. A real file name with a space (``'msg 1.json'``)
    is judged by ``_is_bare_component`` at a ``/`` join or a call tail, where the
    position itself says the string is a file name.
    """
    return (_is_bare_component(value)
            and not any(character.isspace() for character in value))


def _path_literal_ending_in_file(value):
    """Whether a string literal ends in a bare file name (``'/x/mod.rs'``).

    ``' / '.join(...)`` and ``f'{a} / {b}'`` carry a slash but no file name;
    treating them as paths would flood the guard with runtime string joins.
    """
    if '/' not in value:
        return False
    return _is_fragment_component(value.rsplit('/', 1)[1])


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
        return any(_is_fragment_component(part) for part in value.split('/'))
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


def _joins_a_path_string(node, constants=None):
    """Whether an ``Add`` / ``%`` / ``.format`` / f-string carries a path literal.

    List concatenation (``[...] + [...]``) is common in tables and has nothing
    to do with paths, so the shape alone is not enough: one operand must be a
    string that is a path fragment (see ``_path_literal_operand``). With
    ``constants`` the operand may be a module constant — ``FMT = '{}/mod.rs'``
    followed by ``FMT % SMS`` builds the same path as spelling it inline, and
    looking only at direct literals left that spelling silent.
    """
    if isinstance(node, ast.JoinedStr):
        operands = node.values
    else:
        operands = (node.left, node.right)
    for operand in operands:
        if constants is not None:
            value = _string_value(operand, constants)
        elif isinstance(operand, ast.Constant) and isinstance(operand.value, str):
            value = operand.value
        else:
            value = None
        if value is not None and _path_literal_operand(value):
            return True
    return False


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
        if not ((_segments(base, constants, self_path) is not None
                 or _indexes_a_path_container(base, constants))
                and (_is_repo_rooted(base, constants)
                     or _indexes_a_path_container(base, constants))):
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
    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
            and node.func.attr == 'cwd'
            and isinstance(node.func.value, ast.Name) and node.func.value.id == 'Path'
            and not node.args):
        # ``Path.cwd()`` is the repository root while the premise holds, and
        # still *starts* at the root when a chdir made it unknowable — either
        # way an unresolvable join from it is a repository hole worth reporting.
        return True
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


def _is_relative_path_literal(value):
    """Whether a string literal names a path below the repository root.

    Tables keep directories in dictionaries too (``CH = {'feishu':
    'src/pilotdeck-gateway/src/channel/feishu'}``) and index them at the join.
    The host runs tables from the repository root, so a relative literal with
    path separators *is* a repository path — but no ``ROOT`` or ``__file__``
    appears in the expression, so every other test said "not a repository
    path" and the join was neither keyed nor reported, while the identical
    literal spelled directly is keyed. Whitespace and absolute paths stay out
    for the same reasons as everywhere else (display strings, non-repository
    paths).
    """
    if not value or value.startswith('/') or value != value.strip():
        return False
    if any(character.isspace() for character in value):
        return False
    segments = value.split('/')
    return len(segments) > 1 and all(segment not in ('', '.', '..') for segment in segments)


def _container_holds_a_path_literal(node, constants, seen=()):
    """Whether a container constant holds a relative path literal, at any depth.

    ``CH = {'a': {'b': 'src/x'}}`` keeps the directory one index deeper than
    the shapes this guard first looked at; recursing into nested containers is
    what makes ``Path(CH['a']['b']) / 'mod.rs'`` read as a repository path
    instead of dropping out of both the key and the report.
    """
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return False
        return _container_holds_a_path_literal(constants[node.id], constants,
                                               seen + (node.id,))
    if isinstance(node, ast.Subscript):
        return _container_holds_a_path_literal(node.value, constants, seen)
    if isinstance(node, (ast.Tuple, ast.List, ast.Set)):
        elements = node.elts
    elif isinstance(node, ast.Dict):
        elements = node.values
    else:
        return False
    return any(
        (isinstance(element, ast.Constant) and isinstance(element.value, str)
         and _is_relative_path_literal(element.value))
        or _container_holds_a_path_literal(element, constants, seen)
        for element in elements)


def _indexes_a_path_container(node, constants):
    """Whether ``node`` reads a directory out of a container constant.

    The directory a join starts at can be written as a literal inside a dict or
    tuple; when the index itself is opaque (``Path(CH[name]) / 'mod.rs'``) the path
    is still a repository path and the join must be reported, not dropped. An
    f-string interpolating such an index (``f'{CH["k"]}/{name}'``) is the same
    build, so it counts too.
    """
    if isinstance(node, ast.Subscript):
        return _container_holds_a_path_literal(node.value, constants)
    if isinstance(node, ast.JoinedStr):
        return any(isinstance(part, ast.FormattedValue)
                   and _indexes_a_path_container(part.value, constants)
                   for part in node.values)
    if isinstance(node, ast.BinOp):
        return _indexes_a_path_container(node.left, constants)
    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
            and node.func.id in ('str', 'Path')):
        return bool(node.args) and _indexes_a_path_container(node.args[0], constants)
    if isinstance(node, ast.Attribute) and node.attr in ('resolve', 'parent',
                                                         'absolute', 'expanduser'):
        return _indexes_a_path_container(node.value, constants)
    return False


def _binding_before(pairs, name, position):
    """``(index, value)`` for the binding ``name`` has at ``position``.

    The last ``name = ...`` before ``position`` is the value the interpreter
    sees there. Reading a later binding instead let ``BASE = parents[4]; ROOT =
    BASE; BASE = parents[5]`` look correct, and a snapshot taken once per
    spelling could not follow a value captured through an alias at all.
    """
    found = None
    for index, (bound, value) in enumerate(pairs):
        if index >= position:
            break
        if bound == name:
            found = (index, value)
    return found


def _levels_above_file(node, pairs, position, seen=()):
    """How many path components ``node`` removes from ``__file__``, or None.

    ``Path(__file__).resolve().parents[5]`` removes six (the file name plus five
    directories); ``FIXTURES.parents[3]`` adds to whatever ``FIXTURES`` already
    removed. Names resolve against the bindings *before* ``position``, so an
    alias captures the value its target had on that line and a later rebinding
    cannot retroactively change it. Only shapes with a statically known depth
    answer; anything else returns None so the caller stays silent rather than
    guessing.
    """
    if isinstance(node, ast.Name):
        if node.id == '__file__':
            return 0
        if node.id in seen:
            return None
        binding = _binding_before(pairs, node.id, position)
        if binding is None:
            return None
        index, value = binding
        return _levels_above_file(value, pairs, index, seen + (node.id,))
    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
            and node.func.id == 'Path' and len(node.args) == 1):
        return _levels_above_file(node.args[0], pairs, position, seen)
    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
            and node.func.attr in ('resolve', 'absolute', 'expanduser')
            and not node.args):
        return _levels_above_file(node.func.value, pairs, position, seen)
    if isinstance(node, ast.Attribute) and node.attr == 'parent':
        inner = _levels_above_file(node.value, pairs, position, seen)
        return None if inner is None else inner + 1
    if (isinstance(node, ast.Subscript) and isinstance(node.value, ast.Attribute)
            and node.value.attr == 'parents'):
        inner = _levels_above_file(node.value.value, pairs, position, seen)
        index = node.slice
        if (inner is None or not isinstance(index, ast.Constant)
                or not isinstance(index.value, int) or index.value < 0):
            return None
        return inner + index.value + 1
    return None


def _root_definition_problems(tree, self_path, root):
    """Report a module-level ``ROOT`` that does not name the repository root.

    Every resolver short-circuits a ``ROOT`` name to "the repository root"
    without reading its definition, so a table that computes it one level short
    (``parents[4]``) builds ``src/<file>`` at runtime while this scanner keys
    ``<file>`` — one component, dropped as "no discovery key", with no anchor
    demanded and nothing reported. The depth is checked only where it is
    statically visible from ``__file__``; every other spelling keeps the old
    assumption, because guessing would fail legitimate tables.
    """
    if root != FIXTURES:
        # ``self_path`` is relative to ``root``, so it only has repository
        # depth when the scan really walks the fixtures tree.
        return []
    expected = len(self_path)
    pairs, _ = _module_level_pairs(tree)
    problems = []
    for name in sorted(ROOT_NAMES):
        for position, (bound, value) in enumerate(pairs):
            if bound != name:
                continue
            # **Every** binding is checked, not just the last one: a table
            # that computes ``ROOT`` one level short and only rebinds it
            # correctly afterwards has already built its joins against the
            # wrong root, and the last-wins reading reported nothing. Two
            # bindings spelled the same way are not one binding either — the
            # second may resolve through a dependency that changed in between
            # (``BASE = parents[4]; ROOT = BASE; BASE = parents[5]; ROOT =
            # BASE``), so the check is per statement rather than per spelling.
            spelling = ast.unparse(value)
            levels = _levels_above_file(value, pairs, position)
            if levels is None or levels == expected:
                continue
            message = (f'{name} does not name the repository root (it resolves '
                       f'{levels} levels above this file, expected {expected}): '
                       f'{spelling}')
            if message not in problems:
                problems.append(message)
    return problems


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
                if alias.asname:
                    aliases[alias.asname] = alias.name
                else:
                    # ``import os.path`` binds the name ``os`` to the *package*,
                    # not to the submodule: ``os.chdir`` still exists and
                    # ``os.path.join`` still resolves. Mapping the bare name to
                    # the dotted spelling made ``os.path.join`` read as
                    # ``os.path.path.join`` and hid every join call in a table
                    # that spells the import that way.
                    top = alias.name.split('.')[0]
                    aliases[top] = top
        elif isinstance(node, ast.ImportFrom) and node.module and not node.level:
            for alias in node.names:
                if alias.name != '*':
                    aliases[alias.asname or alias.name] = f'{node.module}.{alias.name}'
    return aliases


def _spells_a_repository_path(node, constants, seen=()):
    """Whether the *value* of ``node`` is a repository path, not data about one.

    Only the value-producing spine is followed: ``str(...)`` / ``Path(...)``
    wrappers, ``/`` joins, ``.resolve()`` / ``.parent`` and module constants.
    A call that merely takes a repository path as one of its arguments —
    ``difflib.unified_diff(..., fromfile=str(SOURCE))``, ``sorted(unknown)``
    next to a source path — hands back text or a list, not the path, and must
    not read as a path join.
    """
    if isinstance(node, ast.Name):
        if node.id in ROOT_NAMES or node.id == '__file__':
            return True
        if node.id in seen or node.id not in constants:
            return False
        return _spells_a_repository_path(constants[node.id], constants, seen + (node.id,))
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        return True
    if isinstance(node, ast.Subscript):
        # ``join(CH['k'], name)``: the directory comes out of a container, so
        # the value is a repository path even though no ``ROOT`` appears.
        return _container_holds_a_path_literal(node.value, constants)
    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
            and node.func.id in ('str', 'Path') and node.args):
        return _spells_a_repository_path(node.args[0], constants, seen)
    if isinstance(node, ast.Attribute):
        return _spells_a_repository_path(node.value, constants, seen)
    return False


def _joined_elements(node):
    """The values a ``join`` call actually concatenates.

    ``'/'.join((a, b))`` joins the tuple's elements, not the tuple; a
    comprehension joins its element expression. Anything else is one opaque
    value and is judged as such.
    """
    if len(node.args) != 1:
        return list(node.args)
    argument = node.args[0]
    if isinstance(argument, (ast.Tuple, ast.List, ast.Set)):
        return list(argument.elts)
    if isinstance(argument, (ast.GeneratorExp, ast.ListComp, ast.SetComp)):
        return [argument.elt]
    return [argument]


_STDLIB_JOIN_CALLS = frozenset({'os.path.join', 'posixpath.join', 'ntpath.join'})


def _dotted_spelling(node):
    """The dotted name an ``os.path``-style expression spells, or None."""
    parts = []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if not isinstance(node, ast.Name):
        return None
    parts.append(node.id)
    return '.'.join(reversed(parts))


def _joined_tail(node):
    """The piece a join call appends: the last element when a container is passed.

    ``os.sep.join(('sms', 'mod.rs'))`` joins the tuple's elements, so the file
    name is its last one. A *name* bound to a container is deliberately not
    followed here: ``' '.join(command)`` over a list of cargo arguments is not
    a path build, and following it made the guard report it.
    """
    if not node.args:
        return None
    argument = node.args[-1]
    if isinstance(argument, (ast.Tuple, ast.List)):
        return argument.elts[-1] if argument.elts else None
    return argument


_FILESYSTEM_JOIN_CALLS = _STDLIB_JOIN_CALLS | {'os.sep.join'}


def _filesystem_join_text(node, constants):
    """The text a join of string literals builds, or None.

    ``os.path.join('sms', 'mod.rs')`` produces ``sms/mod.rs`` from pieces the
    table spells in full, yet the literal ``sms/mod.rs`` appears nowhere in the
    file, so the discovery loop never schedules the table when the file
    changes. Returning the joined text lets the caller key it and demand an
    anchor, which is what ``Path('sms') / 'mod.rs'`` already gets.

    Two kinds of join count, and they do **not** share reset semantics.
    ``os.path.join`` / ``posixpath.join`` / ``ntpath.join`` are filesystem
    joins: an argument that starts with ``/`` discards everything before it
    (``os.path.join('sms', '/tmp', 'mod.rs')`` is ``/tmp/mod.rs``).
    ``os.sep.join`` is a plain ``str.join`` — ``os.sep`` is a string — so
    ``os.sep.join(('sms', '/tmp', 'mod.rs'))`` is ``sms//tmp/mod.rs`` inside
    the repository; applying the filesystem reset there dropped a call that
    really does build a path inside the tree. The analyzer models the POSIX
    separator the discovery keys are spelled with.

    The *string* spellings (``+``, ``'/'.join``, ``%``, f-strings) are
    deliberately left out of this resolution: the same shape builds test-cell
    labels in ``network-timing/negative_private.py`` (``p + '/' + phase +
    '/idle/' + order``), and keying those would demand anchors for things that
    are not paths at all. A string join is only followed when a repository root
    or a repository-directory base proves it is a path. A runtime operand — the
    boot and temp directories ``os.path.join(_boot_root, ...)`` start from —
    yields None and stays out of the repository.
    """
    if not isinstance(node, ast.Call):
        return None
    func = node.func
    if not isinstance(func, ast.Attribute) or func.attr != 'join':
        return None
    spelling = _dotted_spelling(func)
    if spelling in _STDLIB_JOIN_CALLS:
        resets = True
    elif spelling == 'os.sep.join':
        resets = False
    else:
        return None
    pieces = []
    for element in _joined_elements(node):
        value = _string_value(element, constants)
        if value is None:
            return None
        if resets and _resets_join(value):
            # ``os.path.join('sms', '/tmp', 'mod.rs')`` is ``/tmp/mod.rs``:
            # the absolute argument discards everything before it. Joining the
            # pieces verbatim demanded an anchor for ``tmp/mod.rs``, a file the
            # code never builds.
            pieces = [value]
        else:
            pieces.append(value)
    return '/'.join(pieces)


def _is_drive_prefixed(value):
    """Whether a join argument carries a Windows drive prefix (``C:/x``).

    ``C:/tmp`` is absolute on Windows and relative on POSIX, so a join carrying
    one builds a different key per platform; nothing may be keyed and the shape
    is reported. The drive-*relative* spelling ``C:tmp`` is just as
    platform-dependent (on the same drive it means ``tmp`` under the drive's
    current directory, on POSIX a literal ``C:tmp`` component), so any
    letter-colon prefix counts.
    """
    return len(value) > 1 and value[0].isalpha() and value[1] == ':'


def _is_drive_segment(part):
    """Whether one joined component starts with a Windows drive (``C:``)."""
    return len(part) >= 2 and part[0].isalpha() and part[1] == ':'


def _resets_join(value):
    """Whether a *filesystem* join argument discards the pieces before it."""
    if value.startswith('/') or value.startswith('\\'):
        return True
    # ``C:\\x`` is absolute on Windows even when the table runs elsewhere.
    return _is_drive_prefixed(value)


def _relative_path_segments(node, constants):
    """Segments of a repository path spelled with relative literals only.

    The host runs tables from the repository root, so a path assembled with
    ``os.path.join`` from relative literals *is* a repository path even though
    no ``ROOT`` or ``__file__`` appears — the same premise
    ``_is_relative_path_literal`` already applies to a container's literal
    element. Anything that only exists at runtime returns None: it is neither
    keyed nor guessed.
    """
    text = _filesystem_join_text(node, constants)
    if not text or _resets_join(text):
        return None
    if any(_is_drive_segment(part) or part.startswith('\\')
           for part in text.split('/')):
        return None
    segments = _normalised_segments(
        tuple(part for part in text.split('/') if part not in ('', '.')))
    if segments is None or len(segments) < 2:
        return None
    # A space *inside* a component is ordinary (``'my file.rs'`` is a legal
    # file name the host's fixed-string grep matches); a component padded with
    # whitespace is an operator-shaped string (``' / '``), not a path piece.
    if any(not part or part != part.strip() for part in segments):
        return None
    return segments


def _is_path_join_call(func, constants, aliases):
    """Whether a call is a string join, in any of the spellings tables use.

    ``'/'.join(parts)`` is the common one, and its receiver may be a module
    constant (``SEP.join(...)``). ``from os.path import join`` reaches the same
    unmodelled stdlib function by bare name, and ``from os.path import join as
    j`` reaches it through the alias map — exactly like the enumeration helpers,
    which is why that alias check already exists. All of these build a path this
    scanner cannot follow, so a repository path among the arguments must be
    reported rather than silently dropped.
    """
    if isinstance(func, ast.Attribute):
        return (func.attr == 'join'
                and _string_value(func.value, constants) is not None)
    if isinstance(func, ast.Name):
        if func.id == 'join':
            return True
        qualified = aliases.get(func.id)
        if not qualified:
            return False
        module, _, name = qualified.rpartition('.')
        return name == 'join' and module in FS_MODULES
    return False


def _names_sys_path(node, aliases, bound):
    """Whether an expression denotes the interpreter's ``sys.path`` list.

    ``sys.path`` is reached through ``import sys as s`` (``s.path``), through
    ``from sys import path`` (a bare name the import map records), and through
    a plain binding (``P = sys.path``) — the alias map alone only covers the
    module spellings.
    """
    spelling = _dotted_spelling(node)
    if spelling is not None:
        head, _, rest = spelling.partition('.')
        module = aliases.get(head)
        if module is not None:
            spelling = f'{module}.{rest}' if rest else module
        if spelling == 'sys.path':
            return True
    return isinstance(node, ast.Name) and node.id in bound


def _sys_path_target(node, aliases, bound=frozenset()):
    """Whether an *assignment target* writes ``sys.path`` itself.

    Only a spelling of ``sys.path`` (through the import map) or a name bound to
    it (``P = sys.path``; ``bound``) can modify the interpreter's list: other
    plain names are rebound, never mutated — ``P = sys.path; P = [...]`` leaves
    ``sys.path`` alone. Subscript targets (``sys.path[:] = ...``,
    ``sys.path[0:0] = ...``, ``sys.path[1] = ...``) and starred targets write
    into the list itself.
    """
    while isinstance(node, (ast.Subscript, ast.Starred)):
        node = node.value
    return _names_sys_path(node, aliases, bound)


def _sys_path_bindings(tree, aliases):
    """Names a table binds to ``sys.path`` itself (``P = sys.path``).

    Closed over assignment chains to a fixpoint: ``P = sys.path; Q = P`` gives
    ``Q`` the same meaning, and reading only the direct binding left
    ``Q.append(...)`` as a silent insertion of a repository directory.
    """
    bound = set()
    pairs = []
    for node in ast.walk(tree):
        if not isinstance(node, (ast.Assign, ast.AnnAssign)):
            continue
        pairs.extend(_assignment_pairs(node)[0])
    grew = True
    while grew:
        grew = False
        for name, value in pairs:
            if name in bound:
                continue
            if _names_sys_path(value, aliases, frozenset(bound)):
                bound.add(name)
                grew = True
    return frozenset(bound)


def _inserted_elements(value, constants=None, seen=()):
    """The directory expressions an insertion adds (container elements too).

    A named list — ``DIRS = [str(FIXTURES / '_shared')]`` followed by
    ``sys.path.extend(DIRS)`` — keeps the same element behind a constant, so
    the name is followed through the table exactly as ``_string_value`` does.
    Concatenation (``sys.path + [d]``, ``[d] + sys.path``, ``sys.path |= [d]``)
    and ``*`` expansion (``extend([*DIRS])``) add the elements of both sides:
    the repository directory is an element of the list that ends up in
    ``sys.path`` either way.
    """
    if isinstance(value, (ast.List, ast.Tuple, ast.Set)):
        elements = []
        for element in value.elts:
            if isinstance(element, ast.Starred):
                # ``[*DIRS]`` / ``[*[dir]]``: ``*`` splices the container's
                # elements, so the insertion adds whatever that container holds
                # rather than the container itself.
                elements.extend(_inserted_elements(element.value, constants, seen))
            else:
                elements.append(element)
        return elements
    if isinstance(value, ast.BinOp) and isinstance(value.op, (ast.Add, ast.BitOr)):
        return (_inserted_elements(value.left, constants, seen)
                + _inserted_elements(value.right, constants, seen))
    if isinstance(value, ast.Starred):
        return _inserted_elements(value.value, constants, seen)
    if (constants is not None and isinstance(value, ast.Name)
            and value.id not in seen and value.id in constants):
        if value.id in getattr(constants, 'mutated', frozenset()):
            # ``DIRS = []; DIRS.append(dir)``: the container no longer holds the
            # literal the scanner read, so expanding it would report the *old*
            # contents and hide the real one. The name itself is the element;
            # the sys.path check reports a mutated element as unknowable.
            return [value]
        return _inserted_elements(constants[value.id], constants, seen + (value.id,))
    return [value]


# Trees that only exist inside this repository. An absolute literal naming one
# of them can import a repository helper, so it is not a third-party directory
# however it is spelled; the marker is used instead of the checkout path so the
# reading survives running from a copy of the tree.
REPOSITORY_MARKERS = ('/src/pilotdeck-gateway/', '/scripts/', '/specs/',
                      '/src/pilotdeck-cli/', '/src/pilotdeck-core/')


def _is_inside_the_repository_text(text):
    """Whether an absolute path literal names a tree of this repository."""
    normalised = text.replace('\\', '/').rstrip('/')
    return any(marker in normalised + '/' for marker in REPOSITORY_MARKERS)


def _carries_a_directory_literal(value, constants, changes_directory=False,
                                 aliases=None):
    """Whether one inserted value names a directory this scanner cannot search.

    An absolute literal outside the repository (``/tmp/thirdparty``) is the
    same harmless insertion as any third-party ``sys.path`` entry: no module
    under it can hide a repository file, so it is not reported — but an
    absolute literal that *does* name one of the repository's own trees is a
    real helper directory, and the same goes for a bare single component once
    the table has changed directory (``os.chdir('…/fixtures');
    sys.path.insert(0, '_shared')``). A ``..``-prefixed literal is read against
    the repository root the host starts tables in, so it points outside.
    """
    aliases = aliases or {}
    text = _string_value(value, constants)
    if text is None:
        text = _filesystem_join_text(value, constants)
    if isinstance(value, ast.Call) and isinstance(value.func, ast.Attribute):
        spelling = _dotted_spelling(value.func)
        if spelling is not None:
            head, _, rest = spelling.rpartition('.')
            if head:
                spelling = f'{aliases.get(head, head)}.{rest}'
        if spelling in _FILESYSTEM_JOIN_CALLS:
            # ``sys.path.insert(0, os.path.join(ROOT, 'src/…'))``: the joined
            # text is not a literal of its own, but a repository directory
            # among the pieces is the same insertion.
            return any(_carries_a_directory_literal(element, constants,
                                                   changes_directory, aliases)
                       for element in _joined_elements(value))
    if text is not None:
        if text.startswith('..'):
            return False
        if (text.startswith('/') or text.startswith('\\')
                or _is_drive_prefixed(text)):
            return _is_inside_the_repository_text(text)
        if changes_directory and text not in ('', '.') and '/' not in text \
                and '\\' not in text:
            return True
        return '/' in text or '\\' in text
    return _spells_a_repository_path(value, constants)


def _inserts_a_repository_directory_into_sys_path(node, constants, aliases,
                                                  bound=frozenset(),
                                                  changes_directory=False):
    """Whether a call adds a repository directory to ``sys.path``.

    ``sys.path.insert(0, str(FIXTURES / '_shared'))`` makes a bare ``import
    negative_control`` resolve; ``_shared_module_keys`` only searches the
    directories it knows (the fixture tree and the table's own), so the
    helper's discovery key is never derived and the table looks self-contained.
    A helper changed there would not schedule the table. Any path-carrying
    argument to ``sys.path.insert/append/extend`` is therefore reported,
    including the elements of a container passed to ``extend`` — and the
    target is resolved through module aliases and plain bindings, so
    ``import sys as s; s.path.append(...)`` and ``P = sys.path; P.append(...)``
    are the same call.
    """
    if not isinstance(node, ast.Call) or not node.args:
        return False
    func = node.func
    if not isinstance(func, ast.Attribute) or func.attr not in (
            'insert', 'append', 'extend'):
        return False
    if not _names_sys_path(func.value, aliases, bound):
        return False
    arguments = list(node.args)
    if func.attr == 'insert':
        # ``sys.path.insert(0, directory)`` — the first argument is the index.
        arguments = arguments[1:]
    elements = [_element for argument in arguments
                for _element in _inserted_elements(argument, constants)]
    if any(_carries_a_directory_literal(element, constants, changes_directory,
                                        aliases)
           for element in elements):
        return True
    # A named list the table mutates after building it (``D = [];
    # D.append(dir); sys.path.extend(D)``) no longer holds the literal the
    # scanner read, so its contents are unknowable and the insertion is
    # reported rather than assumed harmless.
    return _sys_path_elements_name_a_mutated_container(elements, constants)


def _sys_path_elements_name_a_mutated_container(elements, constants):
    """Whether an inserted element is a name the table mutates afterwards."""
    mutated = getattr(constants, 'mutated', frozenset())
    return any(isinstance(element, ast.Name) and element.id in mutated
               for element in elements)


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
            if func.attr == 'join' and _dotted_spelling(func) in _FILESYSTEM_JOIN_CALLS:
                tail = _joined_tail(node)
                return tail is not None and _bare_tail(tail, constants, seen)
            if func.attr in PATH_CALL_ATTRS:
                return _bare_tail(func.value, constants, seen)
    return False


def _qualified_spelling(node, aliases):
    """The dotted spelling of an expression with its head resolved by import.

    ``from pathlib import Path as P`` makes ``P(ROOT, 'x')`` the same call as
    ``Path(ROOT, 'x')``; the import map is the only place that difference
    lives, so every spelling check goes through here.
    """
    spelling = _dotted_spelling(node)
    if spelling is None:
        return None
    head, _, rest = spelling.partition('.')
    module = aliases.get(head) if aliases else None
    if module is None or module == head:
        return spelling
    return f'{module}.{rest}' if rest else module


def _trailing_literal_text(node, constants, seen=(), aliases=None):
    """The literal path text an expression ends with, in path order.

    A discovery key is the last two components, yet whitespace anywhere in the
    changed path makes it unschedulable (the host splits the whole path on
    whitespace), so the full tail is reconstructed: ``os.path.join(ROOT,
    'my dir', 'mod.rs')`` yields ``my dir/mod.rs`` even though ``ROOT`` itself
    has no literal value. Import aliases are resolved so ``from pathlib import
    Path as P`` and ``import os as o`` reach the same shapes.
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return ''
        return _trailing_literal_text(constants[node.id], constants,
                                      seen + (node.id,), aliases)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        return (_trailing_literal_text(node.left, constants, seen, aliases) + '/'
                + _trailing_literal_text(node.right, constants, seen, aliases))
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name):
            if _qualified_spelling(func, aliases) not in ('Path', 'pathlib.Path'):
                return ''
            parts = node.args
        elif isinstance(func, ast.Attribute):
            spelling = _qualified_spelling(func, aliases)
            if func.attr == 'join' and spelling in _FILESYSTEM_JOIN_CALLS:
                parts = _joined_elements(node)
            elif func.attr in ('joinpath', 'with_name'):
                parts = [func.value] + list(node.args)
            elif func.attr in PATH_CALL_ATTRS:
                return _trailing_literal_text(func.value, constants, seen, aliases)
            else:
                # ``template.format(...)`` / ``.upper()``: the result is text,
                # not a path build, so it contributes no trailing components.
                return ''
        else:
            return ''
        return '/'.join(_trailing_literal_text(part, constants, seen, aliases)
                        for part in parts)
    if isinstance(node, ast.Attribute):
        return _trailing_literal_text(node.value, constants, seen, aliases)
    return ''


def _key_contains_whitespace(node, constants, self_path=(), aliases=None):
    """Whether the discovery key this path builds contains whitespace.

    The host reads its changed-file list through ``for f in $changed``, so a
    changed path like ``src/…/my dir/sms/mod.rs`` arrives as two fragments and
    the key ``sms/mod.rs`` inside it matches nothing. **Every** component is
    therefore checked, not only the last two: whitespace earlier in the path
    breaks the same split. ``SMS / 'my file.rs'`` ends in a bare component and
    ``resolve`` reports it, but a path that spells the whole thing inline —
    ``ROOT / 'src/…/channel/sms/my file.rs'`` — ends in a slash-carrying
    literal, so ``_bare_tail`` is false and the expression used to be dropped
    in silence.

    Two readings keep the net off ordinary strings: an absolute piece resets
    the join, so what precedes it is not part of the path (``os.path.join(ROOT,
    '/tmp/third party/x')`` names nothing inside the repository), and a tail
    that is not a file name — ``os.sep.join(('test result', 'passed/failed
    cases'))`` — is display text unless a repository root proves otherwise.
    """
    rooted = _is_repo_rooted(node, constants)
    segments = _segments(node, constants, self_path)
    if segments is None:
        segments = _relative_path_segments(node, constants)
    if segments is None:
        if not rooted:
            return False
        text = _trailing_literal_text(node, constants, aliases=aliases)
        if '//' in text or any(_is_drive_prefixed(part)
                               for part in text.split('/')):
            # An absolute argument discarded everything before it: the result
            # is not a path under the repository root any more.
            return False
        segments = tuple(part for part in text.split('/') if part not in ('', '.'))
    if len(segments) < 2:
        return False
    if '.' not in segments[-1] and not rooted:
        # No file name at the end and no repository root in sight: a joined
        # label such as ``'passed/failed cases'``, not a discovery key.
        return False
    return any(character.isspace()
               for segment in segments for character in segment)


def _is_path_shape(node, constants, aliases=None):
    """Whether an expression is a path this scanner must evaluate or report.

    Only shapes that end in a bare file name qualify. A join whose last
    component is an opaque runtime value (``root / relative``, ``''.join(rows)``)
    cannot be a repository file key, so it is neither keyed nor reported; the
    alternative — treating every division and every ``.join`` as a path — makes
    the guard cry wolf on ordinary string handling. A path whose derived key
    contains whitespace is the exception: it is never schedulable, so it has to
    be reported however its directory is spelled.
    """
    if _key_contains_whitespace(node, constants, aliases=aliases):
        return True
    if isinstance(node, ast.BinOp):
        if isinstance(node.op, ast.Div):
            return _bare_tail(node, constants)
        return _joins_a_path_string(node, constants)   # ``+`` / ``%`` with a path literal
    if isinstance(node, ast.JoinedStr):
        return _joins_a_path_string(node, constants)
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name):
            return (_qualified_spelling(func, aliases) in ('Path', 'pathlib.Path')
                    and bool(node.args) and _bare_tail(node, constants))
        if isinstance(func, ast.Attribute):
            if func.attr in PATH_CALL_ATTRS:
                return _bare_tail(node, constants)
            if func.attr == 'join':
                tail = _joined_tail(node) if node.args else None
                return tail is not None and _bare_tail(tail, constants)
            if func.attr in ('format', 'format_map'):
                value = _string_value(func.value, constants)
                return value is not None and _path_literal_ending_in_file(value)
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
    unfollowable = _unfollowable_module_names(tree, constants)
    local_ambiguous = _ambiguous_local_names(tree, constants)
    shadowed = _shadowed_names(tree) | unfollowable | local_ambiguous
    aliases = _module_aliases(tree)
    sys_path_bound = _sys_path_bindings(tree, aliases)
    repository_returns = _repo_returning_functions(tree, constants)
    tainted = _repository_parameters(tree, constants, repository_returns)
    keys = {}
    problems = list(_root_definition_problems(tree, self_path, root))

    def bare_literal_directory(node):
        # A filesystem join whose head is a single bare literal component
        # (``os.path.join('sms', name)``) starts at a directory whose name the
        # text spells out; the file name is runtime, so the ``sms/<file>`` key
        # is unknowable. ``relative_directory`` deliberately rejects a single
        # component (``Path(MODES['fast']) / name`` names a mode), so only a
        # *literal* head counts here — a container index or a name never does.
        if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                and node.func.id == 'Path' and len(node.args) == 1
                and not node.keywords):
            node = node.args[0]
        value = _string_value(node, constants)
        return value is not None and _is_bare_component(value)

    def join_piece_is_opaque(element):
        """Whether one argument of a join hides the segment it contributes.

        A runtime value is unknowable, and a drive-prefixed literal
        (``C:/tmp``) is absolute on Windows and relative on POSIX — the key
        depends on the platform, so nothing may be keyed.
        """
        value = _string_value(element, constants)
        if value is not None and _is_drive_prefixed(value):
            return True
        return _segments(element, constants, self_path) is None

    def opaque_join_segment(node):
        """Whether a join mixes a known directory with runtime pieces.

        ``os.path.join('src/…/channel', name, 'mod.rs')`` builds
        ``…/sms/mod.rs`` when ``name`` is ``sms``; the directory half is in the
        text and the file name is not, so no anchor can carry the key and the
        discovery loop never schedules the table.

        An absolute argument of a *filesystem* join keeps the call silent
        instead of reporting it: ``os.path.join('sms', '/tmp', 'mod.rs')`` is
        ``/tmp/mod.rs``, outside the repository, so no discovery key exists.
        A drive-prefixed piece is different and is reported — see
        ``join_piece_is_opaque``.
        """
        if not isinstance(node, ast.Call):
            return False
        func = node.func
        if not (isinstance(func, ast.Attribute) and func.attr == 'join'
                and _string_value(func.value, constants) is None):
            return False
        elements = _joined_elements(node)
        if len(elements) < 2:
            return False
        if _dotted_spelling(func) in _STDLIB_JOIN_CALLS:
            for element in elements:
                value = _string_value(element, constants)
                if value is not None and value.startswith('/'):
                    return False
        base = elements[0]
        if not (relative_directory(base) is not None
                or bare_literal_directory(base)
                or _is_repo_rooted(base, constants, shadowed=shadowed)
                or _indexes_a_path_container(base, constants)):
            return False
        return any(join_piece_is_opaque(element) for element in elements[1:])

    def relative_directory(node):
        # A directory written entirely with relative literals: the host runs
        # tables from the repository root, so ``os.path.join('sms', name)``
        # starts at a real repository directory even though no ``ROOT``
        # appears in the expression.
        segments = _segments(node, constants, self_path)
        if segments is None:
            segments = _relative_path_segments(node, constants)
        # One component is a label, not a repository path the discovery loop
        # could ever key: ``Path(MODES['fast']) / name`` names a mode, and the
        # guard has always stayed silent on it. Only a spelled-out ``dir/…``
        # counts as a repository directory.
        return segments if segments and len(segments) > 1 else None

    def repo_rooted(node):
        # A directory that only reaches the join through a parameter or a call
        # return is still a repository path: the loop cannot key it from the
        # text either, so it has to be reported like any other unsupported base.
        return (_is_repo_rooted(node, constants, shadowed=shadowed)
                or _carries_repository_path(node, tainted, repository_returns)
                or _indexes_a_path_container(node, constants)
                or relative_directory(node) is not None)

    def report_whitespace_key(expression):
        # The discovery loop reads its changed-file list through ``for f in
        # $changed``, so a path with a space arrives as two fragments and
        # matches no branch at all. An anchor spelled with the space could
        # never fire, so the key is not merely blind — it is unschedulable,
        # and accepting it would turn a real hole into a green tick.
        problems.append('whitespace in a discovery key cannot be scheduled '
                        '(the discovery loop splits the changed-file list on '
                        f'whitespace): {expression}')

    def record(segments, expression):
        # A single component is not a discovery key: the loop always keys
        # ``dir/file``. Every caller reports it here rather than at its own
        # call site, so a comprehension and a ``/`` join agree. Returns whether
        # a key was recorded, so a caller can tell "resolved and keyed" from
        # "nothing came out of it".
        if len(segments) < 2:
            problems.append(f'no discovery key (needs a directory and a file name): {expression}')
            return False
        if any(character.isspace() for segment in segments for character in segment):
            report_whitespace_key(expression)
            return False
        keys.setdefault('/'.join(segments[-2:]), []).append(expression)
        return True

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
        if name in unfollowable:
            # ``A, *rest = ROOT / 'a', ROOT / 'b'``: the names cannot be paired
            # with their values, so the directory behind this use is unknown.
            # Saying so is more useful than calling it a rebinding.
            problems.append('unsupported unpacking of a repository path (this '
                            f'scanner cannot pair the name with a value): '
                            f'{name}: {expression}')
            return
        if name in local_ambiguous:
            problems.append('ambiguous name assigned different values in '
                            f'several places (this scanner cannot tell which '
                            f'one is used): {name}: {expression}')
            return
        problems.append('ambiguous module-level name rebound elsewhere: '
                        f'{name}: {expression}')

    def report_unresolvable(expression):
        problems.append('unresolvable repository path (this scanner cannot '
                        f'follow it): {expression}')

    def resolve(node, expression):
        segments = _segments(node, constants, self_path)
        if segments is None:
            segments = _relative_path_segments(node, constants)
        if segments is None:
            if _key_contains_whitespace(node, constants, self_path, aliases):
                # ``os.path.join(ROOT, '…/my file.rs')``: the join spelling is
                # not in the resolver's family, but its key is just as
                # unschedulable as a resolved one.
                report_whitespace_key(expression)
                return False
            ambiguous = ambiguous_names(node)
            if ambiguous:
                report_ambiguous(ambiguous[0], expression)
            elif repo_rooted(node) or opaque_join_segment(node):
                report_unresolvable(expression)
            return False
        if (getattr(constants, 'cwd_unknown', False)
                and not _is_repo_rooted(node, constants, shadowed=shadowed)):
            # ``os.chdir(...)`` moved the process away from the repository
            # root, so a path spelled relative to it no longer starts there:
            # ``os.chdir('…/channel/sms'); Path('mod.rs')`` reads
            # ``sms/mod.rs``, and the single-component result used to be
            # dropped as "no discovery key" while nothing was reported.
            report_unresolvable(expression)
            return False
        if not _bare_tail(node, constants):
            # The directory is spelled inline, so the key is findable in the
            # file text — unless the key itself carries whitespace, in which
            # case the host can never schedule it (see ``record``).
            if _key_contains_whitespace(node, constants, self_path, aliases):
                report_whitespace_key(expression)
            return False
        return record(segments, expression)

    def report_runtime_tail(node):
        """Report a join whose directory is known but whose file name is not.

        ``SMS / name`` — with ``name`` a parameter, a runtime loop variable or a
        comprehension source this scanner cannot enumerate — is the same hole as
        ``SMS.glob('*.rs')``: the loop needs a ``dir/file`` literal the text does
        not contain. Only the outermost join of a chain reports (an inner opaque
        prefix is consumed by the join above it), and only when the directory
        half resolves, so ``ROOT / PICK[key] / 'x.rs'`` still reports once, at
        the join that is actually opaque.
        """
        if (_segments(node.left, constants, self_path) is None
                and not _indexes_a_path_container(node.left, constants)):
            return
        if _segments(node.right, constants, self_path) is not None:
            return
        if not (repo_rooted(node) or relative_directory(node.left) is not None
                or bare_literal_directory(node.left)):
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
        resolves and a joined argument does not.

        A runtime segment is not only ever the *last* one:
        ``Path('src/…/channel', name, 'mod.rs')`` and
        ``…joinpath(name, 'mod.rs')`` build ``…/sms/mod.rs`` when ``name`` is
        ``sms``, so the literal tail alone proved nothing. Every argument after
        the directory is checked for the same reason the ``os.path.join``
        spelling above always checked them.
        """
        func = node.func
        if isinstance(func, ast.Attribute) and func.attr in ('joinpath', 'with_name'):
            base = func.value
            arguments = node.args
        elif (isinstance(func, ast.Attribute) and func.attr == 'join'
                and _string_value(func.value, constants) is None):
            # ``os.path.join(SMS, name)`` / ``os.sep.join((str(SMS), name))``:
            # the dotted stdlib spellings. They are neither a string receiver
            # (``'/'.join(...)``, reported by ``_is_path_join_call``) nor one of
            # the ``pathlib`` methods above, and ``_is_path_shape`` only looks at
            # ``join`` calls ending in a bare literal — so a runtime file name
            # here produced no key and no report at all, while every sibling
            # spelling of ``SMS / name`` was caught.
            elements = _joined_elements(node)
            if len(elements) < 2:
                return
            if _dotted_spelling(func) in _STDLIB_JOIN_CALLS:
                for element in elements:
                    value = _string_value(element, constants)
                    if value is not None and value.startswith('/'):
                        # ``os.path.join('sms', '/tmp', 'mod.rs')`` is
                        # ``/tmp/mod.rs``: outside the repository, so the call
                        # names no discovery key at all.
                        return
            base = elements[0]
            arguments = elements[1:]
        elif (isinstance(func, ast.Name) and func.id == 'Path'
                and len(node.args) >= 2 and not node.keywords):
            base = node.args[0]
            arguments = node.args[1:]
        else:
            return
        if not (_is_repo_rooted(base, constants, shadowed=shadowed)
                or _carries_repository_path(node, tainted, repository_returns)
                or _indexes_a_path_container(base, constants)
                or relative_directory(base) is not None
                or bare_literal_directory(base)):
            return
        if not any(join_piece_is_opaque(argument) for argument in arguments):
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
                if (_joins_a_path_string(node, constants) or separators
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
                if _is_path_shape(node, constants, aliases):
                    resolve(node, ast.unparse(node))
                else:
                    report_runtime_tail(node)
            elif _is_path_shape(node, constants, aliases):
                resolve(node, ast.unparse(node))
            self.generic_visit(node)

        def visit_JoinedStr(self, node):
            joined = _joins_a_path_string(node, constants)
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
                    if not ((_segments(target, constants, self_path) is not None
                             and _is_repo_rooted(target, constants))
                            or _indexes_a_path_container(target, constants)):
                        continue
                    if (isinstance(following, ast.Constant)
                            and isinstance(following.value, str)
                            and '/' in following.value):
                        joined = True
                        break
                    # ``f'{SMS}{SEP}mod.rs'``: the separator is interpolated, so
                    # the fragment that follows the directory is a formatted
                    # value rather than a literal. It is still a path build when
                    # that value resolves to a string carrying a slash.
                    if isinstance(following, ast.FormattedValue):
                        piece = _string_value(following.value, constants)
                        if piece is not None and '/' in piece:
                            joined = True
                            break
            if joined and (repo_rooted(node)
                           or _indexes_a_path_container(node, constants)):
                problems.append('unsupported f-string path of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            self.generic_visit(node)

        def report_sys_path_write(self, kind, node):
            problems.append(f'unsupported sys.path {kind} of a repository '
                            'directory (a module imported from it is '
                            'invisible to this scanner): '
                            f'{ast.unparse(node)}')

        def assigns_sys_path(self, node):
            """Whether an assignment writes into ``sys.path`` itself."""
            if isinstance(node, ast.Assign):
                targets = node.targets
            else:
                targets = [node.target]
            if not any(_sys_path_target(target, aliases, sys_path_bound)
                       for target in targets):
                return False
            return self.sys_path_write_carries_a_directory(node.value)

        def sys_path_write_carries_a_directory(self, value):
            """Whether the value stored into ``sys.path`` adds a repository dir.

            The elements are read exactly like a call's arguments: a literal
            naming a repository directory counts, and so does a name the table
            mutates afterwards (``DIRS = []; DIRS.append(dir)``), whose
            contents are no longer the literal the scanner read. Sharing this
            with the ``insert/append/extend`` branch keeps the two spellings
            from drifting apart.
            """
            elements = _inserted_elements(value, constants)
            changed = getattr(constants, 'cwd_unknown', False)
            if any(_carries_a_directory_literal(element, constants, changed,
                                                aliases)
                   for element in elements):
                return True
            return _sys_path_elements_name_a_mutated_container(elements,
                                                               constants)

        def visit_Assign(self, node):
            if self.assigns_sys_path(node):
                self.report_sys_path_write('replacement', node)
            self.generic_visit(node)

        def visit_AnnAssign(self, node):
            # ``sys.path: list = [dir]`` runs the same store as a plain
            # assignment; NodeVisitor dispatches on the concrete node class, so
            # without this branch the annotated spelling was a silent hole.
            if node.value is not None and self.assigns_sys_path(node):
                self.report_sys_path_write('replacement', node)
            self.generic_visit(node)

        def visit_AugAssign(self, node):
            if (isinstance(node.op, (ast.Add, ast.BitOr))
                    and _sys_path_target(node.target, aliases, sys_path_bound)
                    and self.sys_path_write_carries_a_directory(node.value)):
                self.report_sys_path_write('extension', node)
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
            elif _inserts_a_repository_directory_into_sys_path(
                    node, constants, aliases, sys_path_bound,
                    getattr(constants, 'cwd_unknown', False)):
                # ``sys.path.insert(0, str(FIXTURES / '_shared'))`` followed by
                # ``import negative_control`` makes the module importable
                # without spelling the helper's directory anywhere the import
                # closure looks: the table needs the helper's discovery key and
                # this scanner derived none, so the mutation is reported rather
                # than left as a silent green tick.
                problems.append('unsupported sys.path insertion of a repository '
                                'directory (a module imported from it is '
                                f'invisible to this scanner): {ast.unparse(node)}')
            elif (_enumerates_a_directory(func, aliases)
                    and any(repo_rooted(argument) for argument in node.args)):
                # ``from os import walk`` / ``from os import listdir`` reaches the
                # same unmodelled call, and ``from os import walk as w`` reaches
                # it through the alias.
                problems.append('unsupported enumeration of a repository path '
                                f'(this scanner cannot follow it): {ast.unparse(node)}')
            elif (_is_path_join_call(func, constants, aliases)
                    and any(_spells_a_repository_path(element, constants)
                            for element in _joined_elements(node))):
                # ``'/'.join((str(SMS), name))`` and the stdlib spellings
                # ``from os.path import join`` / ``... import join as j`` reach
                # the same unmodelled call. The receiver is a string in every
                # one of them, so a repository path among the joined values
                # means a key this scanner can never derive from the text.
                problems.append('unsupported path join of a repository path '
                                f'(use ``/`` joins): {ast.unparse(node)}')
            keyed = False
            if _is_path_shape(node, constants, aliases):
                keyed = resolve(node, ast.unparse(node))
            if not keyed:
                # A shape that ends in a bare file name can still hide a
                # runtime segment *before* that name — ``Path('src/…/channel',
                # name, 'mod.rs')`` / ``…joinpath(name, 'mod.rs')``: the shape
                # resolves its tail, so ``resolve`` keys nothing and says
                # nothing. The check is skipped only when a key came out, so
                # one expression is never reported for being both keyed and
                # unknowable.
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

    A single-component path (``ROOT / 'rust-toolchain.toml'``) is dropped here
    exactly as ``sweep`` drops it: the discovery loop keys a changed file as
    ``dir/file`` and never emits a key for a repository-root file, so no anchor
    could ever satisfy it. Keeping it would fail a legitimate table with a
    problem its author cannot fix.
    """
    tree = ast.parse(text)
    problems = []
    if '/' not in table:
        # ``git grep -- "$FX/**/*negative*.py"`` needs at least one directory
        # below the fixtures root: measured with ``git ls-files``, the ``**/``
        # form returns 0 files directly under ``fixtures/`` while ``*negative*.py``
        # returns 18. A top-level table is therefore only ever emitted when the
        # table file itself changes (``neg-for-diff.sh:27``), so no anchor could
        # schedule it — accepting one here would turn a real hole into a green
        # tick. Such tables stay in ``KNOWN_BLIND`` with the host pathspec as the
        # reason.
        problems.append('top-level table: the discovery loop\'s grep never reaches a '
                        f'file sitting directly under fixtures/, so anchors cannot '
                        f'schedule it: {table}')
        return problems
    required, scan_problems = bare_concat_keys(tree, table)
    problems.extend(problem for problem in scan_problems
                    if not problem.startswith('no discovery key'))
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
        if '/' not in rel:
            # ``violations`` already refuses a top-level table, but the sweep
            # did not: a table sitting directly under ``fixtures/`` that spells
            # every path out in full produced no key, no problem and no register
            # row, so it was invisible to both nets at once. The host pathspec
            # (``$FX/**/*negative*.py``) never lists such a file, so an anchor
            # could not schedule it either — it is permanently blind and has to
            # be registered as such, like the two rows that predate this net.
            hard.append('top-level table: the discovery loop\'s grep never '
                        'reaches a file sitting directly under fixtures/, so '
                        'anchors cannot schedule it')
        if hard:
            unresolvable[rel] = hard
    return blind, unresolvable


def _unbacked_register_rows(blind, unresolvable, register=None):
    """``KNOWN_BLIND`` rows the scan no longer flags, in register order.

    A row is backed when the table still hides a key or still carries a shape
    the scanner cannot follow. Once the table is fixed both go away and the row
    has to go with them, so this returning anything is the register drifting.
    """
    register = KNOWN_BLIND if register is None else register
    return [rel for rel in sorted(register)
            if rel not in blind and rel not in unresolvable]


class DiscoveryAnchorTests(unittest.TestCase):
    def test_the_registry_is_not_empty(self):
        """An empty or gutted TABLES list must not turn every check vacuous."""
        self.assertTrue(TABLES, 'TABLES is empty: the guard would measure nothing')

    def test_every_table_declaring_anchors_is_registered(self):
        """``TABLES`` is the register; a row cannot be dropped from it silently.

        Anchors are only checked for the tables listed in ``TABLES``, and a
        table's anchors always match its own text, so deleting a row used to
        leave everything green while the guard stopped measuring that table —
        the register could be diluted one row at a time without a red test.
        """
        declared = set()
        for path in sorted(FIXTURES.rglob('*negative*.py')):
            rel = path.relative_to(FIXTURES).as_posix()
            if path.name in HOST_FILTERED or path.name.startswith('test_'):
                continue
            tree = ast.parse(path.read_text(encoding='utf-8'))
            for statement in tree.body:
                if isinstance(statement, ast.Assign):
                    targets = statement.targets
                elif isinstance(statement, ast.AnnAssign):
                    targets = [statement.target]
                else:
                    continue
                if any(isinstance(target, ast.Name) and target.id == 'DISCOVERY_ANCHORS'
                       for target in targets):
                    declared.add(rel)
        self.assertEqual(sorted(declared), sorted(TABLES),
                         'the tables that declare DISCOVERY_ANCHORS and TABLES disagree')

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
                "def row(key):\n"
                "    return ROOT / PICK[key] / 'mod.rs'\n",
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

    def test_every_register_entry_still_names_a_blind_table(self):
        """A register row must still describe a table the scan flags.

        ``sweep`` drops every key that already occurs in the table's own text
        before registering it, so the earlier form of this test ("at least one
        key is missing from the text") was true by construction — a claim of
        discrimination it did not have. Membership is the falsifiable half: once
        a table is fixed all its keys appear in its text and it leaves both
        registers, so the row has to go with it.
        """
        blind, unresolvable = sweep()
        self.assertEqual(_unbacked_register_rows(blind, unresolvable), [])

    def test_the_register_membership_check_is_falsifiable(self):
        """A made-up row must come back as an unbacked row, not be swallowed."""
        blind, unresolvable = sweep()
        rows = _unbacked_register_rows(
            blind, unresolvable, {'example/negative_controls.py': '1 key'})
        self.assertEqual(rows, ['example/negative_controls.py'])

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
            f'# {len(measured)} files (the host glob reaches the ones under '
            f'tests/fixtures/, not the top-level fixtures/ tables; #2141), '
            f'{sum(1 for rel in measured if blind.get(rel) and not unresolvable.get(rel))} blind only, '
            f'{sum(1 for rel in measured if unresolvable.get(rel) and not blind.get(rel))} '
            f'with unresolvable shapes only, '
            f'{sum(1 for rel in measured if blind.get(rel) and unresolvable.get(rel))} in both; '
            f'{total_keys} keys: {total_keys - import_keys} bare-filename joins and '
            f'{import_keys} shared-helper import keys.')
        source = (FIXTURES / 'test_negative_discovery_anchors.py').read_text(encoding='utf-8')
        line = next((row for row in source.splitlines()
                     if row.startswith('# ')
                     and 'files (the host glob reaches the ones under' in row),
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

    def test_local_aliases_with_different_values_fail_closed(self):
        """Two functions, two directories, one name: no key is right, silence is not.

        ``src = ROOT / 'x'`` in one function and ``src = ROOT / 'y'`` in another
        leaves the name out of the constant table, and the module-level report net
        only covers module-level names — so both joins produced no key *and* no
        problem, and the table read as "needs no anchor" while the discovery loop
        never scheduled it. An unknown directory must always be reported.
        """
        different = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def a():\n"
            "    src = ROOT / 'src/example/x'\n"
            "    return src / 'mod.rs'\n"
            "def b():\n"
            "    src = ROOT / 'src/example/y'\n"
            "    return src / 'mod.rs'\n"
        )
        problems = violations(different, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous name assigned different values' in p for p in problems),
                        problems)

    def test_local_aliases_with_the_same_value_still_carry_their_keys(self):
        """The reverse control: the same expression twice is not ambiguous.

        Whichever assignment ran, the value is the same, so the joins must be keyed
        like a single-assignment alias — refusing to resolve would demand nothing
        and hide a real directory.
        """
        same = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ('x/mod.rs', 'x/other.rs')\n"
            "def a():\n"
            "    src = ROOT / 'src/example/x'\n"
            "    return src / 'mod.rs'\n"
            "def b():\n"
            "    src = ROOT / 'src/example/x'\n"
            "    return src / 'other.rs'\n"
        )
        self.assertEqual(violations(same, 'example/negative_controls.py'), [])
        dropped = same.replace("DISCOVERY_ANCHORS = ('x/mod.rs', 'x/other.rs')",
                               "DISCOVERY_ANCHORS = ()")
        problems = violations(dropped, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: x/mod.rs' in p for p in problems), problems)

    def test_reassigned_display_locals_stay_silent(self):
        """A local reassigned between non-path values is not a repository path."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def render(rows):\n"
            "    label = 'a'\n"
            "    label = 'b'\n"
            "    return label + ': ' + rows\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_dotted_stdlib_join_with_a_runtime_file_name_fails_closed(self):
        """``os.path.join(SMS, name)`` is ``SMS / name`` in another spelling.

        The dotted stdlib form has neither a string receiver (``'/'.join(...)``,
        which ``_is_path_join_call`` catches) nor a bare literal tail (which
        ``_is_path_shape`` catches), so a runtime file name here produced no key
        and no report while every sibling spelling was reported. ``os.sep.join``
        reaches the same function through a tuple argument.
        """
        for spelling in ("os.path.join(SMS, name)",
                         "os.sep.join((str(SMS), name))"):
            synthetic = (
                "import os\n"
                "ROOT = Path(__file__).resolve().parents[5]\n"
                "SMS = ROOT / 'src/example/x'\n"
                "DISCOVERY_ANCHORS = ()\n"
                "def row(name):\n"
                f"    return {spelling}\n"
            )
            problems = violations(synthetic, 'example/negative_controls.py')
            self.assertTrue(any('unresolvable repository path' in p for p in problems),
                            (spelling, problems))

    def test_a_local_rebound_by_a_loop_target_fails_closed(self):
        """Assign-then-``for`` is the same ambiguity as assign-then-assign.

        ``src = ROOT / 'x'`` followed by ``for src in sources`` means the join
        below may name either value. The whole-file rebinding set used to drop
        the name from every net, so the join was neither keyed nor reported.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def rows(sources):\n"
            "    src = ROOT / 'src/example/x'\n"
            "    for src in sources:\n"
            "        print(src / 'mod.rs')\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous name assigned different values' in p for p in problems),
                        problems)

    def test_a_local_rebound_in_another_function_fails_closed(self):
        """A ``for`` target in a third function must not launder the ambiguity.

        The two assignments already disagree on their own; the unrelated loop is
        no reason to stop reporting them.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def a():\n"
            "    src = ROOT / 'src/example/x'\n"
            "    return src / 'mod.rs'\n"
            "def b():\n"
            "    src = ROOT / 'src/example/y'\n"
            "    return src / 'mod.rs'\n"
            "def c():\n"
            "    for src in []:\n"
            "        pass\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous name assigned different values' in p for p in problems),
                        problems)

    def test_identical_alias_expressions_over_different_locals_fail_closed(self):
        """``src = base`` twice is only one value when ``base`` is one value.

        The expression is byte-identical in both functions, but each ``base`` is
        a different directory. Merging on the AST alone made ``src`` a constant
        pointing at an unresolvable name, so the joins were silent.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def a():\n"
            "    base = ROOT / 'src/example/x'\n"
            "    src = base\n"
            "    return src / 'mod.rs'\n"
            "def b():\n"
            "    base = ROOT / 'src/example/y'\n"
            "    src = base\n"
            "    return src / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous name assigned different values' in p for p in problems),
                        problems)

    def test_an_identical_alias_over_a_module_constant_still_carries_its_key(self):
        """The reverse control: a module constant meaning is scope-free."""
        same = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "BASE = ROOT / 'src/example/x'\n"
            "DISCOVERY_ANCHORS = ('x/mod.rs', 'x/other.rs')\n"
            "def a():\n"
            "    src = BASE\n"
            "    return src / 'mod.rs'\n"
            "def b():\n"
            "    src = BASE\n"
            "    return src / 'other.rs'\n"
        )
        self.assertEqual(violations(same, 'example/negative_controls.py'), [])

    def test_a_local_unpacking_that_cannot_be_paired_fails_closed(self):
        """``src, *rest = ROOT / 'a', ROOT / 'b'`` inside a function is a hole too.

        The unpairable-binding net only looked at module level, so the local
        spelling of the r13 shape dropped the name entirely.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def a():\n"
            "    src, *rest = (ROOT / 'src/example/x', ROOT / 'src/example/y')\n"
            "    return src / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous name assigned different values' in p for p in problems),
                        problems)

    def test_a_local_holding_a_container_of_paths_stays_silent(self):
        """The reverse control for the transitive alias net.

        ``command = ['node', f'{baseline}/loader.mjs']`` mentions a repository
        path inside a list that is later an output directory. Following "any
        expression that mentions a repository path" made every join on
        ``command`` look like a repository join — two real tables gained a
        spurious finding. Only a name built *as a path* from a repository
        directory carries the taint.
        """
        synthetic = (
            "import sys\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def execute(name, command):\n"
            "    return Path(command[-1]) / 'oracle.json'\n"
            "def run(baseline):\n"
            "    command = ['node', f'{baseline}/loader.mjs', str(ROOT / 'x')]\n"
            "    return execute('x', command)\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_dotted_parents_index_seeds_the_alias_net(self):
        """``(ROOT / 'x/gateway.rs').parents[0]`` is still a repository path.

        The ambiguity net only seeded from ``.parent`` / ``.resolve()``, so two
        functions whose same-named local is built with ``.parents[0]`` seeded
        nothing: the joins produced no key and no report.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def a():\n"
            "    base = (ROOT / 'src/example/x/gateway.rs').parents[0]\n"
            "    return base / 'mod.rs'\n"
            "def b():\n"
            "    base = (ROOT / 'src/example/y/gateway.rs').parents[0]\n"
            "    return base / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('ambiguous name assigned different values' in problem
                for problem in problems), problems)

    def test_an_fstring_alias_carries_the_scope_ambiguity(self):
        """``src = f'{base}'`` is an alias, not a display string.

        Taint only followed the plain spine, so the interpolation cut it: two
        scopes, two directories behind ``base``, one silent join on ``src``.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def a():\n"
            "    base = ROOT / 'src/example/x'\n"
            "    src = f'{base}'\n"
            "    return Path(src) / 'mod.rs'\n"
            "def b():\n"
            "    base = ROOT / 'src/example/y'\n"
            "    src = f'{base}'\n"
            "    return Path(src) / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('ambiguous name assigned different values' in problem
                for problem in problems), problems)

    def test_an_unpairable_unpacking_carries_a_tainted_name(self):
        """``src, *rest = (base, 1)`` hands the container to its targets.

        The element-wise spine test looked at the tuple itself, not inside it,
        so a repository directory reached the target without any report.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def a():\n"
            "    base = ROOT / 'src/example/x'\n"
            "    src, *rest = (base, 1)\n"
            "    return src / 'mod.rs'\n"
            "def b():\n"
            "    base = ROOT / 'src/example/y'\n"
            "    src, *rest = (base, 1)\n"
            "    return src / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('ambiguous name assigned different values' in problem
                for problem in problems), problems)

    def test_an_imported_name_is_not_promoted_from_one_local_assignment(self):
        """An import binds a value the file text does not contain.

        ``from external_config import base`` plus one local ``base = ...`` used
        to make that assignment a file-wide constant: the other site resolved
        to the local directory, keyed a key it never builds, and reported
        nothing at the site the scanner cannot follow.
        """
        synthetic = (
            "from external_config import base\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def a():\n"
            "    base = ROOT / 'src/example/x'\n"
            "    src = base\n"
            "    return src / 'mod.rs'\n"
            "def b():\n"
            "    src = base\n"
            "    return src / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('ambiguous name assigned different values' in problem
                for problem in problems), problems)

    def test_a_container_index_of_a_path_literal_is_keyed(self):
        """``Path(CH['feishu']) / 'mod.rs'`` names the directory the literal spells.

        No ``ROOT`` appears in the expression, so the join used to be neither
        keyed nor reported — while the identical literal spelled directly is
        keyed. The host runs tables from the repository root, so a relative
        literal with separators is a repository path.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ('feishu/mod.rs',)\n"
            "CH = {'feishu': 'src/pilotdeck-gateway/src/channel/feishu'}\n"
            "rows = [('a', Path(CH['feishu']) / 'mod.rs', 'x')]\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_an_opaque_container_index_fails_closed(self):
        """A container the scanner cannot index is reported, not dropped."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "CH = {'feishu': 'src/pilotdeck-gateway/src/channel/feishu'}\n"
            "def row(name):\n"
            "    return Path(CH[name]) / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_container_of_plain_values_stays_silent(self):
        """The reverse control for the container rule.

        Only a *path* literal inside the container counts. A lookup table of
        ordinary words is not a directory, and joining on it must not report.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "MODES = {'fast': 'fast', 'safe': 'safe'}\n"
            "def row(name):\n"
            "    return Path(MODES['fast']) / name\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_duplicate_container_key_takes_the_last_value(self):
        """``{'k': x, 'k': y}`` builds ``y`` at runtime, so the guard must too.

        First-wins keyed ``x``: the table was green with an anchor for a
        directory the code never builds, and the anchor for the real one looked
        stale — a false pass on the axis this guard exists to protect.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "CH = {'k': 'src/pilotdeck-gateway/src/channel/x',\n"
            "      'k': 'src/pilotdeck-gateway/src/channel/y'}\n"
            "DISCOVERY_ANCHORS = ('y/mod.rs',)\n"
            "rows = [Path(CH['k']) / 'mod.rs']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_mutated_container_fails_closed(self):
        """``CH['k'] = ...`` makes the literal stale; the index is reported.

        The value the runtime indexes is no longer the one written in the
        container literal, and only the runtime knows the new one, so keying
        either spelling would be a guess.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "CH = {'k': 'src/pilotdeck-gateway/src/channel/x'}\n"
            "CH['k'] = 'src/pilotdeck-gateway/src/channel/y'\n"
            "rows = [Path(CH['k']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_nested_container_index_resolves(self):
        """``CH['a']['b']`` reaches the inner dictionary, not a dead end.

        Stopping at the first index left the join neither keyed nor reported,
        and an anchor declared for the real directory was reported stale.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "CH = {'a': {'b': 'src/pilotdeck-gateway/src/channel/x'}}\n"
            "DISCOVERY_ANCHORS = ('x/mod.rs',)\n"
            "rows = [Path(CH['a']['b']) / 'mod.rs']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_container_f_string_is_reported(self):
        """``f'{CH["k"]}/{name}'`` interpolates a repository directory."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "CH = {'k': 'src/pilotdeck-gateway/src/channel/x'}\n"
            "def row(name):\n"
            "    return f'{CH[\"k\"]}/{name}'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unsupported f-string path' in problem for problem in problems),
            problems)

    def test_a_container_in_a_bare_join_is_reported(self):
        """``join(CH['k'], name)`` is the stdlib spelling of the same join."""
        synthetic = (
            "from os.path import join\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "CH = {'k': 'src/pilotdeck-gateway/src/channel/x'}\n"
            "def row(name):\n"
            "    return join(CH['k'], name)\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unsupported path join' in problem for problem in problems),
            problems)

    def test_an_imported_name_assigned_a_path_is_reported(self):
        """An ``import`` binding and a local path assignment are two meanings.

        Refusing to substitute the name is right, but silence was not: the join
        that reads it produced no key and no report, so the table looked clean.
        """
        synthetic = (
            "from external_config import base\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def a():\n"
            "    base = ROOT / 'src/pilotdeck-gateway/src/channel/x'\n"
            "def b():\n"
            "    src = base\n"
            "    return src / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('ambiguous name' in problem for problem in problems), problems)

    def test_a_conditional_module_binding_rebound_elsewhere_is_reported(self):
        """``if flag: SMS = ROOT / 'x'`` plus ``def row(SMS)`` is two directories.

        The conditional binding counts as module level for substitution, and
        the parameter counts as a rebinding, so neither net may key either
        value: the join is reported instead.
        """
        synthetic = (
            "import os\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ('sms/mod.rs',)\n"
            "if os.environ.get('X'):\n"
            "    SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
            "def row(SMS):\n"
            "    return SMS / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('ambiguous name' in problem for problem in problems), problems)

    def test_a_root_defined_at_the_wrong_depth_is_reported(self):
        """``ROOT = ...parents[4]`` is not the repository root.

        Every resolver short-circuits a ``ROOT`` name to the root, so a table
        one level short built ``src/<file>`` at runtime while the scanner keyed
        ``<file>`` — one component, dropped as "no discovery key", with no
        anchor and no report.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[4]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'Cargo.toml']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('does not name the repository root' in problem for problem in problems),
            problems)

    def test_a_self_referential_container_does_not_recurse_forever(self):
        """``CH = {'k': CH['k']}`` must terminate, not blow the stack.

        A table written that way raises ``NameError`` at import, so it can
        never reach the host; the guard's only job here is to stay a static
        check and leave the join unresolved instead of crashing the whole step
        with ``RecursionError``.
        """
        synthetic = (
            "from settings import CH\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "CH = {'k': CH['k']}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(CH['k']) / 'mod.rs']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_write_through_an_alias_invalidates_the_original(self):
        """``ALIAS = CH`` names the same dictionary, so both spellings are stale."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "CH = {'k': 'src/pilotdeck-gateway/src/channel/x'}\n"
            "ALIAS = CH\n"
            "ALIAS['k'] = 'src/pilotdeck-gateway/src/channel/y'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(CH['k']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_write_through_a_shallow_copy_invalidates_the_container(self):
        """``CP = CH.copy()`` copies the top level only.

        A write through the copy into a *nested* container rewrites the very
        object ``CH['a']`` reads, so the literal behind ``CH`` is stale just as
        if the write had been spelled through ``CH``. The earlier reading
        treated every copy as an unrelated object and keyed the old value; the
        blunt direction is deliberate — the guard reports the shape rather
        than guess which level the runtime writes to.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "CH = {'a': {'b': 'src/pilotdeck-gateway/src/channel/x'}}\n"
            "CP = CH.copy()\n"
            "CP['a']['b'] = 'src/pilotdeck-gateway/src/channel/y'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(CH['a']['b']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_nested_container_mutation_is_attributed_to_the_root(self):
        """``CH['a'].update(...)`` mutates the inner dictionary ``CH`` holds.

        The receiver is a subscript, not a name, so the write used to be
        invisible and the first-wins literal was keyed while the runtime read
        the new value.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "CH = {'a': {'b': 'src/pilotdeck-gateway/src/channel/x'}}\n"
            "CH['a'].update({'b': 'src/pilotdeck-gateway/src/channel/y'})\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(CH['a']['b']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_dict_unpack_makes_the_key_unknowable(self):
        """``{'k': 'old', **OTHER}`` may overwrite ``k`` at runtime."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "CH = {'k': 'src/pilotdeck-gateway/src/channel/x',\n"
            "      **{'k': 'src/pilotdeck-gateway/src/channel/y'}}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(CH['k']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_every_root_binding_is_checked_for_depth(self):
        """A wrong ``ROOT`` used before a correct rebinding still built joins.

        Only the last binding used to be checked, so ``ROOT = ...parents[4]``
        followed by ``ROOT = ...parents[5]`` passed — while the joins written
        between them had already used the wrong root.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[4]\n"
            "rows = [ROOT / 'Cargo.toml']\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('does not name the repository root' in problem for problem in problems),
            problems)

    def test_filesystem_joins_of_relative_literals_are_keyed(self):
        """``os.path.join('sms', 'mod.rs')`` hides the key the loop greps for.

        These three build ``sms/mod.rs`` from pieces the table spells in full,
        but the literal itself never occurs in the text, so the discovery loop
        never scheduled the table when the file changed. ``Path('sms') /
        'mod.rs'`` has been keyed since r9; the filesystem spellings had to
        follow, and ``Path.cwd()`` is the repository root because the host runs
        tables from there.
        """
        shapes = {
            'os.path.join': "rows = [os.path.join('sms', 'mod.rs')]",
            'os.sep.join': "rows = [os.sep.join(('sms', 'mod.rs'))]",
            'path.cwd': "rows = [Path.cwd() / 'sms' / 'mod.rs']",
        }
        for label, line in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "import os\n"
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + line + "\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('missing anchor: sms/mod.rs' in problem for problem in problems),
                    problems)

    def test_string_joins_that_are_not_paths_stay_silent(self):
        """The same shape builds labels and argument lists, so it stays out.

        ``' '.join(command)`` over a list of cargo arguments and
        ``'/'.join(('headers', 'idle'))`` building a test-cell label are not
        path builds; keying them demanded anchors for things the discovery
        loop never keys — the first is a real table's shape
        (``tool-budget/negative_project.py``), the second
        ``network-timing/negative_private.py`` writes seventeen of. A runtime
        boot directory is not a repository path either.
        """
        shapes = {
            'runtime boot dir': "rows = [os.path.join(_boot_root, 'scripts', 'cli-parity')]",
            'argument list': ("command = ['cargo', 'test', str(ROOT / 'src/Cargo.toml')]\n"
                              "rows = [' '.join(command)]"),
            'cell label': "rows = ['/'.join(('headers', 'idle'))]",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "import os\n"
                    "from pathlib import Path\n"
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + body + "\n"
                )
                self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_write_through_a_nested_alias_invalidates_the_container(self):
        """``ALIAS = CH['a']`` names the *same* inner dictionary.

        ``ALIAS['b'] = ...`` therefore rewrites what ``CH['a']['b']`` reads.
        Pairing the names is what closes the hole: without it the stale
        literal was keyed while the runtime used the new one.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "CH = {'a': {'b': 'src/pilotdeck-gateway/src/channel/x'}}\n"
            "ALIAS = CH['a']\n"
            "ALIAS['b'] = 'src/pilotdeck-gateway/src/channel/y'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(CH['a']['b']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_runtime_key_that_may_collide_makes_the_dict_unknowable(self):
        """``{'k': x, K: y}`` picks ``y`` when ``K == 'k'`` at runtime."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "K = input()\n"
            "CH = {'k': 'src/pilotdeck-gateway/src/channel/x',\n"
            "      K: 'src/pilotdeck-gateway/src/channel/y'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(CH['k']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_root_dependencies_resolve_at_the_binding_site(self):
        """``BASE`` rebound later must not be read back into the earlier ROOT.

        ``BASE = parents[4]; ROOT = BASE`` then ``BASE = parents[5]`` built
        ``src/Cargo.toml`` at runtime, but the check resolved BASE to its final
        value and passed.
        """
        synthetic = (
            "BASE = Path(__file__).resolve().parents[4]\n"
            "ROOT = BASE\n"
            "rows = [ROOT / 'Cargo.toml']\n"
            "BASE = Path(__file__).resolve().parents[5]\n"
            "ROOT = BASE\n"
            "DISCOVERY_ANCHORS = ()\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('does not name the repository root' in problem for problem in problems),
            problems)

    def test_a_runtime_middle_segment_is_reported(self):
        """The runtime piece can sit between a known directory and file name.

        ``os.path.join('src/.../channel', name, 'mod.rs')`` is
        ``.../sms/mod.rs`` when ``name == 'sms'``; checking only the tail left
        it silent, while the loop can never derive the key from the text.
        """
        shapes = {
            'middle': ("def row(name):\n"
                       "    return os.path.join('src/pilotdeck-gateway/src/channel',\n"
                       "                          name, 'mod.rs')"),
            'single head': "def row(n):\n    return os.path.join('sms', n)",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "import os\n"
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + body + "\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem for problem in problems),
                    problems)

    def test_a_runtime_boot_directory_stays_silent(self):
        """``os.path.join(_boot_root, name, 'mod.rs')`` starts outside the repo."""
        synthetic = (
            "import os\n"
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return os.path.join(_boot_root, name, 'mod.rs')\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_file_name_with_a_space_cannot_be_scheduled(self):
        """A key the discovery loop can never match must be reported, not keyed.

        The loop reads its changed-file list through ``for f in $changed``, so
        ``…/sms/my file.rs`` arrives as two fragments and matches no branch at
        all. Keying it and demanding ``sms/my file.rs`` as an anchor asked for
        something that could never fire — a green tick over a real hole.
        """
        synthetic = (
            "import os\n"
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.path.join('src/pilotdeck-gateway/src/channel/sms',\n"
            "                      'my file.rs')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('whitespace in a discovery key cannot be scheduled' in problem
                for problem in problems),
            problems)
        self.assertFalse(
            any('missing anchor: sms/my file.rs' in problem for problem in problems),
            problems)

    def test_a_chdir_makes_the_working_directory_unknowable(self):
        """``os.chdir(...)`` breaks the "tables start at the repository root" premise."""
        synthetic = (
            "import os\n"
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "os.chdir('src/pilotdeck-gateway/src/channel/sms')\n"
            "rows = [Path.cwd() / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_an_absolute_join_argument_resets_the_prefix(self):
        """``os.path.join('sms', '/tmp', 'mod.rs')`` is ``/tmp/mod.rs``.

        Joining the pieces verbatim demanded an anchor for ``tmp/mod.rs``, a
        repository key the code never builds — the reset is what makes the call
        name nothing the discovery loop can key.
        """
        synthetic = (
            "import os\n"
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.path.join('sms', '/tmp', 'mod.rs')]\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_drive_prefixed_join_argument_is_reported(self):
        """``C:/tmp`` is absolute on Windows and relative on POSIX.

        ``os.path.join('sms', 'C:/tmp', 'mod.rs')`` is ``sms/C:/tmp/mod.rs`` on
        POSIX and ``C:/tmp/mod.rs`` on Windows, so which key the host sees
        depends on the platform: the call is reported instead of keyed.
        """
        synthetic = (
            "import os\n"
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.path.join('sms', 'C:/tmp', 'mod.rs')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_setitem_write_invalidates_the_container(self):
        """``operator.setitem(CH, ...)`` and ``CH.__setitem__(...)`` mutate too."""
        shapes = {
            'dunder': "CH.__setitem__('k', 'src/pilotdeck-gateway/src/channel/y')",
            'operator': ("import operator\n"
                         "operator.setitem(CH, 'k', 'src/pilotdeck-gateway/src/channel/y')"),
        }
        for label, line in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "CH = {'k': 'src/pilotdeck-gateway/src/channel/x'}\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + line + "\n"
                    "rows = [Path(CH['k']) / 'mod.rs']\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem for problem in problems),
                    problems)

    def test_a_parent_component_is_normalised(self):
        """``SMS / 'a/..' / 'x.rs'`` is ``sms/x.rs``; ``..`` past the root is not a path.

        Keeping the ``..`` segment emitted the key ``../x.rs``, which the host
        never fires on: an anchor that looks real and can never schedule the
        table (``edgeclaw-text/negative.py`` really writes ``text.parent / '..'
        / '..' / '..' / 'js_value.rs'``).
        """
        header = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
        )
        normalised = violations(
            header + "rows = [SMS / 'a/..' / 'x.rs']\n", 'example/negative_controls.py')
        self.assertTrue(
            any('missing anchor: sms/x.rs' in problem for problem in normalised),
            normalised)
        above_root = violations(
            header + "rows = [ROOT / '..' / 'x.rs']\n", 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in above_root),
            above_root)

    def test_a_top_level_table_outside_the_register_is_reported(self):
        """A table directly under ``fixtures/`` cannot be scheduled at all.

        It has to be registered; otherwise a table that spells every path out
        in full produced no key, no problem and no row — invisible to both nets
        at once. One directory down, the same table is measured normally.
        """
        body = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "rows = [('a', ROOT / 'src/example/named.rs', 'x')]\n"
        )
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'newfeat').mkdir()
            (root / 'newfeat' / 'negative_controls.py').write_text(body, encoding='utf-8')
            nested_blind, nested_unresolvable = sweep(root)
            (root / 'flat_negative.py').write_text(body, encoding='utf-8')
            flat_blind, flat_unresolvable = sweep(root)
        self.assertEqual(nested_unresolvable, {})
        self.assertEqual(nested_blind, {})
        self.assertEqual(flat_blind, {})
        self.assertEqual(sorted(flat_unresolvable), ['flat_negative.py'])
        self.assertTrue(any('top-level table' in problem
                            for problem in flat_unresolvable['flat_negative.py']))
        self.assertNotIn('flat_negative.py', KNOWN_BLIND)

    def test_a_top_level_table_cannot_be_scheduled_by_anchors(self):
        """Anchors only help where the host's grep looks.

        ``neg-for-diff.sh:29`` greps ``"$FX/**/*negative*.py"``, and that pathspec
        needs a directory below the fixtures root (measured: ``git ls-files``
        returns 0 top-level files for it, 18 for ``*negative*.py``). A table
        sitting directly in ``fixtures/`` is emitted only when the table itself
        changes, so moving it into ``TABLES`` after adding anchors would report
        coverage the loop never has — the guard must refuse it instead.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ('x/mod.rs',)\n"
            "rows = [ROOT / 'src/example/x/mod.rs']\n"
        )
        problems = violations(synthetic, 'top_level_negative.py')
        self.assertTrue(any('top-level table' in p for p in problems), problems)

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

    def test_the_other_string_join_spellings_fail_closed(self):
        """``join`` by name or alias, ``%`` and ``.format`` build the same path.

        The ``/``, f-string and ``+`` spellings reported since r9. These four
        were silent in every earlier revision: a table written this way produced
        no key and no report, so it never had to register an anchor and the
        discovery loop never scheduled it. ``from os.path import join as j`` is
        the alias form the enumeration helpers already had to learn.
        """
        shapes = {
            'alias join': "from os.path import join as j\nrows = [j(SMS, 'mod.rs')]\n",
            'method join': "rows = ['/'.join((str(SMS), 'mod.rs'))]\n",
            'percent': "FMT = '{}/mod.rs'\nrows = [FMT % SMS]\n",
            'format': "FMT = '{}/mod.rs'\nrows = [FMT.format(SMS)]\n",
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
                self.assertTrue(problems, f'{label}: silent, so the table would never be scheduled')

    def test_a_display_join_stays_silent(self):
        """``' / '.join(...)`` and a ``%`` counter are labels, not paths."""
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/example/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def report(killed, total, parts):\n"
            "    print(' / '.join(parts))\n"
            "    print('%d/%d killed' % (killed, total))\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_file_name_with_a_space_is_reported_not_keyed(self):
        """``'msg 1.json'`` is an ordinary name, but the host cannot schedule it.

        The discovery loop splits its changed-file list on whitespace, so no
        anchor for ``sms/my file.rs`` can ever fire; the key is unschedulable
        and has to be reported rather than demanded.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/example/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [SMS / 'my file.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('whitespace in a discovery key cannot be scheduled' in problem
                            for problem in problems), problems)
        self.assertFalse(any('missing anchor: sms/my file.rs' in problem
                             for problem in problems), problems)

    def test_a_single_component_path_is_not_a_discovery_key(self):
        """``ROOT / 'rust-toolchain.toml'`` gets no key, so it needs no anchor.

        The discovery loop keys a changed file as ``dir/file`` and never emits a
        key for a repository-root file. Reporting it here would fail a
        legitimate table with a problem no anchor can satisfy — the negative of
        every other case in this class.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'rust-toolchain.toml']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

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

    def test_an_annotated_second_assignment_is_ambiguous_too(self):
        """The annotated form takes part in ambiguity, not only in resolution.

        ``SMS: Path = ROOT / 'x'`` twice with different values means the site
        depends on which line ran; resolving the last one would key a directory
        the earlier use never built. The same value twice stays resolvable.
        """
        different = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS: Path = ROOT / 'src/example/x'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [SMS / 'mod.rs']\n"
            "SMS: Path = ROOT / 'src/example/y'\n"
        )
        problems = violations(different, 'example/negative_controls.py')
        self.assertTrue(any('ambiguous' in problem for problem in problems), problems)
        self.assertFalse(any('y/mod.rs' in problem for problem in problems), problems)
        same = different.replace("example/y", "example/x")
        problems = violations(same, 'example/negative_controls.py')
        self.assertFalse(any('ambiguous' in problem for problem in problems), problems)
        self.assertTrue(any('missing anchor: x/mod.rs' in problem
                            for problem in problems), problems)

    def test_a_length_mismatched_unpacking_fails_closed(self):
        """``A, B = ROOT / 'x'`` cannot be paired up any more than a starred one."""
        synthetic = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "A, B = ROOT / 'src/example/x'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [A / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('unsupported unpacking of a repository path' in problem
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

    def test_an_annotated_module_constant_is_resolved(self):
        """``SMS: Path = ROOT / '...'`` binds at import time like a plain ``=``.

        The annotation is a type note, not a different kind of binding: leaving
        the form out of the constant table made every use of it neither keyed nor
        reported, so an annotated directory could hide a bare file name with
        nothing turning red.
        """
        missing = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS: Path = ROOT / 'src/example/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [SMS / 'mod.rs']\n"
        )
        problems = violations(missing, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: sms/mod.rs' in problem
                            for problem in problems), problems)
        anchored = missing.replace("DISCOVERY_ANCHORS = ()",
                                   "DISCOVERY_ANCHORS = ('sms/mod.rs',)")
        self.assertEqual(violations(anchored, 'example/negative_controls.py'), [])

    def test_element_wise_unpacking_binds_each_name(self):
        """``A, B = ROOT / 'x', ROOT / 'y'`` pairs each name with its own value.

        A table that lists several directories on one line carried no key at all
        before: the target was a tuple, so neither name entered the constant
        table and both joins were invisible.
        """
        synthetic = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS, MMS = ROOT / 'src/example/sms', ROOT / 'src/example/mms'\n"
            "DISCOVERY_ANCHORS = ('sms/mod.rs', 'mms/mod.rs')\n"
            "rows = [SMS / 'mod.rs', MMS / 'mod.rs']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])
        dropped = synthetic.replace(
            "('sms/mod.rs', 'mms/mod.rs')", '()')
        problems = violations(dropped, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: sms/mod.rs' in problem
                            for problem in problems), problems)
        self.assertTrue(any('missing anchor: mms/mod.rs' in problem
                            for problem in problems), problems)

    def test_a_starred_module_unpacking_fails_closed(self):
        """``A, *rest = ROOT / 'x', ROOT / 'y'`` cannot be paired up.

        The lengths differ, so which value a name holds is not knowable
        statically. Dropping the names silently is the hole; reporting them is
        the answer, and an ordinary ``a, b = 1, 2`` unpacking stays quiet.
        """
        synthetic = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS, *REST = ROOT / 'src/example/sms', ROOT / 'src/example/mms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [SMS / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('unsupported unpacking of a repository path' in problem
                            for problem in problems), problems)
        plain = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "COUNT, *REST = 1, 2, 3\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [(COUNT, REST)]\n"
        )
        self.assertEqual(violations(plain, 'example/negative_controls.py'), [])

    def test_an_interpolated_separator_in_an_f_string_fails_closed(self):
        """``f'{SMS}{SEP}mod.rs'`` hides the slash behind a module constant.

        The fragment after the directory is a formatted value rather than a
        literal, so the rule that catches ``f'{SMS}/mod.rs'`` skipped it whole:
        no key, no report.
        """
        shapes = {
            'separator constant': "SEP = '/'\nrows = [f'{SMS}{SEP}mod.rs']\n",
            'fragment constant': "MOD = '/mod.rs'\nrows = [f'{SMS}{MOD}']\n",
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
                self.assertTrue(any('unsupported f-string path' in problem
                                    for problem in problems), problems)

    def test_a_loop_body_assignment_is_not_a_module_constant(self):
        """``for row in rows: count = ...`` rebinds per iteration, not per import.

        Treating a loop-body target as a module constant substitutes a value the
        rest of the file never sees: a display counter then reads as a repository
        path and the table fails on a f-string that builds nothing.
        """
        synthetic = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/example/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "cases = []\n"
            "with open('log') as log:\n"
            "    for row in log:\n"
            "        count = SMS.read_text().count(row)\n"
            "print('\\n'.join(f'{name}: {count} matches' for name, count in cases))\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

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

    def test_a_write_through_a_nested_alias_invalidates_the_container(self):
        """``A = CH; B = A['a']; B['b'] = ...`` rewrites ``CH['a']['b']``.

        The alias graph used to pair a subscript only when its root was itself
        bound to a container literal, so one plain alias in between hid the
        write and the stale key was accepted.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "CH = {'a': {'b': 'src/pilotdeck-gateway/src/channel/x'}}\n"
            "A = CH\n"
            "B = A['a']\n"
            "B['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(CH['a']['b']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_second_root_binding_with_the_same_spelling_is_checked(self):
        """``ROOT = BASE`` twice is two bindings, not one repeated spelling.

        The depth check de-duplicated by expression text, so the second
        ``ROOT = BASE`` was skipped even though ``BASE`` had been rebound to a
        shallower value in between.
        """
        synthetic = (
            "BASE = Path(__file__).resolve().parents[5]\n"
            "ROOT = BASE\n"
            "BASE = Path(__file__).resolve().parents[4]\n"
            "ROOT = BASE\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'Cargo.toml']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('does not name the repository root' in problem for problem in problems),
            problems)

    def test_a_dependency_captured_through_an_alias_keeps_its_value(self):
        """``B = A`` captures the value ``A`` had there, not its final one."""
        synthetic = (
            "A = Path(__file__).resolve().parents[4]\n"
            "B = A\n"
            "A = Path(__file__).resolve().parents[5]\n"
            "ROOT = B\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'Cargo.toml']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('does not name the repository root' in problem for problem in problems),
            problems)

    def test_a_runtime_middle_argument_of_path_is_reported(self):
        """``Path('src/…/channel', name, 'mod.rs')`` hides the key in the middle.

        Only the last argument used to be checked, so a runtime segment placed
        before a literal file name produced neither a key nor a report.
        """
        shapes = {
            'Path': ("def row(name):\n"
                     "    return Path('src/pilotdeck-gateway/src/channel', name, 'mod.rs')\n"),
            'joinpath': ("def row(name):\n"
                         "    return Path('src/pilotdeck-gateway/src/channel').joinpath(\n"
                         "        name, 'mod.rs')\n"),
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n" + body
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem for problem in problems),
                    problems)

    def test_a_relative_path_after_chdir_is_reported(self):
        """``os.chdir(...); Path('mod.rs')`` reads ``sms/mod.rs``, not ``mod.rs``.

        Only an explicit ``Path.cwd()`` was invalidated by a chdir, so a
        relative literal was still resolved from the repository root and the
        single-component result was dropped as "no discovery key".
        """
        synthetic = (
            "import os\n"
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "os.chdir('src/pilotdeck-gateway/src/channel/sms')\n"
            "rows = [Path('mod.rs')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_relative_join_after_chdir_is_reported(self):
        """The same premise break applies to a spelled-out relative join."""
        synthetic = (
            "import os\n"
            "DISCOVERY_ANCHORS = ()\n"
            "os.chdir('src/pilotdeck-gateway/src/channel')\n"
            "rows = [os.path.join('sms', 'mod.rs')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem for problem in problems),
            problems)

    def test_a_chdir_alias_makes_the_working_directory_unknowable(self):
        """``from os import chdir as cd`` reaches the same call.

        The enumeration helpers already resolve their aliases; the chdir check
        only looked at the attribute name, so an aliased call left the
        repository-root premise standing.
        """
        shapes = {
            'aliased': "from os import chdir as cd\ncd('src/pilotdeck-gateway/src/channel/sms')\n",
            'module alias': "import os as operating\noperating.chdir('src/sms')\n",
            # ``cd = os.chdir`` is an assignment alias, not an import alias;
            # and the import map keeps one target per name, so a later
            # ``from os.path import join as cd`` used to erase the earlier
            # binding even though the ``cd(...)`` call is still there.
            'assignment alias': "import os\ncd = os.chdir\ncd('src/sms')\n",
            'import rebound later': (
                "from os import chdir as cd\n"
                "from os.path import join as cd\n"
                "cd('src/sms')\n"),
            'context manager': (
                "from contextlib import chdir as cd\n"
                "with cd('src/sms'):\n"
                "    pass\n"),
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n" + body
                    + "rows = [Path('mod.rs')]\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem for problem in problems),
                    problems)

    def test_a_sys_path_insertion_of_a_repository_directory_is_reported(self):
        """A module imported from an inserted directory is invisible here.

        ``sys.path.insert(0, str(FIXTURES / '_shared'))`` + ``import
        negative_control`` gives the table a helper dependency the import
        closure never sees, so the helper's key is never derived and the table
        looked self-contained.
        """
        shapes = {
            'module constant': (
                "import sys\n"
                "FIXTURES = Path(__file__).resolve().parents[1]\n"
                "sys.path.insert(0, str(FIXTURES / '_shared'))\n"
                "import negative_control\n"),
            'from sys import path': (
                "from sys import path\n"
                "path.append(str(Path(__file__).resolve().parents[1] / '_negkit'))\n"
                "import negative_submit_migration\n"),
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n" + body
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unsupported sys.path insertion' in problem for problem in problems),
                    problems)

    def test_a_string_join_does_not_reset_on_an_absolute_piece(self):
        """``os.sep`` is a string: ``os.sep.join`` concatenates, it does not reset.

        ``os.sep.join(('…/sms', '/tmp', 'mod.rs'))`` is
        ``…/sms//tmp/mod.rs`` inside the repository, so the key is
        ``tmp/mod.rs``. Applying the filesystem reset there hid a real path.
        """
        synthetic = (
            "import os\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.sep.join((\n"
            "    'src/pilotdeck-gateway/src/channel/sms', '/tmp', 'mod.rs'\n"
            "))]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('missing anchor: tmp/mod.rs' in problem for problem in problems),
            problems)

    def test_a_parent_component_inside_a_relative_join_is_normalised(self):
        """``os.path.join(SMS, 'sms/..', 'mod.rs')`` is ``SMS/mod.rs``.

        The ``..`` was only normalised for ``/`` joins, so a relative join
        credited the useless anchor ``../mod.rs`` while the runtime read
        ``channel/mod.rs``.
        """
        synthetic = (
            "import os\n"
            "DISCOVERY_ANCHORS = ('../mod.rs',)\n"
            "rows = [os.path.join(\n"
            "    'src/pilotdeck-gateway/src/channel', 'sms/..', 'mod.rs')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('missing anchor: channel/mod.rs' in problem for problem in problems),
            problems)
        self.assertTrue(
            any('stale anchor' in problem and '../mod.rs' in problem
                for problem in problems),
            problems)

    def test_a_contextlib_or_dotted_import_chdir_is_recognised(self):
        """``contextlib.chdir`` and ``import os.path`` move the directory too.

        The check only knew the ``os`` module spelling, so ``with
        contextlib.chdir(...):`` and a table that imported ``os.path`` (which
        binds ``os`` to ``os.path`` in the import map) left the repository-root
        premise standing and keyed a path the runtime builds elsewhere.
        """
        shapes = {
            'contextlib': "import contextlib\ncontextlib.chdir('src/x')\n",
            'contextlib alias': "import contextlib as ctx\nctx.chdir('src/x')\n",
            'os.path import': "import os.path\nos.chdir('src/x')\n",
            'from contextlib import chdir': "from contextlib import chdir\nchdir('src/x')\n",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n" + body
                    + "rows = [Path('mod.rs')]\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem for problem in problems),
                    problems)

    def test_every_sys_path_spelling_is_reported(self):
        """An alias, a rebound binding or ``+=`` hides the same insertion.

        ``sys.path`` is a list object reached through several spellings; the
        check only knew ``sys.path.append`` / ``insert`` / ``extend`` directly,
        so ``import sys as s``, ``P = sys.path``, ``sys.path += [...]`` and an
        ``extend`` over a named list all left the imported helper invisible.
        """
        shapes = {
            'module alias': "import sys as s\n"
                            "s.path.append(str(FIXTURES / '_shared'))\n",
            'rebound binding': "P = sys.path\n"
                               "P.append(str(FIXTURES / '_shared'))\n",
            'augmented': "sys.path += [str(FIXTURES / '_shared')]\n",
            'extend a literal list': "sys.path.extend([str(FIXTURES / '_shared')])\n",
            'extend a named list': "D = [str(FIXTURES / '_shared')]\n"
                                   "sys.path.extend(D)\n",
            # A slice *assignment* replaces the list's contents through the
            # same store the call spellings reach; reading only the call forms
            # left it keyless.
            'slice assignment': "sys.path[:0] = [str(FIXTURES / '_shared')]\n",
            'annotated assignment': "sys.path: list = [str(FIXTURES / '_shared')]\n",
            'or-assignment': "sys.path |= [str(FIXTURES / '_shared')]\n",
            'starred element': "sys.path[:0] = [*[str(FIXTURES / '_shared')]]\n",
            'two-hop augmented': "P = sys.path\n"
                                 "P += [str(FIXTURES / '_shared')]\n",
            'mutated named list': "D = []\n"
                                  "D.append(str(FIXTURES / '_shared'))\n"
                                  "sys.path[:0] = D\n",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "import sys\n"
                    "from pathlib import Path\n"
                    "FIXTURES = Path(__file__).resolve().parents[1]\n"
                    "DISCOVERY_ANCHORS = ()\n" + body
                    + "import negative_control\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unsupported sys.path' in problem for problem in problems),
                    problems)

    def test_a_shallow_copy_carries_a_nested_write_back_to_the_original(self):
        """``CH.copy()`` and its five siblings share every nested container.

        A write below the copy's own top level rewrites what ``CH['a']``
        reads, so the literal behind ``CH`` is stale and the key must not be
        accepted. Only the ``CH.copy()`` spelling was an edge before; the rest
        were edges to nothing.
        """
        mapping = ("CH = {'a': {'b': 'src/pilotdeck-gateway/src/channel/x'}}\n"
                   "DISCOVERY_ANCHORS = ('x/mod.rs',)\n")
        sequence = ("CH = [{'b': 'src/pilotdeck-gateway/src/channel/x'}]\n"
                    "DISCOVERY_ANCHORS = ('x/mod.rs',)\n")
        shapes = {
            'dict()': (mapping, "CP = dict(CH)\n",
                       "CP['a']['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                       "rows = [Path(CH['a']['b']) / 'mod.rs']\n"),
            'copy.copy()': (mapping, "import copy\nCP = copy.copy(CH)\n",
                            "CP['a']['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                            "rows = [Path(CH['a']['b']) / 'mod.rs']\n"),
            '{**CH}': (mapping, "CP = {**CH}\n",
                       "CP['a']['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                       "rows = [Path(CH['a']['b']) / 'mod.rs']\n"),
            'CH.copy()': (mapping, "CP = CH.copy()\n",
                          "CP['a']['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                          "rows = [Path(CH['a']['b']) / 'mod.rs']\n"),
            'list()': (sequence, "CP = list(CH)\n",
                       "CP[0]['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                       "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
            '[*CH]': (sequence, "CP = [*CH]\n",
                      "CP[0]['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                      "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
            'tuple()': (sequence, "CP = tuple(CH)\n",
                        "CP[0]['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                        "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
            'sorted()': (sequence, "CP = sorted(CH)\n",
                         "CP[0]['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                         "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
            'copy.deepcopy()': (sequence, "import copy\nCP = copy.deepcopy(CH)\n",
                                "CP[0]['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                                "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
            'from copy import copy as cp2': (
                sequence, "from copy import copy as cp2\nCP = cp2(CH)\n",
                "CP[0]['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
            'dict(CH, extra=1)': (mapping, "CP = dict(CH, extra=1)\n",
                                  "CP['a']['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                                  "rows = [Path(CH['a']['b']) / 'mod.rs']\n"),
            '[None, *CH]': (sequence, "CP = [None, *CH]\n",
                            "CP[1]['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                            "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
            'CH + [x]': (sequence, "CP = CH + [{'b': 'x'}]\n",
                         "CP[0]['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                         "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
            'CH[:]': (sequence, "CP = CH[:]\n",
                      "CP[0]['b'] = 'src/pilotdeck-gateway/src/channel/sms'\n",
                      "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
            'nested +=  through a copy': (
                sequence, "CP = CH.copy()\n",
                "CP[0]['b'] += '-sms'\n",
                "rows = [Path(CH[0]['b']) / 'mod.rs']\n"),
        }
        for label, (head, copy, write, rows) in shapes.items():
            with self.subTest(shape=label):
                synthetic = ("from pathlib import Path\n" + head
                             + copy + write + rows)
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem for problem in problems),
                    problems)

    def test_a_shallow_copys_own_top_level_write_stays_silent(self):
        """A copy's top level is a separate object: rebinding it touches nothing.

        ``{**CH}['k'] = ...`` leaves ``CH['k']`` alone, so the reverse control
        must stay green — reporting it would turn every copy into a false red.
        """
        shapes = {
            'copy()': "CP = CH.copy()\n",
            '{**CH}': "CP = {**CH}\n",
            'dict()': "CP = dict(CH)\n",
        }
        for label, copy in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "CH = {'k': 'src/pilotdeck-gateway/src/channel/x'}\n"
                    "DISCOVERY_ANCHORS = ('x/mod.rs',)\n" + copy
                    + "CP['k'] = 'src/pilotdeck-gateway/src/channel/sms'\n"
                    "rows = [Path(CH['k']) / 'mod.rs']\n"
                )
                self.assertEqual(
                    violations(synthetic, 'example/negative_controls.py'), [])

    def test_whitespace_in_an_inline_spelled_key_is_reported(self):
        """A key with a space is unschedulable however its directory is spelled.

        ``SMS / 'my file.rs'`` ends in a bare component and was already
        reported, but ``ROOT / '…/channel/sms/my file.rs'`` spells the whole
        path inline: ``_bare_tail`` is false, so the expression was dropped
        while the host still could not schedule ``sms/my file.rs``.
        """
        shapes = {
            'path divide': "rows = [ROOT / 'src/pilotdeck-gateway/src/channel/sms/my file.rs']\n",
            'Path call': "rows = [Path(ROOT, 'src/pilotdeck-gateway/src/channel/sms/my file.rs')]\n",
            'os.path.join': "rows = [os.path.join(ROOT, 'src/pilotdeck-gateway/src/channel/sms/my file.rs')]\n",
            # The key is built segment by segment, so the space may sit in a
            # *directory* piece the file name never repeats; reading only the
            # trailing segment missed it.
            'directory segment': "rows = [ROOT / 'a b' / 'mod.rs']\n",
            'nested directory segment': "rows = [ROOT / 'x' / 'a b']\n",
            'joined directory segment': "rows = [os.path.join(ROOT, 'a', 'my file.rs')]\n",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "import os\n"
                    "from pathlib import Path\n"
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "DISCOVERY_ANCHORS = ()\n" + body
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('whitespace in a discovery key cannot be scheduled' in problem
                        for problem in problems),
                    problems)

    def test_a_whitespace_path_outside_the_repository_stays_silent(self):
        """Only a *discovery key* is unschedulable.

        ``Path('/tmp') / 'x y' / 'mod.rs'`` resets at the absolute piece and
        names no file under the repository, so it produces no key at all — the
        whitespace check must not turn it into a false red.
        """
        shapes = {
            'absolute reset': "rows = [Path('/tmp') / 'x y' / 'mod.rs']\n",
            'absolute outside': "rows = [Path('/tmp/x y')]\n",
            'outside join': "rows = [os.path.join('/tmp', 'a b', 'mod.rs')]\n",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "import os\n"
                    "from pathlib import Path\n"
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "DISCOVERY_ANCHORS = ()\n" + body
                )
                self.assertEqual(
                    violations(synthetic, 'example/negative_controls.py'), [])

    def test_an_absolute_sys_path_entry_outside_the_repository_stays_silent(self):
        """``/tmp/thirdparty`` cannot hide a repository module, so it is silent.

        The insertion guard reported any argument carrying a slash, which turned
        every third-party ``sys.path`` entry into a false red.
        """
        synthetic = (
            "import sys\n"
            "DISCOVERY_ANCHORS = ()\n"
            "sys.path.insert(0, '/tmp/thirdparty')\n"
            "import negative_control\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_sys_path_snapshot_restored_unchanged_stays_silent(self):
        """``saved = sys.path[:] ... sys.path[:] = saved`` adds nothing.

        The restore writes the *same* list back, so no repository directory is
        inserted. Rooting the slice copy at the ``sys`` name made the snapshot
        look mutated and turned the restore into a false red (measured on
        ``negative-adaptive``, which saved its sys.path exactly this way).
        """
        shapes = {
            'in a function': (
                "import sys\n"
                "def isolate():\n"
                "    saved = sys.path[:]\n"
                "    try:\n"
                "        pass\n"
                "    finally:\n"
                "        sys.path[:] = saved\n"
            ),
            'third-party named list': (
                "import sys\n"
                "D = ['/tmp/thirdparty']\n"
                "sys.path[:0] = D\n"
            ),
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = ("DISCOVERY_ANCHORS = ()\n" + body
                             + "import negative_control\n")
                self.assertEqual(
                    violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_drive_prefixed_segment_inside_a_join_is_reported(self):
        """``C:/tmp`` is an absolute path even when joined after a directory.

        On Windows ``Path('sms') / 'C:/tmp'`` is ``C:/tmp``; treating the piece
        as a relative segment keyed ``tmp/mod.rs`` inside the repository. A
        leading backslash is the same shape. Both must be reported instead.
        """
        shapes = {
            'path divide': "rows = [Path('sms') / 'C:/tmp' / 'mod.rs']\n",
            'os.path.join': "rows = [os.path.join('sms', 'C:/tmp', 'mod.rs')]\n",
            'os.sep.join': "rows = [os.sep.join(('sms', 'C:/tmp', 'mod.rs'))]\n",
            'leading backslash': "rows = [os.path.join('sms', r'\\\\tmp', 'mod.rs')]\n",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = ("import os\nfrom pathlib import Path\n"
                             "DISCOVERY_ANCHORS = ()\n" + body)
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem for problem in problems),
                    problems)


if __name__ == '__main__':
    unittest.main(verbosity=2)
