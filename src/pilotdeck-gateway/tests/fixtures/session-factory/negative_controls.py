#!/usr/bin/env python3
"""Commit first; never bypass HEAD. Compile each mutant, then run its exact test.

Use --compile for the separate compile audit, --audit for read-only anchors.
The full suite belongs on the coordinator host; a guarded sample is enough here.
"""
import json
import os
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed
from _negative_common import mutate

factory = root / 'src/pilotdeck-gateway/src/runtime/session/factory.rs'
handle = factory.with_name('handle.rs')
owner = factory.with_name('owner.rs')
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
oracle = Path(__file__).with_name('oracle.json')
prefix = 'runtime::session::factory::tests::'
cases = [
    ('same-key-concurrent', owner, 'dirty: false,', 'dirty: true,', None, prefix+'same_key_concurrent', '#745 same-key-concurrent conclusion'),
    ('same-key-cached', owner, 'dirty: false,', 'dirty: true,', None, prefix+'same_key_cached', '#745 same-key-cached conclusion'),
    ('create-owner-shutdown', owner, 'return Err(RuntimeError::new(message));', 'let _ = message; return Ok(handle);', 'create_active', prefix+'create_owner_shutdown', '#745 create-owner-shutdown conclusion'),
    ('create-factory-dispose', factory, 'self.mcp.dispose().await', 'Ok(())', None, prefix+'create_factory_dispose', '#745 create-factory-dispose MCP stopped'),
    ('create-dispose-before-mcp', factory, 'self.mcp.dispose().await', 'Ok(())', None, prefix+'create_dispose_before_mcp', '#745 create-dispose-before-mcp conclusion'),
    ('recreate-idle', factory, 'fn can_recreate(&self) -> bool {\n        true\n    }', 'fn can_recreate(&self) -> bool {\n        false\n    }', None, prefix+'recreate_idle', '#745 recreate-idle conclusion'),
    ('recreate-draining', factory, 'release_all(std::mem::take(&mut self.releases)).await', 'std::mem::forget(self); Ok(())', None, prefix+'recreate_draining', '#745 recreate-draining lease cleanup'),
    ('recreate-running', factory, 'writer.sequence as f64,', '0.0,', None, prefix+'recreate_running', '#745 recreate-running captured writer'),
    ('recreate-disposed', live, 'if self.disposed {', 'if false && self.disposed {', None, prefix+'recreate_disposed', '#745 recreate expected failure'),
    ('recreate-read-reject', factory, 'let _ = prepared.resources.release().await;\n                    (storage.close)().await', 'let result = (storage.close)().await;\n                    let _ = prepared.resources.release().await; result', None, prefix+'recreate_read_reject', '#745 recreate-read-reject rollback order'),
    ('recreate-rollback-reject', factory, 'causes: vec![error, rollback],', 'causes: vec![rollback, error],', None, prefix+'recreate_rollback_reject', '#745 recreate-rollback-reject conclusion'),
    ('resume-missing', factory, '(self.options.diagnostic)(diagnostic);', 'let _ = diagnostic;', None, prefix+'resume_missing', '#745 resume-missing conclusion'),
    ('resume-valid', factory, 'target.insert(key.clone(), value.clone());', 'if !key.equals_str("firstPrompt") { target.insert(key.clone(), value.clone()); }', None, prefix+'resume_valid', '#745 resume-valid conclusion'),
    ('resume-malformed', factory, '(self.options.diagnostic)(diagnostic);', 'let _ = diagnostic;', None, prefix+'resume_malformed', '#745 resume-malformed conclusion'),
    ('resume-eisdir', factory, 'code: Some(code.into()),', 'code: None,', None, prefix+'resume_eisdir', '#745 resume-eisdir code'),
    ('resume-load-reject', factory, 'let read = (self.load)().await?;', 'let read = (self.load)().await.unwrap_or(TranscriptRead { entries: vec![], diagnostics: vec![] });', None, prefix+'resume_load_reject', '#745 resume-load-reject expected failure'),
    ('resume-checkpoint-reject', factory, '(self.load_checkpoint)().await?;', 'let _ = (self.load_checkpoint)().await;', None, prefix+'resume_checkpoint_reject', '#745 resume-checkpoint-reject expected failure'),
    ('resume-rollback-reject', factory, 'causes: vec![error, rollback],', 'causes: vec![rollback, error],', None, prefix+'resume_rollback_reject', '#745 resume-rollback-reject conclusion'),
    ('resume-release-reject', factory, 'let rollback = (storage.close)().await;\n                    let _ = prepared.resources.release().await;', 'let rollback = (storage.close)().await;\n                    prepared.resources.release().await?;', None, prefix+'resume_release_reject', '#745 resume-release-reject conclusion'),
    ('admission-read-reject', factory, 'let rollback = (storage.close)().await;', 'let rollback = if self.options.admission.is_some() { Ok(()) } else { (storage.close)().await };', None, prefix+'admission_read_reject', '#745 admission-read-reject rollback order'),
    ('default-reload', handle, 'Err(RuntimeError::new(\n            "Agent session runtime reload is unavailable.",\n        ))', 'Ok(super::factory::ReloadSnapshot { session_id:Default::default(),state:Default::default(),cwd:Default::default(),transcript_path:Default::default(),file_state:Default::default(),transcript_writer_state:Default::default() })', None, prefix+'reload_capability_is_not_fake_success', '#745 unavailable reload'),
    ('terminal-file-state', factory, 'if matches!(&result, Ok(Some(event)) if is(field(event,"type"),"turn_completed"))', 'if false', None, prefix+'reload_retains_saved_files_until_turn_terminal', '#745 file state at terminal conclusion'),
    ('initial-seed-absence', factory, '.map(|r| r.file_state.clone())', '.map(|r| r.file_state.clone()).or_else(|| checkpoint.snapshot().ok())', None, prefix+'initial_reload_preserves_absent_seed', '#745 initial seed absence conclusion'),
    ('unpolled-turn', factory, 'if !self.finished\n            && self.started.load(Ordering::SeqCst)\n            &&', 'if', None, prefix+'unpolled_turn_does_not_publish_file_state', '#745 unpolled turn file conclusion'),
    ('cancelled-create', factory, 'release_all(std::mem::take(&mut self.releases)).await', 'std::mem::forget(self); Ok(())', None, prefix+'cancelled_create_waiter_disposes_late_candidate', '#745 cancelled create conclusion'),
    ('cancelled-replacement', factory, 'release_all(std::mem::take(&mut self.releases)).await', 'std::mem::forget(self); Ok(())', None, prefix+'never_finishing_predecessor_does_not_block_other_keys', '#745 predecessor-never-finishes lease cleanup'),
    ('reload-file-state', root/'src/pilotdeck-gateway/src/host/modules/tool_context.rs', 'read_file_state: Some(self.read_file_state.lock().unwrap().clone()),', 'read_file_state: None,', None, prefix+'actual_file_state_matches_ts', '#745 actual file state conclusion'),
    ('production-host', root/'src/pilotdeck-gateway/src/runtime/turn/host_factory.rs', "Agent loop transport 'native' has no sidecar host factory.", 'Wrong host transport error.', None, prefix+'production_adapter_dispatches_to_host', '#745 host factory execution conclusion'),
    ('submit-projection', live, 'if let Some(source) = reload_source {', 'if let Some(source) = reload_source.filter(|_| false) {', None, prefix+'invalid_projection_releases_submit_slot', '#745 invalid projection rejects submission'),
    ('submit-status', live, 'state.status = super::live_session::SessionStatus::Idle;', 'state.status = super::live_session::SessionStatus::Running;', None, prefix+'invalid_projection_releases_submit_slot', '#745 failed submit status reset'),
    ('submit-identity', live, 'state.current_turn_id = None;', 'state.current_turn_id = Some("stale".into());', None, prefix+'invalid_projection_releases_submit_slot', '#745 failed submit identity reset'),
    ('durable-before-projection', factory, 'self.inner.append(key, entry)?;\n        *entries = candidate;', 'project(&candidate).map_err(|e| crate::storage::LedgerError::Invalid(e.message))?;\n        self.inner.append(key, entry)?;\n        *entries = candidate;', None, prefix+'committed_projection_failure_is_not_rolled_back', '#745 invalid projection durable commit'),
    ('projection-completion', factory, '&& done\n        {', '&& true\n        {', None, prefix+'durable_projection_matches_ts', '#745 projection conclusion'),
    ('projection-usage', factory, 'first + second', 'first', None, prefix+'durable_projection_matches_ts', '#745 projection conclusion'),
    ('projection-metadata', factory, 'key.equals_str("title") || key.equals_str("linkedPullRequest")', 'false', None, prefix+'durable_projection_matches_ts', '#745 projection conclusion'),
    ('projection-invalid-compact', factory, 'return text("text");', 'return true;', None, prefix+'durable_projection_matches_ts', '#745 projection conclusion'),
    ('projection-js-trim', factory, 'crate::metadata::js_trim(&text)', 'text.trim()', None, prefix+'durable_projection_matches_ts', '#745 projection conclusion'),
    ('projection-clone-content', factory, 'object.insert("content".into(), JsValue::Array(blocks));', 'let _ = blocks;', None, prefix+'durable_projection_matches_ts', '#745 projection conclusion'),
    ('terminal-file-state-once', factory, 'if !self.finished\n            && self.started.load(Ordering::SeqCst)', 'if self.started.load(Ordering::SeqCst)', None, prefix+'reload_retains_saved_files_until_turn_terminal', '#745 completed stream cannot publish later state'),
    ('queued-candidate', factory, 'release_all(std::mem::take(&mut self.releases)).await', 'std::mem::forget(self); Ok(())', None, prefix+'queued_candidate_cancellation_closes_storage', '#745 queued candidate lease cleanup'),
    ('session-started-seed', factory, 'if !self.finished\n            && self.started.load(Ordering::SeqCst)\n            &&', 'if', None, prefix+'session_started_cancellation_preserves_absent_seed', '#745 session-started cancellation seed'),
    ('hydrate-invalid-content', factory, 'validate_web_history(&read.entries)?;', 'let _ = &read.entries;', None, prefix+'storage_hydration_matches_ts_errors', '#745 hydrate must reject'),
    ('reload-consistency', factory, 'if before.sequence == after.sequence && before.last_entry_id == after.last_entry_id {', 'if true {', None, prefix+'reload_retries_concurrent_append', '#745 reload version conclusion'),
    ('projection-null-usage', factory, 'let value = property(usage, name)?;', 'let value = field(usage, name);', None, prefix+'durable_projection_matches_ts', '#745 projection rejects malformed usage/metadata'),
    ('fixture-regeneration', oracle, '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"', '"sourceCommit": "0000000000000000000000000000000000000000"', None, prefix+'regenerates_identically', '#280 session-factory/oracle.json differs'),
]

