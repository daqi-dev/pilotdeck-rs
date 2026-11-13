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

Known limit: the sweep reads the live tree without a lock, so this gate must
run serially with a host negative control in the same tree — a concurrent
truncating rewrite can hand ``sweep`` half a file. CI jobs each check out
their own copy, so the exposure is local-only (r25, review P2c-1).
"""
import ast
import fnmatch
import re
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
# 281 files (the host glob reaches the ones under tests/fixtures/, not the top-level fixtures/ tables; #2141), 202 blind only, 3 with unresolvable shapes only, 76 in both; 1484 keys: 416 bare-filename joins and 1068 shared-helper import keys.
# Why a key a table needs shows up in no table text: the import line spells a
# module name, while the host keys the helper file it lives in.
IMPORTED_HELPER_REASON = 'imported shared fixture helper'

KNOWN_BLIND = {
    '_negkit/negative_compare_verdicts.py': '1 key',
    '_negkit/negative_contract.py': '4 keys',
    '_negkit/negative_non_head.py': '5 keys',
    '_negkit/negative_submit_migration.py': '10 keys',
    '_shared/negative_control.py': '4 keys + 6 unresolvable',
    '_shared/negative_sqlite_header.py': '6 keys',
    '_shared/regeneration-negative.py': '6 keys',
    'agent-event-projector/negative.py': '5 keys',
    'always-on-2294/negative_controls.py': '7 keys + 1 unresolvable',
    'anchor-marker/negative_controls.py': '3 keys',
    'api-server-channel/negative_controls.py': '6 keys + 1 unresolvable',
    'api-web/negative_controls.py': '10 keys',
    'attach-followups-a/negative_controls.py': '6 keys',
    'attach-twopass-1460/negative_controls.py': '6 keys',
    'background-create-gate/negative_controls.py': '6 keys',
    'background-output/negative.py': '6 keys',
    'background-panic/negative_controls.py': '4 keys + 2 unresolvable',
    'background-persist-progress/negative_controls.py': '6 keys',
    'background-persist-restart/negative_controls.py': '6 keys',
    'background-subtick-gate/negative_controls.py': '8 keys',
    'baseline-provenance/negative_controls.py': '3 keys',
    'channel-bluebubbles/negative_controls.py': '9 keys + 2 unresolvable',
    'channel-core/negative_controls.py': '11 keys + 2 unresolvable',
    'channel-feishu/negative_controls.py': '12 keys + 2 unresolvable',
    'channel-loader-startup/negative_controls.py': '6 keys',
    'channel-slack/negative_controls.py': '15 keys',
    'channel-sms-head-timeout/negative_controls.py': '7 keys',
    'channel-unmigrated-startup/negative_controls.py': '6 keys',
    'channel-unmigrated/negative_controls.py': '8 keys + 1 unresolvable',
    'cli-parity-isolation/negative_controls.py': '1 key + 1 unresolvable',
    'cli-parity-lock-bounded/negative_controls.py': '7 keys',
    'cli-parity-mock-gateway/negative_controls.py': '1 key + 1 unresolvable',
    'closure_guard_negative.py': '3 keys + 2 unresolvable',
    'commands-catalog/negative_controls.py': '4 keys',
    'console-panic-safety/negative_controls.py': '4 keys + 2 unresolvable',
    'config-changes/negative_coverage.py': '6 keys',
    'config-error-names/negative_controls.py': '4 keys + 1 unresolvable',
    'config-store/negative_controls.py': '6 keys',
    'connection-context/negative.py': '7 keys + 3 unresolvable',
    'cron-schedule/negative_controls.py': '8 keys',
    'cron-scheduler/negative_controls.py': '9 keys',
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
    'edgeclaw-llm-config/negative.py': '2 keys',
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
    'edgeclaw-retriever/negative.py': '2 keys',
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
    'grep-etxtbsy/negative_controls.py': '7 keys',
    'handle-path-macos/negative_controls.py': '6 keys',
    'hooks-output/negative.py': '1 key',
    'host-event-pump/negative_controls.py': '1 key',
    'html-timing/negative_controls.py': '3 keys + 1 unresolvable',
    'http-store-park/negative_controls.py': '6 keys + 1 unresolvable',
    'interaction-bus/negative_controls.py': '4 keys + 1 unresolvable',
    'interaction-values/negative_controls.py': '4 keys',
    'issue-1246/negative_controls.py': '3 keys',
    'issue-1437-methods/negative_controls.py': '6 keys',
    'issue-1604-require-text/negative_controls.py': '6 keys',
    'issue-2495/negative_controls.py': '6 keys',
    'issue-297-domain-fold/negative_controls.py': '6 keys',
    'issue-297/negative_controls.py': '6 keys',
    'js-math-zero/negative.py': '2 keys',
    'ledger-diffs/negative_controls.py': '2 keys + 1 unresolvable',
    'live-e2e-tmpdir/negative_controls.py': '6 keys',
    'live-e2e/negative_controls.py': '6 keys + 4 unresolvable',
    'live-history/negative.py': '1 key',
    'locale-adaptive/negative_controls.py': '7 keys',
    'locale-contract/negative_controls.py': '6 keys',
    'locked-storage/negative_controls.py': '6 keys',
    'manager-gateway/negative_controls.py': '7 keys + 2 unresolvable',
    'mcp-detached-dispatch/negative_controls.py': '6 keys',
    'mcp-gateway/negative_controls.py': '6 keys',
    'mcp-hooks-spawn/negative_controls.py': '1 key + 2 unresolvable',
    'mcp-http-corrupt-body/negative_controls.py': '6 keys',
    'mcp-http-encoding/negative.py': '6 keys',
    'mcp-resolver-lease/negative_controls.py': '7 keys',
    'memory-maintenance/negative_controls.py': '2 keys',
    'metadata-from-env/negative_controls.py': '8 keys',
    'mode-routing/negative_controls.py': '6 keys',
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
    'native-collation/negative_controls.py': '8 keys',
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
    'native-ws-caps/negative_controls.py': '9 keys',
    'native-ws-entry/negative_controls.py': '11 keys + 1 unresolvable',
    'native-ws-notify/negative_controls.py': '6 keys',
    'negative-adaptive/isolation_negative.py': '2 keys',
    'negative-adaptive/negative_controls.py': '2 keys + 1 unresolvable',
    'negative-output-directory/negative.py': '7 keys',
    'negative-output-directory/process_probe.py': '1 key + 1 unresolvable',
    'negative_oracle_guard.py': '1 key + 3 unresolvable',
    'negative_scope.py': '1 unresolvable',
    'network-timing/negative_controls.py': '7 keys',
    'network-timing/negative_private.py': '2 keys + 2 unresolvable',
    'node-io/negative_controls.py': '4 keys',
    'node-provenance/negative.py': '6 keys + 1 unresolvable',
    'operation-ledger/negative.py': '6 keys',
    'output-schema/negative.py': '5 keys',
    'p01-preflight/negative_controls.py': '6 keys + 1 unresolvable',
    'parity-lock-release/negative_controls.py': '6 keys + 1 unresolvable',
    # #2398 parity-subprocess-hardening 与 #1891 sidecar-liveness：随 #2136
    # fix-r41 合并 main（c3100a2b7）到达的两张新表，无 DISCOVERY_ANCHORS，
    # 按 #2141 规则以实测键数登记为 blind；补锚点归 #2141 修表清单。
    'parity-subprocess-hardening/negative_controls.py': '2 keys',
    'pending-reanchor/negative_controls.py': '4 keys',
    'permission-mode-surrogate/negative_controls.py': '6 keys',
    'phased-run-drained/negative_controls.py': '6 keys',
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
    'regex-proto-budget/negative_controls.py': '7 keys + 1 unresolvable',
    'replacement/negative-518.py': '3 keys',
    'replacement/negative.py': '1 key',
    'retry-after-dialects/negative_controls.py': '6 keys',
    'retirement-negctl-roster/negative_controls.py': '5 keys',
    'run-registry/count-negative.py': '8 keys + 1 unresolvable',
    'run-registry/timestamp-negative.py': '7 keys + 1 unresolvable',
    'runtime-spawn-panic/helpers_negative.py': '3 keys',
    'runtime-spawn-panic/negative_controls.py': '4 keys + 2 unresolvable',
    'same-json-surfaces/negative_controls.py': '2 keys',
    'sdk-cli-lifecycle/negative_controls.py': '6 keys + 2 unresolvable',
    'sdk-format/negative_controls.py': '8 keys + 2 unresolvable',
    'sdk-mcp-p2/negative.py': '6 keys + 2 unresolvable',
    'sdk-mcp-session-tools/negative_controls.py': '6 keys',
    'sdk-run/negative_controls.py': '9 keys + 3 unresolvable',
    'sdk-sessions-list/negative_controls.py': '6 keys + 1 unresolvable',
    'sdk-sessions-messages/negative_controls.py': '10 keys + 1 unresolvable',
    'sdk-settings/negative_controls.py': '8 keys + 2 unresolvable',
    'sdk-signal-read-lease/negative_controls.py': '6 keys',
    'sdk-signals-close-order-pin/negative_controls.py': '6 keys + 2 unresolvable',
    'sdk-signals/negative_controls.py': '6 keys',
    'sdk-terminal/negative_controls.py': '8 keys + 2 unresolvable',
    'session-admission/negative.py': '5 keys + 1 unresolvable',
    'session-adopt/switch_negative.py': '7 keys',
    'session-assembly-1316/negative_controls.py': '4 keys + 2 unresolvable',
    'session-cwd-timing/negative_controls.py': '7 keys',
    'session-delete-2392/negative_controls.py': '6 keys',
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
    'session-reservation-2391/negative_controls.py': '6 keys',
    'settings-gateway/negative_controls.py': '5 keys + 1 unresolvable',
    'settings/negative_controls.py': '2 keys',
    'shared-runner-failclosed/negative_controls.py': '2 keys',
    'sidecar-liveness-1891/negative_controls.py': '6 keys',
    'sidecar-payload/negative.py': '1 key + 1 unresolvable',
    'skills-external/negative_controls.py': '6 keys',
    'skills-external/negative_controls_949.py': '6 keys',
    'skills-tree-files/negative.py': '2 keys + 2 unresolvable',
    'skills/negative_controls.py': '3 keys',
    'slot-probe-residual/negative-controls.py': '2 keys',
    'sms-stringify-depth/negative_controls.py': '4 keys + 2 unresolvable',
    'spill-isolation/negative_controls.py': '3 keys',
    'stats-cwd/negative_controls.py': '9 keys',
    'stress-followups/negative_controls.py': '2 keys + 1 unresolvable',
    'surrogate-collation-matrix/negative_controls.py': '7 keys',
    'switch-state-machine/negative_controls.py': '7 keys + 1 unresolvable',
    'task-budget-zero/negative.py': '2 keys',
    'tool-budget/negative_project.py': '1 key + 1 unresolvable',
    'transcript-resolve/negative_controls.py': '1 key',
    'transcript-trim/negative_controls.py': '6 keys',
    'treelock-budget/negative_controls.py': '7 keys',
    'turn-gateway-clock-edge/negative_controls.py': '6 keys',
    'turn-gateway/negative-fence.py': '10 keys',
    'turn-gateway/negative.py': '6 keys',
    'turn-publish/negative.py': '5 keys',
    'turn-read/negative.py': '5 keys',
    'turn-replay/negative.py': '7 keys',
    'turn-runner/forward_negative.py': '13 keys',
    'turn-runner/input_negative.py': '10 keys',
    'turn-runner/integrity_negative.py': '11 keys + 1 unresolvable',
    'turn-runner/live_negative.py': '8 keys',
    'turn-runner/negative_controls.py': '4 keys',
    'turn-runner/recording_negative.py': '5 keys',
    'turn-runner/runner_negative.py': '4 keys',
    'turn-submit-admission/negative_controls.py': '2 keys',
    'turn-submit-panic-close/negative.py': '5 keys + 2 unresolvable',
    'turn-submit/negative.py': '6 keys',
    'ui-server-parity-isolation/negative_controls.py': '3 unresolvable',
    'unsafe-config-boundary/negative_controls.py': '4 keys',
    'upload-gateway/negative_controls.py': '3 keys',
    'user-dialog-channel/negative.py': '8 keys + 1 unresolvable',
    'user-dialog-guard-order/negative_controls.py': '6 keys',
    'user-dialog-http-store-1555/negative_controls_1555.py': '6 keys',
    'user-dialog-http-store/negative_controls_1232.py': '6 keys',
    'user-dialog-http-store/negative_controls_1277.py': '6 keys',
    'user-dialog-http-store/negative_controls_1947.py': '13 keys',
    'user-dialog-http-store/negative_controls_2152.py': '6 keys',
    'user-dialog-lock-scope/negative_controls.py': '4 keys + 2 unresolvable',
    'utf16-residual-sort/negative_controls.py': '10 keys',
    'web-conversion-lifecycle/negative.py': '7 keys',
    'webhook-channel/negative_controls.py': '10 keys',
    'wecom-callback/negative_controls.py': '12 keys + 2 unresolvable',
    'weixin-channel/negative_controls.py': '16 keys + 2 unresolvable',
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
    Assignment aliases are the same edge on both sides: ``cd = os.chdir`` (the
    call) and ``m = os`` (the module), including a dotted spelling
    (``osp = os.path``) and any number of hops (``m2 = m``).
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
            spelling = _dotted_spelling(value)
            if spelling is None:
                continue
            module_part, _, last = spelling.rpartition('.')
            if (name not in names
                    and ((module_part and last in CHDIR_CALLS
                          and module_part in modules)
                         or (not module_part and last in names))):
                names.add(name)
                grew = True
            if name in modules:
                continue
            if (spelling in CHDIR_MODULES
                    or (isinstance(value, ast.Name) and value.id in modules)):
                # ``m = os`` / ``osp = os.path`` / ``m2 = m``: a module reached
                # through an assignment alias still has the ``chdir`` attribute,
                # so the alias is a module edge exactly like an import alias.
                modules.add(name)
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
    nested container still rewrites the literal behind ``CH``. The copy may
    itself be of a *sub*container (``CP = CH['a'].copy()``): the copy then
    holds the objects ``CH['a']`` holds, so the edge carries the depth of the
    copied container as a shift and only writes that deep see the root.
    Following the graph transitively is what closes ``A = CH; B = A['a'];
    B['b'] = ...``; the earlier reading only paired a subscript whose root was
    itself bound to a container literal, so a plain alias in between hid the
    write and the stale key was accepted.

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
    # (name, root, levels between them); ``copy_of`` marks shallow copies —
    # (name, root, shift), where ``shift`` is the depth of the copied container
    # — whose top level is a separate object while every nested one is shared.
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
                    # or a subscript of one: ``SAVED = sys.path[:]`` snapshots
                    # an *attribute* chain, and rooting it at ``sys`` made
                    # every later ``sys.path[:] = SAVED`` restore read as a
                    # mutation of the snapshot. A *sub*container slice
                    # (``CP = CH['a'][:]``) is a shallow copy one level down,
                    # so it carries the depth of the sliced container.
                    if (root is not None
                            and isinstance(value.value, (ast.Name, ast.Subscript))
                            and depth is not None):
                        copy_of.append((name, root,
                                        _container_depth(value.value)))
                    continue
                # ``SUB = CH['a']`` names the *same* nested object one level
                # down; the closure below follows any number of aliases.
                if root is not None and depth is not None:
                    same_object.append((name, root, min(depth, 2)))
            else:
                for root, shift in _shallow_copy_roots(value, aliases):
                    copy_of.append((name, root, shift))
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
        for name, root, shift in copy_of:
            # Only the nested containers survive the copy. A write on the
            # copy at depth d (d >= 1) reaches the root at depth d + shift; a
            # root write at depth e reaches the copy only when the mutated
            # object is one the copy holds, that is e >= shift + 1. The copy's
            # own top level (depth 0) is a separate object either way.
            onto_root = {min(depth + shift, 2) for depth in depths.get(name, ())
                         if depth >= NESTED}
            onto_name = {depth - shift for depth in depths.get(root, ())
                         if depth - shift >= NESTED}
            if not onto_root <= depths.setdefault(root, set()):
                depths[root] |= onto_root
                grew = True
            if not onto_name <= depths.setdefault(name, set()):
                depths[name] |= onto_name
                grew = True
    return frozenset(name for name, hits in depths.items() if hits)


# Copy constructors that keep the elements themselves: a new top-level object
# whose nested containers are the very same ones the source holds.
SHALLOW_COPY_CALLS = frozenset({'dict', 'list', 'tuple', 'set', 'frozenset',
                                'sorted', 'copy.copy', 'copy.deepcopy'})


def _shallow_copy_roots(value, aliases=None):
    """``(root name, depth of the copied container)`` pairs for ``value``.

    ``CH.copy()``, ``copy.copy(CH)``, ``dict(CH)``, ``list(CH)``, ``tuple(CH)``,
    ``sorted(CH)``, ``{**CH}``, ``[*CH, extra]`` and ``CH + [x]`` all build a
    new top-level object that still holds the very same nested containers, so a
    write through any of them *below its own top level* rewrites what
    ``CH['a']`` reads. The source may be a *sub*container — ``CH['a'].copy()``
    copies the dict ``CH['a']`` names, so the copy shares the objects one level
    below that, and the returned depth shifts every write accordingly. The
    import alias is resolved for the bare spellings (``from copy import copy as
    cp2``), ``dict(CH, extra=1)`` keeps the source in its first positional
    argument, and every ``*`` expansion counts — requiring exactly one element
    left ``[None, *CH]`` as an edge to nothing. A copy's own top-level key is
    rebound on the copy alone — replacing ``{**CH}['k']`` leaves ``CH['k']``
    untouched — so the edge only carries writes that reach a nested container;
    see ``_mutated_containers``.
    """
    aliases = aliases or {}

    def copy_edge(source):
        root = _container_root_name(source)
        if root is None:
            return None
        depth = _container_depth(source)
        return None if depth is None else (root, min(depth, 2))

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
                    edge = copy_edge(sources[0])
                    return [edge] if edge is not None else []
            return []
        if isinstance(func, ast.Attribute):
            if func.attr == 'copy' and not value.args:
                # ``CH.copy()`` / ``CH['a'].copy()``: a bound-method call on the
                # container — the receiver is the source, however deep it sits,
                # and no dotted spelling of it exists to read first.
                edge = copy_edge(func.value)
                return [edge] if edge is not None else []
            spelling = _dotted_spelling(func)
            if spelling is None:
                # ``f().dict(...)``: the receiver is not a name, so no module
                # spelling can be resolved from it.
                return []
            head, _, rest = spelling.rpartition('.')
            if head:
                spelling = f'{aliases.get(head, head)}.{rest}'
            if spelling in SHALLOW_COPY_CALLS and value.args:
                # ``copy.copy(CH['a'])``: the module receiver is not a
                # container; the first argument is the source.
                edge = copy_edge(value.args[0])
                return [edge] if edge is not None else []
            return []
    if isinstance(value, ast.Dict):
        return [edge for key, element in zip(value.keys, value.values)
                if key is None and (edge := copy_edge(element)) is not None]
    if isinstance(value, (ast.List, ast.Tuple, ast.Set)):
        return [edge for element in value.elts
                if isinstance(element, ast.Starred)
                and (edge := copy_edge(element.value)) is not None]
    if isinstance(value, ast.BinOp) and isinstance(value.op, ast.Add):
        # ``CH + [x]`` / ``[x] + CH``: list concatenation copies the top level
        # and keeps every element object. An operand that is itself a plain
        # container (``CH['a']``) is not a copy expression, so it has no roots
        # of its own — read it as the container it names.
        roots = []
        for operand in (value.left, value.right):
            found = _shallow_copy_roots(operand, aliases)
            if found:
                roots.extend(found)
                continue
            edge = copy_edge(operand)
            if edge is not None:
                roots.append(edge)
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
    """Every operand of an ``a + b + c`` concatenation, any nesting shape.

    Python parses the unparenthesised chain left-associatively, but a table
    may parenthesise its own grouping — ``head + ('/dir' + '/sub/')`` puts
    the separators in a *right*-nested subtree the left-spine-only walk
    never saw. Flattening both sides keeps the operand list one reading
    regardless of where the parentheses sit (r33, arbitration design round:
    the right-nested separators let the concatenated-slice head pass
    silently).
    """
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return _add_operands(node.left) + _add_operands(node.right)
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


def _resolves_to_string_constant(node, constants, seen=()):
    """Whether ``node`` statically resolves to a string constant."""
    if isinstance(node, ast.Constant):
        return isinstance(node.value, str)
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return False
        return _resolves_to_string_constant(constants[node.id], constants,
                                            seen + (node.id,))
    return False


def _slices_a_string_constant(node, constants, seen=()):
    """Whether ``node`` reads a *substring* of a spelled string constant.

    ``WORD = 'src/pilotdeck-gateway/src'`` then ``Path(WORD[:3]) / …`` builds a
    repository path out of a prefix the contiguous text no longer carries:
    the discovery key exists only inside the string constant, so the join is
    as unresolvable as any other opaque head (r31, review P0). A *constant
    index* reads exactly one display character (``WORD[0]`` in a counter
    f-string) and stays silent. Every slice — constant bounds or not — may
    spell a directory, and a slice that is statically one character
    (``WORD[0:1]``) is reported too: fail-closed, one extra report beats a
    hole (the split that r30 introduced when it stopped treating the string
    itself as the container). An f-string interpolating such a slice is the
    same build, and the interpolated reading carries the visited names so a
    self-referential constant chain terminates instead of recursing
    (r33, arbitration design round: the JoinedStr reading dropped its
    ``seen``, and the ``+`` branch made the cycle reachable). A ``+``
    concatenation carries whatever either side reads:
    ``Path(WORD[:3] + '/pilotdeck-gateway')`` still builds from the substring
    (r32, review P0 — the predicate stopped at the ``BinOp`` and let the head
    pass silently). An *all-literal* ``+`` chain has no slice on either side
    and keeps its silence: the network-timing precedent concatenates unit-test
    labels, and a whole-string ``'sms' + '/' + 'mod.rs'`` is the README's
    registered blind shape, not a new report.
    """
    if isinstance(node, ast.Subscript):
        return (isinstance(node.slice, ast.Slice)
                and _resolves_to_string_constant(node.value, constants))
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return False
        return _slices_a_string_constant(constants[node.id], constants,
                                         seen + (node.id,))
    if isinstance(node, ast.JoinedStr):
        return any(isinstance(part, ast.FormattedValue)
                   and _slices_a_string_constant(part.value, constants,
                                                 seen)
                   for part in node.values)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return (_slices_a_string_constant(node.left, constants, seen)
                or _slices_a_string_constant(node.right, constants, seen))
    return False


def _element_holds_a_path_literal(node, constants, seen):
    """Whether an *element* of a container spells a relative path literal.

    The element position is where a resolved name may land on a plain
    constant: ``S = 'src/x'`` then ``CH = {'a': S}`` — the container still
    spells the path through the name (r29, review P0). An element may itself
    be a nested container, so container shapes recurse here too.
    """
    if isinstance(node, ast.Constant):
        return (isinstance(node.value, str)
                and _is_relative_path_literal(node.value))
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return False
        return _element_holds_a_path_literal(constants[node.id], constants,
                                             seen + (node.id,))
    return _container_holds_a_path_literal(node, constants, seen)


def _container_holds_a_path_literal(node, constants, seen=()):
    """Whether a container constant holds a relative path literal, at any depth.

    ``CH = {'a': {'b': 'src/x'}}`` keeps the directory one index deeper than
    the shapes this guard first looked at; recursing into nested containers is
    what makes ``Path(CH['a']['b']) / 'mod.rs'`` read as a repository path
    instead of dropping out of both the key and the report. Only real
    containers count here: a name resolving to a *string* is a subscripted
    display string (``WORD[0]`` in ``f'{WORD[0]}/{total} killed'``), not a
    table of directories, and must stay silent — treating the constant
    itself as the container reported that display f-string (r30, review P1).
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
    return any(_element_holds_a_path_literal(element, constants, seen)
               for element in elements)


