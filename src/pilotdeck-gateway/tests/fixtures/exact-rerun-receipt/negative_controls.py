#!/usr/bin/env python3
"""#273: subprocess --exact reruns must prove the libtest summary, not just exit 0.

Run after committing the candidate. Never bypass negative_process's HEAD guard.
Each case renames the hardcoded --exact filter (or the call-site test name that
feeds it) so the child binary runs zero tests and still exits 0; every bound
parent test must then fail at its "#273 ... rerun matched no test" assertion.
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
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='fresh evidence directory')
    parser.add_argument('mutations', nargs='*')
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
    env = os.environ.copy()
    env['CARGO_PROFILE_DEV_DEBUG'] = 'line-tables-only'
    env['CARGO_PROFILE_TEST_DEBUG'] = 'line-tables-only'
    mutations = [
        ('umask-exact-rename', cli_source,
         '"skills_migrate::tests::copy_tree_preserves_modes_like_node_async_cp",',
         '"skills_migrate::tests::copy_tree_preserves_modes_like_node_async_cp_renamed",',
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
         '"plugin_markdown_alias_dag_child",',
         '"plugin_markdown_alias_dag_child_renamed",',
         'plugin_markdown_alias_dag',
         '#273 traversal rerun matched no test'),
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
        with mutating_sources(root, [source]) as snapshots:
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
        with mutating_sources(root, [source]):
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
