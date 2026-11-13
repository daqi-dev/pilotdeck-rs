#!/usr/bin/env python3
"""#273/#942: subprocess --exact reruns must prove the libtest summary, not just exit 0.

Run after committing the candidate. Never bypass negative_process's HEAD guard.
Each case either renames the hardcoded --exact filter (or the call-site test
name that feeds it) so the child binary runs zero tests and still exits 0, or
switches a worker child to libtest's terse format which drops the per-test
"test <name> ... ok" line; every bound parent test must then fail at its
"#273 ... rerun matched no test" assertion. The #942 rows hold the remaining
in-repo rerun sites to the same receipt (bound to their "#942 ..." messages;
the grep helper is bound to its handshake panic because its receipt is the
handshake itself, not a libtest summary). The rss-probe row is macOS-only
(its bound parent is `#[cfg(target_os = "macos")]`-gated) and the two
locale-pin rows need the real TS baseline (PDRS_BASELINE); absent hosts
record those rows SKIPPED rather than KILLED or INVALID — the rss-probe skip
only after observing the premise (fix-r4 sibling#4: anchor count, the bound
parent's own fn, and a tree-locked `cargo test --list` that carries the
target's probe control while selecting no such parent). A run with zero
KILLED rows still exits nonzero: nothing was verified.
"""
import argparse
import difflib
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

# Bound parent test -> cargo target selection for the workspace manifest.
CARGO_TARGETS = {
    'skills::tests::marker_write_reports_the_write_stage_like_node':
        ['-p', 'pilotdeck-cli', '--bin', 'pilotdeck'],
    'skills_migrate::tests::copy_tree_preserves_modes_like_node_async_cp':
        ['-p', 'pilotdeck-cli', '--bin', 'pilotdeck'],
    'runtime::model::stats::dateparse_tests::stats_dateparse_matches_real_ts_in_three_timezones':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'runtime::memory::edgeclaw::storage::overview::lock_tests::scan_lock_topologies_finish':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'ts_entries_and_failure_residue_replay':
        ['-p', 'pilotdeck-gateway', '--test', 'edgeclaw_entries'],
    'ts_databases_and_trees_replay_snapshot_lifecycle':
        ['-p', 'pilotdeck-gateway', '--test', 'edgeclaw_snapshots'],
    'plugin_markdown_alias_dag':
        ['-p', 'pilotdeck-gateway', '--test', 'plugin_traversal'],
    'plugin_markdown_clique_03':
        ['-p', 'pilotdeck-gateway', '--test', 'plugin_traversal'],
    'concurrent_clear_and_writes_match_real_ts':
        ['-p', 'pilotdeck-gateway', '--test', 'edgeclaw_entries'],
    'concurrent_stage_and_swap_matches_real_ts':
        ['-p', 'pilotdeck-gateway', '--test', 'edgeclaw_snapshots'],
    # #942: the remaining subprocess --exact rerun sites.
    'runtime::turn::read::tests::run_events_pathological_depth_last_seq_is_bounded':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'runtime::manager::tests::manager_deep_project_key_returns_range_error_without_aborting':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'runtime::tools::builtin::grep::tests::sigkill_of_the_test_process_leaves_no_fake_rg_orphans':
        ['-p', 'pilotdeck-gateway', '--lib'],
    'dump_request_creates_file':
        ['-p', 'pilotdeck-gateway', '--test', 'dump_request'],
    'dump_request_write_failure_semantics':
        ['-p', 'pilotdeck-gateway', '--test', 'dump_request'],
    'dump_request_write_over_file_size_limit_surfaces_efbig':
        ['-p', 'pilotdeck-gateway', '--test', 'dump_request'],
    'dump_request_empty_tmpdir_never_lands_in_cwd':
        ['-p', 'pilotdeck-gateway', '--test', 'dump_request'],
    'dump_request_not_triggered_by_complete':
        ['-p', 'pilotdeck-gateway', '--test', 'dump_request'],
    'cold_restart_fences_accepted_and_running_without_inventing_unknown_result':
        ['-p', 'pilotdeck-gateway', '--test', 'a52_fixtures'],
    'relative_base_normal_cwd':
        ['-p', 'pilotdeck-gateway', '--test', 'transcript_resolve'],
    'private_ca_matrix':
        ['-p', 'pilotdeck-gateway', '--test', 'private_ca'],
    'replays_ts_proxy_schemes':
        ['-p', 'pilotdeck-gateway', '--test', 'mcp_proxy_schemes'],
    'replays_ts_mcp_http_proxy_scenarios':
        ['-p', 'pilotdeck-gateway', '--test', 'mcp_proxy'],
    'deleted_process_cwd_fails_turn_environment':
        ['-p', 'pilotdeck-gateway', '--test', 'hooks_runtime'],
    'deleted_process_cwd_fails_the_decision':
        ['-p', 'pilotdeck-gateway', '--test', 'permission'],
    'count_transport_uses_environment_proxy':
        ['-p', 'pilotdeck-gateway', '--test', 'count_proxy'],
    'bounded_peak_rss_is_lower_than_full':
        ['-p', 'pilotdeck-gateway', '--test', 'html_budget_probe'],
    'repository_home_fallback_matches_ts_homedir':
        ['-p', 'pilotdeck-gateway', '--test', 'edgeclaw_accessors'],
    # #942 fix-r2: the two locale-pin ambient regressions (r3 review Codex P1)
    # and the FIFO sentinel rename row (r3 review sibling P3#2).
    'real_ts_entries_locale_receipt_survives_single_thread_ambient':
        ['-p', 'pilotdeck-gateway', '--test', 'edgeclaw_entries'],
    'real_ts_snapshots_locale_receipt_survives_single_thread_ambient':
        ['-p', 'pilotdeck-gateway', '--test', 'edgeclaw_snapshots'],
    'fifo_manifests_cannot_block_capture_or_list':
        ['-p', 'pilotdeck-gateway', '--test', 'workspace_snapshot'],
}

