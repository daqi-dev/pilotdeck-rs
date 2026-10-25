#!/usr/bin/env python3
"""Compile temporary product mutations; require a failing real-TS replay, restore bytes.

Run exclusively. No fixture or test is mutated. Compile failures and timeouts
are INVALID evidence. All process cleanup targets this script's own group.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
repo = rust.parent
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
source = rust / 'pilotdeck-gateway/src/runtime/turn/submit.rs'
host_source = rust / 'pilotdeck-gateway/src/runtime/turn/submit_host.rs'
with mutating_sources(repo, [source, host_source]) as snapshots:
    original = snapshots[source]
    host_original = snapshots[host_source]
    env = dict(os.environ, CARGO_NET_OFFLINE='true')
    assert Path(env['CARGO_TARGET_DIR']).resolve() == rust / 'target'
    assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
    test = 'runtime::turn::submit::tests'
    host_test = 'runtime::turn::submit_host::tests'
    # Each mutation changes production behavior. The chosen case names are real
    # generated scenarios; all golden assertions remain unchanged.
    mutants = [
        ('permission-check-skipped', 'let value = field(input, key);', 'let value = &JsValue::Undefined;', 'invalid-mode', 'invalid-mode: step', None),
        ('run-id-check-skipped', 'let id = field(input, "runId");', 'let id = &JsValue::Undefined;', 'invalid-run-id', 'invalid-run-id: step', None),
        ('budget-check-skipped', 'let budget = field(input, "maxBudgetUsd");', 'let budget = &JsValue::Undefined;', 'invalid-budget', 'invalid-budget: step', None),
        ('trusted-context-check-skipped', 'if let value = field(input, "trustedContext")', 'if let value = &JsValue::Undefined', 'invalid-trusted-context', 'invalid-trusted-context: step', None),
        ('busy-code-changed', 'if sdk_admission {\n                    "SESSION_BUSY"\n                } else {\n                    "session_busy"\n                }', 'if sdk_admission { "session_busy" } else { "SESSION_BUSY" }', 'session-busy', 'session-busy: step', None),
        ('sdk-capability-ignored', 'let error = if !self.host.has_sdk_config() {', 'let error = if false && !self.host.has_sdk_config() {', 'sdk-unavailable', 'sdk-unavailable: step', None),
        ('sdk-validation-ignored', '.validate_sdk_config(field(&input, "sdkSessionConfig"))\n                    .err()', '.validate_sdk_config(field(&input, "sdkSessionConfig"))\n                    .err().filter(|_| false)', 'sdk-invalid', 'sdk-invalid: step', None),
        ('sdk-dirty-skipped', '.mark_session_dirty(&run.session_key, "sdk_session_config_changed");', '.mark_session_dirty(&run.session_key, "");', 'sdk-changed', 'changed SDK config must dirty the cached handle', None),
        ('sdk-abort-ignored', 'let event = if completion.is_cancelled() {', 'let event = if false && completion.is_cancelled() {', 'abort-during-sdk-failure', 'abort-during-sdk-failure: step', None),
        ('refresh-error-propagated', 'let _ = self.host.refresh().await;', 'self.host.refresh().await?;', 'refresh-failed-keeps-snapshot', 'refresh-failed-keeps-snapshot: step', None),
        ('accepted-commit-skipped', 'self.host.commit_accepted_input(run).await?;', 'let _ = run;', 'commit-failed', 'commit-failed: step', None),
        ('projection-drop-event', '                publisher.gateway_event(event);\n            }\n            if let Some(guard) = &mut failure_guard', '                let _ = event;\n            }\n            if let Some(guard) = &mut failure_guard', 'durable-accept-and-session-end', 'durable-accept-and-session-end: step', None),
        ('projection-duplicate-event', '                publisher.gateway_event(event);\n            }\n            if let Some(guard) = &mut failure_guard', '                publisher.gateway_event(event.clone()); publisher.gateway_event(event);\n            }\n            if let Some(guard) = &mut failure_guard', 'durable-accept-and-session-end', 'durable-accept-and-session-end: step', None),
        ('early-terminal-before-session-end', '                publisher.gateway_event(event);\n            }\n            if let Some(guard) = &mut failure_guard', '                let terminal = is(&event, "type", "turn_completed"); publisher.gateway_event(event); if terminal { publisher.finish().await; }\n            }\n            if let Some(guard) = &mut failure_guard', 'durable-accept-and-session-end', 'durable-accept-and-session-end: step', None),
        ('finally-error-swallowed', 'self.host.after_turn(&run)', 'let _ = self.host.after_turn(&run); Ok(())', 'finally-failed', 'finally-failed: step', None),
        ('input-ack-before-native-durable', 'let mut stream = handle\n            .submit(prepared.input, prepared.options)', 'publisher.gateway_event(object([("type", "input_accepted".into()), ("runId", JsValue::String(run.run_id.clone()))]));\n        let mut stream = handle\n            .submit(prepared.input, prepared.options)', 'durable-accept-and-session-end', 'durable-accept-and-session-end: step', None),
        ('stale-completion-filter-skipped', '|| !self\n                    .owner\n                    .is_current_completion(&run.session_key, completion)', '|| false', 'superseded-completion', 'superseded-completion: step', None),
        ('model-selection-event-skipped', 'publisher.gateway_event(model_event(model, &source, run));', 'let _ = model;', 'actual-model-selection', 'actual-model-selection: step', None),
        ('actual-text-model-skipped', 'set(&mut event, "model", model.clone());', 'let _ = model;', 'actual-model-selection', 'actual-model-selection: step', None),
        ('lease-release-skipped', 'let _ = submitter.host.finish_pump(&worker_run).await;', 'let _ = &worker_run;', 'lease-release', 'lease-release: step', None),
        ('fingerprint-includes-reconnect-metadata', '["runId", "interactionBinding", "telemetry"]', '["ignoredRunId", "ignoredBinding", "ignoredTelemetry"]', 'execution-fingerprint', 'execution-fingerprint: step', None),
        ('explicit-null-sdk-treated-absent', 'let sdk_admission = !matches!(field(&input, "sdkSessionConfig"), JsValue::Undefined);', 'let sdk_admission = sdk;', 'sdk-null-busy', 'sdk-null-busy: step', None),
        ('cancel-hides-recovery-failure', 'Ok(events) if events.is_empty() && completion.is_cancelled()', 'Ok(events) if completion.is_cancelled()', 'abort-and-recovery-failed', 'abort-and-recovery-failed: step', None),
        ('history-epoch-error-hidden', 'if let Err(error) = outcome', 'if let Err(error) = outcome && error.message != "history epoch changed"', 'history-epoch-failed', 'history-epoch-failed: step', None),
        ('utf16-mode-lossy', 'JsString::from_utf16(message)', 'String::from_utf16_lossy(&message).into()', 'invalid-mode', 'failed: message', None),
        ('utf16-busy-lossy', 'JsValue::String(JsString::from_utf16(units))\n}', 'JsValue::String(String::from_utf16_lossy(&units).into())\n}', 'session-busy', 'UTF-16 busy status message', None),
        ('workspace-reservation-skipped', 'if !self.workspace_admissions.reserve(id, &run)', 'if false && !self.workspace_admissions.reserve(id, &run)', 'workspace-busy-before-sdk', 'workspace-busy-before-sdk: step', None),
        ('workspace-release-skipped', '.release(&workspace, self.run);', '.release(&workspace, &RunRef { session_key: "wrong".into(), project_key: None, run_id: "wrong".into() });', 'workspace-released-after-turn', 'workspace-released-after-turn: step', None),
        ('cwd-update-skipped', 'self.host.set_session_cwd(&run.session_key, cwd)', 'Ok::<(), SubmitError>(())', 'workspace-cwd', 'workspace-cwd: step', None),
        ('timeout-disabled', 'tokio::time::sleep_until(started + Duration::from_millis(delay)).await;', 'tokio::time::sleep_until(started + Duration::from_millis(delay + 10000)).await;', 'timeout-active', 'timeout-active: step', None),
        ('timeout-status-skipped', 'publisher.gateway_event(with_run(failure, &run));', 'let _ = failure;', 'timeout-active', 'timeout-active: step', None),
        ('timeout-error-skipped', 'publisher.gateway_event(event);\n                    publisher.close_live();', 'let _ = event;\n                    publisher.close_live();', 'timeout-active', 'timeout-active: step', None),
        ('timeout-abort-skipped', 'let _ = handle.abort(&format!("timeout:{}", display(&run.run_id)).into());', 'let _ = &handle;', 'timeout-active', 'timeout-active: step', None),
        ('timeout-close-skipped', 'let close = self.owner.close(&run.session_key);', 'let close = std::future::ready(Ok::<(), crate::runtime::project::RuntimeError>(()));', 'timeout-active', 'timeout-active: step', None),
        ('timeout-prepare-revived', 'if timed_out.load(Ordering::SeqCst) {\n            return Ok(());\n        }', 'if false && timed_out.load(Ordering::SeqCst) { return Ok(()); }', 'timeout-prepare-no-revival-live-consumer', 'timeout-prepare-no-revival-live-consumer: step', None),
        ('deadline-skipped', '"operationDeadline",\n                deadline.as_str().into(),', '"wrongDeadline",\n                deadline.as_str().into(),', 'timeout-active', 'timeout-active: step', None),
        ('timeout-status-record-error-ignored', 'if self.host.record_failure(&run, &record).await.is_ok()', 'if self.host.record_failure(&run, &record).await.is_err()', 'timeout-status-record-failed', 'timeout-status-record-failed: step', None),
        ('snapshot-pre-skipped', 'recorder.capture(snapshot, true).await?', 'recorder.capture(snapshot, false).await?', 'snapshot-success', 'snapshot-success: step', None),
        ('snapshot-pre-error-ignored', 'if result.state != SnapshotState::Committed', 'if false && result.state != SnapshotState::Committed', 'snapshot-pre-failed', 'snapshot-pre-failed: step', None),
        ('snapshot-post-skipped', 'let _ = recorder.capture(input, false).await;', 'let _ = input;', 'snapshot-agent-failed', 'snapshot-agent-failed: step', None),
        ('snapshot-success-post-added', '!progress.committed || (progress.succeeded && !timed_out)', '!progress.committed', 'snapshot-success', 'snapshot-success: step', None),
        ('snapshot-timeout-kind-wrong', '"timeout".into()\n            } else', '"unknown".into()\n            } else', 'timeout-snapshot', 'timeout-snapshot: step', None),
        ('timeout-live-close-skipped', 'publisher.close_live();', 'let _ = &publisher;', 'timeout-active', 'timeout-active: step', None),
        ('invalid-deadline-accepted', 'value.abs() > 8_640_000_000_000_000.0', 'value.abs() > f64::MAX', 'timeout-invalid-deadline', 'timeout-invalid-deadline: step', None),
        ('failureguard-model-observe-skipped', '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "turn_continued") && let Some(trip) = guard.observe(&event)', 'guard-model-trip', 'guard-model-trip: step', None),
        ('snapshot-capability-refusal-skipped', 'if workspace.snapshots_required &&', 'if false && workspace.snapshots_required &&', 'invalid-mode', 'snapshot-recorder-unavailable: explicit capability refusal', 'snapshot-recorder-unavailable'),
        ('snapshot-cwd-refusal-skipped', 'return Err(SubmitError::new(\n                    "CAPABILITY_UNAVAILABLE",\n                    "Workspace snapshot cwd is unavailable.",\n                ));', '/* silently skip missing snapshot cwd */', 'invalid-mode', 'snapshot-cwd-unavailable: explicit capability refusal', 'snapshot-cwd-unavailable'),
        ('cwd-capability-error-ignored', 'let Err(error) = self.host.set_session_cwd(&run.session_key, cwd)', 'let Err(error) = self.host.set_session_cwd(&run.session_key, cwd).or(Ok::<(), SubmitError>(()))', 'invalid-mode', 'cwd-host-unavailable: explicit capability refusal', 'cwd-host-unavailable'),
        # Production-host mutants live in submit_host.rs; the lease guard's
        # exactly-once assertion lives in the unit tests there. The host lease
        # mutant skips the release outright: a mutation that would release
        # twice cannot be written against the FnOnce ownership type, so the
        # repeated-finish_pump counter assertion is the double-release guard.
        ('guard-config-not-provided', 'self.services\n            .run_policy()\n            .and_then(|policy| policy.get("failureGuard").cloned())', 'None', 'guard-model-trip', 'guard-model-trip: step', None),
        ('host-lease-release-skipped', 'let release = self\n            .leases\n            .lock()\n            .unwrap_or_else(|poisoned| poisoned.into_inner())\n            .remove(&(run.session_key.clone(), run.run_id.clone()));', 'let release: Option<SubmitLeaseRelease> = None;', None, 'finish_pump_releases_a_lease_exactly_once', None),
        # opus P2-1: the host's own cwd capability refusal, distinct from the
        # submit.rs call-site mutant cwd-capability-error-ignored above.
        ('cwd-host-unavailable-ok', 'None => Err(SubmitError::new(\n                "CAPABILITY_UNAVAILABLE",\n                "Session cwd host capability is unavailable.",\n            )),', 'None => Ok(()),', 'invalid-mode', 'cwd-host-unavailable: explicit capability refusal', 'cwd-host-unavailable'),
        # DeepSeek P1-1: reverting either poison-tolerant lock to unwrap()
        # strands the lease after an unrelated panic.
        ('lease-poison-unwrap-finish', 'unwrap_or_else(|poisoned| poisoned.into_inner())\n            .remove(', 'unwrap()\n            .remove(', None, 'finish_pump_releases_a_lease_after_the_leases_mutex_is_poisoned', None),
        ('lease-poison-unwrap-prepare', 'unwrap_or_else(|poisoned| poisoned.into_inner())\n                    .insert(', 'unwrap()\n                    .insert(', None, 'finish_pump_releases_a_lease_after_the_leases_mutex_is_poisoned', None),
    ]
    mutant_source = {'guard-config-not-provided': host_source,
                     'host-lease-release-skipped': host_source,
                     'cwd-host-unavailable-ok': host_source,
                     'lease-poison-unwrap-finish': host_source,
                     'lease-poison-unwrap-prepare': host_source}
    mutant_test = {'host-lease-release-skipped': host_test,
                   'lease-poison-unwrap-finish': host_test,
                   'lease-poison-unwrap-prepare': host_test}
    summary = []
    try:
        for name, before, _, _, _, _ in mutants:
            anchor_source = mutant_source.get(name, source)
            anchor_text = (host_original if anchor_source == host_source else original).decode()
            assert anchor_text.count(before) == 1, (name, 'anchor is not unique')
        (output / 'original-sha256.json').write_text(json.dumps({
            str(source.relative_to(repo)): hashlib.sha256(original).hexdigest(),
            str(host_source.relative_to(repo)): hashlib.sha256(host_original).hexdigest()}, indent=2)+'\n')
        selected = [('baseline', None, None, None, None, None)] + mutants
        if len(sys.argv) > 2:
            selected = [entry for entry in selected if entry[0] in sys.argv[2:]]
            assert selected, 'unknown control'
        for name, before, after, case, marker, capability in selected:
            assert source.read_bytes() == original and host_source.read_bytes() == host_original, 'source changed outside this run'
            target = mutant_source.get(name, source)
            target_original = host_original if target == host_source else original
            used_test = mutant_test.get(name, test)
            if before: target.write_text(target_original.decode().replace(before, after, 1))
            slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True, timeout=10)
            slot_deadline = time.monotonic() + 1800
            while slots.returncode == 1:
                assert time.monotonic() < slot_deadline, 'cargo slots unavailable for 30 minutes'
                time.sleep(30)
                slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True, timeout=10)
            assert slots.returncode == 0
            command = ['cargo', 'test', '--offline', '--manifest-path', str(rust / 'Cargo.toml'), '-p', 'pilotdeck-gateway', '--lib', used_test, '--', '--nocapture']
            run_env = dict(env)
            if case: run_env['PDRS_SUBMIT_CASE'] = case
            else: run_env.pop('PDRS_SUBMIT_CASE', None)
            if capability: run_env['PDRS_SUBMIT_CAPABILITY'] = capability
            else: run_env.pop('PDRS_SUBMIT_CAPABILITY', None)
            timed_out = False
            try:
                run = run_owned(command, cwd=repo, env=run_env, timeout=240)
                raw = run.stdout + run.stderr
                returncode = run.returncode
                if name == 'baseline':
                    # The production-host unit filter must also be green before
                    # any mutation of submit_host.rs is judged.
                    host_run = run_owned(['cargo', 'test', '--offline', '--manifest-path', str(rust / 'Cargo.toml'), '-p', 'pilotdeck-gateway', '--lib', host_test, '--', '--nocapture'], cwd=repo, env=run_env, timeout=240)
                    raw += '\n' + host_run.stdout + host_run.stderr
                    returncode = returncode or host_run.returncode
            except subprocess.TimeoutExpired as error:
                timed_out = True
                raw = (error.output or '') + (error.stderr or '')
                returncode = -signal.SIGKILL
            finally:
                target.write_bytes(target_original)
            valid = not timed_out and ((returncode == 0 and 'test result: ok. 4 passed' in raw and 'test result: ok. 10 passed' in raw) if name == 'baseline' else (
                returncode == 101 and 'test result: FAILED' in raw and 'could not compile' not in raw
                # Product panics (a poison-unwrap mutant) are failure evidence
                # alongside assertion diffs.
                and ('assertion' in raw or 'panicked at' in raw) and marker in raw and used_test in raw))
            (output / f'{name}.log').write_text('COMMAND '+' '.join(command)+'\n'+slots.stdout+slots.stderr+raw+f'\nexit={returncode}\n')
            summary.append(dict(name=name, case=case, before=before, after=after, exit=returncode, marker=marker, valid=valid))
            (output/'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2)+'\n')
            print(f'{name}: {"PASS" if valid else "INVALID"} exit={returncode}', flush=True)
            if not valid: raise SystemExit(1)
    finally:
        source.write_bytes(original)
        host_source.write_bytes(host_original)
        assert source.read_bytes() == original
        assert host_source.read_bytes() == host_original
    print(f'{len(summary)} controls checked; original bytes restored', flush=True)