def audit():
    rows = []
    for name, path, old, new, scope, test, assertion in cases:
        try:
            mutate(path.read_text(), scope, [(old, new)])
            rows.append(dict(mutation=name, status='CHECKED', test=test, assertion=assertion))
        except Exception as error:
            rows.append(dict(mutation=name, status='INVALID', reason=str(error)))
    print(json.dumps(rows, indent=2))
    return int(any(row['status'] != 'CHECKED' for row in rows))

if sys.argv[1:] == ['--audit']:
    raise SystemExit(audit())
compile_only = len(sys.argv) > 1 and sys.argv[1] == '--compile'
args = sys.argv[2:] if compile_only else sys.argv[1:]
if not args:
    raise SystemExit('usage: negative_controls.py [--compile] OUT [mutation names]')
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
    raise SystemExit('caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
names = set(args[1:])
if names - {c[0] for c in cases}:
    raise SystemExit('unknown mutation')
chosen = [c for c in cases if not names or c[0] in names]
out = Path(args[0]); out.mkdir(parents=True, exist_ok=False)
command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib']

def run(label, test, compile=False):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    (out / (label+'.slots.log')).write_text(slot.stdout+slot.stderr)
    args = ['--no-run'] if compile else [test,'--','--exact','--include-ignored','--nocapture']
    result = run_owned(command+args,cwd=root,env=env,timeout=900)
    with (out / (label+'.log')).open('x') as log:
        log.write(result.stdout+result.stderr)
    return result

results=[]
try:
    with mutating_sources(root, list(dict.fromkeys(c[1] for c in chosen))) as originals:
        for name,path,old,new,scope,test,assertion in chosen:
            row=dict(mutation=name,test=test,assertion=assertion,status='INVALID',reason='',compiled=False)
            try:
                changed=mutate(originals[path].decode(),scope,[(old,new)])
                if not compile_only and not passed(run(name+'-positive',test),test):
                    raise RuntimeError('bound positive test failed')
                path.write_text(changed)
                if run(name+'-compile',test,compile=True).returncode:
                    raise RuntimeError('mutant compile failed')
                row['compiled']=True
                if compile_only:
                    row.update(status='COMPILED',reason='mutant no-run passed')
                else:
                    status,reason=classify(run(name+'-negative',test),Case(name,path,old,new,test,assertion,scope))
                    row.update(status=status,reason=reason)
            except Exception as error:
                row.update(status='INVALID',reason=f'{type(error).__name__}: {error}')
            finally:
                path.write_bytes(originals[path])
            if not compile_only:
                try:
                    if not passed(run(name+'-restored',test),test):
                        row.update(status='INVALID',reason=row['reason']+'; restored test failed')
                except Exception as error:
                    row.update(status='INVALID',reason=row['reason']+f'; restored: {error}')
            results.append(row)
            print(f'{name}: {row["status"]}: {row["reason"]}',flush=True)
except Exception as error:
    for row in results:
        row.update(status='INVALID', reason=row['reason']+f'; source guard: {error}')
    results.extend(dict(mutation=c[0],test=c[5],assertion=c[6],status='INVALID',compiled=False,reason=f'source guard: {error}') for c in chosen if not any(row['mutation']==c[0] for row in results))
counts={status:sum(r['status']==status for r in results) for status in ('KILLED','SURVIVED','INVALID','COMPILED')}
with (out/'summary.json').open('x') as summary:
    json.dump(dict(controls=results,counts=counts),summary,indent=2);summary.write('\n')
print(json.dumps(counts),flush=True)
raise SystemExit(int(any(r['status']!=('COMPILED' if compile_only else 'KILLED') for r in results)))
