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
handshake itself, not a libtest summary).
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

# Bound parent test -> cargo target selection for the workspace manifest.
CARGO_TARGETS = {
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
}

# Bound parents that are themselves #[ignore]d: libtest needs --ignored to
# select them at all (their rerun children already carry their own --ignored).
IGNORED_PARENTS = {'bounded_peak_rss_is_lower_than_full'}


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
    env = os.environ.copy()
    env['CARGO_PROFILE_DEV_DEBUG'] = 'line-tables-only'
    env['CARGO_PROFILE_TEST_DEBUG'] = 'line-tables-only'
    mutations = [
        ('umask-exact-rename', cli_source,
         '"skills_migrate::tests::copy_tree_preserves_modes_like_node_async_cp"',
         '"skills_migrate::tests::copy_tree_preserves_modes_like_node_async_cp_renamed"',
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
        ('entries-worker-terse', entries_source,
         '"--nocapture",',
         '"--format=terse",',
         'concurrent_clear_and_writes_match_real_ts',
         '#273 entries worker rerun matched no test'),
        ('snapshots-worker-terse', snapshots_source,
         '"--nocapture",',
         '"--format=terse",',
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
         '        .args(["--exact", "dump_child", "--ignored", "--nocapture"])\n'
         '        .env("PDRS_DUMP_PROTOCOL", protocol)',
         '        .args(["--exact", "dump_child_renamed", "--ignored", "--nocapture"])\n'
         '        .env("PDRS_DUMP_PROTOCOL", protocol)',
         'dump_request_creates_file',
         '#942 dump rerun (enabled gate) matched no test'),
        ('dump-write-fail-exact-rename', dump_source,
         '                .args(["--exact", "dump_child", "--ignored", "--nocapture"])\n'
         '                .env("PDRS_DUMP_PROTOCOL", protocol)',
         '                .args(["--exact", "dump_child_renamed", "--ignored", "--nocapture"])\n'
         '                .env("PDRS_DUMP_PROTOCOL", protocol)',
         'dump_request_write_failure_semantics',
         '#942 dump rerun (write-fail) matched no test'),
        ('dump-xfsz-exact-rename', dump_source,
         '--exact dump_child --ignored --nocapture',
         '--exact dump_child_renamed --ignored --nocapture',
         'dump_request_write_over_file_size_limit_surfaces_efbig',
         '#942 dump rerun (xfsz) matched no test'),
        ('dump-empty-tmpdir-exact-rename', dump_source,
         '        .args(["--exact", "dump_child", "--ignored", "--nocapture"])\n'
         '        .env("PDRS_DUMP_PROTOCOL", "openai")\n'
         '        .env("PDRS_DUMP_MARKER", &marker)\n'
         '        .env("PDRS_DUMP_MODE", "ok")\n'
         '        .env("PILOTDECK_DUMP_REQUEST", "1")\n'
         '        .env("TMPDIR", "")',
         '        .args(["--exact", "dump_child_renamed", "--ignored", "--nocapture"])\n'
         '        .env("PDRS_DUMP_PROTOCOL", "openai")\n'
         '        .env("PDRS_DUMP_MARKER", &marker)\n'
         '        .env("PDRS_DUMP_MODE", "ok")\n'
         '        .env("PILOTDECK_DUMP_REQUEST", "1")\n'
         '        .env("TMPDIR", "")',
         'dump_request_empty_tmpdir_never_lands_in_cwd',
         '#942 dump rerun (empty-tmpdir) matched no test'),
        ('dump-complete-exact-rename', dump_source,
         '        .args(["--exact", "dump_child", "--ignored", "--nocapture"])\n'
         '        .env("PDRS_DUMP_PROTOCOL", "openai")\n'
         '        .env("PDRS_DUMP_MARKER", &marker)\n'
         '        .env("PDRS_DUMP_MODE", "complete")',
         '        .args(["--exact", "dump_child_renamed", "--ignored", "--nocapture"])\n'
         '        .env("PDRS_DUMP_PROTOCOL", "openai")\n'
         '        .env("PDRS_DUMP_MARKER", &marker)\n'
         '        .env("PDRS_DUMP_MODE", "complete")',
         'dump_request_not_triggered_by_complete',
         '#942 dump rerun (complete) matched no test'),
        ('cold-restart-exact-rename', a52_source,
         '.args(["--exact", "cold_restart_child", "--nocapture"])',
         '.args(["--exact", "cold_restart_child_renamed", "--nocapture"])',
         'cold_restart_fences_accepted_and_running_without_inventing_unknown_result',
         '#942 cold-restart rerun matched no test'),
        ('transcript-resolve-exact-rename', transcript_source,
         'matrix("relative-normal", "relative_base_normal_cwd");',
         'matrix("relative-normal", "relative_base_normal_cwd_renamed");',
         'relative_base_normal_cwd',
         '#942 transcript-resolve rerun matched no test'),
        ('private-ca-exact-rename', private_ca_source,
         '.args(["--exact", "private_ca_matrix", "--nocapture"])',
         '.args(["--exact", "private_ca_matrix_renamed", "--nocapture"])',
         'private_ca_matrix',
         '#942 private-ca rerun matched no test'),
        ('proxy-schemes-exact-rename', schemes_source,
         '.args(["--exact", "replays_ts_proxy_schemes", "--nocapture"])',
         '.args(["--exact", "replays_ts_proxy_schemes_renamed", "--nocapture"])',
         'replays_ts_proxy_schemes',
         '#942 proxy-schemes rerun matched no test'),
        ('mcp-proxy-exact-rename', mcp_proxy_source,
         '            .args([\n'
         '                "--exact",\n'
         '                "replays_ts_mcp_http_proxy_scenarios",\n'
         '                "--nocapture",\n'
         '            ])',
         '            .args([\n'
         '                "--exact",\n'
         '                "replays_ts_mcp_http_proxy_scenarios_renamed",\n'
         '                "--nocapture",\n'
         '            ])',
         'replays_ts_mcp_http_proxy_scenarios',
         '#942 mcp-proxy rerun matched no test'),
        ('deleted-cwd-turn-exact-rename', hooks_source,
         '            .args([\n'
         '                "--exact",\n'
         '                "deleted_process_cwd_fails_turn_environment",\n'
         '                "--nocapture",\n'
         '            ])',
         '            .args([\n'
         '                "--exact",\n'
         '                "deleted_process_cwd_fails_turn_environment_renamed",\n'
         '                "--nocapture",\n'
         '            ])',
         'deleted_process_cwd_fails_turn_environment',
         '#942 deleted-cwd turn-environment rerun matched no test'),
        ('deleted-cwd-decision-exact-rename', permission_source,
         '            .args([\n'
         '                "--exact",\n'
         '                "deleted_process_cwd_fails_the_decision",\n'
         '                "--nocapture",\n'
         '            ])',
         '            .args([\n'
         '                "--exact",\n'
         '                "deleted_process_cwd_fails_the_decision_renamed",\n'
         '                "--nocapture",\n'
         '            ])',
         'deleted_process_cwd_fails_the_decision',
         '#942 deleted-cwd decision rerun matched no test'),
        ('count-proxy-exact-rename', count_source,
         '        .args([\n'
         '            "--exact",\n'
         '            "count_transport_uses_environment_proxy",\n'
         '            "--nocapture",\n'
         '        ])',
         '        .args([\n'
         '            "--exact",\n'
         '            "count_transport_uses_environment_proxy_renamed",\n'
         '            "--nocapture",\n'
         '        ])',
         'count_transport_uses_environment_proxy',
         '#942 count-proxy rerun matched no test'),
        ('rss-probe-exact-rename', rss_source,
         '.args(["--ignored", "--exact", "probe", "--nocapture"])',
         '.args(["--ignored", "--exact", "probe_renamed", "--nocapture"])',
         'bounded_peak_rss_is_lower_than_full',
         '#942 rss-probe rerun matched no test'),
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

    def run(label, test):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=10)
        save(f'{label}.slots.log', slots.stdout + slots.stderr)
        # No -q: cargo forwards it to libtest as --quiet, which replaces the
        # per-test "test <name> ... ok/FAILED" lines this script's pass/kill
        # criteria are written against (a quiet pass is a lone "." and a quiet
        # failure is "<name> --- FAILED").
        result = run_owned([
            'cargo', 'test', '--manifest-path', 'src/Cargo.toml', *CARGO_TARGETS[test],
            '--', '--exact', test, '--nocapture',
            *(['--ignored'] if test in IGNORED_PARENTS else []),
        ], cwd=root, env=env, timeout=900)
        log = result.stdout + result.stderr
        save(f'{label}.log', log)
        save(f'{label}.exit', f'{result.returncode}\n')
        return result, log

    def passed(result, test, log):
        return (result.returncode == 0 and f'test {test} ... ok' in log
                and '1 passed; 0 failed' in log)

    def classify(result, test, assertion, log):
        if passed(result, test, log):
            return 'SURVIVED', 'bound test still passed'
        if (result.returncode != 0 and f'test {test} ... FAILED' in log
                and assertion in log and 'could not compile' not in log):
            return 'KILLED', 'bound test failed at its #273 assertion'
        return 'INVALID', 'no attributable runtime failure (build error, wrong panic, or missing test)'

    summary = []
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
            result, log = run(f'{name}-negative', test)
            if positive:
                status, reason = classify(result, test, assertion, log)
            else:
                status, reason = 'INVALID', 'positive control did not pass the bound test'
        # Restoration and SHA validation belong to mutating_sources, including on failure.
        assert source.read_bytes() == original, f'{name}: source not restored'
        with mutating_sources(root, [source], baseline=args.baseline):
            result, restored = run(f'{name}-restored', test)
            if positive and not passed(result, test, restored):
                status, reason = 'INVALID', reason + '; restored bound test did not pass'
        summary.append({'name': name, 'status': status, 'reason': reason,
                        'sourceSha256': hashlib.sha256(original).hexdigest()})
        save(f'{name}.json', json.dumps(summary[-1], indent=2) + '\n')
        print(f'{name}: {status}: {reason}', flush=True)
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    save('summary.json', json.dumps({'controls': summary, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    # Every selected case must be accounted for and killed; never stop at the
    # first failure, and never exit 0 while any case survived or was invalid.
    return int(not summary or any(row['status'] != 'KILLED' for row in summary))


if __name__ == '__main__':
    sys.exit(main())