def _element_holds_a_bare_component(node, constants, seen):
    """Whether an *element* of a container spells a bare component.

    The mirror of ``_element_holds_a_path_literal`` with the single-component
    test in place of the separator one.
    """
    if isinstance(node, ast.Constant):
        return (isinstance(node.value, str)
                and _is_bare_component(node.value))
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return False
        return _element_holds_a_bare_component(constants[node.id], constants,
                                                seen + (node.id,))
    return _container_holds_a_bare_component(node, constants, seen)


def _container_holds_a_bare_component(node, constants, seen=()):
    """Whether a container constant holds a bare component, at any depth.

    ``D = {'one': 'src'}`` then ``Path(D[name]) / 'x.rs'`` — with ``name``
    runtime — may well start at the ``src`` directory, exactly as the literal
    subscript does; the value is only hidden behind the index, not absent
    (r28, review P0). Mirrors ``_container_holds_a_path_literal``: only real
    containers count, so a subscripted display string stays silent (r30,
    review P1).
    """
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return False
        return _container_holds_a_bare_component(constants[node.id], constants,
                                                 seen + (node.id,))
    if isinstance(node, ast.Subscript):
        return _container_holds_a_bare_component(node.value, constants, seen)
    if isinstance(node, (ast.Tuple, ast.List, ast.Set)):
        elements = node.elts
    elif isinstance(node, ast.Dict):
        elements = node.values
    else:
        return False
    return any(_element_holds_a_bare_component(element, constants, seen)
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


def _leading_path_piece(node, aliases=None):
    """The piece a path chain builds from, without its ``/``-joined tail.

    ``Path(D[name]) / 'a' / 'lib.rs'``, ``os.path.join(D[name], 'a', 'x.rs')``
    and ``Path(D[name], 'a', 'x.rs')`` all build from ``D[name]``: unwrapping
    the joins and calls leaves the leading piece, which is what decides
    whether the chain starts at a directory a container might name.
    """
    while isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        node = node.left
    while True:
        if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                and node.func.id in ('str', 'Path') and len(node.args) == 1
                and not node.keywords):
            node = node.args[0]
        elif (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                and node.func.attr != 'join'):
            # A method on the piece itself — ``Path(D[name]).resolve()`` and
            # ``Path(D[name]).joinpath('a', 'b')``: the *receiver* carries
            # the piece, not the literal arguments after it (r29, review P0 —
            # taking the first argument read the tail and dropped the dynamic
            # head). ``join`` keeps reading its first argument: a stdlib join
            # builds from its arguments, not from the ``os.path`` receiver.
            # Keyword arguments do not change the receiver — excluding them
            # let ``Path(D[name]).with_suffix(suffix='')`` fall through to the
            # generic call branch and read the empty-string tail instead
            # (r30, review P0). But the receiver resolves through the import
            # map to a *module* — ``os.path.realpath(D[name], strict=False)``
            # — is a module *function*: the path sits in its first argument,
            # and descending into the ``os.path`` module object let that
            # dynamic head pass silently (r31, review P0).
            receiver = node.func.value
            spelling = _dotted_spelling(receiver)
            if (spelling is not None and aliases
                    and spelling.partition('.')[0] in aliases and node.args):
                node = node.args[0]
            else:
                node = receiver
        elif isinstance(node, ast.Call) and node.args:
            node = node.args[0]
        elif isinstance(node, ast.Attribute):
            node = node.value
        else:
            return node


def _single_component_directory(node, constants):
    """Whether ``node`` reads a one-component directory out of a container.

    ``MODES = {'fast': 'fast'}`` then ``Path(MODES['fast']) / name`` starts at
    a directory whose only component the container spells: the file name is
    runtime, so the ``fast/<file>`` key is unknowable — the same hole as a
    runtime file name behind a spelled-out directory. The value has to be a
    string a bare component could name; a table of numbers or labels with
    separators is not a directory lookup and stays silent. An f-string
    interpolating such an index is the same build, exactly as in
    ``_indexes_a_path_container``.
    """
    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
            and node.func.id in ('str', 'Path') and len(node.args) == 1
            and not node.keywords):
        node = node.args[0]
    if isinstance(node, ast.Subscript):
        element = _container_element(node.value, node.slice, constants)
        if element is None:
            # A dynamic index, a mutated container or a key this scanner
            # cannot pair hides the value rather than proving it absent: the
            # container still spelling bare components means the runtime may
            # read one of them as the directory, so the lookup stays a
            # directory read instead of silently reading "not one" (r28,
            # review P0). ``D = {'one': 'src'}`` with ``D[name]`` is the same
            # hole ``D['one']`` already keyed.
            return _container_holds_a_bare_component(node.value, constants)
        # A literal index onto a *name* (``D = {'one': S}`` with ``S =
        # 'src'``) reaches this function with the element still a name, and
        # the f-string net — the caller of this lookup — does not go through
        # the literal-index spellings, so the name has to be resolved here:
        # ``f"{D['one']}/{name}"`` must report exactly as the written-out
        # ``f"{'src'}/{name}"`` does (r30, review P1: the r29 comment claimed
        # the other net covered the named spelling, which only holds for
        # slash-join arguments, not f-strings). A mutated container makes the
        # written value stale, so it falls back to the container reading
        # rather than trusting the snapshot.
        if isinstance(element, ast.Name):
            resolved = None
            if element.id not in getattr(constants, 'mutated', frozenset()):
                resolved = constants.get(element.id)
            if isinstance(resolved, ast.Constant):
                return (isinstance(resolved.value, str)
                        and _is_bare_component(resolved.value))
            return _container_holds_a_bare_component(node.value, constants)
        return (isinstance(element, ast.Constant)
                and isinstance(element.value, str)
                and _is_bare_component(element.value))
    if isinstance(node, ast.JoinedStr):
        return any(isinstance(part, ast.FormattedValue)
                   and _single_component_directory(part.value, constants)
                   for part in node.values)
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


def _runtime_parents_index(node, pairs, position, seen=()):
    """Whether ``node`` takes ``.parents`` through an index this scanner cannot read.

    Walks the same binding chain ``_levels_above_file`` walks, so the two
    cannot disagree about what a name meant at a given statement. ``parents[-
    1]`` and other *literal* indexes are not runtime values — the depth check
    keeps its old reading for them — and a value that never spells ``.parents``
    answers False, keeping the historic assumption for shapes that were never
    statically visible at all.

    The structural walk used to stop at the shapes it recognized — a
    ``Path(...)`` wrapper, a zero-argument ``resolve()`` — so a method call
    *with* arguments (``parents[int(0)].joinpath('..')``) or a join wrapping
    the chain read back as "no runtime index" and the unverifiable root was
    assumed anyway (r29, review P0). The whole expression is now searched: a
    runtime ``.parents`` index anywhere below fails closed, and every name in
    it is followed through its binding, so a chain hidden behind a wrapper
    name is still read. A literal index keeps its old silent reading wherever
    it sits; the depth it spells may still be invisible (``joinpath('..')``
    after it), which stays with the historic "not statically visible" class
    rather than this fail-closed one.
    """
    if isinstance(node, ast.Name):
        if node.id == '__file__' or node.id in seen:
            return False
        binding = _binding_before(pairs, node.id, position)
        if binding is None:
            return False
        index, value = binding
        return _runtime_parents_index(value, pairs, index, seen + (node.id,))

    def dynamic(sub):
        index = sub.slice
        if (isinstance(index, ast.UnaryOp)
                and isinstance(index.op, (ast.UAdd, ast.USub))):
            # ``parents[-1]`` spells its literal through a unary op; it is not
            # a runtime value, and the depth check keeps its old silent
            # reading for it (``_levels_above_file`` cannot verify it either).
            index = index.operand
        return (not isinstance(index, ast.Constant)
                or not isinstance(index.value, int))

    for sub in ast.walk(node):
        if (isinstance(sub, ast.Subscript)
                and isinstance(sub.value, ast.Attribute)
                and sub.value.attr == 'parents' and dynamic(sub)):
            return True
        if isinstance(sub, ast.Name):
            if sub.id == '__file__' or sub.id in seen:
                continue
            binding = _binding_before(pairs, sub.id, position)
            if binding is None:
                continue
            index, value = binding
            if _runtime_parents_index(value, pairs, index,
                                       seen + (sub.id,)):
                return True
    return False


def _root_definition_problems(tree, self_path, root):
    """Report a module-level ``ROOT`` that does not name the repository root.

    Every resolver short-circuits a ``ROOT`` name to "the repository root"
    without reading its definition, so a table that computes it one level short
    (``parents[4]``) builds ``src/<file>`` at runtime while this scanner keys
    ``<file>`` — one component, dropped as "no discovery key", with no anchor
    demanded and nothing reported. The depth is checked only where it is
    statically visible from ``__file__``; every other spelling keeps the old
    assumption, because guessing would fail legitimate tables. The one
    exception is a ``.parents`` reached through a runtime index
    (``parents[int('4')]``, ``parents[IDX]``): the depth is not merely
    invisible but unverifiable, and assuming the root keyed joins from a
    directory the runtime may not have used, so it is reported instead.
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
                if levels is None and _runtime_parents_index(value, pairs,
                                                             position):
                    # ``ROOT = Path(__file__).resolve().parents[int('4')]``:
                    # the depth is behind a runtime index, so assuming the
                    # repository root here keyed every join from a directory
                    # the runtime may not have used. The depth is unverifiable
                    # rather than merely invisible, which fails closed.
                    message = (f'{name} reaches .parents through a runtime '
                               f'index (unresolvable depth, this scanner '
                               f'cannot verify it names the repository root): '
                               f'{spelling}')
                    if message not in problems:
                        problems.append(message)
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


# The opaque target a relative import (``from . import os``) binds its name to:
# a module path no static reading can spell, so it is compared for identity
# only and never maps anywhere (r29, review P0).
_RELATIVE_IMPORT_TARGET = '<relative import>'


def _alias_bindings(tree):
    """``{name: {module spellings}}`` for every name an import statement binds.

    A name can be bound more than once — at the top level and inside a
    function, or by two functions — and each binding is collected: scopes
    decide which module a use site sees, not a merge order. A relative import
    (``from . import os``) binds its name to a package-local module this
    scanner cannot resolve, so it counts as its own opaque target (r29,
    review P0): a scope mixing it with an absolute import of the same name
    is a conflict — the join nets must not silently pick either side — while
    a sole relative binding stays out of the resolution map and keeps the
    literal-spelling fallback.
    """
    bindings = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.asname:
                    bindings.setdefault(alias.asname, set()).add(alias.name)
                else:
                    # ``import os.path`` binds the name ``os`` to the *package*,
                    # not to the submodule: ``os.chdir`` still exists and
                    # ``os.path.join`` still resolves. Mapping the bare name to
                    # the dotted spelling made ``os.path.join`` read as
                    # ``os.path.path.join`` and hid every join call in a table
                    # that spells the import that way.
                    top = alias.name.split('.')[0]
                    bindings.setdefault(top, set()).add(top)
        elif isinstance(node, ast.ImportFrom):
            if node.module and not node.level:
                for alias in node.names:
                    if alias.name != '*':
                        bound = alias.asname or alias.name
                        bindings.setdefault(bound, set()).add(
                            f'{node.module}.{alias.name}')
            elif node.level:
                # ``from . import os``: a package-relative import binds the
                # name to a module this scanner cannot resolve. It still
                # counts as a binding — skipping it whole let a function-local
                # ``import random as os`` look like the only binding of ``os``
                # while the module-level relative one was the real join path
                # (r29, review P0) — so it is collected as its own opaque
                # target: mixing it with an absolute import of the same name
                # is a conflict, and a sole relative binding keeps the
                # literal-spelling fallback rather than resolving anywhere.
                for alias in node.names:
                    if alias.name != '*':
                        bound = alias.asname or alias.name
                        bindings.setdefault(bound, set()).add(
                            _RELATIVE_IMPORT_TARGET)
    return bindings


def _module_aliases(tree):
    """Map every name an import statement binds to the module path it names.

    A name whose bindings disagree — ``import os`` at the top level and
    ``import random as os`` inside a function — means a different module at
    different use sites, and one merged map silently picked the last binding
    it met: the function-local ``os`` overwrote the module-level one and a
    genuine ``os.path.join`` read as ``random.path.join``, dropping the join
    out of every net (r28, review P0; a regression r25 introduced by
    merging scopes). Such names are left out of the map — a use then falls
    back to its literal spelling, the module-level reading — and
    ``_alias_binding_conflicts`` reports them at their use sites. A name
    bound only by a relative import maps nowhere for the same reason: where
    the package's module sits is a runtime fact (r29, review P0), so the
    literal spelling is the safer reading.
    """
    return {name: next(iter(targets))
            for name, targets in _alias_bindings(tree).items()
            if len(targets) == 1
            and _RELATIVE_IMPORT_TARGET not in targets}


def _alias_binding_conflicts(tree):
    """Names whose import bindings do not agree on a single module."""
    return {name for name, targets in _alias_bindings(tree).items()
            if len(targets) > 1}


def _used_module_heads(tree):
    """Names the file uses as the head of a dotted spelling or a bare call."""
    heads = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Attribute):
            root = node
            while isinstance(root, ast.Attribute):
                root = root.value
            if isinstance(root, ast.Name):
                heads.add(root.id)
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
            heads.add(node.func.id)
    return heads


def _spells_a_repository_path(node, constants, seen=()):
    """Whether the *value* of ``node`` is a repository path, not data about one.

    Only the value-producing spine is followed: ``str(...)`` / ``Path(...)``
    wrappers, ``/`` joins, bare attribute chains such as ``.parent`` and
    module constants. A *method call* breaks the chain: ``.resolve()`` is
    not followed (a documented blind class — the README's follow-up list
    registers the ``__file__``-derived ``sys.path`` insertion spellings that
    ride on it, r37), and a call that merely takes a repository path as one
    of its arguments — ``difflib.unified_diff(..., fromfile=str(SOURCE))``,
    ``sorted(unknown)`` next to a source path — hands back text or a list,
    not the path, and must not read as a path join.
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