# Bound parents that are themselves #[ignore]d: libtest needs --ignored to
# select them at all (their rerun children already carry their own --ignored).
IGNORED_PARENTS = {
    'bounded_peak_rss_is_lower_than_full',
    'real_ts_entries_locale_receipt_survives_single_thread_ambient',
    'real_ts_snapshots_locale_receipt_survives_single_thread_ambient',
}

# `bounded_peak_rss_is_lower_than_full` and its RSS probe child compile only
# on macOS (`#[cfg(target_os = "macos")]` in html_budget_probe.rs): elsewhere
# the bound test does not exist, the positive control can never pass, and the
# row would be INVALID by construction. Default runs on other platforms drop
# the row; an explicit selection there is recorded SKIPPED (neither KILLED nor
# INVALID) and does not fail the run. fix-r4 (sibling#4): the skip is observed
# before it is recorded — see the platform block in main() for the anchor, fn
# and listing checks; a premise the listing disproves is INVALID instead.
DARWIN_ONLY = {'rss-probe-exact-rename'}

# #942 fix-r2 (r3 review Codex P1 + Claude boundary P2#1): the two locale-pin
# rows are bound to ambient regressions that re-run the real_ts regenerate
# tests through the real TS baseline (`PDRS_BASELINE`). Without the baseline
# the positive control can never pass (the regenerate child panics at its
# #280 expect), so hosts without it record the rows SKIPPED — neither KILLED
# nor INVALID — and a default full run does not fail on them.
# #1396: even with PDRS_BASELINE present, the regenerate compares oracle.json
# bytes against the committed file — on Linux the directory traversal order
# differs from macOS so the regenerated oracle.json diverges outside the exact
# masks (baseline.rs:350 panics). The positive control is structurally INVALID
# on Linux; record SKIPPED there same as DARWIN_ONLY (neither KILLED nor
# INVALID), macOS with PDRS_BASELINE still runs them for real coverage.
# Evidence: issue #1396 quotes cb8 (spark-ca13) output — merge-base 43f0db194947
# and HEAD d7e0bc4ac both INVALID with identical sourceSha256, panic at
# baseline.rs:350 "#280 edgeclaw-entries/oracle.json differs from the
# regenerated .../oracle.json: differs from committed bytes outside the exact
# masks". macOS (macmini) same row KILLED — the divergence is platform-specific.
BASELINE_MACOS_ONLY = {'entries-locale-pin', 'snapshots-locale-pin'}
# kept as empty set so existing references compile; all members moved above.
# If a future "needs PDRS_BASELINE" row is added here, first check whether its
# fixture oracle.json contains a directory listing (readdir order) — if it does,
# put it in BASELINE_MACOS_ONLY instead, because Linux traversal order differs
# from macOS and the positive control will be structurally INVALID on Linux
# (see #1396 for the forensics).
BASELINE_ONLY: set[str] = set()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='fresh evidence directory')
    parser.add_argument('mutations', nargs='*')
    # HEAD after the candidate is committed. Before that, a pre-commit full run
    # can pass the SHA printed by "git stash create" (a dangling commit of the
    # current working tree, no ref moved): every mutated byte is still checked
    # against, and restored to, that recorded tree.
    parser.add_argument('--baseline', default='HEAD')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    root = Path(__file__).resolve().parents[5]
    cli_source = root / 'src/pilotdeck-cli/src/skills_migrate.rs'
    skills_source = root / 'src/pilotdeck-cli/src/skills.rs'
    dateparse_source = root / 'src/pilotdeck-gateway/src/runtime/model/stats_dateparse_tests.rs'
    lock_source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/overview_lock_tests.rs'
    entries_source = root / 'src/pilotdeck-gateway/tests/edgeclaw_entries.rs'
    snapshots_source = root / 'src/pilotdeck-gateway/tests/edgeclaw_snapshots.rs'
    traversal_source = root / 'src/pilotdeck-gateway/tests/plugin_traversal.rs'
    read_source = root / 'src/pilotdeck-gateway/src/runtime/turn/read_tests.rs'
    grep_source = root / 'src/pilotdeck-gateway/src/runtime/tools/builtin/grep.rs'
    dump_source = root / 'src/pilotdeck-gateway/tests/dump_request.rs'
    a52_source = root / 'src/pilotdeck-gateway/tests/a52_fixtures.rs'
    transcript_source = root / 'src/pilotdeck-gateway/tests/transcript_resolve.rs'
    private_ca_source = root / 'src/pilotdeck-gateway/tests/private_ca.rs'
    schemes_source = root / 'src/pilotdeck-gateway/tests/mcp_proxy_schemes.rs'
    mcp_proxy_source = root / 'src/pilotdeck-gateway/tests/mcp_proxy.rs'
    hooks_source = root / 'src/pilotdeck-gateway/tests/hooks_runtime.rs'
    permission_source = root / 'src/pilotdeck-gateway/tests/permission.rs'
    count_source = root / 'src/pilotdeck-gateway/tests/count_proxy.rs'
    rss_source = root / 'src/pilotdeck-gateway/tests/html_budget_probe.rs'
    accessors_source = root / 'src/pilotdeck-gateway/tests/edgeclaw_accessors.rs'
    manager_source = root / 'src/pilotdeck-gateway/src/runtime/manager/tests.rs'
    fifo_source = root / 'src/pilotdeck-gateway/tests/workspace_snapshot.rs'
    env = os.environ.copy()
    env['CARGO_PROFILE_DEV_DEBUG'] = 'line-tables-only'
    env['CARGO_PROFILE_TEST_DEBUG'] = 'line-tables-only'
    mutations = [
        ('marker-exact-rename', skills_source,
         '"::marker_child_over_file_size_limit"',
         '"::marker_child_over_file_size_limit_renamed"',
         'skills::tests::marker_write_reports_the_write_stage_like_node',
         '#1145 marker child emitted no WRITE_ERROR line'),
        ('umask-exact-rename', cli_source,
         '"::copy_tree_preserves_modes_like_node_async_cp"',
         '"::copy_tree_preserves_modes_like_node_async_cp_renamed"',
         'skills_migrate::tests::copy_tree_preserves_modes_like_node_async_cp',
         '#273 umask rerun matched no test'),
        ('dateparse-exact-rename', dateparse_source,
         'runtime::model::stats::dateparse_tests::dateparse_zone_child',
         'runtime::model::stats::dateparse_tests::dateparse_zone_child_renamed',
         'runtime::model::stats::dateparse_tests::stats_dateparse_matches_real_ts_in_three_timezones',
         '#273 TZ rerun matched no test'),
        ('lock-exact-rename', lock_source,
         'runtime::memory::edgeclaw::storage::overview::lock_tests::scan_lock_child',
         'runtime::memory::edgeclaw::storage::overview::lock_tests::scan_lock_child_renamed',
         'runtime::memory::edgeclaw::storage::overview::lock_tests::scan_lock_topologies_finish',
         '#273 lock topology rerun matched no test'),
        ('entries-locale-rename', entries_source,
         'locale("ts_entries_and_failure_residue_replay")',
         'locale("ts_entries_and_failure_residue_replay_renamed")',
         'ts_entries_and_failure_residue_replay',
         '#273 entries locale rerun matched no test'),
        ('snapshots-locale-rename', snapshots_source,
         'in_oracle_locale("ts_databases_and_trees_replay_snapshot_lifecycle")',
         'in_oracle_locale("ts_databases_and_trees_replay_snapshot_lifecycle_renamed")',
         'ts_databases_and_trees_replay_snapshot_lifecycle',
         '#273 snapshot locale rerun matched no test'),
        ('traversal-exact-rename', traversal_source,
         '"--exact", "plugin_markdown_alias_dag_child", "--ignored"',
         '"--exact", "plugin_markdown_alias_dag_child_renamed", "--ignored"',
         'plugin_markdown_alias_dag',
         '#273 traversal rerun matched no test: alias DAG'),
        # The clique child shares wait_for_traversal with the DAG child but is
        # spawned by the plugin_markdown_clique_0{3,7} and _1{0,20} parents.
        ('clique-exact-rename', traversal_source,
         '"--exact",\n                "plugin_markdown_clique_child",',
         '"--exact",\n                "plugin_markdown_clique_child_renamed",',
         'plugin_markdown_clique_03',
         '#273 traversal rerun matched no test: markdown clique'),
        # Renaming a worker's --exact filter would only hit the 20 s boot
        # barrier timeout (an unattributable hang), so sever the receipt
        # instead: terse drops the per-test line the summary check requires.
        # #942 fix-r2: anchored to the full worker args block — the file now
        # also pins --test-threads=2 in its locale spawn, so the bare
        # trailing-comma --nocapture string is no longer unique.
        ('entries-worker-terse', entries_source,
         '                .args([\n'
         '                    "--exact",\n'
         '                    "concurrent_clear_and_writes_match_real_ts",\n'
         '                    "--nocapture",\n'
         '                ])',
         '                .args([\n'
         '                    "--exact",\n'
         '                    "concurrent_clear_and_writes_match_real_ts",\n'
         '                    "--format=terse",\n'
         '                ])',
         'concurrent_clear_and_writes_match_real_ts',
         '#273 entries worker rerun matched no test'),
        ('snapshots-worker-terse', snapshots_source,
         '                .args([\n'
         '                    "--exact",\n'
         '                    "concurrent_stage_and_swap_matches_real_ts",\n'
         '                    "--nocapture",\n'
         '                ])',
         '                .args([\n'
         '                    "--exact",\n'
         '                    "concurrent_stage_and_swap_matches_real_ts",\n'
         '                    "--format=terse",\n'
         '                ])',
         'concurrent_stage_and_swap_matches_real_ts',
         '#273 snapshot worker rerun matched no test'),
        # ---- #942: the remaining subprocess --exact rerun sites. ----
        ('lastseq-depth-exact-rename', read_source,
         'const CHILD: &str = "runtime::turn::read::tests::run_events_pathological_depth_last_seq_child";',
         'const CHILD: &str = "runtime::turn::read::tests::run_events_pathological_depth_last_seq_child_renamed";',
         'runtime::turn::read::tests::run_events_pathological_depth_last_seq_is_bounded',
         '#942 lastseq-depth rerun matched no test'),
        # The grep helper is SIGKILLed by design, so its "receipt" is the
        # handshake panic, not a libtest summary of the helper itself.
        ('grep-helper-exact-rename', grep_source,
         '"runtime::tools::builtin::grep::tests::orphaned_fake_rg_helper",',
         '"runtime::tools::builtin::grep::tests::orphaned_fake_rg_helper_renamed",',
         'runtime::tools::builtin::grep::tests::sigkill_of_the_test_process_leaves_no_fake_rg_orphans',
         'helper exited before the fake rg handshake'),
        # run_case is shared by creates_file and disabled_by_default; both
        # fail at the same receipt assertion, bind the enabled-gate parent.
        ('dump-enabled-gate-exact-rename', dump_source,
         '        .args([\n'
         '            "--exact",\n'
         '            "dump_child",\n'
         '            "--ignored",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])\n'
         '        .env("PDRS_DUMP_PROTOCOL", protocol)',
         '        .args([\n'
         '            "--exact",\n'
         '            "dump_child_renamed",\n'
         '            "--ignored",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])\n'
         '        .env("PDRS_DUMP_PROTOCOL", protocol)',
         'dump_request_creates_file',
         '#942 dump rerun (enabled gate) matched no test'),
        ('dump-write-fail-exact-rename', dump_source,
         '                .args([\n'
         '                    "--exact",\n'
         '                    "dump_child",\n'
         '                    "--ignored",\n'
         '                    "--nocapture",\n'
         '                    "--test-threads=2",\n'
         '                ])\n'
         '                .env("PDRS_DUMP_PROTOCOL", protocol)',
         '                .args([\n'
         '                    "--exact",\n'
         '                    "dump_child_renamed",\n'
         '                    "--ignored",\n'
         '                    "--nocapture",\n'
         '                    "--test-threads=2",\n'
         '                ])\n'
         '                .env("PDRS_DUMP_PROTOCOL", protocol)',
         'dump_request_write_failure_semantics',
         '#942 dump rerun (write-fail) matched no test'),
        ('dump-xfsz-exact-rename', dump_source,
         '--exact dump_child --ignored --nocapture --test-threads=2',
         '--exact dump_child_renamed --ignored --nocapture --test-threads=2',
         'dump_request_write_over_file_size_limit_surfaces_efbig',
         '#942 dump rerun (xfsz) matched no test'),
        ('dump-empty-tmpdir-exact-rename', dump_source,
         '        .args([\n'
         '            "--exact",\n'
         '            "dump_child",\n'
         '            "--ignored",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])\n'
         '        .env("PDRS_DUMP_PROTOCOL", "openai")\n'
         '        .env("PDRS_DUMP_MARKER", &marker)\n'
         '        .env("PDRS_DUMP_MODE", "ok")\n'
         '        .env("PILOTDECK_DUMP_REQUEST", "1")\n'
         '        .env("TMPDIR", "")',
         '        .args([\n'
         '            "--exact",\n'
         '            "dump_child_renamed",\n'
         '            "--ignored",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])\n'
         '        .env("PDRS_DUMP_PROTOCOL", "openai")\n'
         '        .env("PDRS_DUMP_MARKER", &marker)\n'
         '        .env("PDRS_DUMP_MODE", "ok")\n'
         '        .env("PILOTDECK_DUMP_REQUEST", "1")\n'
         '        .env("TMPDIR", "")',
         'dump_request_empty_tmpdir_never_lands_in_cwd',
         '#942 dump rerun (empty-tmpdir) matched no test'),
        ('dump-complete-exact-rename', dump_source,
         '        .args([\n'
         '            "--exact",\n'
         '            "dump_child",\n'
         '            "--ignored",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])\n'
         '        .env("PDRS_DUMP_PROTOCOL", "openai")\n'
         '        .env("PDRS_DUMP_MARKER", &marker)\n'
         '        .env("PDRS_DUMP_MODE", "complete")',
         '        .args([\n'
         '            "--exact",\n'
         '            "dump_child_renamed",\n'
         '            "--ignored",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])\n'
         '        .env("PDRS_DUMP_PROTOCOL", "openai")\n'
         '        .env("PDRS_DUMP_MARKER", &marker)\n'
         '        .env("PDRS_DUMP_MODE", "complete")',
         'dump_request_not_triggered_by_complete',
         '#942 dump rerun (complete) matched no test'),
        ('cold-restart-exact-rename', a52_source,
         '                .args([\n'
         '                    "--exact",\n'
         '                    "cold_restart_child",\n'
         '                    "--nocapture",\n'
         '                    "--test-threads=2",\n'
         '                ])',
         '                .args([\n'
         '                    "--exact",\n'
         '                    "cold_restart_child_renamed",\n'
         '                    "--nocapture",\n'
         '                    "--test-threads=2",\n'
         '                ])',
         'cold_restart_fences_accepted_and_running_without_inventing_unknown_result',
         '#942 cold-restart rerun matched no test'),
        ('transcript-resolve-exact-rename', transcript_source,
         'matrix("relative-normal", "relative_base_normal_cwd");',
         'matrix("relative-normal", "relative_base_normal_cwd_renamed");',
         'relative_base_normal_cwd',
         '#942 transcript-resolve rerun matched no test'),
        ('private-ca-exact-rename', private_ca_source,
         '            .args([\n'
         '                "--exact",\n'
         '                "private_ca_matrix",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         '            .args([\n'
         '                "--exact",\n'
         '                "private_ca_matrix_renamed",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         'private_ca_matrix',
         '#942 private-ca rerun matched no test'),
        ('proxy-schemes-exact-rename', schemes_source,
         '            .args([\n'
         '                "--exact",\n'
         '                "replays_ts_proxy_schemes",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         '            .args([\n'
         '                "--exact",\n'
         '                "replays_ts_proxy_schemes_renamed",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         'replays_ts_proxy_schemes',
         '#942 proxy-schemes rerun matched no test'),
        ('mcp-proxy-exact-rename', mcp_proxy_source,
         '            .args([\n'
         '                "--exact",\n'
         '                "replays_ts_mcp_http_proxy_scenarios",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         '            .args([\n'
         '                "--exact",\n'
         '                "replays_ts_mcp_http_proxy_scenarios_renamed",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         'replays_ts_mcp_http_proxy_scenarios',
         '#942 mcp-proxy rerun matched no test'),
        ('deleted-cwd-turn-exact-rename', hooks_source,
         '            .args([\n'
         '                "--exact",\n'
         '                "deleted_process_cwd_fails_turn_environment",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         '            .args([\n'
         '                "--exact",\n'
         '                "deleted_process_cwd_fails_turn_environment_renamed",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         'deleted_process_cwd_fails_turn_environment',
         '#942 deleted-cwd turn-environment rerun matched no test'),
        ('deleted-cwd-decision-exact-rename', permission_source,
         '            .args([\n'
         '                "--exact",\n'
         '                "deleted_process_cwd_fails_the_decision",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         '            .args([\n'
         '                "--exact",\n'
         '                "deleted_process_cwd_fails_the_decision_renamed",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         'deleted_process_cwd_fails_the_decision',
         '#942 deleted-cwd decision rerun matched no test'),
        ('count-proxy-exact-rename', count_source,
         '        .args([\n'
         '            "--exact",\n'
         '            "count_transport_uses_environment_proxy",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])',
         '        .args([\n'
         '            "--exact",\n'
         '            "count_transport_uses_environment_proxy_renamed",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])',
         'count_transport_uses_environment_proxy',
         '#942 count-proxy rerun matched no test'),
        ('rss-probe-exact-rename', rss_source,
         '            .args([\n'
         '                "--ignored",\n'
         '                "--exact",\n'
         '                "probe",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         '            .args([\n'
         '                "--ignored",\n'
         '                "--exact",\n'
         '                "probe_renamed",\n'
         '                "--nocapture",\n'
         '                "--test-threads=2",\n'
         '            ])',
         'bounded_peak_rss_is_lower_than_full',
         '#942 rss-probe rerun matched no test'),
        # #942 fix-r1 (review P2): the two homedir rerun sites share one
        # spawn helper, so one rename kills the runnable parent; the other
        # caller needs a TS baseline and cannot be a positive control here.
        ('homedir-exact-rename', accessors_source,
         '        .args([\n'
         '            "--exact",\n'
         '            "home_fallback_child",\n'
         '            "--ignored",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])\n'
         '        .env("PDRS_HOME_FALLBACK_CHILD", mode)',
         '        .args([\n'
         '            "--exact",\n'
         '            "home_fallback_child_renamed",\n'
         '            "--ignored",\n'
         '            "--nocapture",\n'
         '            "--test-threads=2",\n'
         '        ])\n'
         '        .env("PDRS_HOME_FALLBACK_CHILD", mode)',
         'repository_home_fallback_matches_ts_homedir',
         '#942 homedir rerun matched no test'),
        # #942 fix-r1 (review P2): #934's deep projectKey child had only the
        # DEEP= marker; now bound to the same receipt as the other sites.
        ('manager-deep-exact-rename', manager_source,
         '    const CHILD: &str =\n'
         '        "runtime::manager::tests::manager_deep_project_key_returns_range_error_without_aborting";',
         '    const CHILD: &str =\n'
         '        "runtime::manager::tests::manager_deep_project_key_returns_range_error_without_aborting_renamed";',
         'runtime::manager::tests::manager_deep_project_key_returns_range_error_without_aborting',
         '#942 manager deep rerun matched no test'),
        # #942 fix-r2 (r3 review Codex P1): dropping the locale pin puts the
        # regenerate child's indirect println (common/baseline.rs) back into
        # the receipt line under an ambient RUST_TEST_THREADS=1 — exactly
        # what the bound ambient regressions re-run.
        ('entries-locale-pin', entries_source,
         '        .args([\n'
         '            "--exact",\n'
         '            test,\n'
         '            "--include-ignored",\n'
         '            "--test-threads=2",\n'
         '            "--nocapture",\n'
         '        ])',
         '        .args([\n'
         '            "--exact",\n'
         '            test,\n'
         '            "--include-ignored",\n'
         '            "--nocapture",\n'
         '        ])',
         'real_ts_entries_locale_receipt_survives_single_thread_ambient',
         '#942 entries locale receipt broke under ambient single-thread libtest'),
        ('snapshots-locale-pin', snapshots_source,
         '        .args([\n'
         '            "--exact",\n'
         '            test,\n'
         '            "--include-ignored",\n'
         '            "--test-threads=2",\n'
         '            "--nocapture",\n'
         '        ])',
         '        .args([\n'
         '            "--exact",\n'
         '            test,\n'
         '            "--include-ignored",\n'
         '            "--nocapture",\n'
         '        ])',
         'real_ts_snapshots_locale_receipt_survives_single_thread_ambient',
         '#942 snapshots locale receipt broke under ambient single-thread libtest'),
        # #942 fix-r2 (r3 review sibling P3#2): the FIFO site's receipt is the
        # sentinel contains, not a libtest summary — renaming the child
        # filter makes libtest run zero tests, exit 0, and emit no sentinels.
        ('fifo-child-exact-rename', fifo_source,
         '"fifo_manifests_child_round",',
         '"fifo_manifests_child_round_renamed",',
         'fifo_manifests_cannot_block_capture_or_list',
         '#293 fix-r5 the capture over the writer-less FIFO manifest recaptured and committed'),
    ]
    if args.mutations:
        selected = set(args.mutations)
        if not selected <= {row[0] for row in mutations}:
            parser.error('unknown mutation name')
        mutations = [row for row in mutations if row[0] in selected]
    args.output.mkdir(parents=True, exist_ok=False)

    def save(name, text):
        with (args.output / name).open('x') as stream:
            stream.write(text)

    summary = []
    if sys.platform != 'darwin':
        # DARWIN_ONLY rows cannot run here (the bound parent does not compile
        # on this platform): record SKIPPED — neither KILLED nor INVALID, so a
        # default full run on Linux does not fail on a structurally absent
        # control. Default runs record the drop for the evidence directory.
        # #1396: BASELINE_MACOS_ONLY rows also require macOS — their positive
        # control regenerates oracle.json and compares bytes, but Linux
        # directory traversal order differs from macOS so the comparison fails
        # outside the exact masks regardless of PDRS_BASELINE presence.
        # fix-r4 (sibling#4): the DARWIN_ONLY skip premise is observed, not
        # assumed — the same shape audit-rerun-boundary got for its Linux-only
        # rows. The anchor count and the bound parent's own `fn <leaf>(` must
        # still hold (a renamed parent keeps the mutation anchor and would
        # otherwise skip silently forever), and a `cargo test --list` under
        # the same `mutating_sources` tree lock must carry the target's
        # known-to-exist control test (`probe` compiles on every host) while
        # not selecting the macOS-only parent. Deliberately NOT applied to
        # cron-store/native-management-style platform rows: their bound tests
        # skip *at runtime* (the test exists and the listing selects it on
        # every host), so "the listing selects nothing" can never be their
        # premise; their guard is the full-row `--check` anchor instead.
        non_darwin = DARWIN_ONLY | BASELINE_MACOS_ONLY
        requested = set(args.mutations) if args.mutations else non_darwin
        for name in sorted(requested & DARWIN_ONLY):
            # A DARWIN_ONLY name the table no longer carries is set/table
            # drift, not a skip: record the row INVALID instead of letting
            # next() abort the whole sweep on a StopIteration (fix-r5,
            # review P2a — every row must be accounted for before summary).
            row = next((r for r in mutations if r[0] == name), None)
            if row is None:
                entry = {'name': name, 'status': 'INVALID',
                         'reason': 'DARWIN_ONLY names a row the mutations '
                                   'table does not carry'}
                summary.append(entry)
                save(f'{name}.json', json.dumps(entry, indent=2) + '\n')
                print(f'{name}: {entry["status"]}: {entry["reason"]}', flush=True)
                continue
            darwin_source, old, test = row[1], row[2], row[4]
            with mutating_sources(root, [darwin_source], baseline=args.baseline) as snapshots:
                content = snapshots[darwin_source].decode()
                # A definition shape, not a substring: a deleted parent whose
                # name survives in a comment or a string literal would
                # otherwise still count as defined (fix-r5, review P2a).
                defined = re.search(
                    rf'^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn '
                    rf'{re.escape(test)}\s*\(', content, re.M)
                count = content.count(old)
                if count != 1:
                    entry = {'name': name, 'status': 'INVALID',
                             'reason': f'anchor count {count}'}
                elif not defined:
                    entry = {'name': name, 'status': 'INVALID',
                             'reason': f'bound parent {test} is not defined in the '
                                       'source; renamed or deleted — the row cannot be '
                                       'skipped on a rename'}
                else:
                    slots = wait_for_slot(root / 'scripts/cargo-slots.sh',
                                         timeout=600, interval=10)
                    save(f'{name}-list.slots.log', slots.stdout + slots.stderr)
                    try:
                        result = run_owned([
                            'cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                            *CARGO_TARGETS[test], '--', '--list',
                        ], cwd=root, env=env, timeout=900)
                        log = result.stdout + result.stderr
                        save(f'{name}-list.log', log)
                        save(f'{name}-list.exit', f'{result.returncode}\n')
                    except subprocess.TimeoutExpired as error:
                        result, log = None, f'--list timed out after {error.timeout}s'
                    if result is None or result.returncode != 0 or 'could not compile' in log:
                        entry = {'name': name, 'status': 'INVALID',
                                 'reason': f'cannot observe the skip premise: '
                                           f'{log.splitlines()[-1] if log else "no listing"}'}
                    else:
                        # Whole-line compares: a test whose name merely ends in
                        # the bound parent's would satisfy a substring match.
                        listed_lines = result.stdout.split('\n')
                        if 'probe: test' not in listed_lines:
                            entry = {'name': name, 'status': 'INVALID',
                                     'reason': 'cannot observe the skip premise: the listing '
                                               'carried no known test (probe not listed)'}
                        elif f'{test}: test' in listed_lines:
                            entry = {'name': name, 'status': 'INVALID',
                                     'reason': 'skip premise broken: the listing selects the '
                                               f'bound parent {test} on {sys.platform}; fix '
                                               'the cfg or DARWIN_ONLY'}
                        else:
                            entry = {'name': name, 'status': 'SKIPPED',
                                     'reason': 'bound parent compiles only on macOS '
                                               '(#[cfg(target_os = "macos")]) — verified: '
                                               '--list carries the probe control and selects '
                                               'no such test, and fn is still defined in the '
                                               'source'}
            summary.append(entry)
            save(f'{name}.json', json.dumps(entry, indent=2) + '\n')
            print(f'{name}: {entry["status"]}: {entry["reason"]}', flush=True)
        for name in sorted(requested & BASELINE_MACOS_ONLY):
            summary.append({'name': name, 'status': 'SKIPPED',
                           'reason': 'positive control regenerates oracle.json '
                                     'whose byte order is macOS-specific '
                                     '(#1396: Linux dir traversal differs)'})
            print(f'{name}: SKIPPED: oracle.json byte order is macOS-specific', flush=True)
        mutations = [row for row in mutations if row[0] not in non_darwin]
    if not os.environ.get('PDRS_BASELINE'):
        # BASELINE_ONLY rows cannot run here (the bound parent re-runs a real
        # TS regenerate): record SKIPPED, same accounting as the platform rows.
        # (BASELINE_MACOS_ONLY members are already dropped above on non-macOS;
        # on macOS they still need PDRS_BASELINE and are handled here.)
        needs_baseline = BASELINE_ONLY | (BASELINE_MACOS_ONLY if sys.platform == 'darwin' else set())
        requested = set(args.mutations) if args.mutations else needs_baseline
        for name in sorted(requested & needs_baseline):
            summary.append({'name': name, 'status': 'SKIPPED',
                            'reason': 'bound parent needs the real TS baseline '
                                      '(PDRS_BASELINE) for its positive control'})
            print(f'{name}: SKIPPED: needs PDRS_BASELINE', flush=True)
        mutations = [row for row in mutations if row[0] not in needs_baseline]
    # (args.output.mkdir and save moved above the platform-skip blocks: the
    # DARWIN_ONLY premise check leaves its own evidence files before any row
    # runs — fix-r4 sibling#4.)

    def run(label, test, compile_only=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=10)
        save(f'{label}.slots.log', slots.stdout + slots.stderr)
        # No -q: cargo forwards it to libtest as --quiet, which replaces the
        # per-test "test <name> ... ok/FAILED" lines this script's pass/kill
        # criteria are written against (a quiet pass is a lone "." and a quiet
        # failure is "<name> --- FAILED").
        # No `--nocapture` (#1053): capture keeps a printing parent's stdout
        # (the dateparse rows echo their child's stdout) out of the
        # "test <name> ... ok/FAILED" status line that passed()/classify()
        # match exactly (#938 r14).
        # --test-threads=2 (#942 fix-r1): single-threaded libtest prints the
        # test name before the body runs, so a printing parent (the dateparse
        # rows echo their child's stdout) would splice its output into the
        # "test <name> ... ok" line under an ambient RUST_TEST_THREADS=1 and
        # fail the positive control; the explicit flag pins the layout.
        result = run_owned([
            'cargo', 'test', '--manifest-path', 'src/Cargo.toml', *CARGO_TARGETS[test],
            *(['--no-run'] if compile_only else [
                '--', '--exact', test, '--test-threads=2',
                *(['--ignored'] if test in IGNORED_PARENTS else []),
            ]),
        ], cwd=root, env=env, timeout=900)
        log = result.stdout + result.stderr
        save(f'{label}.log', log)
        save(f'{label}.exit', f'{result.returncode}\n')
        return result, log

    def passed(result, test, log):
        return (result.returncode == 0 and f'test {test} ... ok' in log
                and ' 1 passed; 0 failed' in log)

    def classify(result, test, assertion, log):
        if passed(result, test, log):
            return 'SURVIVED', 'bound test still passed'
        if (result.returncode != 0 and f'test {test} ... FAILED' in log
                and assertion in log and 'could not compile' not in log):
            return 'KILLED', 'bound test failed at its #273 assertion'
        return 'INVALID', 'no attributable runtime failure (build error, wrong panic, or missing test)'

    for name, source, old, new, test, assertion in mutations:
        with mutating_sources(root, [source], baseline=args.baseline) as snapshots:
            original = snapshots[source]
            content = original.decode()
            if content.count(old) != 1:
                save(f'{name}.json', json.dumps(
                    {'name': name, 'status': 'INVALID',
                     'reason': f'anchor count {content.count(old)}'}, indent=2) + '\n')
                summary.append({'name': name, 'status': 'INVALID',
                                'reason': f'anchor count {content.count(old)}'})
                continue
            result, log = run(f'{name}-positive', test)
            positive = passed(result, test, log)
            changed = content.replace(old, new, 1)
            save(f'{name}.patch', ''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True), fromfile=str(source), tofile=str(source))))
            source.write_text(changed)
            build, build_log = run(f'{name}-compile', test, compile_only=True)
            compiled = build.returncode == 0 and 'could not compile' not in build_log
            if not compiled:
                status, reason = 'INVALID', 'mutant failed no-run compilation; not a kill'
            else:
                result, log = run(f'{name}-negative', test)
                if positive:
                    status, reason = classify(result, test, assertion, log)
                else:
                    status, reason = 'INVALID', 'positive control did not pass the bound test'
        # Restoration and byte-level SHA validation belong to mutating_sources's
        # finally, which runs under the tree lock — re-checking here would race
        # another runner's mutation of the same file (fix-r1 review).
        with mutating_sources(root, [source], baseline=args.baseline):
            result, restored = run(f'{name}-restored', test)
            if positive and not passed(result, test, restored):
                status, reason = 'INVALID', reason + '; restored bound test did not pass'
        summary.append({'name': name, 'status': status, 'reason': reason,
                        'compiled': compiled, 'sourceSha256': hashlib.sha256(original).hexdigest()})
        save(f'{name}.json', json.dumps(summary[-1], indent=2) + '\n')
        print(f'{name}: {status}: {reason}', flush=True)
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'SKIPPED')}
    save('summary.json', json.dumps({'controls': summary, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    # Every selected case must be accounted for and killed; never stop at the
    # first failure, and never exit 0 while any case survived or was invalid.
    # SKIPPED (platform- or baseline-absent, explicitly recorded above) is
    # not a failure, but a run that killed nothing proved nothing: with zero
    # KILLED rows the exit must stay nonzero so a runner consuming the code
    # cannot read an all-SKIPPED selection as verified (r3 review boundary
    # P2#1).
    return int(not summary
               or counts['KILLED'] == 0
               or any(row['status'] not in ('KILLED', 'SKIPPED')
                      for row in summary))


if __name__ == '__main__':
    sys.exit(main())
