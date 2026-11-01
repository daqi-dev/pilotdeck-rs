#!/usr/bin/env python3
"""#1233 dispose-flush / restart-recovery controls: compile, fail at the bound
assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.

Rows are (name, path, old, new, target, test, assertion); ``target`` is the
cargo test target ('background_persist' integration test or 'lib' unit tests).
#1233 fix-r1 (P1-1): assertions carry the *expanded* panic text -- a literal
``{mode}`` never matches what the test printed, so such a row was always
INVALID, and the dispose-flush-phase mutant fails at the start-refusal
assertion, not the draining one.
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
snapshot_source = root / 'src/pilotdeck-gateway/src/runtime/background_snapshot.rs'
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
     'background_persist',
     'dispose_flush_failure_stays_draining_and_memoizes_the_rejection',
     '#1233 flush failure rejects dispose'),
    # D1: the phase advances to disposed despite the failed flush. The mutant
    # fails first at the start-refusal assertion (the message differs from the
    # draining oracle), so bind both expanded modes of that assertion.
    ('dispose-flush-phase', source,
     '            if outcome.is_ok() {\n'
     '                self.inner.state.lock().unwrap().phase = Phase::Disposed;\n'
     '            }',
     '            self.inner.state.lock().unwrap().phase = Phase::Disposed;',
     'background_persist',
     'dispose_flush_failure_stays_draining_and_memoizes_the_rejection',
     ['#1233 reject start refusal', '#1233 sync-throw start refusal']),
    # D1: every dispose() call re-wraps the memoized rejection, so the two
    # errors are no longer the same allocation.
    ('dispose-rejection-not-memoized', source,
     '        let _ = disposed.wait_for(Option::is_some).await;\n'
     '        disposed.borrow().clone().unwrap()',
     '        let _ = disposed.wait_for(Option::is_some).await;\n'
     '        match disposed.borrow().clone().unwrap() {\n'
     '            Ok(()) => Ok(()),\n'
     '            Err(error) => Err(Arc::new(BackgroundTaskError::error(error.message.clone()))),\n'
     '        }',
     'background_persist',
     'dispose_flush_failure_stays_draining_and_memoizes_the_rejection',
     ['#1233 reject the rejection is memoized',
      '#1233 sync-throw the rejection is memoized']),
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
     'background_persist',
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
     'background_persist',
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
     'background_persist',
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
     'background_persist',
     'spill_write_failure_restart_fails_closed',
     '#1233 an unusable spill directory must fail the restart'),
    # D4: appended chunks are no longer persisted for a restart to find.
    ('append-persist-removed', source,
     '        if let Some(store) = &self.snapshot_store {\n'
     '            let store = Arc::clone(store);\n'
     '            let entry = Arc::clone(entry);\n'
     '            tokio::spawn(async move {\n'
     '                let _ = BackgroundTaskRuntime::persist_entry_static(&store, &entry, seq).await;\n'
     '            });\n'
     '        }',
     '        let _ = (&self.snapshot_store, entry);',
     'background_persist',
     'crashed_running_task_recovers_as_unknown_with_durable_output',
     '#1233 running snapshot with six durable bytes'),
    # Restored terminal tasks are rewritten as unknown.
    ('restore-unsettled-gate', source,
     '            let was_unsettled = matches!(task.status, TaskStatus::Pending | TaskStatus::Running);',
     '            let was_unsettled = true;',
     'background_persist',
     'terminal_recovery_restart_restores_completed_tasks_with_output',
     '#1233 restored status'),
    # D6: durable output is claimed even when the bytes never landed.
    ('durable-claimed-without-landing', output_store,
     '        self.spill.as_ref().is_some_and(|spill| {\n'
     '            *spill.tail.borrow() == Some(true)\n'
     '                && fs::metadata(&spill.path).is_ok_and(|metadata| metadata.len() == self.total)\n'
     '        })',
     '        true',
     'background_persist',
     'spill_write_failure_keeps_snapshot_without_durable_claims',
     '#1233 unsettled claim only'),
    # #1233 fix-r1 (P0-1): the store-side drop rule flips, so the superseded
    # direction is dropped instead of the stale one.
    ('persist-order-store', snapshot_source,
     '            let mut accepted = self.accepted.lock().unwrap();\n'
     '            let task_id = snapshot.task.task_id.clone();\n'
     '            if seq < accepted.get(&task_id).copied().unwrap_or(0) {',
     '            let mut accepted = self.accepted.lock().unwrap();\n'
     '            let task_id = snapshot.task.task_id.clone();\n'
     '            if seq > accepted.get(&task_id).copied().unwrap_or(0) {',
     'lib',
     'runtime::background_snapshot::tests::superseded_sequence_writes_are_dropped',
     '#1233 a superseded snapshot write cannot overwrite the newer state'),
    # #1233 fix-r1 (P0-1): appends stop advancing the sequence, so a stale
    # append persist ties with the newer one and can overwrite it.
    ('persist-order-append-seq', source,
     '        state.output.append(chunk);\n'
     '        state.task.output_bytes = state.output.total;\n'
     '        state.persist_seq += 1;\n'
     '        state.persist_seq',
     '        state.output.append(chunk);\n'
     '        state.task.output_bytes = state.output.total;\n'
     '        let _ = state.persist_seq;\n'
     '        0',
     'lib',
     'runtime::background::persist_order_tests::stale_append_persist_cannot_overwrite_a_newer_append',
     '#1233 a superseded append persist cannot overwrite a newer append'),
    # #1233 fix-r1 (P0-1): the settle stops outranking the appends, so a late
    # append persist can overwrite the terminal snapshot.
    ('persist-order-settle-seq', source,
     '            state.persist_seq += 1;\n'
     '            let persist_seq = state.persist_seq;',
     '            let persist_seq = 0;',
     'lib',
     'runtime::background::persist_order_tests::stale_append_persist_cannot_overwrite_the_terminal_snapshot',
     '#1233 a superseded append persist cannot overwrite the terminal snapshot'),
    # #1233 fix-r1 (P1-2): each corruption defect is accepted instead of
    # failing the store constructor closed.
    ('corrupt-bad-json-soft', snapshot_source,
     '    let snapshot = deserialize_snapshot_file(&text).map_err(|e| {\n'
     '        io::Error::new(\n'
     '            ErrorKind::InvalidData,\n'
     '            format!(\n'
     '                "Background task snapshot {} is not valid JSON; recovery is required: {}",\n'
     '                file_path.display(),\n'
     '                e\n'
     '            ),\n'
     '        )\n'
     '    })?;',
     '    let snapshot = match deserialize_snapshot_file(&text) {\n'
     '        Ok(snapshot) => snapshot,\n'
     '        Err(_) => return Ok(Vec::new()),\n'
     '    };',
     'background_persist',
     'corrupted_snapshot_file_fails_closed',
     '#1233 corrupt-bad-json must fail the store constructor'),
    ('corrupt-version-soft', snapshot_source,
     '    if snapshot.version != SNAPSHOT_VERSION {',
     '    if false {',
     'background_persist',
     'corrupted_snapshot_file_fails_closed',
     '#1233 corrupt-version-2 must fail the store constructor'),
    ('corrupt-duplicate-soft', snapshot_source,
     '        if !seen.insert(&entry.task.task_id) {\n'
     '            return Err(io::Error::new(\n'
     '                ErrorKind::InvalidData,\n'
     '                format!(\n'
     '                    "Background task snapshot {} repeats taskId {:?}; recovery is required.",\n'
     '                    file_path.display(),\n'
     '                    entry.task.task_id\n'
     '                ),\n'
     '            ));\n'
     '        }',
     '        if !seen.insert(&entry.task.task_id) {\n'
     '            continue;\n'
     '        }',
     'background_persist',
     'corrupted_snapshot_file_fails_closed',
     '#1233 corrupt-duplicate-id must fail the store constructor'),
    ('corrupt-status-soft', snapshot_source,
     '    if !matches!(\n'
     '        task.status.as_str(),\n'
     '        "pending" | "running" | "completed" | "failed" | "cancelled" | "unknown"\n'
     '    ) {',
     '    if false {',
     'background_persist',
     'corrupted_snapshot_file_fails_closed',
     '#1233 corrupt-invalid-status must fail the store constructor'),
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
    guarded = list(dict.fromkeys([source, output_store, snapshot_source, tests,
        Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py']))

    def save(name, text):
        with (out / name).open('x') as stream:
            stream.write(text)

    def run(label, test, target, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        save(label + '.slots.log', slot.stdout + slot.stderr)
        # negative_suite needs libtest's named result lines, including positives.
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway']
        if target == 'lib':
            command += ['--lib']
        else:
            command += ['--test', target]
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
        name, path, old, new, target, test, assertion = row
        item = dict(mutation=name, test=test, target=target, assertion=assertion,
                    status='INVALID', reason='not executed', compiled=False)
        try:
            # No caller-supplied baseline, dirty-file snapshot, or alternate git.
            with mutating_sources(root, guarded) as originals:
                original = originals[path].decode()
                check_anchor(row, original)
                if not compile_only and not passed(run(name + '-positive', test, target), test):
                    raise RuntimeError('bound positive control did not pass')
                try:
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', test, target, compile=True).returncode != 0:
                        raise RuntimeError('mutant failed compilation; not a runtime kill')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='cargo test --no-run passed')
                    else:
                        case = Case(name, path, old, new, test, assertion)
                        status, reason = classify(run(name + '-negative', test, target), case)
                        item.update(status=status, reason=reason)
                finally:
                    # Bytes come only from the HEAD-validated guard; it also
                    # restores and verifies every guarded source on exit.
                    path.write_bytes(originals[path])
                if not passed(run(name + '-restored', test, target), test):
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
