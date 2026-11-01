#!/usr/bin/env python3
"""#1216 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.

Rows bind one mutation to one named cell replay in
`runtime::cron::tests` (a lib test, run with `--exact`); the kill evidence is
that test's own panic payload carrying its `#1216` message.
"""
from pathlib import Path
import json
import os
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/cron/store.rs'
migration = root / 'src/pilotdeck-gateway/src/runtime/cron/migration.rs'
tests = root / 'src/pilotdeck-gateway/src/runtime/cron/tests.rs'
TREE = 'once: replayed file tree differs from the TS oracle'
mutations = [
    # Migration routing removed: projectKey-routed tasks stay in their source
    # directory, so the recorded move never happens (issue scope 3, 迁移跳过).
    ('migration-routing-removed', migration,
     '    let target_dir = resolve_cron_paths(&home_string(input), &project_key)?.project_dir;\n'
     '    Ok(if blocked_task_dirs.contains(&target_dir) {\n'
     '        source_dir.to_path_buf()\n'
     '    } else {\n'
     '        target_dir\n'
     '    })',
     '    let target_dir = resolve_cron_paths(&home_string(input), &project_key)?.project_dir;\n'
     '    Ok(if blocked_task_dirs.contains(&target_dir) {\n'
     '        source_dir.to_path_buf()\n'
     '    } else {\n'
     '        source_dir.to_path_buf()\n'
     '    })',
     'runtime::cron::tests::migration_tasks_move_to_projectkey_dir_matches_ts',
     f'#1216 migration tasks-move-to-projectkey-dir {TREE}'),
    # Corrupt tasks.json stops being "blocked": the parse-error branch now
    # reports a writable empty store, so the migration rewrites it and drops
    # the preservation warning (issue scope 3, 损坏文件不报错).
    ('corrupt-tasks-parse-error', migration,
     '        Err(_) => return Ok((Vec::new(), false)),',
     '        Err(_) => return Ok((Vec::new(), true)),',
     'runtime::cron::tests::migration_blocked_dir_keeps_source_tasks_matches_ts',
     f'#1216 migration blocked-dir-keeps-source-tasks {TREE}'),
    # Same, for the non-array `tasks` branch.
    ('corrupt-tasks-not-array', migration,
     '        _ => Ok((Vec::new(), false)),',
     '        _ => Ok((Vec::new(), true)),',
     'runtime::cron::tests::migration_blocked_dir_tasks_not_array_matches_ts',
     f'#1216 migration blocked-dir-tasks-not-array {TREE}'),
    # The store writes tasks.json in place instead of the temp+rename swap, so
    # an open fd observes the new bytes (issue scope 3, 非原子写).
    ('non-atomic-task-write', source,
     '    let result = (|| -> Result<(), CronStoreError> {\n'
     '        fs::write(&temp, payload)?;\n'
     '        match fs::rename(&temp, path) {\n'
     '            Ok(()) => {}\n'
     '            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {\n'
     '                fs::copy(&temp, path)?;\n'
     '            }\n'
     '            Err(error) => return Err(error.into()),\n'
     '        }\n'
     '        Ok(())\n'
     '    })();',
     '    let result = (|| -> Result<(), CronStoreError> {\n'
     '        fs::write(path, payload)?;\n'
     '        Ok(())\n'
     '    })();',
     'runtime::cron::tests::store_atomic_replace_keeps_old_inode_bytes_matches_ts',
     '#1216 store atomic-replace-keeps-old-inode-bytes: op read-fd result differs from the TS oracle'),
    # The migration marker is written under a different name, so every
    # completed migration's recorded tree misses the marker.
    ('migration-marker-renamed', migration,
     '        root_dir.join("store-migration-v1.json"),',
     '        root_dir.join("store-migration-v2.json"),',
     'runtime::cron::tests::migration_tasks_without_projectkey_stay_matches_ts',
     f'#1216 migration tasks-without-projectkey-stay {TREE}'),
    # `slice(-0)` keeps every record in JS; draining them reverts the parity
    # fix the limit-0 cell exists to pin.
    ('list-runs-zero-limit-drains', source,
     '        let skip = if limit == 0 {\n'
     '            0\n'
     '        } else {\n'
     '            records.len().saturating_sub(limit)\n'
     '        };',
     '        let skip = records.len().saturating_sub(limit);',
     'runtime::cron::tests::store_list_runs_limit_0_matches_ts',
     '#1216 store list-runs-limit-0: op list-runs result differs from the TS oracle'),
    # schedule.type is never inspected, so once-without-runAt and
    # cron-without-expression tasks survive normalizeTask.
    ('normalize-schedule-kind-ignored', source,
     '    let schedule_kind = |name: &str| {\n'
     '        schedule\n'
     '            .get("type")\n'
     '            .and_then(JsValue::as_string)\n'
     '            .is_some_and(|kind| kind.equals_str(name))\n'
     '    };',
     '    let schedule_kind = |_: &str| false;',
     'runtime::cron::tests::store_list_normalize_matches_ts',
     '#1216 store list-normalize: op list result differs from the TS oracle'),
]


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check_anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        print('\n'.join(errors) if errors else f'#1216 {len(mutations)} unique anchors checked')
        return int(bool(errors))
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    if not args:
        raise SystemExit('provide NEW_OUT and optional mutation names, or --check')
    names = args[1:]
    selected = [row for row in mutations if not names or row[0] in names]
    if not selected or (names and len(selected) != len(names)):
        raise SystemExit('unknown or duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    summary = []
    guarded = list(dict.fromkeys([source, migration, tests, Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py']))

    def save(name, text):
        with (out / name).open('x') as stream:
            stream.write(text)

    def run(label, test, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        save(label + '.slots.log', slot.stdout + slot.stderr)
        # negative_suite needs libtest's named result lines, including positives.
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib']
        command += ['--no-run'] if compile else [test, '--', '--exact', '--show-output']
        save(label + '.command.json', json.dumps(command) + '\n')
        try:
            result = run_owned(command, cwd=root / 'src', env=env, timeout=300)
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes)
                              else value or '' for value in (error.stdout, error.stderr))
            save(label + '.log', raw + f'\n{type(error).__name__}: {error}\n')
            raise
        save(label + '.log', result.stdout + result.stderr)
        return result

    for row in selected:
        name, path, old, new, test, assertion = row
        item = dict(mutation=name, test=test, assertion=assertion,
                    status='INVALID', reason='not executed', compiled=False)
        try:
            # No caller-supplied baseline, dirty-file snapshot, or alternate git.
            with mutating_sources(root, guarded) as originals:
                original = originals[path].decode()
                check_anchor(row, original)
                if not compile_only and not passed(run(name + '-positive', test), test):
                    raise RuntimeError('bound positive control did not pass')
                try:
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', test, compile=True).returncode != 0:
                        raise RuntimeError('mutant failed compilation; not a runtime kill')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='cargo test --no-run passed')
                    else:
                        case = Case(name, path, old, new, test, assertion)
                        status, reason = classify(run(name + '-negative', test), case)
                        item.update(status=status, reason=reason)
                finally:
                    # Bytes come only from the HEAD-validated guard; it also
                    # restores and verifies every guarded source on exit.
                    path.write_bytes(originals[path])
                if not passed(run(name + '-restored', test), test):
                    raise RuntimeError('bound restored control did not pass')
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        summary.append(item)
        print(f"{name}: {item['status']}: {item['reason']}", flush=True)
    save('summary.json', json.dumps(summary, indent=2) + '\n')
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    return int(any(row['status'] != ('COMPILED' if compile_only else 'KILLED') for row in summary))


if __name__ == '__main__':
    raise SystemExit(main())
