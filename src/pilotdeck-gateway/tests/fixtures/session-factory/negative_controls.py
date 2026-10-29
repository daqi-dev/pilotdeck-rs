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
from negative_suite import Case, classify, invalidate_completed, passed
from _negative_common import mutate

factory = root / 'src/pilotdeck-gateway/src/runtime/session/factory.rs'
handle = factory.with_name('handle.rs')
owner = factory.with_name('owner.rs')
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
oracle = Path(__file__).with_name('oracle.json')
test_source = factory.with_name('factory_tests.rs')
snapshot = Path(__file__).with_name('file-snapshot.mts')
probe = Path(__file__).with_name('probe.mts')
prefix = 'runtime::session::factory::tests::'
cases = [
    ('same-key-concurrent', owner, 'dirty: false,', 'dirty: true,', None, prefix+'same_key_concurrent', '#745 same-key-concurrent conclusion'),
    ('same-key-cached', owner, 'dirty: false,', 'dirty: true,', None, prefix+'same_key_cached', '#745 same-key-cached conclusion'),
    ('same-key-concurrent-skip-cache', owner, 'if !record.dirty || !self.0.options.factory.can_recreate() {', 'if false {', 'create_active', prefix+'same_key_concurrent', '#745 same-key-concurrent conclusion'),
    ('same-key-cached-skip-cache', owner, 'if !record.dirty || !self.0.options.factory.can_recreate() {', 'if false {', 'create_active', prefix+'same_key_cached', '#745 same-key-cached conclusion'),
    ('writer-abort-panic-cleanup', handle, 'Err(RuntimeError::new(message))', 'let _ = message; std::panic::resume_unwind(panic)', None, prefix+'writer_abort_panic_releases_lease_before_session_dispose', '#1022 abort panic still drains active turn'),
    ('writer-zero-budget', factory, '!options.writer_busy_wait_timeout.is_zero(),', 'true,', None, prefix+'writer_fence_rejects_zero_budget', '#745 zero fence budget rejected'),
    ('writer-expired-budget-skips-try', factory, 'mutex.try_lock_owned().map_err(|_| writer_busy())', 'Err(writer_busy())', None, prefix+'writer_fence_exhausted_drain_budget_acquires_idle_lease', '#745 exhausted budget still acquires idle writer'),
    ('create-owner-shutdown', owner, 'return Err(RuntimeError::new(message));', 'let _ = message; return Ok(handle);', 'create_active', prefix+'create_owner_shutdown', '#745 create-owner-shutdown conclusion'),
    ('create-factory-dispose', factory, 'self.mcp.dispose().await', 'Ok(())', None, prefix+'create_factory_dispose', '#745 create-factory-dispose MCP stopped'),
    ('create-dispose-before-mcp', factory, 'self.mcp.dispose().await', 'Ok(())', None, prefix+'create_dispose_before_mcp', '#745 create-dispose-before-mcp conclusion'),
    ('recreate-idle', factory, 'fn can_recreate(&self) -> bool {\n        true\n    }', 'fn can_recreate(&self) -> bool {\n        false\n    }', None, prefix+'recreate_idle', '#745 recreate-idle conclusion'),
    ('recreate-draining', factory, 'release_all(std::mem::take(&mut self.releases)).await', 'std::mem::forget(self); Ok(())', None, prefix+'recreate_draining', '#745 recreate-draining lease cleanup'),
    ('recreate-running', factory, 'let read = storage.restore().await?;\n            for diagnostic', 'let read = storage.restore().await?; storage.recorder.writer().restore_state(0.0, &JsValue::Null).map_err(|e| RuntimeError::new(e.to_string()))?;\n            for diagnostic', None, prefix+'recreate_running', '#745 recreate-running durable writer'),
    ('recreate-disposed', live, 'if self.disposed {', 'if false && self.disposed {', None, prefix+'recreate_disposed', '#745 recreate expected failure'),
    ('recreate-read-reject', factory, 'let _ = prepared.resources.release().await;\n                    storage.close_owned().await', 'let result = storage.close_owned().await;\n                    let _ = prepared.resources.release().await; result', None, prefix+'recreate_read_reject', '#745 recreate-read-reject rollback order'),
    ('recreate-rollback-reject', factory, 'causes: vec![error, rollback],', 'causes: vec![rollback, error],', None, prefix+'recreate_rollback_reject', '#745 recreate-rollback-reject conclusion'),
    ('resume-missing', factory, '(self.options.diagnostic)(diagnostic);', 'let _ = diagnostic;', None, prefix+'resume_missing', '#745 resume-missing conclusion'),
    ('resume-valid', factory, 'target.insert(key.clone(), value.clone());', 'if !key.equals_str("firstPrompt") { target.insert(key.clone(), value.clone()); }', None, prefix+'resume_valid', '#745 resume-valid conclusion'),
    ('resume-malformed', factory, '(self.options.diagnostic)(diagnostic);', 'let _ = diagnostic;', None, prefix+'resume_malformed', '#745 resume-malformed conclusion'),
    ('resume-eisdir', factory, 'code: Some(code.into()),', 'code: None,', None, prefix+'resume_eisdir', '#745 resume-eisdir code'),
    ('resume-load-reject', factory, 'let read = (self.load)().await?;', 'let read = (self.load)().await.unwrap_or(TranscriptRead { entries: vec![], diagnostics: vec![] });', None, prefix+'resume_load_reject', '#745 resume-load-reject expected failure'),
    ('resume-checkpoint-reject', factory, '(self.load_checkpoint)().await?;', 'let _ = (self.load_checkpoint)().await;', None, prefix+'resume_checkpoint_reject', '#745 resume-checkpoint-reject expected failure'),
    ('resume-rollback-reject', factory, 'causes: vec![error, rollback],', 'causes: vec![rollback, error],', None, prefix+'resume_rollback_reject', '#745 resume-rollback-reject conclusion'),
    ('resume-release-reject', factory, 'let rollback = storage.close_owned().await;\n                    let _ = prepared.resources.release().await;', 'let rollback = storage.close_owned().await;\n                    prepared.resources.release().await?;', None, prefix+'resume_release_reject', '#745 resume-release-reject conclusion'),
    ('admission-read-reject', factory, 'let rollback = storage.close_owned().await;', 'let rollback = if self.options.admission.is_some() { Ok(()) } else { storage.close_owned().await };', None, prefix+'admission_read_reject', '#745 admission-read-reject rollback order'),
    ('default-reload', handle, 'Err(RuntimeError::new(\n            "Agent session runtime reload is unavailable.",\n        ))', 'Ok(super::factory::ReloadSnapshot { session_id:Default::default(),state:Default::default(),cwd:Default::default(),transcript_path:Default::default(),file_state:Default::default(),transcript_writer_state:Default::default() })', None, prefix+'reload_capability_is_not_fake_success', '#745 unavailable reload'),
    ('terminal-file-state', factory, 'if !self.finished\n                && (matches!', 'if false\n                && (matches!', None, prefix+'reload_retains_saved_files_until_turn_terminal', '#745 file state at terminal conclusion'),
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
    ('terminal-next-once', factory, 'if !self.finished\n                && (matches!', 'if true\n                && (matches!', None, prefix+'reload_file_state_matches_ts_timing', '#745 post-terminal seed oracle'),
    ('terminal-seed-oracle', factory, 'if !self.finished\n                && (matches!', 'if false\n                && (matches!', None, prefix+'reload_file_state_matches_ts_timing', '#745 terminal seed oracle'),
    ('matrix-messages', factory, 'state: self.projection.snapshot()?,', 'state: { let mut state=self.projection.snapshot()?; state.messages.clear(); state },', None, prefix+'recreate_idle', '#745 recreate-idle reload messages oracle'),
    ('matrix-title', factory, 'state: self.projection.snapshot()?,', 'state: { let mut state=self.projection.snapshot()?; state.metadata.insert("title".into(), "wrong".into()); state },', None, prefix+'resume_valid', '#745 resume-valid metadata oracle'),
    ('matrix-title-source', factory, 'state: self.projection.snapshot()?,', 'state: { let mut state=self.projection.snapshot()?; state.metadata.insert("titleSource".into(), "wrong".into()); state },', None, prefix+'resume_valid', '#745 resume-valid metadata oracle'),
    ('matrix-last-entry', factory, 'transcript_writer_state: writer,', 'transcript_writer_state: WriterState { last_entry_id: None, ..writer },', None, prefix+'recreate_idle', '#745 recreate-idle writer oracle'),
    ('recreate-file-seed', factory, '.map(|r| r.file_state.clone())', '.map(|_| SeedState::default())', None, prefix+'recreate_preserves_snapshot_seed_and_uses_durable_state', '#745 recreated file seed oracle'),
    ('checkpoint-before-restore', factory, 'let read = (self.load)().await?;', 'let read = (self.load)().await?; (self.load_checkpoint)().await?;', None, prefix+'resume_checkpoint_reject', '#745 resume-checkpoint-reject checkpoint writer oracle'),
    ('matrix-exact-rollback', factory, 'Failed to {} and roll back agent session {}.', 'Failed to {} and rollback agent session {}.', None, prefix+'resume_rollback_reject', '#745 resume-rollback-reject exact error oracle'),
    ('matrix-diagnostic-severity', factory, '(self.options.diagnostic)(diagnostic);', 'let mut diagnostic=diagnostic.clone(); diagnostic.severity="warning"; (self.options.diagnostic)(&diagnostic);', None, prefix+'resume_malformed', '#745 resume-malformed diagnostic fields oracle'),
    ('matrix-diagnostic-line', factory, '(self.options.diagnostic)(diagnostic);', 'let mut diagnostic=diagnostic.clone(); diagnostic.line=None; (self.options.diagnostic)(&diagnostic);', None, prefix+'resume_malformed', '#745 resume-malformed diagnostic fields oracle'),
    ('writer-before-read', factory, 'let read = storage.restore().await?;\n            for diagnostic', 'let read = storage.restore().await?; if let Some(reload) = &reload { storage.recorder.writer().restore_state(reload.transcript_writer_state.sequence as f64, &JsValue::Null).map_err(|e| RuntimeError::new(e.to_string()))?; }\n            for diagnostic', None, prefix+'recreate_running_before_read', '#745 interleaving durable writer after drain'),
    ('writer-after-publish', factory, 'let read = storage.restore().await?;\n            for diagnostic', 'let read = storage.restore().await?; if let Some(reload) = &reload { storage.recorder.writer().restore_state(reload.transcript_writer_state.sequence as f64, &JsValue::Null).map_err(|e| RuntimeError::new(e.to_string()))?; }\n            for diagnostic', None, prefix+'recreate_running_after_publish_resume', '#745 interleaving durable writer after drain'),
    ('replacement-controller', live, 'self.state.lock().unwrap().reload = Some(Arc::new(source));', 'self.state.lock().unwrap().signal.abort(); self.state.lock().unwrap().reload = Some(Arc::new(source));', None, prefix+'recreate_controller_is_independent_of_actual_predecessor', '#745 replacement controller independent oracle'),
    ('production-owner-cancel', factory, 'release_all(std::mem::take(&mut self.releases)).await', 'std::mem::forget(self); Ok(())', None, prefix+'production_owner_cancelled_create_close_and_shutdown', '#745 owner cancellation lease cleanup'),
    ('fence-ts-oldState', oracle, '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "active",\n      "oldInFlight": 0,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 1,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "fulfilled",\n          "handle": 2\n        }\n      ],\n      "oldPendingAtBoundary": false\n    }', '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "disposed",\n      "oldInFlight": 0,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 1,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "fulfilled",\n          "handle": 2\n        }\n      ],\n      "oldPendingAtBoundary": false\n    }', None, prefix+'fence_idle_recreate_beforeload', '#745 fence TS complete observation'),
    ('fence-ts-oldInFlight', oracle, '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "active",\n      "oldInFlight": 0,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 1,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "fulfilled",\n          "handle": 2\n        }\n      ],\n      "oldPendingAtBoundary": false\n    }', '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "active",\n      "oldInFlight": 1,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 1,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "fulfilled",\n          "handle": 2\n        }\n      ],\n      "oldPendingAtBoundary": false\n    }', None, prefix+'fence_idle_recreate_beforeload', '#745 fence TS complete observation'),
    ('fence-ts-registryAfterEvent', oracle, '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "active",\n      "oldInFlight": 0,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 1,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "fulfilled",\n          "handle": 2\n        }\n      ],\n      "oldPendingAtBoundary": false\n    }', '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "active",\n      "oldInFlight": 0,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 9,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "fulfilled",\n          "handle": 2\n        }\n      ],\n      "oldPendingAtBoundary": false\n    }', None, prefix+'fence_idle_recreate_beforeload', '#745 fence TS complete observation'),
    ('fence-ts-oldPendingAtBoundary', oracle, '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "active",\n      "oldInFlight": 0,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 1,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "fulfilled",\n          "handle": 2\n        }\n      ],\n      "oldPendingAtBoundary": false\n    }', '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "active",\n      "oldInFlight": 0,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 1,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "fulfilled",\n          "handle": 2\n        }\n      ],\n      "oldPendingAtBoundary": true\n    }', None, prefix+'fence_idle_recreate_beforeload', '#745 fence TS complete observation'),
    ('fence-ts-results', oracle, '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "active",\n      "oldInFlight": 0,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 1,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "fulfilled",\n          "handle": 2\n        }\n      ],\n      "oldPendingAtBoundary": false\n    }', '    {\n      "name": "fence-idle-recreate-beforeload",\n      "oldState": "active",\n      "oldInFlight": 0,\n      "registryAtBoundary": 1,\n      "registryAfterEvent": 1,\n      "results": [\n        {\n          "label": "fence-first",\n          "status": "rejected"\n        }\n      ],\n      "oldPendingAtBoundary": false\n    }', None, prefix+'fence_idle_recreate_beforeload', '#745 fence TS complete observation'),
    ('fixture-regeneration', oracle, '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"', '"sourceCommit": "0000000000000000000000000000000000000000"', None, prefix+'regenerates_identically', '#280 session-factory/oracle.json differs'),
    ('checkpoint-staging-snapshot', snapshot, 'if(writing && temporary(file))continue;', 'if(false && temporary(file))continue;', None, prefix+'regenerates_identically', '#745 checkpoint staged bytes excluded'),
    ('checkpoint-renamed-snapshot', snapshot, 'if(writing && temporary(file))continue;', 'if(writing && temporary(file) && fs.existsSync(file))continue;', None, prefix+'regenerates_identically', '#745 checkpoint rename snapshot'),
    ('checkpoint-final-staging', snapshot, 'if(writing && temporary(file))continue;', 'if(temporary(file))continue;', None, prefix+'regenerates_identically', '#745 checkpoint final evidence retains staging'),
    ('checkpoint-snapshot-scope', snapshot, 'if(writing && temporary(file))continue;', 'if(writing && file.endsWith(".tmp"))continue;', None, prefix+'regenerates_identically', '#745 checkpoint unrelated files retained'),
    ('checkpoint-snapshot-uuid', snapshot, '/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\\.tmp$/.test(file.slice(prefix.length))', 'true', None, prefix+'regenerates_identically', '#745 checkpoint unrelated files retained'),
    ('checkpoint-snapshot-errors', snapshot, 'const stat=fs.lstatSync(file);', 'let stat;try{stat=fs.lstatSync(file);}catch{continue;}', None, prefix+'regenerates_identically', '#745 checkpoint persistent errors propagate'),
    ('checkpoint-snapshot-read-errors', snapshot, 'const bytes=fs.readFileSync(file);', 'let bytes;try{bytes=fs.readFileSync(file);}catch{continue;}', None, prefix+'regenerates_identically', '#745 checkpoint persistent errors propagate'),
    ('checkpoint-snapshot-published', snapshot, 'const stat=fs.lstatSync(file);', 'if(file.endsWith(".projections.json"))continue; const stat=fs.lstatSync(file);', None, prefix+'regenerates_identically', '#745 checkpoint published file retained'),
    ('twice-early-return', probe, "process.env.MATRIX_TWICE_RETURN==='early'", 'false', None, prefix+'regenerates_identically', '#745 twice return order observed'),
    ('close-early-return', probe, "process.env.MATRIX_CLOSE_RETURN==='early'", 'false', None, prefix+'regenerates_identically', '#745 close return order observed'),
    ('close-disposal-gate', probe, "fence?.[1]==='close'&&!earlyClose", 'false', None, prefix+'regenerates_identically', '#745 close disposal follows first rejection'),
    ('close-distinct-orders', oracle, '"message": "Agent is not registered: session-745"', '"message": "Scoped service is not registered in gateway-agents: agent:session-745"', None, prefix+'fence_idle_close_beforeload', '#745 close distinct return orders'),
    ('twice-disposal-gate', probe, "(fence?.[1]==='twice'&&!earlyTwice||fence?.[1]==='close'&&!earlyClose)&&id===1", "(fence?.[1]==='twice'&&!earlyTwice||fence?.[1]==='close'&&!earlyClose)&&id===999", None, prefix+'regenerates_identically', '#745 twice disposal follows final publication'),
    ('twice-distinct-orders', oracle, '"secondHandle": 2,\n      "returnsSameHandle": false', '"secondHandle": 3,\n      "returnsSameHandle": true', None, prefix+'fence_idle_twice_beforeload', '#745 twice distinct return orders'),
    ('twice-observed-identity', oracle, '"returnsSameHandle": true', '"returnsSameHandle": false', None, prefix+'fence_idle_twice_beforeload', '#745 twice return identity evidence'),
    ('twice-observed-set', oracle, '"secondHandle": 3,\n      "returnsSameHandle": true', '"secondHandle": 2,\n      "returnsSameHandle": false', None, prefix+'fence_idle_twice_beforeload', '#745 twice Rust identity belongs to TS observations'),
    ('writer-lease-removed', factory, '*lock(&storage.writer_lease) = Some(lease);', 'drop(lease);', None, prefix+'writer_lease_excludes_second_factory_and_alias', '#745 lease excludes duplicate writer'),
    ('writer-drain-wait-removed', factory, 'tokio::time::timeout_at(deadline, previous.dispose("session_recreate".into()))\n                    .await', '{ drop(previous.dispose("session_recreate".into())); Ok::<_, tokio::time::error::Elapsed>(Ok(())) }', None, prefix+'recreate_running_before_read', '#745 fence no storage before old drain'),
    ('writer-timeout-candidate', factory, 'if let Err(error) = outcome {\n                let _ = prepared.resources.release().await;\n                return Err(error);', 'if let Err(_error) = outcome {\n                let _ = prepared.resources.release().await;\n                return Ok(previous.clone());', None, prefix+'writer_fence_timeout_returns_no_candidate', '#745 timeout cannot return publishable candidate'),
    ('writer-timeout-code', factory, 'code: Some("SESSION_WRITER_BUSY".into()),', 'code: Some("WRONG".into()),', None, prefix+'writer_fence_timeout_then_owner_resume', '#745 timeout dedicated writer error'),
    ('writer-owner-retry', handle, 'self.state() == HandleState::Disposed && !self.retains_recreation_source()', 'self.state() == HandleState::Disposed', None, prefix+'writer_fence_timeout_then_owner_resume', '#745 timeout owner resumes after drain'),
    # Removing the reset alone is equivalent for this test's idle writer: the
    # lease's immediate try succeeds even with an expired deadline. Charge slow
    # storage to the original deadline inside initialized so normal rollback runs.
    ('writer-slow-storage-budget', factory, 'let deadline = tokio::time::Instant::now() + writer_wait_remaining;\n        let initialized = async {', 'let _ = writer_wait_remaining;\n        let initialized = async {\n            if tokio::time::Instant::now() >= deadline { return Err(writer_busy()); }', None, prefix+'writer_fence_slow_storage_is_not_writer_timeout', '#745 slow storage has full writer budget'),
    ('writer-draining-snapshot', handle, 'self.0.session.draining_reload_snapshot()', 'self.0.session.reload_snapshot()', 'recreation_snapshot', prefix+'recreate_while_storage_close_is_pending', '#745 draining closed adapter still recreates'),
    ('writer-external-lock-error', factory, 'writer_busy()\n            }', 'RuntimeError::new("untyped lock failure")\n            }', None, prefix+'writer_fence_external_ledger_lock_rejects_before_load', '#745 external lease dedicated error'),
    ('writer-lease-retained-after-close', factory, 'lock(&self.writer_lease).take()', 'None', None, prefix+'recreate_running_after_publish_resume', '#745 interleaving new handle'),
    ('writer-snapshot-before-prepare', factory, '        let prepared = (self.options.prepare)(context.clone(), self.mcp.clone()).await?;\n        // Sample before this attempt grants eligibility or starts disposal:\n        // the first cleanup error must still be reported to its caller.\n        let retry_after_drain = previous\n            .as_ref()\n            .is_some_and(LiveSessionHandle::recreation_drained);\n        let captured_reload = previous\n            .as_ref()\n            .map(LiveSessionHandle::recreation_snapshot)\n            .transpose();\n', '        // Sample before this attempt grants eligibility or starts disposal:\n        // the first cleanup error must still be reported to its caller.\n        let retry_after_drain = previous\n            .as_ref()\n            .is_some_and(LiveSessionHandle::recreation_drained);\n        let captured_reload = previous\n            .as_ref()\n            .map(LiveSessionHandle::recreation_snapshot)\n            .transpose();\n        let prepared = (self.options.prepare)(context.clone(), self.mcp.clone()).await?;\n', None, prefix+'recreate_snapshot_rejects_disposal_during_prepare', '#745 prepare disposal rejects recreation'),
    ('writer-dispose-panic-guard', factory, 'lock(&self.writer_lease).take()', 'None', None, prefix+'writer_close_panic_releases_lease', '#745 panic lease released'),
    ('writer-rollback-panic-guard', factory, 'lock(&self.writer_lease).take()', 'None', None, prefix+'writer_rollback_close_panic_releases_lease', '#745 rollback panic lease released'),
    ('writer-already-locked-contract', factory, 'writer_busy()\n            }', 'RuntimeError::new("untyped lock failure")\n            }', None, prefix+'writer_factory_rejects_already_locked_storage', '#1027 locked store factory error contract'),
    ('writer-configured-budget', factory, 'tokio::time::Instant::now() + self.options.writer_busy_wait_timeout', 'tokio::time::Instant::now() + crate::host::Limits::default().close_timeout', None, prefix+'writer_fence_uses_configured_budget', '#745 configured budget exceeds default close timeout'),
    ('fence-runtime-registry', test_source, 'serde_json::json!(fence_handle_id(h, &handle))', 'serde_json::json!(fence_handle_id(h, &handle) + 1)', None, prefix+'fence_idle_recreate_beforeload', '#745 fence runtime complete observation fence-idle-recreate-beforeload'),
    ('fence-runtime-results', test_source, '"handle":fence_handle_id(h,handle)', '"handle":fence_handle_id(h,handle) + 1', None, prefix+'fence_idle_twice_beforeload', '#745 fence runtime complete observation fence-idle-twice-beforeload'),
    ('fence-runtime-predecessor', test_source, 'HandleState::Disposed => "disposed"', 'HandleState::Disposed => "active"', None, prefix+'fence_idle_recreate_beforeload', '#745 fence runtime complete observation fence-idle-recreate-beforeload'),
    ('prepare-dispose-probe', probe, "process.env.MATRIX_DISPOSE_PREPARE==='1'&&composition===2", "process.env.MATRIX_DISPOSE_PREPARE==='1'&&composition===999", None, prefix+'regenerates_identically', '#745 prepare disposal rejects TS recreate'),
    ('writer-release-hook-order', factory, 'let writer_lease = self.storage.take_writer_lease();', 'let writer_lease = self.storage.take_writer_lease(); self.storage.recorder.writer().begin_close();', None, prefix+'writer_disposal_preserves_final_hooks', '#745 release hook final write committed'),
    ('writer-close-hook-order', factory, 'let result = (self.close)().await;', 'self.recorder.writer().begin_close(); let result = (self.close)().await;', None, prefix+'writer_rollback_close_preserves_final_hook', '#745 close hook final write committed'),
    ('fence-runtime-error', test_source, 'serde_json::json!({"name":error.js_error().map_or("Error", |e|e.name.as_str()), "message":error.message})', 'serde_json::json!({"name":error.js_error().map_or("Error", |e|e.name.as_str()), "message":"wrong"})', None, prefix+'fence_idle_shutdown_beforeload', '#745 fence runtime complete observation fence-idle-shutdown-beforeload'),
    ('fence-runtime-inflight', handle, 'usize::from(\n            self.0\n                .progress\n                .lock()\n                .expect("#745 handle progress")\n                .activity,\n        )', '0', None, prefix+'fence_never_recreate_beforeload', '#745 fence runtime complete observation fence-never-recreate-beforeload'),
    ('writer-panic-sync-drain', factory, 'std::thread::spawn(close);', 'close();', None, prefix+'writer_panic_cleanup_allows_runtime_progress', '#745 panic cleanup permits runtime progress'),
    ('writer-panic-early-unlock', factory, 'let close = move || {\n            writer.close();\n            drop(lease);\n        };', 'drop(lease); let close = move || { writer.close(); };', None, prefix+'writer_panic_cleanup_allows_runtime_progress', '#745 panic retains lease through admitted write'),
    ('writer-panic-retirement-removed', factory, '        self.writer.begin_close();\n        let writer = self.writer.clone();\n        let lease = self._lease.take();\n        let close = move || {\n            writer.close();\n            drop(lease);\n        };\n        // An independent blocking thread also works when a future is dropped\n        // after its runtime has exited. It owns the lease through the drain.\n        std::thread::spawn(close);', '        let _ = self._lease.take();', None, prefix+'writer_rollback_close_panic_releases_lease', '#745 rollback panic writer retired'),
    ('same-key-ticket-bypassed', owner, 'let _exclusive = ticket.enter().await;\n            let outcome = owner.create_active(context).await;', 'let _exclusive = ticket;\n            let outcome = owner.create_active(context).await;', None, prefix+'same_key_ticket_blocks_before_publication', '#1022 ticket second cannot enter before first publication'),
    ('writer-unexpired-try-only', factory, 'tokio::time::timeout_at(deadline, mutex.lock_owned())\n            .await\n            .map_err(|_| writer_busy())', 'mutex.try_lock_owned().map_err(|_| writer_busy())', None, prefix+'writer_waiters_are_fifo_and_cancellation_safe', '#1022 FIFO B waits'),
    ('writer-wait-never-times-out', factory, 'tokio::time::timeout_at(deadline, mutex.lock_owned())', 'tokio::time::timeout_at(deadline + std::time::Duration::from_secs(60), mutex.lock_owned())', None, prefix+'writer_waiters_are_fifo_and_cancellation_safe', '#1022 FIFO expiry is ready'),
    ('writer-resolution-charged-as-total', factory, 'let path = resolved.await?;', 'let path = tokio::time::timeout_at(deadline, resolved).await.map_err(|_| writer_busy())??;', None, prefix+'writer_busy_budget_excludes_path_resolution', '#1022 path IO excluded from busy budget'),
    ('drain-file-state-not-refreshed', factory, 'Ok(file_state) => reload.file_state = file_state,', 'Ok(_file_state) => {}', None, prefix+'recreation_uses_file_state_saved_during_drain', '#1022 seed replacement retains drained read state'),
    ('drain-file-state-error-ignored', factory, 'match previous.drained_file_state() {', 'match previous.drained_file_state().or_else(|_| Ok(SeedState::default())) {', None, prefix+'failed_drained_seed_releases_prepared_resources', '#1022 seed failure propagated'),
    ('drain-file-state-error-leaks-prepared', factory, 'let _ = prepared.resources.release().await;\n                    return Err(error);\n                }\n            }\n        }\n        let writer_wait_remaining', 'std::mem::forget(prepared.resources);\n                    return Err(error);\n                }\n            }\n        }\n        let writer_wait_remaining', None, prefix+'failed_drained_seed_releases_prepared_resources', '#1022 seed failure releases prepared project lease'),
    ('drain-turn-drop-seed-removed', factory, 'if !self.finished\n            && self.started.load(Ordering::SeqCst)\n            &&', 'if false\n            && self.started.load(Ordering::SeqCst)\n            &&', None, prefix+'recreation_uses_file_state_saved_during_drain', '#1022 seed drop publishes final checkpoint'),
    ('drain-cancelled-candidate-not-disposed', factory, 'let _ = handle.dispose("session_create_cancelled".into()).await;', 'let _ = handle;', None, prefix+'cancelled_recreation_finishes_drain_and_candidate_cleanup', '#1022 seed cancelled cleanup bounded wait'),
    ('abort-session-cleanup-skipped', handle, 'if let Err(error) = handle.0.session.dispose().await {', 'if let Err(error) = if failures.is_empty() { handle.0.session.dispose().await } else { Ok(()) } {', None, prefix+'writer_abort_panic_releases_lease_before_session_dispose', '#1022 abort panic releases project resources with old handle retained'),
    ('followups-regeneration', oracle, '"name": "abort-throw",', '"name": "wrong-abort-throw",', None, prefix+'followups_regenerates_identically', '#280 session-factory/oracle.json differs'),

    ('abort-str-payload-discarded', handle, '.or_else(|| panic.downcast_ref::<&str>().copied())', '.or(None)', None, prefix+'writer_abort_panic_releases_lease_before_session_dispose', '#1022 abort original panic retained'),
    ('abort-string-payload-discarded', handle, '.map(String::as_str)', '.and_then(|_| None)', None, prefix+'writer_abort_owned_string_panic_retains_error', '#1022 abort original panic retained'),
    ('abort-non-string-fallback-changed', handle, '.unwrap_or("Agent session abort panicked.")', '.unwrap_or("WRONG")', None, prefix+'writer_abort_non_string_panic_retains_fallback', '#1022 abort non-string fallback retained'),
    ('drain-retry-source-not-retained', handle, 'recreation_started = true;', 'recreation_started = false;', None, prefix+'recreation_retry_after_dispose_error_retains_drained_seed', '#1022 seed retry succeeds after completed drain'),
    ('drain-retry-owner-drops-source', owner, '|| record.handle.retains_recreation_source()', '|| false', None, prefix+'recreation_retry_after_dispose_error_retains_drained_seed', '#1022 seed retry retains drained read state'),
    ('drain-retry-repeats-dispose-error', factory, '&& !retry_after_drain', '&& { let _ = retry_after_drain; true }', None, prefix+'recreation_retry_after_dispose_error_retains_drained_seed', '#1022 seed retry succeeds after completed drain'),
    ('drain-retry-owner-repeats-error', owner, 'if !already_drained {', 'if { let _ = already_drained; true } {', None, prefix+'recreation_retry_after_dispose_error_retains_drained_seed', '#1022 seed retry succeeds after completed drain'),
    ('drain-timeout-retry-loses-seed', owner, '|| record.handle.retains_recreation_source()', '|| false', None, prefix+'recreation_retry_after_timeout_retains_drained_seed', '#1022 seed retry retains drained read state'),
    ('drain-read-error-retry-loses-seed', owner, '|| record.handle.retains_recreation_source()', '|| false', None, prefix+'recreation_retry_after_seed_error_retains_drained_seed', '#1022 seed retry retains drained read state'),
    ('dispose-followups-regeneration', oracle, '"name": "dispose-throw",', '"name": "wrong-dispose-throw",', None, prefix+'dispose_failure_followups_regenerates_identically', '#280 session-factory/oracle.json differs'),
    ('dispose-followups-probe-error', Path(__file__).with_name('followups.mts'), "if(name==='dispose-throw')throw Error('DISPOSE_1022')", "if(false)throw Error('DISPOSE_1022')", None, prefix+'dispose_failure_followups_regenerates_identically', '#1022 dispose error retained'),
    ('drain-retry-skips-active-predecessor', handle, '&& !progress.activity', '&& true', 'recreation_drained', prefix+'recreation_retry_rejects_disposed_predecessor_with_active_turn', '#1022 seed retry active predecessor cannot create storage'),
    ('drain-first-dispose-error-swallowed', factory, 'if let Err(error) = outcome {', 'if let Err(error) = outcome && !previous.recreation_drained() {', None, prefix+'recreation_retry_after_dispose_error_retains_drained_seed', '#1022 seed retry first failure propagated'),
]

