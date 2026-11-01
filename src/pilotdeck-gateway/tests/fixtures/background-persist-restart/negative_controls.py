#!/usr/bin/env python3
"""#1233 dispose-flush / restart-recovery controls: compile, fail at the bound
assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.
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
source = root / 'src/pilotdeck-gateway/src/runtime/background.rs'
output_store = root / 'src/pilotdeck-gateway/src/runtime/background/output.rs'
tests = root / 'src/pilotdeck-gateway/tests/background_persist.rs'

flush_outcome = (
    '                outcome = match tokio::spawn(async move { store.flush().await }).await {\n'
    '                    Ok(Ok(())) => Ok(()),\n'
    '                    Ok(Err(error)) => Err(Arc::new(BackgroundTaskError::error(JsString::from(\n'
    '                        error.to_string().as_str(),\n'
    '                    )))),\n'
    '                    Err(joined) => {\n'
    '                        let payload = joined.into_panic();\n'
    '                        let message = crate::runtime::cleanup_task::panic_message(payload.as_ref())\n'
    '                            .map(str::to_owned)\n'
    '                            .unwrap_or_else(|| "Background snapshot flush failed.".to_owned());\n'
    '                        Err(Arc::new(BackgroundTaskError::error(JsString::from(\n'
    '                            message.as_str(),\n'
    '                        ))))\n'
    '                    }\n'
    '                };'
)
flush_swallow = '                let _ = tokio::spawn(async move { store.flush().await }).await;'
restore_block = (
    '            let output = OutputStore::restore_from_spill(\n'
    '                DEFAULT_MEMORY_BYTES,\n'
    '                output_path(disk_spill_dir, &task.task_id),\n'
    '                snapshot.output.total_bytes,\n'
    '            )\n'
    '            .unwrap_or_else(|error| {\n'
    '                let path = output_path(disk_spill_dir, &task.task_id);\n'
    '                let message = if error.kind() == std::io::ErrorKind::InvalidData {\n'
    '                    error.to_string()\n'
    '                } else {\n'
    '                    format!("Could not restore task output from {}.", path.display())\n'
    '                };\n'
    '                panic!("{message}");\n'
    '            });'
)
mutations = [
    # D1: the dispose flush error is swallowed instead of rejecting dispose().
    ('dispose-flush-swallowed', source, flush_outcome, flush_swallow,
     'dispose_flush_failure_stays_draining_and_memoizes_the_rejection',
     '#1233 flush failure rejects dispose'),
    # D1: the phase advances to disposed despite the failed flush.
    ('dispose-flush-phase', source,
     '            if outcome.is_ok() {\n'
     '                self.inner.state.lock().unwrap().phase = Phase::Disposed;\n'
     '            }',
     '            self.inner.state.lock().unwrap().phase = Phase::Disposed;',
     'dispose_flush_failure_stays_draining_and_memoizes_the_rejection',
     '#1233 {mode} runtime stays draining'),
    # D1: every dispose() call re-wraps the memoized rejection.
    ('dispose-rejection-not-memoized', source,
     '        let _ = disposed.wait_for(Option::is_some).await;\n'
     '        disposed.borrow().clone().unwrap()',
     '        let _ = disposed.wait_for(Option::is_some).await;\n'
     '        match disposed.borrow().clone().unwrap() {\n'
     '            Ok(()) => Ok(()),\n'
     '            Err(error) => Err(Arc::new(BackgroundTaskError::error(error.message.clone()))),\n'
     '        }',
     'dispose_flush_failure_stays_draining_and_memoizes_the_rejection',
     '#1233 {mode} the rejection is memoized'),
    # D1: the project resource dispose hides the flush rejection.
    ('resources-flush-hidden', source,
     '            self.tasks.dispose().await.map_err(|error| {\n'
     '                RuntimeError::new(String::from_utf16_lossy(error.message.units())).with_js_error(\n'
     '                    RuntimeJsError {\n'
     '                        name: error.name.into(),\n'
     '                        code: None,\n'
     '                        message: error.message.clone(),\n'
     '                        requires_host: false,\n'
     '                    },\n'
     '                )\n'
     '            })',
     '            self.tasks.dispose().await.map_err(|error| {\n'
     '                RuntimeError::new(String::from_utf16_lossy(error.message.units()))\n'
     '                    .with_js_error(RuntimeJsError {\n'
     '                        name: error.name.into(),\n'
     '                        code: None,\n'
     '                        message: error.message.clone(),\n'
     '                        requires_host: false,\n'
     '                    })\n'
     '            })\n'
     '            .map_err(|mut error| { error.message.truncate(0); error })',
     'background_resources_dispose_propagates_the_flush_rejection',
     '#1233 resources dispose message'),
    # D2: a broken output claim no longer fails the restart.
    ('restore-output-soft', source, restore_block,
     '            let output = OutputStore::restore_from_spill(\n'
     '                DEFAULT_MEMORY_BYTES,\n'
     '                output_path(disk_spill_dir, &task.task_id),\n'
     '                snapshot.output.total_bytes,\n'
     '            )\n'
     '            .unwrap_or_else(|_error| OutputStore::with_spill(DEFAULT_MEMORY_BYTES, None));',
     'restart_output_length_mismatch_fails_closed',
     '#1233 a length mismatch must fail the restart'),
    # D2: an unrestorable task entry is skipped instead of failing the restart.
    ('restore-task-soft', source,
     '            let mut task = restore_task(&snapshot.task).unwrap_or_else(|error| {\n'
     '                panic!("Background task snapshot restore failed: {error}");\n'
     '            });',
     '            let mut task = match restore_task(&snapshot.task) {\n'
     '                Ok(task) => task,\n'
     '                Err(_) => continue,\n'
     '            };',
     'restart_invalid_task_fails_closed',
     '#1233 an unrestorable task must fail the restart'),
    # D3: restore goes back to trusting the persisted flag.
    ('restore-persisted-gate', source, restore_block,
     '            let output = if snapshot.output.persisted {\n'
     '                OutputStore::restore_from_spill(\n'
     '                    DEFAULT_MEMORY_BYTES,\n'
     '                    output_path(disk_spill_dir, &task.task_id),\n'
     '                    snapshot.output.total_bytes,\n'
     '                )\n'
     '                .unwrap_or_else(|error| {\n'
     '                    let path = output_path(disk_spill_dir, &task.task_id);\n'
     '                    let message = if error.kind() == std::io::ErrorKind::InvalidData {\n'
     '                        error.to_string()\n'
     '                    } else {\n'
     '                        format!("Could not restore task output from {}.", path.display())\n'
     '                    };\n'
     '                    panic!("{message}");\n'
     '                })\n'
     '            } else {\n'
     '                OutputStore::with_spill(DEFAULT_MEMORY_BYTES, None)\n'
     '            };',
     'spill_write_failure_restart_fails_closed',
     '#1233 an unusable spill directory must fail the restart'),
    # D4: appended chunks are no longer persisted for a restart to find.
    ('append-persist-removed', source,
     '        if let Some(store) = &self.snapshot_store {\n'
     '            let store = Arc::clone(store);\n'
     '            let entry = Arc::clone(entry);\n'
     '            tokio::spawn(async move {\n'
     '                let _ = BackgroundTaskRuntime::persist_entry_static(&store, &entry).await;\n'
     '            });\n'
     '        }',
     '        let _ = (&self.snapshot_store, entry);',
     'crashed_running_task_recovers_as_unknown_with_durable_output',
     '#1233 running snapshot with six durable bytes'),
    # Restored terminal tasks are rewritten as unknown.
    ('restore-unsettled-gate', source,
     '            let was_unsettled = matches!(task.status, TaskStatus::Pending | TaskStatus::Running);',
     '            let was_unsettled = true;',
     'terminal_recovery_restart_restores_completed_tasks_with_output',
     '#1233 restored status'),
    # D6: durable output is claimed even when the bytes never landed.
    ('durable-claimed-without-landing', output_store,
     '        self.spill.as_ref().is_some_and(|spill| {\n'
     '            *spill.tail.borrow() == Some(true)\n'
     '                && fs::metadata(&spill.path).is_ok_and(|metadata| metadata.len() == self.total)\n'
     '        })',
     '        true',
     'spill_write_failure_keeps_snapshot_without_durable_claims',
     '#1233 unsettled claim only'),
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
        print('\n'.join(errors) if errors else f'#1233 {len(mutations)} unique anchors checked')
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
    guarded = list(dict.fromkeys([source, output_store, tests, Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py']))

    def save(name, text):
        with (out / name).open('x') as stream:
            stream.write(text)

    def run(label, test, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        save(label + '.slots.log', slot.stdout + slot.stderr)
        # negative_suite needs libtest's named result lines, including positives.
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway',
                   '--test', 'background_persist']
        command += ['--no-run'] if compile else [test, '--', '--exact', '--show-output']
        save(label + '.command.json', json.dumps(command) + '\n')
        try:
            result = run_owned(command, cwd=root / 'src', env=env, timeout=600)
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