def _filesystem_join_text(node, constants, aliases=None):
    """The text a join of string literals builds, or None.

    ``os.path.join('sms', 'mod.rs')`` produces ``sms/mod.rs`` from pieces the
    table spells in full, yet the literal ``sms/mod.rs`` appears nowhere in the
    file, so the discovery loop never schedules the table when the file
    changes. Returning the joined text lets the caller key it and demand an
    anchor, which is what ``Path('sms') / 'mod.rs'`` already gets.

    The spelling is resolved through the import map, so ``import os as o``
    reaches the same calls (``o.path.join(...)``), and so does
    ``from os.path import join as j`` — a bare name whose alias carries the
    dotted target. Without that, an aliased join was neither keyed nor
    reported while every plain spelling was (r25, review P2a).

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
    if isinstance(func, ast.Attribute):
        if func.attr != 'join':
            return None
    elif not isinstance(func, ast.Name):
        return None
    spelling = _qualified_spelling(func, aliases)
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


def _join_resets_to_absolute(node, constants, aliases):
    """Whether a filesystem join's absolute argument discards its dynamic head.

    ``os.path.join(D[name], '/tmp', 'mod.rs')`` reads ``/tmp/mod.rs`` at
    runtime — outside the repository — so the single-component-directory
    fallback must not report it: the reset argument has already thrown the
    dynamic head away (r29, review P1). Only the *filesystem* joins reset;
    ``os.sep.join`` concatenates and keeps its own reading, so it never
    reaches this question. The resetting join may also sit *behind* the
    expression the caller hands over — wrapped in a ``Path``/``str`` call,
    ``Path(os.path.join(WORD[:3], '/tmp')) / 'src'`` — and the pathlib
    ``/`` operator resets on an absolute right operand exactly like a join's
    absolute argument, ``Path(WORD[:3]) / '/tmp'`` reading ``/tmp``
    (r33, arbitration design round: the entry saw only the outer chain and
    let both wrapped spellings through to a report the reset semantics
    contradict). Walking the ``/`` spine to its head first keeps one reset
    predicate for every spelling.
    """
    while isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        segment = _string_value(node.right, constants)
        if segment is not None and _resets_join(segment):
            return True
        node = node.left
    while (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
            and node.func.id in ('str', 'Path') and len(node.args) == 1
            and not node.keywords):
        node = node.args[0]
    if not isinstance(node, ast.Call) or not isinstance(
            node.func, (ast.Attribute, ast.Name)):
        return False
    # A bare name reaches the same stdlib join — ``from os.path import join
    # as j`` then ``j(D[name], '/tmp', 'mod.rs')`` resets exactly like the
    # dotted spelling, and the import alias map is resolved here so the
    # check is spelling-independent (r30, review P1: the ``Attribute``-only
    # gate left the imported spelling without the absolute-reset defence).
    if _qualified_spelling(node.func, aliases) not in _STDLIB_JOIN_CALLS:
        return False
    for element in _joined_elements(node):
        value = _string_value(element, constants)
        if value is not None and _resets_join(value):
            return True
    return False


def _relative_path_segments(node, constants, aliases=None):
    """Segments of a repository path spelled with relative literals only.

    The host runs tables from the repository root, so a path assembled with
    ``os.path.join`` from relative literals *is* a repository path even though
    no ``ROOT`` or ``__file__`` appears — the same premise
    ``_is_relative_path_literal`` already applies to a container's literal
    element. Anything that only exists at runtime returns None: it is neither
    keyed nor guessed.
    """
    text = _filesystem_join_text(node, constants, aliases)
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
    sys.path.insert(0, '_shared')``). A ``..``-prefixed literal is resolved
    against the current working directory the host starts tables in, and
    climbing out of the repository root says nothing about where the entry
    ends up: ``../<checkout>/src/…`` re-enters this very tree through a
    sibling copy or worktree, so the insertion is reported rather than assumed
    harmless.
    """
    aliases = aliases or {}
    text = _string_value(value, constants)
    if text is None:
        text = _filesystem_join_text(value, constants, aliases)
    if isinstance(value, ast.Call) and isinstance(value.func, ast.Attribute):
        if _qualified_spelling(value.func, aliases) in _FILESYSTEM_JOIN_CALLS:
            # ``sys.path.insert(0, os.path.join(ROOT, 'src/…'))``: the joined
            # text is not a literal of its own, but a repository directory
            # among the pieces is the same insertion.
            return any(_carries_a_directory_literal(element, constants,
                                                   changes_directory, aliases)
                       for element in _joined_elements(value))
    if text is not None:
        if text == '..' or text.startswith('../') or text.startswith('..\\'):
            # ``..`` may climb right back into this repository (a sibling
            # worktree or another checkout under the same parent); the
            # directory it ends up in is unknowable, so the insertion fails
            # closed like a repository directory.
            return True
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


def _bare_tail(node, constants, seen=(), aliases=None):
    """Whether the last path component comes from a bare file-name literal.

    ``SMS / 'mod.rs'`` and ``SOURCE.with_name('oracle.json')`` both end in a
    name the discovery loop cannot derive from the file text; ``SOURCE /
    'runtime/memory/gateway.rs'`` spells its directory inline and is already
    findable, so it needs no anchor. Join spellings are resolved through the
    import map so ``o.path.join(...)`` reads like ``os.path.join(...)``
    (r25, review P2a).
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return _is_bare_component(node.value)
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return False
        return _bare_tail(constants[node.id], constants, seen + (node.id,), aliases)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        return _bare_tail(node.right, constants, seen, aliases)
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name):
            if func.id == 'Path':
                return bool(node.args) and _bare_tail(node.args[-1], constants, seen,
                                                     aliases)
            if _qualified_spelling(func, aliases) in _STDLIB_JOIN_CALLS:
                # ``from os.path import join as j``: the bare name is the same
                # call as the dotted spelling, so its tail is judged the same.
                tail = _joined_tail(node) if node.args else None
                return tail is not None and _bare_tail(tail, constants, seen, aliases)
        if isinstance(func, ast.Attribute):
            if func.attr in ('joinpath', 'with_name'):
                return bool(node.args) and _bare_tail(node.args[-1], constants, seen,
                                                     aliases)
            if (func.attr == 'join'
                    and _qualified_spelling(func, aliases) in _FILESYSTEM_JOIN_CALLS):
                tail = _joined_tail(node)
                return tail is not None and _bare_tail(tail, constants, seen, aliases)
            if func.attr in PATH_CALL_ATTRS:
                return _bare_tail(func.value, constants, seen, aliases)
    return False


def _tail_spells_a_relative_directory(node, constants, seen=(), aliases=None):
    """Whether the last path component is a relative literal that is not bare.

    ``SMS / 'runtime/memory/gateway.rs'`` and ``SMS / '../x.rs'`` both end in
    a literal that carries separators. The first spells its directory inline,
    so the host's ``git grep -F`` still finds the key in the text; the second's
    separators normalise away (``'./x.rs'``, ``'x.rs/'``, ``'a/../x.rs'`` the
    same) and the key exists nowhere the host can grep. Telling the two apart
    is ``resolve``'s job, against the table's own text; this predicate only
    admits the family so the join is *evaluated* instead of dropped at the
    shape gate (r37, review P0 — the gate used to make both spellings silent).
    An inline spelling that still spells its own last two components
    (``'runtime/memory/gateway.rs'``) stays outside: the host greps the text
    and finds the key there, so the historic silence is correct. A literal
    must carry a ``/``: a lone ``..`` or a backslash-only spelling has nothing
    for the runtime to normalise away and keeps its historic reading.
    Absolute and drive-prefixed literals stay outside too: their reset
    semantics keep the historic silence. The traversal mirrors ``_bare_tail``
    statement for statement so the two predicates admit the same spellings.
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        # A separator must be present for the literal to be this family at
        # all: a lone ``..`` or a backslash-only spelling has no ``/`` for the
        # runtime to normalise away, so it keeps its historic reading.
        if ('/' not in node.value or node.value.startswith('/')
                or _is_drive_prefixed(node.value)):
            return False
        parts = tuple(part for part in node.value.split('/')
                      if part not in ('', '.'))
        collapsed = _collapsed_segments(parts)
        if collapsed and collapsed[0] == '..':
            # A leading parent climbs into the join's base, so the key's
            # directory comes from outside the tail and the tail alone can
            # never spell it (``'../x.rs'`` under ``…/channel/sms`` keys
            # ``channel/x.rs``).
            return True
        if len(collapsed) < 2:
            # ``'./mod.rs'``, ``'mod.rs/'`` and ``'a/../mod.rs'`` all
            # normalise to a bare file name: the key's directory is the
            # base's last component, which the tail never spells.
            return True
        # ``'runtime/memory/gateway.rs'`` keeps spelling its own last two
        # components, so the host's ``git grep -F`` still finds the key in
        # the text and the join keeps the historic silence; only a tail
        # whose ``..`` folding makes its own last two components a string
        # the text does not spell (``'a/b/../c.rs'`` keys ``a/c.rs``)
        # joins the recorded family.
        return '/'.join(collapsed[-2:]) not in node.value
    if isinstance(node, ast.Name):
        if node.id in seen or node.id not in constants:
            return False
        return _tail_spells_a_relative_directory(
            constants[node.id], constants, seen + (node.id,), aliases)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        return _tail_spells_a_relative_directory(node.right, constants, seen,
                                                  aliases)
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name):
            if func.id == 'Path':
                return bool(node.args) and _tail_spells_a_relative_directory(
                    node.args[-1], constants, seen, aliases)
            if _qualified_spelling(func, aliases) in _STDLIB_JOIN_CALLS:
                tail = _joined_tail(node) if node.args else None
                return (tail is not None
                        and _tail_spells_a_relative_directory(
                            tail, constants, seen, aliases))
        if isinstance(func, ast.Attribute):
            if func.attr in ('joinpath', 'with_name'):
                return bool(node.args) and _tail_spells_a_relative_directory(
                    node.args[-1], constants, seen, aliases)
            if (func.attr == 'join'
                    and _qualified_spelling(func, aliases) in _FILESYSTEM_JOIN_CALLS):
                tail = _joined_tail(node)
                return (tail is not None
                        and _tail_spells_a_relative_directory(
                            tail, constants, seen, aliases))
            if func.attr in PATH_CALL_ATTRS:
                return _tail_spells_a_relative_directory(
                    func.value, constants, seen, aliases)
    return False


def _ends_in_a_literal_tail(node, constants, aliases=None):
    """Whether a join ends in a literal the scanner can judge, bare or not.

    ``_bare_tail`` alone is the shape of a key that *must* be anchored; adding
    the relative-directory literal admits the joins whose separators might
    normalise away, so ``resolve`` can compare the derived key against the
    table text and decide between recording (the key is not greppable) and the
    historic inline silence (it is). Callers that only want the must-anchor
    shapes keep using ``_bare_tail`` directly.
    """
    return (_bare_tail(node, constants, aliases=aliases)
            or _tail_spells_a_relative_directory(node, constants,
                                                  aliases=aliases))


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
        segments = _relative_path_segments(node, constants, aliases)
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
            return _ends_in_a_literal_tail(node, constants, aliases=aliases)
        return _joins_a_path_string(node, constants)   # ``+`` / ``%`` with a path literal
    if isinstance(node, ast.JoinedStr):
        return _joins_a_path_string(node, constants)
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name):
            qualified = _qualified_spelling(func, aliases)
            if qualified in ('Path', 'pathlib.Path'):
                return bool(node.args) and _ends_in_a_literal_tail(
                    node, constants, aliases=aliases)
            if qualified in _STDLIB_JOIN_CALLS:
                # ``from os.path import join as j``: a bare name is the same
                # filesystem join as the dotted spelling (r25, review P2a).
                tail = _joined_tail(node) if node.args else None
                return (tail is not None
                        and _ends_in_a_literal_tail(tail, constants,
                                                     aliases=aliases))
            return False
        if isinstance(func, ast.Attribute):
            if func.attr in PATH_CALL_ATTRS:
                return _ends_in_a_literal_tail(node, constants, aliases=aliases)
            if func.attr == 'join':
                tail = _joined_tail(node) if node.args else None
                return (tail is not None
                        and _ends_in_a_literal_tail(tail, constants,
                                                     aliases=aliases))
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
    ``import`` line. Every other dynamic-import spelling — a call through a
    plain name (``from importlib import import_module as load`` then
    ``load('_shared.x')``, the unaliased ``import_module('_shared.x')``, or a
    function alias ``im = importlib.import_module`` then ``im('…')``), the
    module name in a keyword argument (``import_module(name='…')``), or a
    computed name (``__import__(RUNTIME_NAME)``) — is a *silent blind class*,
    not a boundary: it yields no key and reports no unresolvable shape, so the
    import closure of a table can pass unscheduled. It is registered as such
    in the README's follow-up list and pinned by
    ``test_the_registered_dynamic_import_closures_stay_silent`` (fix-r34,
    review P0; the family stopped per the slice-family arbitration).
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


def _text_without_anchor_declarations(text, tree):
    """The table's text with the ``DISCOVERY_ANCHORS`` assignments removed.

    The declarations exist precisely to put the discovery keys into the text
    the host greps, so reading the raw text would make every recorded key
    look greppable and drop the requirement the moment it is satisfied
    (r37): the greppability question the last-operand rule asks is whether
    the key is spelled anywhere *else*. Whole spanned lines are dropped — a
    statement sharing a line with a declaration loses its text too, which
    can only record more, never less.
    """
    lines = text.splitlines(keepends=True)
    drop = set()
    for statement in tree.body:
        targets = []
        if isinstance(statement, ast.Assign):
            targets = statement.targets
        elif isinstance(statement, (ast.AnnAssign, ast.AugAssign)):
            targets = [statement.target]
        if not any(isinstance(target, ast.Name)
                   and target.id == 'DISCOVERY_ANCHORS' for target in targets):
            continue
        drop.update(range(statement.lineno, statement.end_lineno + 1))
    return ''.join(line for number, line in enumerate(lines, start=1)
                   if number not in drop)