def audit():
    rows = []
    for name, path, old, new, scope, test, assertion in cases:
        try:
            original = path.read_text()
            changed = mutate(original, scope, [(old, new)])
            if changed == original:
                raise ValueError('mutation leaves source unchanged')
            source = factory.with_name('factory_tests.rs').read_text()
            test_name = test.removeprefix(prefix)
            compact = ''.join(source.split())
            if 'fn' + test_name + '(' not in compact and test_name + '=>' not in compact:
                raise ValueError('bound full test not found: ' + test)
            # Static audit checks a literal/formatted failure marker, not semantic
            # inequality. A compiled runtime kill proves the latter.
            literals = source.split('"')[1::2]
            def marker_matches(literal):
                pieces = literal.split('{')
                parts = [pieces[0]] + [piece.split('}', 1)[-1] for piece in pieces[1:]]
                if not assertion.startswith(parts[0]) or not assertion.endswith(parts[-1]):
                    return False
                offset = 0
                for part in parts:
                    at = assertion.find(part, offset)
                    if at < 0:
                        return False
                    offset = at + len(part)
                return True
            if not test_name.endswith('regenerates_identically') and not any(
                marker_matches(literal) for literal in literals
                if '#745 ' in literal or '#1022 ' in literal or '#1027 ' in literal):
                raise ValueError('bound failure message not found: ' + assertion)
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
    args = ['--no-run'] if compile else [test,'--','--exact','--include-ignored']
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
    invalidate_completed(results, f'source guard: {error}')
    results.extend(dict(mutation=c[0],test=c[5],assertion=c[6],status='INVALID',compiled=False,reason=f'source guard: {error}') for c in chosen if not any(row['mutation']==c[0] for row in results))
counts={status:sum(r['status']==status for r in results) for status in ('KILLED','SURVIVED','INVALID','COMPILED')}
with (out/'summary.json').open('x') as summary:
    json.dump(dict(controls=results,counts=counts),summary,indent=2);summary.write('\n')
print(json.dumps(counts),flush=True)
raise SystemExit(int(any(r['status']!=('COMPILED' if compile_only else 'KILLED') for r in results)))