def bare_concat_keys(tree, table, root=FIXTURES, text=''):
    """``({key: [source expression, ...]}, problems)`` for one table.

    A key is the discovery key ``dir/file`` of a path the table builds from a
    bare file name, however the join is spelled: ``/``, ``Path(a, b)``,
    ``joinpath``, ``with_name``, ``with_suffix``, or a comprehension over a
    literal tuple. Every path shape the scanner cannot resolve is reported, so a
    table that hides its directory behind an unsupported expression fails the
    guard instead of quietly passing it.

    ``text`` is the table's own source; the last-join-operand rule below reads
    it (minus the ``DISCOVERY_ANCHORS`` declarations themselves) to tell "the
    key is spelled in the file, so the host's ``git grep -F`` finds it" from
    "the tail's separators normalise away, so the text never spells the key".
    An empty ``text`` (the default) records instead of staying silent —
    fail-closed when a caller has no text to compare with.
    """
    self_path = (FIXTURES_FROM_ROOT + tuple(table.split('/')) if root == FIXTURES
                 else tuple(table.split('/')))
    text = _text_without_anchor_declarations(text, tree)
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
    for name in sorted(_alias_binding_conflicts(tree) & _used_module_heads(tree)):
        # ``import os`` at the top level and ``import random as os`` inside a
        # function: the scanner cannot tell which module a use site sees, so
        # the join nets have to read such a name by its literal spelling and
        # the disagreement itself has to be on the record (r28, review P0).
        problems.append('conflicting import bindings for a module name (this '
                        f'scanner cannot tell which module a use sees): {name}')

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
        if isinstance(func, ast.Attribute):
            if not (func.attr == 'join'
                    and _string_value(func.value, constants) is None):
                return False
        elif not (isinstance(func, ast.Name)
                  and _qualified_spelling(func, aliases) in _STDLIB_JOIN_CALLS):
            return False
        elements = _joined_elements(node)
        if len(elements) < 2:
            return False
        if _qualified_spelling(func, aliases) in _STDLIB_JOIN_CALLS:
            for element in elements:
                value = _string_value(element, constants)
                if value is not None and value.startswith('/'):
                    return False
        base = elements[0]
        if not (relative_directory(base) is not None
                or bare_literal_directory(base)
                or _is_repo_rooted(base, constants, shadowed=shadowed)
                or _indexes_a_path_container(base, constants)
                or _single_component_directory(base, constants)):
            return False
        return any(join_piece_is_opaque(element) for element in elements[1:])

    def relative_directory(node):
        # A directory written entirely with relative literals: the host runs
        # tables from the repository root, so ``os.path.join('sms', name)``
        # starts at a real repository directory even though no ``ROOT``
        # appears in the expression.
        segments = _segments(node, constants, self_path)
        if segments is None:
            segments = _relative_path_segments(node, constants, aliases)
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
            segments = _relative_path_segments(node, constants, aliases)
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
            elif (repo_rooted(node) or opaque_join_segment(node)
                  or (_slices_a_string_constant(
                          _leading_path_piece(node, aliases), constants)
                      and not _join_resets_to_absolute(node, constants,
                                                       aliases))):
                # A head built from a *substring* of a spelled constant —
                # ``Path(WORD[:3]) / …`` with ``WORD = 'src/pilotdeck-
                # gateway/src'`` — leaves the discovery key nowhere in the
                # file text either, so it reports exactly like the other
                # opaque heads (r31, review P0). Unless a filesystem join's
                # absolute argument has already discarded that head —
                # ``os.path.join(WORD[:3], '/tmp', 'mod.rs')`` reads
                # ``/tmp/mod.rs``, outside the repository — which keeps the
                # absolute-path silence the single-component fallback below
                # already honours (r32, review P1).
                report_unresolvable(expression)
            elif (_single_component_directory(
                      _leading_path_piece(node, aliases), constants)
                  and not _join_resets_to_absolute(node, constants, aliases)):
                # ``Path(D[name]) / 'a' / 'lib.rs'`` with ``D = {'one': 'src'}``:
                # no segment resolves, no argument is opaque on its own, yet
                # the chain starts at a directory the container may name — the
                # same unresolvable build the literal subscript keys (r28,
                # review P0). The head decides, not the tail it joins to —
                # unless a filesystem join's absolute argument has already
                # discarded that head (``os.path.join(D[name], '/tmp',
                # 'mod.rs')`` reads ``/tmp/mod.rs``, outside the repository),
                # which keeps the absolute-path silence instead of a report
                # the reset semantics contradict (r29, review P1).
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
        if not _bare_tail(node, constants, aliases=aliases):
            # The directory is *usually* spelled inline when the tail is not a
            # bare component, so the key is findable in the file text — unless
            # the key itself carries whitespace, in which case the host can
            # never schedule it (see ``record``). It is not findable at all
            # when the tail's own separators normalise away: ``SMS /
            # '../x.rs'`` reads ``channel/x.rs`` and ``'./mod.rs'`` /
            # ``'mod.rs/'`` / ``'a/../mod.rs'`` read ``sms/mod.rs``, none of
            # which the text spells, so the host's ``git grep -F`` never
            # hits and the row used to be neither keyed nor reported — the
            # table silently lost its schedule (r37, review P0; the
            # ``_collapsed_segments`` docstring's own leading-``..`` example
            # was exactly this shape). Record whenever the normalised key is
            # absent from the file text, which also covers an inline spelling
            # whose ``..`` folds across the key's own two components; an
            # inline spelling that still contains the key keeps today's
            # silence, and so does a key the file spells anywhere else
            # (another row's anchor, a comment — the host greps the whole
            # file either way).
            if _key_contains_whitespace(node, constants, self_path, aliases):
                report_whitespace_key(expression)
                return False
            if len(segments) >= 2 and '/'.join(segments[-2:]) not in text:
                return record(segments, expression)
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
                or bare_literal_directory(node.left)
                or _single_component_directory(node.left, constants)):
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
            # spelling of ``SMS / name`` was caught. The spelling goes through
            # the import map so ``o.path.join(SMS, name)`` is the same call
            # (r25, review P2a).
            elements = _joined_elements(node)
            if len(elements) < 2:
                return
            if _qualified_spelling(func, aliases) in _STDLIB_JOIN_CALLS:
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
                or _single_component_directory(base, constants)
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
                            or _indexes_a_path_container(target, constants)
                            or _single_component_directory(target, constants)
                            or _slices_a_string_constant(target, constants)):
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
                        # ``f'{WORD[:3] + "/"}{name}'``: the separator rides a
                        # literal operand of the *head* concatenation itself,
                        # and the opaque name that follows is the file name —
                        # the same build with the SEP and the name swapped
                        # (r32, review P0). The head has already qualified as
                        # reading a substring of a spelled constant, so a
                        # display counter never reaches this reading.
                        if (isinstance(target, ast.BinOp)
                                and any(isinstance(operand, ast.Constant)
                                        and isinstance(operand.value, str)
                                        and '/' in operand.value
                                        for operand in _add_operands(target))):
                            joined = True
                            break
            if joined and (repo_rooted(node)
                           or _indexes_a_path_container(node, constants)
                           or _single_component_directory(node, constants)
                           or _slices_a_string_constant(node, constants)):
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
    (and the first version of this guard) would see the first. Every binding
    form counts — ``Assign``, an ``AnnAssign`` with a value and an ``AugAssign``
    — matching the register check that already collected the annotated
    spelling; a bare annotation (``DISCOVERY_ANCHORS: tuple``) binds nothing
    at runtime and is not a declaration.
    """
    found = []
    for statement in tree.body:
        if isinstance(statement, ast.Assign):
            if not any(isinstance(target, ast.Name)
                       and target.id == 'DISCOVERY_ANCHORS'
                       for target in statement.targets):
                continue
        elif isinstance(statement, ast.AnnAssign):
            if not (isinstance(statement.target, ast.Name)
                    and statement.target.id == 'DISCOVERY_ANCHORS'
                    and statement.value is not None):
                continue
        elif isinstance(statement, ast.AugAssign):
            if not (isinstance(statement.target, ast.Name)
                    and statement.target.id == 'DISCOVERY_ANCHORS'):
                continue
        else:
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
    if not fnmatch.fnmatch(table, '*/*negative*.py'):
        # The key grep's pathspec ``"$FX/**/*negative*.py"`` is a git *default*
        # pathspec: its ``*`` crosses ``/``, but the pattern's own ``/`` after
        # ``**`` still demands the match's "negative" appear after at least one
        # separator — measured with ``git ls-files``, ``x-negative/run.py``
        # and ``negative-output-directory/process_probe.py`` are not matched
        # while ``a/x-negative/run.py`` is, and ``fnmatch(rel,
        # '*/*negative*.py')`` is the exact equivalent (fnmatch's ``*``
        # crosses ``/`` too; both readings measured equal on the real tree,
        # 274 files either way). A table whose every "negative" sits before
        # its first ``/`` is therefore emitted only when the table file itself
        # changes (``neg-for-diff.sh:27``), never by a key grep — the same
        # family as a top-level table, and accepting anchors for it would
        # turn a permanently blind table into a green tick.
        problems.append('table the host key grep cannot reach: every '
                        '"negative" in the path sits before its first "/", so '
                        'the pathspec "$FX/**/*negative*.py" never matches it '
                        'and anchors cannot schedule it (only a change to the '
                        f'table file itself emits it): {table}')
        return problems
    required, scan_problems = bare_concat_keys(tree, table, text=text)
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
        # The whole relative path is matched, not only the basename: the host
        # emits any ``*negative*.py`` when the table file itself changes
        # (``neg-for-diff.sh:27``, a shell ``case`` whose ``*`` crosses ``/``),
        # and the sweep net mirrors that emission net so every file the host
        # can emit is registered — fnmatch has no FNM_PATHNAME, the same
        # cross-separator reading. The :29 *key* grep is narrower than this
        # net, and the marker below records the difference.
        if not fnmatch.fnmatch(rel, '*negative*.py') or rel in TABLES:
            continue
        if path.name in HOST_FILTERED or path.name.startswith('test_'):
            # The two exclusions the host applies to its own output list, no
            # more: a file skipped here is never emitted as a table there
            # either, so the sweep never has to measure something the host
            # cannot run.
            continue
        text = path.read_text(encoding='utf-8')
        tree = ast.parse(text)
        keys, problems = bare_concat_keys(tree, rel, root=root, text=text)
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
        elif not fnmatch.fnmatch(rel, '*/*negative*.py'):
            # The mirror of the ``violations`` refusal above, as a register
            # row: a table below the root whose every "negative" sits before
            # its first ``/`` (``negative-output-directory/process_probe.py``)
            # is in the :27 emission net but never in the :29 key grep, so
            # no anchor can schedule it — permanently blind, one directory
            # deeper than the top-level family (r28, review P0).
            hard.append('table the host key grep cannot reach: every '
                        '"negative" in the path sits before its first "/", so '
                        'the pathspec "$FX/**/*negative*.py" never matches it '
                        'and anchors cannot schedule it (only a change to the '
                        'table file itself emits it)')
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


def _duplicate_test_names(source):
    """``{class name: [duplicated ``def test_`` names]}`` for one module.

    A class body that defines the same test name twice keeps only the last
    definition, so the earlier one is dead code no run ever executes — that is
    how a control silently stops existing. The whole file is parsed here so the
    check below can re-read this very module. A definition inside a compound
    statement of the class body (``if``/``try``/``with``/``for``/``match``)
    still lands in the class namespace when the body runs, so those count in
    source order too — a ``def`` under ``if True:`` overwrites the one above it
    exactly as a top-level ``def`` does (r28, review P2). Arms that cannot
    both run do **not** count twice: an ``if``/``elif``/``else`` statement and
    a ``match`` each execute at most one arm, so the statement contributes one
    definition of a name however many arms spell it, while two definitions in
    the *same* arm — or in arms of different statements — still collide (r29,
    review P2: the r28 drill-down read both arms of one ``if`` as duplicates).
    """
    duplicates = {}
    for node in ast.parse(source).body:
        if not isinstance(node, ast.ClassDef):
            continue

        def definition_runs(body):
            """``[(name, runs)]`` — how often one pass can define each name.

            An ``if``/``match`` statement contributes the *maximum* of its
            arms (exactly one of them runs, the no-arm case contributing
            zero); a ``try`` mixes the two — the body and ``finally`` always
            run, the handler block and ``else`` never both do — and the other
            compounds (a loop body and its ``else``, a ``with`` body) can run
            in one pass, so those merge additively.
            """
            runs = {}

            def bump(name, count):
                if count <= 0:
                    return
                runs[name] = runs.get(name, 0) + count

            for child in body:
                if (isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef))
                        and child.name.startswith('test_')):
                    bump(child.name, 1)
                    continue
                arms = None
                if isinstance(child, ast.If):
                    arms = [child.body]
                    if child.orelse:
                        arms.append(child.orelse)
                elif isinstance(child, getattr(ast, 'Match', ())):
                    arms = [case.body for case in child.cases]
                if arms is not None:
                    # ``elif`` chains need no flattening of their own: the
                    # nested ``if`` sits inside the ``else`` arm, so one pass
                    # still executes at most one arm of the outer statement.
                    merged = [definition_runs(arm) for arm in arms]
                    spelled = []
                    for part in merged:
                        for name in part:
                            if name not in spelled:
                                spelled.append(name)
                    for name in spelled:
                        bump(name, max(part.get(name, 0) for part in merged))
                    continue
                groups = None
                if isinstance(child, (ast.Try, getattr(ast, 'TryStar', ()))):
                    # A ``try`` is not a flat additive region: the body runs
                    # first, and *if* an exception fires, exactly one handler
                    # runs for it. For a plain ``try`` the handlers are
                    # mutually exclusive — one exception unwinds to one
                    # ``except`` — and ``else`` runs only when no exception
                    # fired, so the handler block and the ``else`` block
                    # never both run: each name contributes
                    # ``max(handlers..., else)``. A ``try*`` (exception
                    # groups) can fan one pass out through *several*
                    # handlers as the group travels, so there the handler
                    # contributions add. ``finally`` always runs — additive
                    # either way. Two definitions in the same handler, or
                    # one in the body and one in a handler, still collide:
                    # the exception fires *after* the body line, so one pass
                    # can land both in the class namespace (r30, review P1:
                    # summing the plain-``try`` handlers reported two
                    # ``except`` clauses that can never both run).
                    handler_runs = [definition_runs(handler.body)
                                    for handler in child.handlers]
                    if isinstance(child, getattr(ast, 'TryStar', ())):
                        handler_part = {}
                        for part in handler_runs:
                            for name, count in part.items():
                                handler_part[name] = (handler_part.get(name, 0)
                                                       + count)
                    else:
                        handler_part = {}
                        for part in handler_runs:
                            for name, count in part.items():
                                handler_part[name] = max(handler_part.get(name, 0),
                                                         count)
                    else_runs = definition_runs(child.orelse)
                    for name in set(handler_part) | set(else_runs):
                        bump(name, max(handler_part.get(name, 0),
                                       else_runs.get(name, 0)))
                    groups = [child.body, child.finalbody]
                elif isinstance(child, (ast.With, getattr(ast, 'AsyncWith', ()))):
                    # ``with`` has no ``else`` clause — neither spelling of it
                    # carries an ``orelse`` field, so only the body runs (the
                    # grouping below reads ``.orelse`` and would raise on a
                    # ``with``; caught while writing this round's tests).
                    groups = [child.body]
                elif isinstance(child, (ast.For, getattr(ast, 'AsyncFor', ()),
                                        ast.While)):
                    # The loop body and its ``else`` both run in one pass, so
                    # they are one additive region, not two.
                    groups = [child.body + child.orelse]
                if groups is not None:
                    # ``groups`` is a list of bodies, each a flat statement
                    # list; recursing on each *statement* separately read a
                    # ``FunctionDef`` as a body and raised, and merging the
                    # runs statement-by-statement would still have lost the
                    # additive counts across the regions.
                    for group in groups:
                        for name, count in definition_runs(group).items():
                            bump(name, count)
            return runs

        repeated = [name for name, count in definition_runs(node.body).items()
                    if count > 1]
        if repeated:
            duplicates[node.name] = repeated
    return duplicates


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
        for path in sorted(FIXTURES.rglob('*.py')):
            rel = path.relative_to(FIXTURES).as_posix()
            # The same whole-path pattern ``sweep`` uses: the host pathspec's
            # ``*`` crosses ``/``, so a table in ``x-negative/`` declaring
            # anchors is scheduled by the host and must be registered here
            # even when its own basename carries no marker.
            if not fnmatch.fnmatch(rel, '*negative*.py'):
                continue
            if path.name in HOST_FILTERED or path.name.startswith('test_'):
                continue
            tree = ast.parse(path.read_text(encoding='utf-8'))
            for statement in tree.body:
                if isinstance(statement, ast.Assign):
                    targets = statement.targets
                elif isinstance(statement, ast.AnnAssign) and statement.value is not None:
                    # A bare annotation (``DISCOVERY_ANCHORS: tuple``) binds
                    # nothing, the same reading ``declared_anchors`` takes.
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

    def test_a_directory_named_table_is_swept_like_a_basename_named_one(self):
        """``x-negative/run.py`` is scheduled by the host and must be scanned.

        The host pathspec ``"$FX/**/*negative*.py"`` uses git's default
        pathspec, whose ``*`` crosses ``/``; a table whose *directory* carries
        the marker reaches the discovery loop even when its basename does not.
        Filtering on the basename never measured such a file, so a new blind
        table there would not have raised (r24, review P2). The unresolvable
        half of the scan follows the same filter.
        """
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            table = root / 'x-negative' / 'run.py'
            table.parent.mkdir()
            table.write_text(
                "from pathlib import Path\n"
                "ROOT = Path(__file__).resolve().parents[5]\n"
                "PICK = {'dir': 'src/pilotdeck-gateway/src/channel/sms'}\n"
                "def row(key):\n"
                "    return ROOT / PICK[key] / 'mod.rs'\n",
                encoding='utf-8')
            blind, unresolvable = sweep(root)
        self.assertEqual(
            sorted(unresolvable), ['x-negative/run.py'],
            '#24-P2 the sweep filtered on the basename and missed a '
            'directory-named table')
        self.assertEqual(blind, {})

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

    def test_every_test_method_name_is_unique_within_its_class(self):
        """A duplicated ``def test_`` name silently shadows the earlier one.

        Python keeps the last definition in a class body, so the earlier test
        becomes dead code and the control it was supposed to carry never runs —
        the direct subcontainer-alias write spent rounds in exactly that state
        (r25, review P1). Re-parsing this very module catches the next one
        before it can hide.
        """
        source = (FIXTURES / 'test_negative_discovery_anchors.py').read_text(encoding='utf-8')
        duplicates = _duplicate_test_names(source)
        self.assertTrue(duplicates == {},
                        '#25-P1 a duplicated test method name shadows the '
                        'earlier definition; rename one of them: '
                        + repr(duplicates))
        # The check must itself be falsifiable: a constructed duplicate has to
        # be flagged, or the green above proves nothing.
        synthetic = (
            'class T(unittest.TestCase):\n'
            '    def test_a(self):\n        pass\n'
            '    def test_b(self):\n        pass\n'
            '    def test_a(self):\n        pass\n'
        )
        flagged = _duplicate_test_names(synthetic)
        self.assertTrue(flagged == {'T': ['test_a']},
                        '#25-P1 the duplicate-name scan did not flag the '
                        'constructed duplicate: ' + repr(flagged))
        # A definition inside a compound statement of the class body lands in
        # the namespace all the same, so a conditional ``def`` shadows the
        # one above it exactly as a top-level ``def`` does — the scan used to
        # stop at direct children and miss it (r28, review P2).
        conditional = (
            'class T(unittest.TestCase):\n'
            '    def test_a(self):\n        pass\n'
            '    if True:\n'
            '        def test_a(self):\n            pass\n'
        )
        flagged = _duplicate_test_names(conditional)
        self.assertTrue(flagged == {'T': ['test_a']},
                        '#28-P2 the duplicate-name scan did not flag the '
                        'conditional class-body duplicate: ' + repr(flagged))

    def test_the_readme_followup_numbers_match_the_scan(self):
        """The README's follow-up paragraph is a numeric copy of the register.

        The summary line above ``KNOWN_BLIND`` is already re-measured, but the
        README's 跟进清单 paragraph quotes the same numbers in prose and no
        test read it — a regeneration that rewrote the register but not the
        README would leave the copy stale (r25, review P2a). The parse is
        pinned to this phrasing on purpose: rewording the paragraph means
        rewriting this test, and that binding is the point.
        """
        blind, unresolvable = sweep()
        measured = set(blind) | set(unresolvable)
        total_keys = sum(len(keys) for keys in blind.values())
        import_keys = sum(1 for keys in blind.values() for rows in keys.values()
                          if rows == [IMPORTED_HELPER_REASON])
        expected = {
            'registered files': (len(measured),),
            'two-set split': (
                sum(1 for rel in measured
                    if blind.get(rel) and not unresolvable.get(rel)),
                sum(1 for rel in measured
                    if unresolvable.get(rel) and not blind.get(rel)),
                sum(1 for rel in measured
                    if blind.get(rel) and unresolvable.get(rel))),
            'gap totals': (len(blind), len(unresolvable)),
            'overlap with the first set': (
                sum(1 for rel in measured
                    if blind.get(rel) and unresolvable.get(rel)),),
            'keys total and import': (total_keys, import_keys),
            'bare keys': (total_keys - import_keys,),
            'unresolvable files and places': (
                len(unresolvable),
                sum(len(rows) for rows in unresolvable.values())),
            # fix-r34 review P2a: the "共 N 条键" tally of the registered
            # tables was the one numeric copy in this paragraph no test
            # re-measured; anchors can be added or dropped without it. The
            # same paragraph also counts the registered tables themselves and
            # one named-but-unregistered table's key split — same drift class,
            # bound the same way (fix-r34 sibling sweep).
            'registered table keys': (
                sum(len(declared_anchors(ast.parse(
                        (FIXTURES / name).read_text(encoding='utf-8')))[0])
                    for name in TABLES),),
            'registered table count': (len(TABLES),),
            'head-timeout split': (
                len(blind['channel-sms-head-timeout/negative_controls.py']),
                sum(1 for rows in blind['channel-sms-head-timeout/'
                                        'negative_controls.py'].values()
                    if rows != [IMPORTED_HELPER_REASON]),
                sum(1 for rows in blind['channel-sms-head-timeout/'
                                        'negative_controls.py'].values()
                    if rows == [IMPORTED_HELPER_REASON])),
            # fix-r35（DS sibling P2）：同段的「其中 N 处是宿主键 grep 够不到
            # 的表本身——N 张顶层 + N 张 negative 前置目录」是最后一批手抄数字，
            # 与 sweep 的两条不可解标记（top-level / host key grep cannot reach）
            # 逐项可复算——按标记的判据（'/' 不在 rel / fnmatch 不中）计数绑定，
            # 与本测试其余数字同漂移类、同绑法。
            'host-grep-unreachable tables': (
                sum(1 for rel in unresolvable if '/' not in rel)
                + sum(1 for rel in unresolvable
                      if '/' in rel
                      and not fnmatch.fnmatch(rel, '*/*negative*.py')),
                sum(1 for rel in unresolvable if '/' not in rel),
                sum(1 for rel in unresolvable
                    if '/' in rel
                    and not fnmatch.fnmatch(rel, '*/*negative*.py'))),
        }
        patterns = {
            'registered files': r'KNOWN_BLIND` 登记 \*\*(\d+) 个文件\*\*',
            'two-set split': (r'\*\*(\d+) 个仅缺键 \+ (\d+) 个仅含不可解形状'
                              r' \+ (\d+) 个两者都有\*\*'),
            'gap totals': r'合计 (\d+) 个文件存在字面缺失、(\d+) 个文件含不可解形状',
            'overlap with the first set': r'——后者有 (\d+) 个与前者重叠',
            'keys total and import': (r'字面缺失合计 \*\*(\d+) 条键\*\*'
                                      r'（其中 \*\*(\d+) 条是共享助手 import 键\*\*'),
            'bare keys': r'余下 (\d+) 条是裸文件名拼接',
            'unresolvable files and places': (r'\*\*(\d+) 个文件\*\*含 \*\*(\d+) 处\*\*'
                                              r'解析器跟不下去的路径形状'),
            'registered table keys': r'，共 (\d+) 条键）',
            'registered table count': r'只登记 #2133 点名的 (\d+) 张表',
            'head-timeout split': (r'量到 (\d+) 条键（(\d+) 条裸文件名、'
                                   r'(\d+) 条 import）'),
            'host-grep-unreachable tables': (
                r'（其中 (\d+) 处是宿主键 grep 够不到的表本身'
                r'——(\d+) 张顶层 \+ (\d+) 张 negative 前置目录'),
        }
        text = (FIXTURES / 'README.md').read_text(encoding='utf-8')
        for label, pattern in patterns.items():
            with self.subTest(number=label):
                found = [tuple(int(digit) for digit in match.groups())
                         for match in re.finditer(pattern, text)]
                self.assertTrue(found == [expected[label]],
                                '#25-P2a the README follow-up paragraph does '
                                f'not carry the measured {label} (expected '
                                f'{expected[label]}, found {found}); rewording '
                                'the paragraph means rewriting this parse')

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

    def test_an_import_aliased_join_with_a_runtime_file_name_fails_closed(self):
        """``o.path.join(SMS, name)`` is the same hole as ``os.path.join``.

        The alias must reach the *reporting* nets too, not only the keying one:
        ``o.path.join(SMS, name)`` and ``o.sep.join((str(SMS), name))`` were
        neither keyed nor reported, while the plain spellings have reported
        since r15 (r25, review P2a).
        """
        for spelling in ("o.path.join(SMS, name)",
                         "o.sep.join((str(SMS), name))"):
            synthetic = (
                "import os as o\n"
                "ROOT = Path(__file__).resolve().parents[5]\n"
                "SMS = ROOT / 'src/example/x'\n"
                "DISCOVERY_ANCHORS = ()\n"
                "def row(name):\n"
                f"    return {spelling}\n"
            )
            problems = violations(synthetic, 'example/negative_controls.py')
            self.assertTrue(any('unresolvable repository path' in p for p in problems),
                            '#25-P2a the aliased join hid the runtime file '
                            f'name: {spelling}: {problems}')

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

        Only a *string* component can name a directory. A lookup table of
        numbers is not a directory, and joining on it must not report. (A table
        of one-word strings did stay silent once, on the theory that mode
        labels are not directories — but a single component is exactly how a
        top-level repository directory is spelled, so r24 made that direction
        fail closed; see ``_single_component_directory``.)
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "MODES = {'fast': 1, 'safe': 2}\n"
            "def row(name):\n"
            "    return Path(MODES['fast']) / name\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

    def test_a_single_component_head_with_a_runtime_tail_fails_closed(self):
        """``Path(MODES['fast']) / name`` starts at a real, unknowable directory.

        A single component is exactly how a top-level repository directory is
        spelled, and the file name is runtime, so the ``fast/<file>`` key can
        be neither derived nor trusted — the join must be reported like
        ``SMS / name`` is, not dropped out of the parsing family (r24, review
        P1-2). Every path-building spelling follows.
        """
        shapes = {
            'division': (
                "MODES = {'fast': 'fast'}\n"
                "def row(name):\n"
                "    return Path(MODES['fast']) / name\n"
            ),
            'bare subscript': (
                "MODES = {'fast': 'fast'}\n"
                "def row(name):\n"
                "    return MODES['fast'] / name\n"
            ),
            'joinpath': (
                "MODES = {'fast': 'fast'}\n"
                "def row(name):\n"
                "    return Path(MODES['fast']).joinpath(name)\n"
            ),
            'os.path.join': (
                "import os\n"
                "MODES = {'fast': 'fast'}\n"
                "def row(name):\n"
                "    return os.path.join(MODES['fast'], name)\n"
            ),
            'f-string': (
                "MODES = {'fast': 'fast'}\n"
                "def row(name):\n"
                "    return f\"{MODES['fast']}/{name}\"\n"
            ),
            'Path call': (
                "MODES = {'fast': 'fast'}\n"
                "def row(name):\n"
                "    return Path(MODES['fast'], name)\n"
            ),
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = ("from pathlib import Path\n" + body)
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem
                        or 'unsupported f-string path' in problem
                        for problem in problems),
                    '#24-P1-2 the single-component directory head with a '
                    'runtime tail was dropped silently: ' + repr(problems))

    def test_a_single_component_head_with_a_literal_tail_still_keys(self):
        """The consistency half: the literal tail is keyed from the head.

        ``Path(MODES['fast']) / 'mod.rs'`` builds ``fast/mod.rs``, and if that
        key is derived it must also be schedulable — the fail-closed rule and
        the keying rule have to agree on what the head means.
        """
        synthetic = (
            "from pathlib import Path\n"
            "MODES = {'fast': 'fast'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(MODES['fast']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('missing anchor: fast/mod.rs' in problem for problem in problems),
            '#24-P1-2 the single-component directory head no longer keys the '
            'literal tail: ' + repr(problems))

    def test_a_dynamic_container_index_with_a_bare_component_fails_closed(self):
        """``Path(D[name]) / '…/lib.rs'`` may well start at the spelled directory.

        The literal subscript ``D['one']`` keys the join; hiding the same value
        behind a runtime index — or behind a container a write later touches —
        read as "not a directory lookup" and the join dropped out of every
        net, the false-pass shape of the literal spelling with the value only
        hidden, not absent (r28, review P0).
        """
        shapes = {
            'division': (
                "from pathlib import Path\n"
                "D = {'one': 'src'}\n"
                "def row(name):\n"
                "    return Path(D[name]) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
            ),
            'os.path.join': (
                "import os\n"
                "from pathlib import Path\n"
                "D = {'one': 'src'}\n"
                "def row(name):\n"
                "    return os.path.join(D[name], 'pilotdeck-gateway', 'src', 'lib.rs')\n"
            ),
            'mutated container': (
                "from pathlib import Path\n"
                "D = {'one': 'src'}\n"
                "D['two'] = 'sms'\n"
                "def row(name):\n"
                "    return Path(D[name]) / 'mod.rs'\n"
            ),
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                problems = violations(body, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem
                        for problem in problems),
                    '#28-P0 the dynamic container index with a bare component '
                    'was dropped silently: ' + repr(problems))

    def test_a_literal_container_index_with_a_bare_component_still_keys(self):
        """The reverse control: ``D['one']`` keeps keying the join it spells."""
        synthetic = (
            "from pathlib import Path\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(D['one']) / 'pilotdeck-gateway' / 'src' / 'lib.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('missing anchor: src/lib.rs' in problem for problem in problems),
            '#28-P0 the literal container index no longer keys its join: '
            + repr(problems))

    def test_a_dynamic_container_index_of_non_path_values_stays_silent(self):
        """A container of numbers is not a directory lookup, dynamic or not."""
        synthetic = (
            "from pathlib import Path\n"
            "D = {'one': 3}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return Path(D[name]) / 'mod.rs'\n"
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

    def test_a_named_constant_element_in_an_f_string_subscript_is_reported(self):
        """``D = {'one': S}`` with ``S = 'src'`` is the written-out spelling.

        The f-string net does not pass through the literal-index spellings, so
        the container lookup has to resolve the named element itself:
        ``f'{D["one"]}/{name}'`` must report exactly as ``f'{"src"}/{name}'``
        does, or hiding the component behind a name turns the report off (r30,
        review P1 — a name resolving to a plain string is an *element* of the
        container here, never the container itself).
        """
        literal = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "D = {'one': 'src'}\n"
            "def row(name):\n"
            "    return f'{D[\"one\"]}/{name}'\n"
        )
        problems = violations(literal, 'example/negative_controls.py')
        self.assertTrue(
            any('unsupported f-string path' in problem for problem in problems),
            problems)
        named = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "S = 'src'\n"
            "D = {'one': S}\n"
            "def row(name):\n"
            "    return f'{D[\"one\"]}/{name}'\n"
        )
        problems = violations(named, 'example/negative_controls.py')
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

    def test_a_runtime_parents_index_fails_closed(self):
        """``ROOT = …parents[int('4')]`` cannot be verified to name the root.

        The depth check only refused *wrong* literal depths; a runtime index
        was read as "not statically visible" and the root assumption stood, so
        every join after it was keyed from a directory the runtime may not
        have used (r24, review P1-2).
        """
        shapes = {
            'int() call': "ROOT = Path(__file__).resolve().parents[int('4')]\n",
            'name index': "IDX = 4\nROOT = Path(__file__).resolve().parents[IDX]\n",
            'through an alias': (
                "BASE = Path(__file__).resolve().parents[int('4')]\n"
                "ROOT = BASE\n"
            ),
            'conditional': (
                "FLAG = True\n"
                "ROOT = Path(__file__).resolve().parents[4 if FLAG else 5]\n"
            ),
        }
        for label, rootline in shapes.items():
            with self.subTest(shape=label):
                synthetic = ("from pathlib import Path\n" + rootline
                             + "DISCOVERY_ANCHORS = ()\n"
                             + "rows = [ROOT / 'src/example/named.rs']\n")
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('reaches .parents through a runtime index' in problem
                        for problem in problems),
                    '#24-P1-2 the runtime .parents index kept the root '
                    'assumption standing: ' + repr(problems))

    def test_a_literal_parents_index_still_reports_only_the_depth(self):
        """The reverse controls: literal and negative-literal indexes stay keyed.

        ``parents[4]`` is the checked wrong depth and ``parents[-1]`` is the
        absolute root; neither is a runtime index, so both keep their
        existing verdicts instead of the new unresolvable one.
        """
        shapes = {
            'negative literal': (
                "from pathlib import Path\n"
                "ROOT = Path(__file__).resolve().parents[-1]\n"
                "DISCOVERY_ANCHORS = ()\n"
                "rows = [ROOT / 'Cargo.toml']\n"
            ),
            'correct literal': (
                "from pathlib import Path\n"
                "ROOT = Path(__file__).resolve().parents[5]\n"
                "DISCOVERY_ANCHORS = ()\n"
                "rows = [ROOT / 'Cargo.toml']\n"
            ),
        }
        for label, synthetic in shapes.items():
            with self.subTest(shape=label):
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    all('runtime index' not in problem for problem in problems),
                    '#24-P1-2 a literal .parents index was misread as '
                    'runtime: ' + repr(problems))

    def test_a_runtime_parents_index_below_a_parent_attribute_fails_closed(self):
        """``parents[int(0)].parent`` carries the runtime index up with it.

        The r24 net stopped one attribute short of the subscript, so an
        unverifiable ``ROOT`` read as the repository root again — the same
        family one ``.parent`` lower (r28, review P0).
        """
        shapes = {
            'int() call': (
                "ROOT = Path(__file__).resolve().parents[int('0')].parent\n"),
            'name index': (
                "IDX = 0\n"
                "ROOT = Path(__file__).resolve().parents[IDX].parent\n"),
        }
        for label, rootline in shapes.items():
            with self.subTest(shape=label):
                synthetic = ("from pathlib import Path\n" + rootline
                             + "DISCOVERY_ANCHORS = ()\n"
                             + "rows = [ROOT / 'negative_scope.py']\n")
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('reaches .parents through a runtime index' in problem
                        for problem in problems),
                    '#28-P0 the runtime index below .parent kept the root '
                    'assumption standing: ' + repr(problems))

    def test_a_literal_parents_index_below_a_parent_attribute_keeps_its_verdict(self):
        """The reverse control: ``parents[5].parent`` is statically readable.

        Only the depth answer changes (it names a different directory), and
        the depth check keeps delivering it — nothing may read this shape as
        a runtime index.
        """
        synthetic = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5].parent\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'negative_scope.py']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            all('runtime index' not in problem for problem in problems),
            '#28-P0 a literal index below .parent was misread as runtime: '
            + repr(problems))

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

    def test_an_import_aliased_filesystem_join_is_keyed_like_the_plain_spelling(self):
        """``import os as o`` reaches the same joins as ``import os``.

        ``o.path.join('…/sms', 'mod.rs')`` and ``o.sep.join((...))`` built the
        same ``sms/mod.rs`` as the plain spellings, yet the join resolvers read
        only the dotted spelling, so an aliased join was neither keyed nor
        reported — the table looked self-contained while the discovery loop
        could never schedule it (r25, review P2a). The bare-name alias
        ``from os.path import join as j`` and ``from os import sep`` are the
        same hole with the import spelled differently.
        """
        shapes = {
            'o.path.join': "import os as o\nrows = [o.path.join('sms', 'mod.rs')]",
            'o.sep.join': "import os as o\nrows = [o.sep.join(('sms', 'mod.rs'))]",
            'join as j': "from os.path import join as j\nrows = [j('sms', 'mod.rs')]",
            'sep by name': "from os import sep\nrows = [sep.join(('sms', 'mod.rs'))]",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + body + "\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('missing anchor: sms/mod.rs' in problem for problem in problems),
                    '#25-P2a the aliased join was not keyed: ' + repr(problems))

    def test_a_scope_shadowed_import_does_not_hide_the_plain_join(self):
        """A function-local ``import random as os`` must not overwrite ``os``.

        ``_module_aliases`` merged every same-name import in the file into one
        last-wins map, so the function-local binding overwrote the module-level
        one and a genuine ``os.path.join`` read as ``random.path.join`` — the
        join dropped out of every net while the table stayed green (r28,
        review P0; a regression r25 introduced by reading aliases across
        scopes). The conflicted name now falls back to its literal spelling
        and the disagreement itself is reported.
        """
        synthetic = (
            "import os\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.path.join('src', 'pilotdeck-gateway', 'src', 'lib.rs')]\n"
            "def unrelated():\n"
            "    import random as os\n"
            "    return os.random()\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('missing anchor: src/lib.rs' in problem for problem in problems),
            '#28-P0 the scope-shadowed import hid the stdlib join: '
            + repr(problems))
        self.assertTrue(
            any('conflicting import bindings' in problem for problem in problems),
            '#28-P0 the conflicting bindings themselves went unreported: '
            + repr(problems))

    def test_imports_bound_to_the_same_module_in_many_scopes_are_not_a_conflict(self):
        """The reverse control: one target everywhere is still one alias.

        ``import os.path`` twice — at the top level and inside a function —
        binds the same name to the same package at every use site, so both
        joins key and nothing is reported as a conflict.
        """
        synthetic = (
            "import os.path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.path.join('sms', 'mod.rs')]\n"
            "def unrelated():\n"
            "    import os.path\n"
            "    return os.path.join('sms', 'x.rs')\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('missing anchor: sms/mod.rs' in problem for problem in problems),
            '#28-P0 the same-target import in a second scope stopped keying: '
            + repr(problems))
        self.assertTrue(
            all('conflicting import bindings' not in problem
                for problem in problems),
            '#28-P0 a same-target import was read as a conflict: '
            + repr(problems))

    def test_a_non_path_call_through_a_module_alias_stays_silent(self):
        """The reverse control: ``o.getenv(...)`` is not a path build.

        Resolving the alias must widen only the join family, not turn every
        ``o.<attr>`` call into a suspect: a module alias used for something
        other than a path build stays silent.
        """
        synthetic = (
            "import os as o\n"
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [o.getenv('HOME')]\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

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

    def test_a_write_through_a_direct_subcontainer_alias_invalidates_the_container(self):
        """``ALIAS = CH['a']`` names the *same* inner dictionary.

        ``ALIAS['b'] = ...`` therefore rewrites what ``CH['a']['b']`` reads.
        Pairing the names is what closes the hole: without it the stale
        literal was keyed while the runtime used the new one. (Named
        ``..._nested_alias_...`` until r25, when that name turned out to be
        shadowed by the two-hop-chain test below — the definition here was
        dead code no run ever executed.)
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
        name nothing the discovery loop can key. The module alias resets the
        same way (r25): before the alias-aware spelling, ``o.path.join('sms',
        '/tmp', 'mod.rs')`` was *reported* while the plain spelling stayed
        silent — over-reporting, but two spellings of one call disagreeing is
        a drift of its own.
        """
        shapes = {
            'plain': "import os\nrows = [os.path.join('sms', '/tmp', 'mod.rs')]",
            'module alias': "import os as o\nrows = [o.path.join('sms', '/tmp', 'mod.rs')]",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + body + "\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(problems == [],
                                '#25-P2a the aliased absolute reset disagreed '
                                'with the plain spelling: ' + repr(problems))

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

    def test_a_table_the_host_key_grep_cannot_reach_is_refused(self):
        """``x-negative/run.py`` is invisible to the key grep even anchored.

        The ``:29`` pathspec ``"$FX/**/*negative*.py"`` is a git default
        pathspec — its ``*`` crosses ``/``, but the ``/`` after ``**`` still
        demands the match's "negative" appear after at least one separator
        (measured with ``git ls-files``: this shape and
        ``negative-output-directory/process_probe.py`` return nothing,
        ``a/x-negative/run.py`` matches, and ``fnmatch(rel,
        '*/*negative*.py')`` is the exact equivalent, 274 files either way
        on the real tree). A table whose every "negative" sits before its
        first ``/`` is emitted only when the table file itself changes
        (``neg-for-diff.sh:27``), so anchors cannot schedule it — the same
        family as a top-level table, one directory deeper (r28, review P0;
        the r24 README claim that this shape is schedulable was wrong).
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
            "DISCOVERY_ANCHORS = ('sms/mod.rs',)\n"
            "rows = [SMS / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'x-negative/run.py')
        self.assertTrue(
            any('the host key grep cannot reach' in problem for problem in problems),
            '#28-P0 the unreachable-pathshape table passed with anchors: '
            + repr(problems))

    def test_a_negative_directory_deep_enough_stays_schedulable(self):
        """The reverse control: ``a/x-negative/run.py`` is greppable.

        "negative" after at least one separator puts the file inside the
        pathspec, so it is measured normally — only the first-directory
        shape is refused.
        """
        synthetic = (
            "ROOT = Path(__file__).resolve().parents[6]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'src/pilotdeck-gateway/src/channel/sms/mod.rs']\n"
        )
        self.assertEqual(
            violations(synthetic, 'a/x-negative/run.py'), [])

    def test_a_table_the_key_grep_misses_is_marked_by_the_sweep(self):
        """The sweep registers the unreachable shape like a top-level table.

        ``negative-output-directory/process_probe.py`` is in the ``:27``
        emission net but never in the ``:29`` key grep, so the sweep marks
        it unresolvable — permanently blind, registered as such (r28).
        """
        body = (
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "rows = [('a', ROOT / 'src/example/named.rs', 'x')]\n"
        )
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'x-negative').mkdir()
            (root / 'x-negative' / 'run.py').write_text(body, encoding='utf-8')
            blind, unresolvable = sweep(root)
        self.assertEqual(blind, {})
        self.assertEqual(sorted(unresolvable), ['x-negative/run.py'])
        self.assertTrue(any('the host key grep cannot reach' in problem
                            for problem in unresolvable['x-negative/run.py']))
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            (root / 'deep' / 'x-negative').mkdir(parents=True)
            (root / 'deep' / 'x-negative' / 'run2.py').write_text(
                body.replace('parents[5]', 'parents[6]'), encoding='utf-8')
            blind, unresolvable = sweep(root)
        self.assertEqual(blind, {})
        self.assertEqual(unresolvable, {})

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
        path is still a literal, so the key is known. The module alias
        ``im.import_module('x')`` is registered as a keyed shape (README
        follow-up list, fix-r34 blind class), so it needs its own positive
        control here — otherwise a later tightening of the receiver check
        could silently drop it and widen the registered blind class
        (fix-r35, DS sibling P2).
        """
        shapes = {
            'importlib': "import importlib\nhelper = importlib.import_module('_shared.scope')\n",
            'module alias': "import importlib as im\nhelper = im.import_module('_shared.scope')\n",
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

    def test_the_registered_dynamic_import_closures_stay_silent(self):
        """The registered dynamic-import closure shapes keep their pinned silence.

        Only an attribute call ``import_module`` (``importlib.import_module('x')``
        or a module alias ``im.import_module('x')``) and a name call
        ``__import__('x')``, each with a positional string literal, are keyed.
        Every other spelling — a plain name call (renamed ``load('…')``, the
        unaliased ``import_module('…')``, a function alias
        ``im = importlib.import_module`` then ``im('…')``), the module name in
        a keyword argument (``import_module(name='…')``), or a computed name
        (``__import__(RUNTIME_NAME)``) — yields no key and no report, so the
        import closure of a table can pass unscheduled: a false negative the
        fix-r34 review raised as P0. The slice/closure family stopped per the
        arbitration card (``state/decisions-20261109-2136-slice-family-
        arbitration.md``, board 4f8de196d rule 15), so the shapes are
        registered as a blind class in the README's follow-up list instead of
        being patched; this pin holds the current behaviour, and a change in
        *either* direction — keying the shapes, or reporting them as
        unresolvable — must turn this red and reopen the registration, not
        drift past it. The literal spellings above keep keying, so the
        register cannot silently widen to cover this family either.
        """
        shapes = {
            'renamed call': ("from importlib import import_module as load\n"
                             "helper = load('_shared.scope')\n"),
            'unaliased name call': ("from importlib import import_module\n"
                                    "helper = import_module('_shared.scope')\n"),
            'function alias': ("import importlib\n"
                               "im = importlib.import_module\n"
                               "helper = im('_shared.scope')\n"),
            'keyword argument': ("import importlib\n"
                                 "helper = importlib.import_module(name='_shared.scope')\n"),
            'computed name': ("helper = __import__(RUNTIME_NAME)\n"
                              "RUNTIME_NAME = '_shared.scope'\n"),
        }
        for label, line in shapes.items():
            with self.subTest(shape=label):
                with tempfile.TemporaryDirectory() as workspace:
                    root = Path(workspace)
                    (root / '_shared').mkdir()
                    (root / '_shared' / 'scope.py').write_text('', encoding='utf-8')
                    keys = _shared_module_keys(ast.parse(line), root, ())
                self.assertTrue(keys == set(),
                                '#2133-r34 the registered dynamic-import '
                                f'closure shape {label!r} is silent: {sorted(keys)}')
                # SILENT is the whole definition: no key *and* no report —
                # a future report of these shapes as unsupported is the
                # tightening the pin must catch too.
                problems = violations(line + 'DISCOVERY_ANCHORS = ()\n',
                                      'example/negative_controls.py')
                self.assertTrue(problems == [],
                                '#2133-r34 the registered dynamic-import '
                                f'closure shape {label!r} reports: {problems}')

    def test_the_registered_resolved_sys_path_insertion_stays_silent(self):
        """The registered ``.resolve()`` sys.path insertion shapes keep their silence.

        ``_spells_a_repository_path`` follows only bare attribute chains (a
        method call breaks the chain), so ``sys.path.insert(0, str(Path(
        __file__).resolve().parents[1]))`` — the shape dozens of real tables
        use to reach a repository directory — yields no key and no report:
        helper modules imported from that directory stay invisible to the
        import closure, a false negative the r36 MiniMax review raised as
        boundary P0. The chain-following family stopped per the arbitration
        card (``state/decisions-20261109-2136-slice-family-arbitration.md``,
        board 4f8de196d rule 15): no AST patch, the shapes are registered as
        a blind class in the README's follow-up list (r37), and this pin
        holds the current behaviour. A change in *either* direction —
        following the method chain and reporting the insertion, or keying
        the directory — must turn this red and reopen the registration, not
        drift past it. The equivalent ``.parent`` and ``/`` spellings *do*
        report (counter-controls below), so the register cannot silently
        widen to cover this family either.
        """
        shapes = {
            'resolve then parents': (
                "import sys\n"
                "sys.path.insert(0, "
                "str(Path(__file__).resolve().parents[1]))\n"
                "import negative_control\n"),
            'resolve alone': (
                "import sys\n"
                "sys.path.insert(0, str(Path(__file__).resolve()))\n"
                "import negative_control\n"),
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n" + body
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(problems == [],
                                '#2133-r37 the registered resolved sys.path '
                                f'insertion shape {label!r} reports: '
                                f'{problems}')
        counter_controls = {
            'parent chain': (
                "import sys\n"
                "sys.path.insert(0, str(Path(__file__).parent))\n"),
            'join spelling': (
                "import sys\n"
                "FIXTURES = Path(__file__).resolve().parents[1]\n"
                "sys.path.insert(0, str(FIXTURES / '_shared'))\n"),
        }
        for label, body in counter_controls.items():
            with self.subTest(counter=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n" + body
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unsupported sys.path insertion' in problem
                        for problem in problems),
                    '#2133-r37 the counter-control shape '
                    f'{label!r} stopped reporting: {problems}')

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

    def test_a_subscripted_display_string_stays_silent(self):
        """``f'{WORD[0]}/{total} killed'`` is a message, not a path build.

        A name resolving to a plain *string* is not a table of directories:
        the subscript reads a character of the message word, so the new
        element-position constant check must not treat the string itself as
        the container and report a display f-string (r30, review P1 — this
        was the r29 regression that made a passing summary line fail).
        """
        synthetic = (
            "WORD = 'passed'\n"
            "total = 10\n"
            "DISCOVERY_ANCHORS = ()\n"
            "print(f'{WORD[0]}/{total} killed')\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])
        # A word that itself carries a separator is a display too — both
        # container readings (path literal and bare component) must refuse
        # to treat the string itself as the container.
        path_shaped = (
            "WORD = 'a/b'\n"
            "DISCOVERY_ANCHORS = ()\n"
            "print(f'{WORD[0]}/{42} killed')\n"
        )
        self.assertEqual(
            violations(path_shaped, 'example/negative_controls.py'), [])
        # The container spellings keep their reports: a name inside a *real*
        # container is an element position, and a dynamic index onto a
        # container that spells bare components still fails closed.
        container = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "CH = {'k': 'src/pilotdeck-gateway/src/channel/x'}\n"
            "def row(name):\n"
            "    return f'{CH[\"k\"]}/{name}'\n"
        )
        problems = violations(container, 'example/negative_controls.py')
        self.assertTrue(
            any('unsupported f-string path' in problem for problem in problems),
            problems)

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

    def test_a_rebound_anchors_assignment_in_an_annotated_spelling_counts(self):
        """``= (...)`` then ``: tuple = (...)`` is still two assignments.

        ``declared_anchors`` only collected plain ``Assign`` nodes, so an
        annotated or augmented re-binding was not counted while the register
        check collected ``AnnAssign`` — the two disagreed on what a
        declaration is (r24, review P3).
        """
        shapes = {
            'annotated': (
                "from pathlib import Path\n"
                "DISCOVERY_ANCHORS = ()\n"
                "DISCOVERY_ANCHORS: tuple = ('x/mod.rs',)\n"
                "rows = [Path('x/mod.rs')]\n"
            ),
            'augmented': (
                "from pathlib import Path\n"
                "DISCOVERY_ANCHORS = ()\n"
                "DISCOVERY_ANCHORS += ('x/mod.rs',)\n"
                "rows = [Path('x/mod.rs')]\n"
            ),
        }
        for label, synthetic in shapes.items():
            with self.subTest(shape=label):
                _, problems = declared_anchors(ast.parse(synthetic))
                self.assertTrue(
                    any('assigned 2 times' in problem for problem in problems),
                    '#24-P3 the re-binding of DISCOVERY_ANCHORS was not '
                    'counted: ' + repr(problems))
        # A bare annotation binds nothing, so it is not a declaration.
        _, problems = declared_anchors(ast.parse("DISCOVERY_ANCHORS: tuple\n"))
        self.assertEqual(problems, [])

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
        """The same premise break applies to a spelled-out relative join.

        The join spelling goes through the import map (r25): ``o.chdir`` used
        to invalidate the working directory, yet the *relative join* after it
        still had to be spelled ``os.path.join`` or it was keyed from a
        directory the runtime had left.
        """
        shapes = {
            'plain': ("import os\n"
                      "os.chdir('src/pilotdeck-gateway/src/channel')\n"
                      "rows = [os.path.join('sms', 'mod.rs')]\n"),
            'module alias': ("import os as o\n"
                             "o.chdir('src/pilotdeck-gateway/src/channel')\n"
                             "rows = [o.path.join('sms', 'mod.rs')]\n"),
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = (
                    "from pathlib import Path\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    + body
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem for problem in problems),
                    '#25-P2a the aliased relative join after chdir was keyed '
                    f'or silent: {label}: {problems}')

    def test_an_aliased_join_in_a_sys_path_insertion_is_reported(self):
        """``s.path.insert(0, o.path.join(ROOT, 'src/…'))`` inserts a repo dir.

        The directory literal arrives through the join, and the join is
        spelled through a module alias: neither the old string read nor the
        old manual head resolution followed ``o.path.join``, so the insertion
        was silent while ``os.path.join`` spelled the same one red (r25,
        review P2a).
        """
        synthetic = (
            "import os as o\n"
            "import sys as s\n"
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "s.path.insert(0, o.path.join(ROOT, 'src/pilotdeck-gateway/src/channel'))\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(
            any('unsupported sys.path insertion' in problem for problem in problems),
            '#25-P2a the aliased join hid the inserted repository directory: '
            + repr(problems))

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

    def test_a_module_assignment_alias_reaches_chdir(self):
        """``m = os; m.chdir(...)`` moves the directory like ``os.chdir``.

        The import map only knows import aliases, so a chdir-capable module
        reached through a plain assignment left the repository-root premise
        standing and the relative join after it was keyed from a directory
        the runtime had left (r24, review P0-1). Every spelling of the alias
        counts: a dotted module, a second hop and an unpacking.
        """
        shapes = {
            'assignment alias': "import os\nm = os\nm.chdir('src/x')\n",
            'os.path then alias': "import os.path\no = os\no.chdir('src/x')\n",
            'dotted alias': "import os\nm = os.path\nm.chdir('src/x')\n",
            'two hops': "import os\nm = os\nm2 = m\nm2.chdir('src/x')\n",
            'unpacked alias': "import os\nm, s = os, sys\nm.chdir('src/x')\n",
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
                    '#24-P0-1 the module assignment alias did not invalidate the '
                    'working directory: ' + repr(problems))

    def test_a_module_assignment_alias_without_chdir_stays_silent(self):
        """The reverse control: ``m = os`` then ``m.path.join(...)`` moves nothing."""
        synthetic = (
            "import os\n"
            "from pathlib import Path\n"
            "m = os\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [m.path.join('sms', 'mod.rs')]\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

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

    def test_a_separator_bearing_tail_that_normalises_away_is_keyed(self):
        """``SMS / '../x.rs'`` keys ``channel/x.rs`` — it used to key nothing.

        The last operand of a join carrying a separator was dropped at the
        shape gate whatever its runtime meaning, so ``'../x.rs'``,
        ``'./mod.rs'``, ``'mod.rs/'`` and ``'a/../mod.rs'`` built real files
        whose discovery keys the text never spells, and the host's
        ``git grep -F`` could not schedule the table: neither keyed nor
        reported, a silent hole (r37, review P0 — the
        ``_collapsed_segments`` docstring's own ``SMS / '../x.rs'`` example
        was exactly the missed shape). The pathlib spellings now record the
        normalised key; declaring it turns the table green and puts the key
        in the text where the host greps.
        """
        # r38：局部名避开 anchor_selfcheck 的 TABLES 保留名（cases/mutations/
        # controls/...）——「cases =」字样会被 exemption_signals 当成变异表豁免信号，
        # --anchors-only 在 CI 上红（run 42830613311）。命名不改语义，只改拼写。
        tail_shapes = {
            ('leading parent', 'division'): ('../x.rs', 'channel/x.rs'),
            ('leading parent', 'Path'): ('../x.rs', 'channel/x.rs'),
            ('leading parent', 'joinpath'): ('../x.rs', 'channel/x.rs'),
            ('leading dot', 'division'): ('./mod.rs', 'sms/mod.rs'),
            ('leading dot', 'Path'): ('./mod.rs', 'sms/mod.rs'),
            ('leading dot', 'joinpath'): ('./mod.rs', 'sms/mod.rs'),
            ('trailing separator', 'division'): ('mod.rs/', 'sms/mod.rs'),
            ('trailing separator', 'Path'): ('mod.rs/', 'sms/mod.rs'),
            ('trailing separator', 'joinpath'): ('mod.rs/', 'sms/mod.rs'),
            ('internal parent', 'division'): ('a/../mod.rs', 'sms/mod.rs'),
            ('internal parent', 'Path'): ('a/../mod.rs', 'sms/mod.rs'),
            ('internal parent', 'joinpath'): ('a/../mod.rs', 'sms/mod.rs'),
            ('folded across components', 'division'): ('a/b/../c.rs', 'a/c.rs'),
            ('folded across components', 'Path'): ('a/b/../c.rs', 'a/c.rs'),
        }
        for (shape, spelling), (tail, key) in tail_shapes.items():
            with self.subTest(shape=shape, spelling=spelling):
                expression = {
                    'division': f'SMS / {tail!r}',
                    'Path': f'Path(SMS, {tail!r})',
                    'joinpath': f'SMS.joinpath({tail!r})',
                }[spelling]
                header = (
                    "from pathlib import Path\n"
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
                    "DISCOVERY_ANCHORS = ()\n"
                )
                problems = violations(header + f'rows = [{expression}]\n',
                                      'example/negative_controls.py')
                self.assertTrue(
                    any(f'missing anchor: {key} ' in problem
                        for problem in problems),
                    f'#2133-r37 {shape} via {spelling} must demand the '
                    f'normalised key {key}: {problems}')
                declared = (header.replace('DISCOVERY_ANCHORS = ()',
                                           f'DISCOVERY_ANCHORS = ({key!r},)')
                            + f'rows = [{expression}]\n')
                self.assertIn(key, declared)
                self.assertEqual(violations(declared,
                                           'example/negative_controls.py'), [],
                                 f'#2133-r37 {shape} via {spelling} with the '
                                 f'anchor declared: '
                                 f'{violations(declared, "example/negative_controls.py")}')

    def test_a_stdlib_join_with_a_separator_bearing_tail_reports(self):
        """``os.path.join(SMS, '../x.rs')`` reports instead of staying silent.

        The pathlib spellings of the r37 family record the normalised key;
        this scanner cannot resolve a filesystem join whose base is a
        ``Path`` object, so the same tail there fails closed as an
        unresolvable repository path — never the silent drop the shape gate
        used to apply before r37.
        """
        for tail in ('../x.rs', './mod.rs', 'mod.rs/', 'a/../mod.rs'):
            with self.subTest(tail=tail):
                synthetic = (
                    "import os\n"
                    "from pathlib import Path\n"
                    "ROOT = Path(__file__).resolve().parents[5]\n"
                    "SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'\n"
                    "DISCOVERY_ANCHORS = ()\n"
                    f"rows = [os.path.join(SMS, {tail!r})]\n"
                )
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem
                        for problem in problems),
                    f'#2133-r37 os.path.join(SMS, {tail!r}) must not be '
                    f'silent: {problems}')

    def test_an_inline_directory_spelling_keeps_its_historic_silence(self):
        """``SOURCE / 'runtime/memory/gateway.rs'`` needs no anchor.

        The r37 widening admits separator-bearing tails so the normalising
        family is keyed, but a tail that still spells its own last two
        components keeps the historic silence: the host's ``git grep -F``
        finds the key in the text, and demanding an anchor would fail every
        inline spelling in the tree (the first cut of the fix did exactly
        that to five real tables).
        """
        synthetic = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'src/pilotdeck-gateway/src/channel/sms/mod.rs']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

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

    def test_a_copy_of_a_subcontainer_carries_the_nested_write_back(self):
        """``CP = CH['a'].copy()`` shares what ``CH['a']`` holds.

        The shallow-copy edges only started at the root container, so a copy
        of a *sub*container left ``CH`` looking unmutated and the join was
        keyed from a literal the runtime had already replaced through the
        copy (r24, review P0-2). Every copy spelling of a sub-container is an
        edge, and the same goes for the list-shaped family.
        """
        prefix = (
            "from pathlib import Path\n"
            "CH = {'a': {'b': {'p': 'src/pilotdeck-gateway/src/channel/sms'}}}\n"
            "SEQ = [[{'p': 'src/pilotdeck-gateway/src/channel/sms'}]]\n"
            "DISCOVERY_ANCHORS = ('sms/mod.rs',)\n"
        )
        shapes = {
            "CH['a'].copy()": ("CP = CH['a'].copy()\n",
                               "CP['b']['p'] = "
                               "'src/pilotdeck-gateway/src/channel/mms'\n"
                               "rows = [Path(CH['a']['b']['p']) / 'mod.rs']\n"),
            'copy.copy(CH[\'a\'])': ("import copy\nCP = copy.copy(CH['a'])\n",
                                     "CP['b']['p'] = "
                                     "'src/pilotdeck-gateway/src/channel/mms'\n"
                                     "rows = [Path(CH['a']['b']['p']) / 'mod.rs']\n"),
            'copy.deepcopy(CH[\'a\'])': ("import copy\nCP = copy.deepcopy(CH['a'])\n",
                                         "CP['b']['p'] = "
                                         "'src/pilotdeck-gateway/src/channel/mms'\n"
                                         "rows = [Path(CH['a']['b']['p']) / 'mod.rs']\n"),
            "dict(CH['a'])": ("CP = dict(CH['a'])\n",
                              "CP['b']['p'] = "
                              "'src/pilotdeck-gateway/src/channel/mms'\n"
                              "rows = [Path(CH['a']['b']['p']) / 'mod.rs']\n"),
            "{**CH['a']}": ("CP = {**CH['a']}\n",
                             "CP['b']['p'] = "
                             "'src/pilotdeck-gateway/src/channel/mms'\n"
                             "rows = [Path(CH['a']['b']['p']) / 'mod.rs']\n"),
            "CH['a'][:]": ("CP = CH['a'][:]\n",
                           "CP['b']['p'] = "
                           "'src/pilotdeck-gateway/src/channel/mms'\n"
                           "rows = [Path(CH['a']['b']['p']) / 'mod.rs']\n"),
            "SEQ[0].copy()": ("CP = SEQ[0].copy()\n",
                              "CP[0]['p'] = "
                              "'src/pilotdeck-gateway/src/channel/mms'\n"
                              "rows = [Path(SEQ[0][0]['p']) / 'mod.rs']\n"),
            "SEQ[0][:]": ("CP = SEQ[0][:]\n",
                          "CP[0]['p'] = "
                          "'src/pilotdeck-gateway/src/channel/mms'\n"
                          "rows = [Path(SEQ[0][0]['p']) / 'mod.rs']\n"),
            "list(SEQ[0])": ("CP = list(SEQ[0])\n",
                             "CP[0]['p'] = "
                             "'src/pilotdeck-gateway/src/channel/mms'\n"
                             "rows = [Path(SEQ[0][0]['p']) / 'mod.rs']\n"),
            "[*SEQ[0]]": ("CP = [*SEQ[0]]\n",
                          "CP[0]['p'] = "
                          "'src/pilotdeck-gateway/src/channel/mms'\n"
                          "rows = [Path(SEQ[0][0]['p']) / 'mod.rs']\n"),
            "SEQ[0] + [{}]": ("CP = SEQ[0] + [{}]\n",
                              "CP[0]['p'] = "
                              "'src/pilotdeck-gateway/src/channel/mms'\n"
                              "rows = [Path(SEQ[0][0]['p']) / 'mod.rs']\n"),
        }
        for label, (copy, write) in shapes.items():
            with self.subTest(shape=label):
                synthetic = prefix + copy + write
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unresolvable repository path' in problem for problem in problems),
                    '#24-P0-2 the nested write through the sub-container copy '
                    'did not reach the root container: ' + repr(problems))

    def test_a_subcontainer_copys_own_top_level_write_stays_silent(self):
        """The reverse control: a *copy*'s top level is a separate object.

        ``CP = CH['a'].copy(); CP['b'] = ...`` rebinds only the copy's own
        top level, so ``CH['a']['b']`` keeps its literal and the key still
        follows it — reporting this would turn every copy into a false red.
        """
        synthetic = (
            "from pathlib import Path\n"
            "CH = {'a': {'b': {'p': 'src/pilotdeck-gateway/src/channel/x'}}}\n"
            "DISCOVERY_ANCHORS = ('x/mod.rs',)\n"
            "CP = CH['a'].copy()\n"
            "CP['b'] = {'p': 'src/pilotdeck-gateway/src/channel/sms'}\n"
            "rows = [Path(CH['a']['b']['p']) / 'mod.rs']\n"
        )
        self.assertEqual(violations(synthetic, 'example/negative_controls.py'), [])

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

    def test_a_dotdot_prefixed_sys_path_entry_is_reported(self):
        """``sys.path.insert(0, '../…')`` can climb right back into this repo.

        A relative entry resolves against the working directory the host
        starts tables in (the repository root), so ``../<sibling>/…`` ends up
        inside this very tree; the directory it names is not statically
        knowable, so the insertion is reported rather than read as outside
        (r24, review P0-3).
        """
        shapes = {
            'insert': (
                "import sys\n"
                "sys.path.insert(0, "
                "'../wt-2133/src/pilotdeck-gateway/tests/fixtures/native-entry-authority')\n"
            ),
            'append': "import sys\nsys.path.append('../sibling/fixtures-helpers')\n",
            'bare ..': "import sys\nsys.path.insert(0, '..')\n",
            'join spelling': (
                "import os, sys\n"
                "sys.path.insert(0, os.path.join('..', 'helpers'))\n"
            ),
            'slice assignment': "import sys\nsys.path[:0] = ['../helpers']\n",
        }
        for label, body in shapes.items():
            with self.subTest(shape=label):
                synthetic = "DISCOVERY_ANCHORS = ()\n" + body + "import helper\n"
                problems = violations(synthetic, 'example/negative_controls.py')
                self.assertTrue(
                    any('unsupported sys.path' in problem for problem in problems),
                    '#24-P0-3 the dot-dot-prefixed entry was read as outside '
                    'the repository: ' + repr(problems))

    def test_a_non_parent_dotdot_named_entry_stays_silent(self):
        """``'..foo'`` is a plain component, not a parent step.

        Only ``..`` / ``../`` / ``..\\`` climb; a component merely *starting*
        with the two dots is an ordinary relative name and the insertion of
        it stays as silent as the absolute third-party entry.
        """
        synthetic = (
            "import sys\n"
            "DISCOVERY_ANCHORS = ()\n"
            "sys.path.insert(0, '..foo')\n"
            "import helper\n"
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

    def test_a_relative_import_binding_conflicts_with_an_absolute_one(self):
        """``from . import os`` is a binding, not a gap in the import map.

        Skipping the relative form whole let a function-local
        ``import random as os`` look like the *only* binding of ``os`` while
        the module-level relative import was the one the joins really used —
        the join nets silently resolved to the wrong family (r29, review
        P0). Both spellings must be collected, and their disagreement
        reported at the use sites instead of either side winning.
        """
        mixed_scopes = (
            "from . import os\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.path.join('src', 'pilotdeck-gateway', 'src', 'lib.rs')]\n"
            "def f():\n"
            "    import random as os\n"
            "    return os.random()\n"
        )
        problems = violations(mixed_scopes, 'example/negative_controls.py')
        self.assertTrue(any('conflicting import bindings' in problem
                            for problem in problems), problems)
        both_top_level = (
            "from . import os\n"
            "import os\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.path.join('src', 'pilotdeck-gateway', 'src', 'lib.rs')]\n"
        )
        problems = violations(both_top_level, 'example/negative_controls.py')
        self.assertTrue(any('conflicting import bindings' in problem
                            for problem in problems), problems)

    def test_a_sole_relative_import_keeps_the_literal_spelling_fallback(self):
        """One relative binding is unknown, not wrong — the literal still keys.

        Where the package's module sits is a runtime fact, so a sole
        ``from . import os`` maps nowhere and the join falls back to its
        literal spelling instead of resolving through a guessed module
        (r29, review P0). The fallback must not read as a conflict.
        """
        synthetic = (
            "from . import os\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [os.path.join('src', 'pilotdeck-gateway', 'src', 'lib.rs')]\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertFalse(any('conflicting import bindings' in problem
                             for problem in problems), problems)
        self.assertTrue(any('missing anchor: src/lib.rs' in problem
                            for problem in problems), problems)

    def test_a_named_constant_inside_a_container_still_spells_the_path(self):
        """``S = 'src/example/x'`` then ``CH = {'a': S}`` keys like the literal.

        The r28 container recursion only tested elements written out inline,
        so a name resolved to a constant returned False and the spelled-out
        form reported where the named one silently passed (r29, review P0).
        """
        synthetic = (
            "from pathlib import Path\n"
            "S = 'src/example/x'\n"
            "CH = {'a': S}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(CH['a']) / 'mod.rs']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: x/mod.rs' in problem
                            for problem in problems), problems)
        self.assertTrue(any('anchor is not a literal in this file: x/mod.rs'
                            in problem for problem in problems), problems)
        # The dynamic index hides the value instead of pairing it, so the
        # container walk is the only net that can see the named constant —
        # without the constant branch the recursion read "not a container"
        # for every resolved name and the lookup stayed silent.
        dynamic_index = (
            "from pathlib import Path\n"
            "S = 'src/example/x'\n"
            "CH = {'a': S}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return Path(CH[name]) / 'mod.rs'\n"
        )
        problems = violations(dynamic_index, 'example/negative_controls.py')
        self.assertTrue(any('unresolvable repository path' in problem
                            for problem in problems), problems)

    def test_a_named_constant_inside_a_single_component_container_keys(self):
        """``D = {'one': S}`` with ``S = 'src'`` reads the directory the same.

        The mirror of the path-literal case for the single-component lookup:
        the value is only hidden behind the name, not absent (r29, review
        P0).
        """
        literal_index = (
            "from pathlib import Path\n"
            "S = 'src'\n"
            "D = {'one': S}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(D['one']) / 'mod.rs']\n"
        )
        problems = violations(literal_index, 'example/negative_controls.py')
        self.assertTrue(any('missing anchor: src/mod.rs' in problem
                            for problem in problems), problems)
        dynamic_index = (
            "from pathlib import Path\n"
            "S = 'src'\n"
            "D = {'one': S}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return Path(D[name]) / 'mod.rs'\n"
        )
        problems = violations(dynamic_index, 'example/negative_controls.py')
        self.assertTrue(any('unresolvable repository path' in problem
                            for problem in problems), problems)
        # The *selected* element decides, not any sibling: a literal index
        # onto a value that is not a string at all stays silent even when
        # another value in the same container is a bare component — the
        # container-wide fallback is only for indexes this scanner cannot
        # pair (a string with a separator is the path-literal net's case,
        # not this one's).
        precise = (
            "from pathlib import Path\n"
            "N = 42\n"
            "V = 'src'\n"
            "D = {'one': N, 'two': V}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [Path(D['one']) / 'mod.rs']\n"
        )
        problems = violations(precise, 'example/negative_controls.py')
        self.assertEqual(problems, [])

    def test_an_unreadable_container_element_proves_nothing_either_way(self):
        """A container element this scanner cannot read is not a directory read.

        ``D = {'one': UNBOUND}`` — the value is a runtime fact, and neither
        the readable-constant branch nor a literal element applies, so the
        lookup stays with the historic silent class rather than failing
        closed on a guess (the boundary of the r29 fix: only statically
        readable names resolve).
        """
        synthetic = (
            "from pathlib import Path\n"
            "D = {'one': UNBOUND}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return Path(D[name]) / 'mod.rs'\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertEqual(problems, [])

    def test_a_joinpath_receiver_is_drilled_into_not_its_arguments(self):
        """``Path(D[name]).joinpath('a', 'b')`` builds from the receiver.

        Reading the first *argument* of a method on the piece took the tail
        and dropped the dynamic head, so the join fell out of every net while
        the table stayed green (r29, review P0). ``join`` keeps reading its
        first argument: a stdlib join builds from its arguments, not from
        the ``os.path`` receiver.
        """
        synthetic = (
            "from pathlib import Path\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return Path(D[name]).joinpath('pilotdeck-gateway', 'src', 'lib.rs')\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any("joinpath('pilotdeck-gateway'" in problem
                            and 'unresolvable repository path' in problem
                            for problem in problems), problems)
        resolved = (
            "from pathlib import Path\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return Path(D[name]).resolve() / 'x' / 'lib.rs'\n"
        )
        problems = violations(resolved, 'example/negative_controls.py')
        self.assertTrue(any('unresolvable repository path' in problem
                            for problem in problems), problems)

    def test_a_method_call_with_keyword_arguments_still_reads_the_receiver(self):
        """``Path(D[name]).with_suffix(suffix='')`` keeps its dynamic head.

        Keyword arguments do not change the receiver, so the receiver branch
        must drill into ``D[name]`` however the method spells its options:
        excluding keyword calls let this shape fall through to the generic
        branch, read the empty-string tail, and pass with empty anchors while
        the runtime path sat inside the repository (r30, review P0).
        """
        keyworded = (
            "from pathlib import Path\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return Path(D[name]).with_suffix(suffix='') / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        problems = violations(keyworded, 'example/negative_controls.py')
        self.assertTrue(
            any("with_suffix(suffix='') / 'pilotdeck-gateway'" in problem
                and 'unresolvable repository path' in problem
                for problem in problems), problems)

    def test_a_string_slice_head_is_an_unresolvable_repository_path(self):
        """``Path(WORD[:3]) / …`` builds a repository path from a substring.

        A slice of a spelled constant may read a real directory prefix the
        contiguous text no longer carries, so the join is as opaque as any
        other dynamic head; r30's split of element and container positions
        stopped treating the string itself as a container and let this head
        pass silently (r31, review P0 — the same fix's new shape, human
        ruling 11:41: slices fail closed). A *constant index* reads one
        display character and keeps its silence.
        """
        sliced = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row(name):\n"
            "    return Path(WORD[:3]) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        problems = violations(sliced, 'example/negative_controls.py')
        self.assertTrue(
            any("Path(WORD[:3]) / 'pilotdeck-gateway'" in problem
                and 'unresolvable repository path' in problem
                for problem in problems), problems)
        sliced_fstring = (
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row(name):\n"
            "    return f'{WORD[:3]}/{name}'\n"
        )
        problems = violations(sliced_fstring, 'example/negative_controls.py')
        self.assertTrue(
            any('unsupported f-string path' in problem
                and "f'{WORD[:3]}/{name}'" in problem
                for problem in problems), problems)
        # A slice with constant bounds is still a substring — statically
        # one character or not, it fails closed rather than measuring.
        constant_bounds = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row(name):\n"
            "    return Path(WORD[0:3]) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        problems = violations(constant_bounds, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                for problem in problems), problems)
        # The constant-index display readings keep their silence, in both
        # the join and the f-string spellings.
        indexed = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row(name):\n"
            "    return Path(WORD[0]) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        self.assertEqual(violations(indexed, 'example/negative_controls.py'),
                         [])
        dynamic_index = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row(name):\n"
            "    return Path(WORD[name]) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        self.assertEqual(violations(dynamic_index,
                                    'example/negative_controls.py'), [])
        # A statically one-character slice is still a slice — reported on its
        # own, not only when it rides along a longer bound (r32, review P2:
        # the r31 pin only exercised ``[:3]``-shaped bounds).
        one_character = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row(name):\n"
            "    return Path(WORD[0:1]) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        problems = violations(one_character, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                for problem in problems), problems)
        # A filesystem join's absolute argument discards the sliced head —
        # ``os.path.join(WORD[:3], '/tmp', 'mod.rs')`` reads ``/tmp/mod.rs``,
        # outside the repository — so the slice disjunct honours the same
        # absolute-reset silence the single-component fallback already does
        # (r32, review P1: the disjunct fired before the reset check).
        absolute_reset = (
            "import os\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return os.path.join(WORD[:3], '/tmp', 'mod.rs')\n"
        )
        self.assertEqual(violations(absolute_reset,
                                   'example/negative_controls.py'), [])

    def test_a_concatenated_slice_head_is_an_unresolvable_repository_path(self):
        """``Path(WORD[:3] + '/pilotdeck-gateway')`` still reads the substring.

        A ``+`` concatenation carries whatever either side builds, so a sliced
        constant wrapped in the concatenation is the same opaque head the
        bare slice already is — the predicate stopped at the ``BinOp`` and
        let ``Path(WORD[:3] + '/pilotdeck-gateway') / 'src' / 'lib.rs'``
        pass silently while its key sat inside the spelled constant
        (r32, review P0). An all-literal ``+`` chain has no slice on either
        side and keeps the registered silence.
        """
        concatenated = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return Path(WORD[:3] + '/pilotdeck-gateway') / 'src' / 'lib.rs'\n"
        )
        problems = violations(concatenated, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                for problem in problems), problems)
        concatenated_fstring = (
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row(name):\n"
            "    return f'{WORD[:3] + \"/\"}{name}'\n"
        )
        problems = violations(concatenated_fstring,
                              'example/negative_controls.py')
        self.assertTrue(
            any('unsupported f-string path' in problem
                for problem in problems), problems)
        # The all-literal ``+`` concat keeps its registered silence: no slice
        # on either side, and the whole-string blind shape is the README's
        # registration, not a new report (network-timing precedent).
        all_literal = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row():\n"
            "    return Path('sms' + '/' + 'mod.rs')\n"
        )
        self.assertEqual(violations(all_literal,
                                    'example/negative_controls.py'), [])

    def test_a_right_nested_concatenation_head_is_reported(self):
        """Parenthesised ``+`` grouping hides operands from a left-spine walk.

        ``head + ('/dir' + '/sub/')`` puts the separators in a right-nested
        subtree: the operands list only flattened the left spine, so both
        spellings — the f-string interpolation and the ``Path`` head — let
        the sliced head pass silently while its key sat inside the spelled
        constant (r33 arbitration design round, codex review P0 and its
        second syntax shape). Flattening both sides keeps one reading
        wherever the parentheses sit; a slice on either side of the chain
        is the same report, so the reversed head is pinned here too
        (anthropic review P2: only the left-side spelling was nailed).
        """
        right_nested_fstring = (
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row(name):\n"
            "    return f'{WORD[:3] + (\"/pilotdeck-gateway\" + \"/src/\")}{name}'\n"
        )
        problems = violations(right_nested_fstring,
                              'example/negative_controls.py')
        self.assertTrue(
            any('unsupported f-string path' in problem
                for problem in problems), problems)
        right_nested_path_head = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return Path(WORD[:3] + ('/pilotdeck-gateway' + '/src/')) / 'lib.rs'\n"
        )
        problems = violations(right_nested_path_head,
                              'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                for problem in problems), problems)
        # The slice may sit on either side of the concatenation — the
        # reversed head is the same substring read (anthropic review P2:
        # the r32 test only exercised the slice-on-left spelling).
        reversed_head = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return Path('/pilotdeck-gateway' + WORD[:3]) / 'src' / 'lib.rs'\n"
        )
        problems = violations(reversed_head, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                for problem in problems), problems)
        reversed_nested = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return Path('/pilotdeck-gateway' + ('src/' + WORD[:3])) / 'lib.rs'\n"
        )
        problems = violations(reversed_nested, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                for problem in problems), problems)
        # The all-literal registration holds for the nested spelling too:
        # no slice on either side of any nesting is still no report.
        all_literal_nested = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row():\n"
            "    return Path('sms' + ('/' + 'mod.rs'))\n"
        )
        self.assertEqual(violations(all_literal_nested,
                                    'example/negative_controls.py'), [])

    def test_a_self_referential_fstring_constant_terminates(self):
        """A constant cycle through an interpolated ``+`` must not recurse.

        ``name = f'{name + chr(120)}'`` binds the constant to the very
        JoinedStr it spells. The interpolated reading used to drop its
        visited-names tuple, so once the ``+`` branch resolved the left
        operand back into that JoinedStr the walk had no memory and
        recursed until the interpreter gave up — a crash in the guard
        itself (r33 arbitration design round, codex review P0). Carrying
        ``seen`` through the interpolation terminates the cycle; the
        build is an opaque head joined to an opaque tail, so it keeps the
        registered silence rather than reporting.
        """
        self_referential = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "name = f'{name + chr(120)}'\n"
            "def row():\n"
            "    return Path(name) / 'lib.rs'\n"
        )
        self.assertEqual(violations(self_referential,
                                   'example/negative_controls.py'), [])

    def test_a_wrapped_or_operatored_absolute_reset_stays_silent(self):
        """The absolute-reset silence holds behind wrappers and ``/`` chains.

        ``Path(os.path.join(WORD[:3], '/tmp')) / 'src'`` wraps the resetting
        join in a ``Path`` call, and ``Path(WORD[:3]) / '/tmp'`` resets on
        the pathlib operator itself — both read ``/tmp/…``, outside the
        repository, so no discovery key exists to report (r33 arbitration
        design round, codex review P1 and anthropic review P2: the reset
        predicate saw only the outer chain). The entry walks the ``/``
        spine and the wrappers to the join first; a relative tail keeps
        reporting, so the reset cannot silence a real key.
        """
        wrapped_join = (
            "import os\n"
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return Path(os.path.join(WORD[:3], '/tmp')) / 'src' / 'mod.rs'\n"
        )
        self.assertEqual(violations(wrapped_join,
                                    'example/negative_controls.py'), [])
        operatored = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return Path(WORD[:3]) / '/tmp' / 'mod.rs'\n"
        )
        self.assertEqual(violations(operatored,
                                    'example/negative_controls.py'), [])
        # The reset only silences absolute segments: a relative tail keeps
        # the slice head reported exactly as without the reset entry.
        relative_tail = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return Path(WORD[:3]) / 'src' / 'mod.rs'\n"
        )
        problems = violations(relative_tail, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                for problem in problems), problems)

    def test_a_display_counter_with_a_sliced_head_is_reported_fail_closed(self):
        """``f'{WORD[:3] + '/'}{total} killed'`` reports — the pinned ruling.

        A display counter whose head slices a module string constant is
        statically indistinguishable from a repository build: the guard
        cannot read the constant's value into the judgment, and the r31
        fail-closed ruling prefers one extra report over a hole (r33
        arbitration: option (a), the README registers the over-report as
        a known blind shape instead of guessing values).
        """
        display_counter = (
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'failed'\n"
            "def summary(total):\n"
            "    return f'{WORD[:3] + \"/\"}{total} killed'\n"
        )
        problems = violations(display_counter, 'example/negative_controls.py')
        self.assertTrue(
            any('unsupported f-string path' in problem
                for problem in problems), problems)

    def test_the_registered_single_interpolation_nested_form_stays_silent(self):
        """``f'{WORD[:3] + ("/" + name)}'`` stays silent — the pinned twin.

        The display counter above reports fail-closed (r33 arbitration
        option (a)); this same-root nested form has no successor segment,
        so it rides the registered "opaque directory + runtime name"
        silence instead (README: r33 residual registration, found by the
        r33 anthropic review at P2 — a registered silence needs its
        reverse control or a drift toward reporting passes unnoticed).
        """
        single_interpolation = (
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row(name):\n"
            "    return f'{WORD[:3] + (\"/\" + name)}'\n"
        )
        self.assertEqual(violations(single_interpolation,
                                    'example/negative_controls.py'), [])

    def test_a_sliced_head_with_an_absolute_or_drive_segment_stays_silent(self):
        """The two fix-r33-registered blind shapes keep their pinned silence.

        ``Path(WORD[:3]) / <in-repo absolute> / 'lib.rs'`` lands inside the
        repository and ``Path(WORD[:3]) / 'C:/tmp' / 'mod.rs'`` contradicts
        the literal-head drive reporting (r20/r21), yet both stay silent:
        the reset predicate reads any absolute or drive segment as
        "assumed outside the repository" and cannot tell them apart from
        ``/tmp``. fix-r33 extended that silence from joins to the ``/``
        spine — r32 reported both shapes, and their ``os.path.join`` twins
        have been silent since r29/r32 (measured in
        ``_tmp/20261109-2136-harvest/fix-r33/p0-reproduction.log`` and
        ``literal-head-probes.log``; literal heads keep reporting in all
        four spellings). The slice family stopped after its third hole
        (board 4f8de196d rule 15, arbitration card
        ``state/decisions-20261109-2136-slice-family-arbitration.md``):
        these shapes are registered as blind in the README instead of a
        fix-r34, and reopening them needs a human ruling on the
        ``_join_resets_to_absolute`` / ``_resets_join`` semantics.
        """
        in_repo = str(FIXTURES.parents[2] / 'src' / 'lib.rs')
        in_repo_absolute = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return Path(WORD[:3]) / " + repr(in_repo) + " / 'lib.rs'\n"
        )
        self.assertEqual(violations(in_repo_absolute,
                                    'example/negative_controls.py'), [])
        drive_segment = (
            "from pathlib import Path\n"
            "DISCOVERY_ANCHORS = ()\n"
            "WORD = 'src/pilotdeck-gateway/src'\n"
            "def row():\n"
            "    return Path(WORD[:3]) / 'C:/tmp' / 'mod.rs'\n"
        )
        self.assertEqual(violations(drive_segment,
                                    'example/negative_controls.py'), [])

    def test_a_module_function_with_keyword_arguments_reads_its_first_argument(self):
        """``os.path.realpath(D[name], strict=False)`` builds from the argument.

        The receiver ``os.path`` is a module, so the call is a module
        *function*: the path sits in the first argument. r30 read the
        receiver unconditionally and let the dynamic head pass silently
        (r31, review P0 — human ruling 11:41: dispatch on the resolved
        ``func.value``). The instance-method spellings keep reading their
        receiver, keywords or not.
        """
        module_function = (
            "from pathlib import Path\n"
            "import os\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return Path(os.path.realpath(D[name], strict=False)) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        problems = violations(module_function, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                and 'realpath(D[name], strict=False)' in problem
                for problem in problems), problems)
        # Without the keyword the module-function reading is the same — r30
        # dropped this report along with the keyworded shape.
        bare_module_function = (
            "from pathlib import Path\n"
            "import os\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return os.path.realpath(D[name]) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        problems = violations(bare_module_function,
                              'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                for problem in problems), problems)
        # An import alias resolves to the same module, so the dispatch sees
        # through it; the receiver spellings stay on the receiver.
        aliased = (
            "from pathlib import Path\n"
            "import os as fs\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return fs.path.realpath(D[name], strict=False) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        problems = violations(aliased, 'example/negative_controls.py')
        self.assertTrue(
            any('unresolvable repository path' in problem
                for problem in problems), problems)
        # The r30 instance-method reports must not regress.
        instance_method = (
            "from pathlib import Path\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return Path(D[name]).resolve(strict=True) / 'pilotdeck-gateway' / 'src' / 'lib.rs'\n"
        )
        problems = violations(instance_method, 'example/negative_controls.py')
        self.assertTrue(
            any("Path(D[name]).resolve(strict=True) / 'pilotdeck-gateway'"
                in problem
                and 'unresolvable repository path' in problem
                for problem in problems), problems)

    def test_a_runtime_parents_index_behind_a_method_call_is_reported(self):
        """``parents[int(0)].joinpath('..')`` hides the depth behind a call.

        The structural walk used to stop at the shapes it recognized, so a
        method call *with* arguments read back as "no runtime index" and the
        unverifiable root was assumed anyway (r29, review P0). The whole
        expression is now searched, so a runtime ``.parents`` index anywhere
        below fails closed.
        """
        synthetic = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[int(0)].joinpath('..')\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'negative_scope.py']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertTrue(any('runtime index' in problem
                            for problem in problems), problems)
        aliased = (
            "from pathlib import Path\n"
            "IDX = int('4')\n"
            "BASE = Path(__file__).resolve().parents[IDX]\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [BASE / 'negative_scope.py']\n"
        )
        problems = violations(aliased, 'example/negative_controls.py')
        self.assertTrue(any("BASE / 'negative_scope.py'" in problem
                            and 'unresolvable' in problem
                            for problem in problems), problems)

    def test_a_literal_parents_depth_with_a_joinpath_stays_historic(self):
        """``parents[5].joinpath('..')`` keeps its old silent reading.

        The index is a literal, so the depth check keeps its old reading for
        it; the depth the ``joinpath('..')`` then removes stays with the
        historic "not statically visible" class — declared here so the
        fail-closed branch above does not quietly widen to it (r29).
        """
        synthetic = (
            "from pathlib import Path\n"
            "ROOT = Path(__file__).resolve().parents[5].joinpath('..')\n"
            "DISCOVERY_ANCHORS = ()\n"
            "rows = [ROOT / 'negative_scope.py']\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertEqual(problems, [])

    def test_an_absolute_reset_inside_a_stdlib_join_stays_silent(self):
        """``os.path.join(D[name], '/tmp', 'mod.rs')`` reads ``/tmp/mod.rs``.

        The reset argument throws the dynamic head away, so the
        single-component-directory fallback must not report it as a
        repository path (r29, review P1). Only the *filesystem* joins reset;
        the string-join reading keeps its own report, exactly as the
        existing string-join test already asserts.
        """
        synthetic = (
            "import os\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return os.path.join(D[name], '/tmp', 'mod.rs')\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertEqual(problems, [])
        # A Windows-shaped piece keeps the *report*, not the silence: the
        # drive-prefixed and backslash forms have their own net, asserted by
        # ``test_a_drive_prefixed_segment_inside_a_join_is_reported``.

    def test_an_import_aliased_join_with_an_absolute_reset_stays_silent(self):
        """``j(D[name], '/tmp', 'mod.rs')`` reads ``/tmp/mod.rs`` too.

        The reset defence has to be spelling-independent — ``from os.path
        import join as j`` reaches the same stdlib join, and a gate that only
        accepted the ``Attribute`` spelling left the imported one reporting a
        path the runtime never builds inside the repository (r30, review P1).
        """
        synthetic = (
            "from os.path import join as j\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return j(D[name], '/tmp', 'mod.rs')\n"
        )
        problems = violations(synthetic, 'example/negative_controls.py')
        self.assertEqual(problems, [])
        # Without the reset argument the same imported spelling keeps its
        # report — the alias resolution did not quietly widen to silence.
        reporting = (
            "from os.path import join as j\n"
            "D = {'one': 'src'}\n"
            "DISCOVERY_ANCHORS = ()\n"
            "def row(name):\n"
            "    return j(D[name], 'tmp', 'mod.rs')\n"
        )
        problems = violations(reporting, 'example/negative_controls.py')
        self.assertTrue(any('unresolvable repository path' in problem
                            for problem in problems), problems)

    def test_a_match_case_shadow_is_reported(self):
        """``match`` arms land in the class namespace like ``if`` arms do.

        A ``def`` under a ``match`` case overwrites the one above it, and
        one pass executes at most one arm — two definitions in *different*
        arms of the same statement collide only through the shadowing, which
        is exactly what the duplicate-name scan reports (r29, review P2).
        """
        source = (
            'class T(unittest.TestCase):\n'
            '    def test_a(self):\n        pass\n'
            '    match 1:\n'
            '        case 1:\n'
            '            def test_a(self):\n                pass\n'
        )
        self.assertEqual(_duplicate_test_names(source), {'T': ['test_a']})
        same_arm = (
            'class T(unittest.TestCase):\n'
            '    match 1:\n'
            '        case 1:\n'
            '            def test_a(self):\n                pass\n'
            '            def test_a(self):\n                pass\n'
        )
        self.assertEqual(_duplicate_test_names(same_arm), {'T': ['test_a']})

    def test_mutually_exclusive_arms_do_not_collide(self):
        """One pass executes at most one arm of an ``if``/``elif``/``match``.

        The r28 drill-down read both arms of one ``if`` as duplicates (r29,
        review P2); each arm-max statement now contributes one definition of
        a name however many arms spell it, while an ``elif`` chain needs no
        flattening of its own — the nested ``if`` sits inside the ``else``
        arm.
        """
        if_else = (
            'class T(unittest.TestCase):\n'
            '    if True:\n'
            '        def test_a(self):\n            pass\n'
            '    else:\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(if_else), {})
        elif_chain = (
            'class T(unittest.TestCase):\n'
            '    if 0:\n'
            '        def test_a(self):\n            pass\n'
            '    elif 1:\n'
            '        def test_a(self):\n            pass\n'
            '    else:\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(elif_chain), {})
        match_arms = (
            'class T(unittest.TestCase):\n'
            '    match 1:\n'
            '        case 1:\n'
            '            def test_a(self):\n                pass\n'
            '        case 2:\n'
            '            def test_a(self):\n                pass\n'
        )
        self.assertEqual(_duplicate_test_names(match_arms), {})

    def test_a_branch_definition_and_an_outer_definition_collide(self):
        """Arms of different statements — and the body around them — do add up.

        An ``if`` arm and the top-level body around it can both run in one
        pass, so their definitions of one name collide; two definitions in
        the *same* arm are a plain duplicate.
        """
        branch_and_outer = (
            'class T(unittest.TestCase):\n'
            '    def test_a(self):\n        pass\n'
            '    if True:\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(branch_and_outer), {'T': ['test_a']})
        same_arm = (
            'class T(unittest.TestCase):\n'
            '    if True:\n'
            '        def test_a(self):\n            pass\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(same_arm), {'T': ['test_a']})

    def test_a_try_body_and_its_handler_both_define_the_name(self):
        """``try`` is not mutually exclusive: the exception can fire late.

        The body defines the name, the exception fires *after* that line,
        and the handler defines it again — one pass can land both in the
        class namespace, so the body and ``finally`` merge additively with
        the handler block rather than taking an arm maximum (r29; the
        handler/``else`` maximum is this round's fix, see the test below).
        """
        source = (
            'class T(unittest.TestCase):\n'
            '    try:\n'
            '        def test_a(self):\n            pass\n'
            '    except E:\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(source), {'T': ['test_a']})

    def test_mutually_exclusive_try_branches_do_not_collide(self):
        """One exception unwinds to exactly one ``except`` — or skips them all.

        Two handlers of a plain ``try`` never both run, and ``else`` runs only
        when no exception fired, so a name defined in each still counts once;
        summing the regions reported a legal class body as shadowed (r30,
        review P1). ``finally`` always runs and stays additive with whatever
        preceded it.
        """
        handlers = (
            'class T(unittest.TestCase):\n'
            '    try:\n'
            '        raise ValueError()\n'
            '    except ValueError:\n'
            '        def test_a(self):\n            pass\n'
            '    except TypeError:\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(handlers), {})
        handler_and_else = (
            'class T(unittest.TestCase):\n'
            '    try:\n'
            '        pass\n'
            '    except ValueError:\n'
            '        def test_a(self):\n            pass\n'
            '    else:\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(handler_and_else), {})
        finally_adds = (
            'class T(unittest.TestCase):\n'
            '    try:\n'
            '        raise ValueError()\n'
            '    except ValueError:\n'
            '        pass\n'
            '    finally:\n'
            '        def test_a(self):\n            pass\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(finally_adds), {'T': ['test_a']})
        # ``finally`` always runs *after* the body, so a name defined once in
        # each region really lands twice in one pass — the regions add, they
        # do not take a maximum (r30 review P2, pinned after the ruling).
        body_and_finally = (
            'class T(unittest.TestCase):\n'
            '    try:\n'
            '        def test_a(self):\n            pass\n'
            '    finally:\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(body_and_finally),
                         {'T': ['test_a']})

    def test_try_star_handlers_can_all_run_and_do_collide(self):
        """An exception group fans one pass out through several ``except*``.

        ``try*`` handlers are not mutually exclusive the way plain ``except``
        clauses are: the group keeps travelling, so a name defined in two of
        them really can be defined twice in one pass (r30, review P1 — the
        plain-``try`` maximum must not quietly cover the starred spelling).
        """
        source = (
            'class T(unittest.TestCase):\n'
            '    try:\n'
            '        raise ExceptionGroup("e", [ValueError(), TypeError()])\n'
            '    except* ValueError:\n'
            '        def test_a(self):\n            pass\n'
            '    except* TypeError:\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(source), {'T': ['test_a']})

    def test_an_async_with_and_async_for_shadow_is_reported(self):
        """``async with`` and ``async for`` bodies land in the namespace too.

        Neither spelling of ``with`` carries an ``else`` clause, so only the
        body runs; the loop forms keep their ``else`` in the additive
        region. Both were absent from the r28 drill-down (r29, review P2
        from the boundary role).
        """
        async_with = (
            'class T(unittest.TestCase):\n'
            '    async def test_a(self):\n        pass\n'
            '    async with x():\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(async_with), {'T': ['test_a']})
        async_for = (
            'class T(unittest.TestCase):\n'
            '    async def test_a(self):\n        pass\n'
            '    async for i in y():\n'
            '        def test_a(self):\n            pass\n'
        )
        self.assertEqual(_duplicate_test_names(async_for), {'T': ['test_a']})


if __name__ == '__main__':
    unittest.main(verbosity=2)
