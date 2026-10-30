#!/usr/bin/env python3
"""M6 guarded turn-submit mutations. --check is read-only; no git-object override."""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _negkit.contract import Case
from _negkit import submit as adapter

fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
repo = rust.parent
source = rust / 'pilotdeck-gateway/src/runtime/turn/submit.rs'
host_source = rust / 'pilotdeck-gateway/src/runtime/turn/submit_host.rs'
test_source = rust / 'pilotdeck-gateway/src/runtime/turn/submit_tests.rs'

test = 'runtime::turn::submit::tests'
host_test = 'runtime::turn::submit_host::tests'
submit_replay = f'{test}::real_ts_submit_scenarios'
capability_replay = f'{test}::unavailable_capabilities_are_explicitly_refused'
utf16_replay = f'{test}::real_ts_utf16_preflight'
lease_order = f'{test}::lease_release_precedes_after_turn_across_exit_paths'
host_exactly_once = f'{host_test}::finish_pump_releases_a_lease_exactly_once'
host_poison = f'{host_test}::finish_pump_releases_a_lease_after_the_leases_mutex_is_poisoned'
# Each mutation changes production behavior. The chosen case names are real
# generated scenarios; all golden assertions remain unchanged.
mutants = [
    ('permission-check-skipped', 'let value = field(input, key);', 'let value = &JsValue::Undefined;', 'invalid-mode', '#813 invalid-mode: step 0 "observe": submit scenario must match', None),
    ('run-id-check-skipped', 'let id = field(input, "runId");', 'let id = &JsValue::Undefined;', 'invalid-run-id', '#813 invalid-run-id: step 0 "observe": submit scenario must match', None),
    ('budget-check-skipped', 'let budget = field(input, "maxBudgetUsd");', 'let budget = &JsValue::Undefined;', 'invalid-budget', '#813 invalid-budget: step 0 "observe": submit scenario must match', None),
    ('trusted-context-check-skipped', 'let value = field(input, "trustedContext")', 'let value = &JsValue::Undefined', 'invalid-trusted-context', '#813 invalid-trusted-context: step 0 "observe": submit scenario must match', None),
    ('busy-code-changed', 'if sdk_admission {\n                    "SESSION_BUSY"\n                } else {\n                    "session_busy"\n                }', 'if sdk_admission { "session_busy" } else { "SESSION_BUSY" }', 'session-busy', '#813 session-busy: step 0 "observe": submit scenario must match', None),
    ('sdk-capability-ignored', 'let error = if !self.host.has_sdk_config() {', 'let error = if false && !self.host.has_sdk_config() {', 'sdk-unavailable', '#813 sdk-unavailable: step 0 "observe": submit scenario must match', None),
    ('sdk-validation-ignored', '.validate_sdk_config(field(&input, "sdkSessionConfig"))\n                    .err()', '.validate_sdk_config(field(&input, "sdkSessionConfig"))\n                    .err().filter(|_| false)', 'sdk-invalid', '#813 sdk-invalid: step 0 "observe": submit scenario must match', None),
    ('sdk-dirty-skipped', '.mark_session_dirty(&run.session_key, "sdk_session_config_changed");', '.mark_session_dirty(&run.session_key, "");', 'sdk-changed', 'changed SDK config must dirty the cached handle', None),
    ('sdk-abort-ignored', 'let event = if completion.is_cancelled() {', 'let event = if false && completion.is_cancelled() {', 'abort-during-sdk-failure', '#813 abort-during-sdk-failure: step 2 "sdk": submit scenario must match', None),
    ('refresh-error-propagated', 'let _ = self.host.refresh().await;', 'self.host.refresh().await?;', 'refresh-failed-keeps-snapshot', '#813 refresh-failed-keeps-snapshot: step 0 "observe": submit scenario must match', None),
    ('accepted-commit-skipped', 'self.host.commit_accepted_input(run).await?;', 'let _ = run;', 'commit-failed', '#813 commit-failed: step 1 "accept": submit scenario must match', None),
    ('projection-drop-event', '                publisher.gateway_event(event);\n            }\n            if let Some(guard) = &mut failure_guard', '                let _ = event;\n            }\n            if let Some(guard) = &mut failure_guard', 'durable-accept-and-session-end', '#813 durable-accept-and-session-end: step 0 "observe": submit scenario must match', None),
    ('projection-duplicate-event', '                publisher.gateway_event(event);\n            }\n            if let Some(guard) = &mut failure_guard', '                publisher.gateway_event(event.clone()); publisher.gateway_event(event);\n            }\n            if let Some(guard) = &mut failure_guard', 'durable-accept-and-session-end', '#813 durable-accept-and-session-end: step 0 "observe": submit scenario must match', None),
    ('early-terminal-before-session-end', '                publisher.gateway_event(event);\n            }\n            if let Some(guard) = &mut failure_guard', '                let terminal = is(&event, "type", "turn_completed"); publisher.gateway_event(event); if terminal { publisher.finish().await; }\n            }\n            if let Some(guard) = &mut failure_guard', 'durable-accept-and-session-end', '#813 durable-accept-and-session-end: step 2 "terminal": submit scenario must match', None),
    ('finally-error-swallowed', 'self.host.after_turn(&run)', 'let _ = self.host.after_turn(&run); Ok(())', 'finally-failed', '#813 finally-failed: step 3 "end": submit scenario must match', None),
    ('input-ack-before-native-durable', 'let mut stream = handle\n            .submit(prepared.input, prepared.options)', 'publisher.gateway_event(object([("type", "input_accepted".into()), ("runId", JsValue::String(run.run_id.clone()))]));\n        let mut stream = handle\n            .submit(prepared.input, prepared.options)', 'durable-accept-and-session-end', '#813 durable-accept-and-session-end: step 0 "observe": submit scenario must match', None),
    ('stale-completion-filter-skipped', '|| !self\n                    .owner\n                    .is_current_completion(&run.session_key, completion)', '|| false', 'superseded-completion', '#813 superseded-completion: step 2 "accept": submit scenario must match', None),
    # fix-r2 (opus r2 concurrency #1): the request_started dedup event is
    # built from four explicit keys. Both mutants share this anchor: one
    # drops the event, the other reverts to the shared model_event fold —
    # which now leaks the frame's own reasoning/temperature/speed into
    # the wire (the regenerated golden carries those keys on the 'actual'
    # request_started frame, and TS :1661-1667 still emits four keys).
    ('model-selection-event-skipped', 'publisher.gateway_event(object([\n                        ("type", "model_selection_changed".into()),\n                        ("provider", field(model, "provider").clone()),\n                        ("model", field(model, "model").clone()),\n                        ("source", source.clone()),\n                        ("runId", JsValue::String(run.run_id.clone())),\n                    ]));', 'let _ = model;', 'actual-model-selection', '#813 actual-model-selection: step 2 "terminal": submit scenario must match', None),
    ('request-started-fold-leaked', 'publisher.gateway_event(object([\n                        ("type", "model_selection_changed".into()),\n                        ("provider", field(model, "provider").clone()),\n                        ("model", field(model, "model").clone()),\n                        ("source", source.clone()),\n                        ("runId", JsValue::String(run.run_id.clone())),\n                    ]));', 'publisher.gateway_event(model_event(model, &source, run));', 'actual-model-selection', '#813 actual-model-selection: step 2 "terminal": submit scenario must match', None),
    ('actual-text-model-skipped', 'set(&mut event, "model", model.clone());', 'let _ = model;', 'actual-model-selection', '#813 actual-model-selection: step 2 "terminal": submit scenario must match', None),
    ('lease-release-skipped', 'let _ = submitter.host.finish_pump(&worker_run).await;', 'let _ = &worker_run;', 'lease-release', '#813 lease-release: step 3 "end": submit scenario must match', None),
    ('fingerprint-includes-reconnect-metadata', '["runId", "interactionBinding", "telemetry"]', '["ignoredRunId", "ignoredBinding", "ignoredTelemetry"]', 'execution-fingerprint', '#813 execution-fingerprint: step 0 "observe": submit scenario must match', None),
    ('explicit-null-sdk-treated-absent', 'let sdk_admission = !matches!(field(&input, "sdkSessionConfig"), JsValue::Undefined);', 'let sdk_admission = sdk;', 'sdk-null-busy', '#813 sdk-null-busy: step 0 "observe": submit scenario must match', None),
    ('cancel-hides-recovery-failure', 'Ok(events) if events.is_empty() && completion.is_cancelled()', 'Ok(events) if completion.is_cancelled()', 'abort-and-recovery-failed', '#813 abort-and-recovery-failed: step 2 "recovery": submit scenario must match', None),
    ('history-epoch-error-hidden', 'if let Err(error) = outcome', 'if let Err(error) = outcome && error.message != "history epoch changed"', 'history-epoch-failed', '#813 history-epoch-failed: step 0 "observe": submit scenario must match', None),
    ('utf16-mode-lossy', 'JsString::from_utf16(message)', 'String::from_utf16_lossy(&message).into()', 'invalid-mode', 'failed: message', None),
    ('utf16-busy-lossy', 'JsValue::String(JsString::from_utf16(units))\n}', 'JsValue::String(String::from_utf16_lossy(&units).into())\n}', 'session-busy', 'UTF-16 busy status message', None),
    ('workspace-reservation-skipped', 'if !self.workspace_admissions.reserve(id, &run)', 'if false && !self.workspace_admissions.reserve(id, &run)', 'workspace-busy-before-sdk', '#813 workspace-busy-before-sdk: step 0 "observe": submit scenario must match', None),
    ('workspace-release-skipped', '.release(&workspace, self.run);', '.release(&workspace, &RunRef { session_key: "wrong".into(), project_key: None, run_id: "wrong".into() });', 'workspace-released-after-turn', '#813 workspace-released-after-turn: step 4 "second": submit scenario must match', None),
    ('cwd-update-skipped', 'self.host.set_session_cwd(&run.session_key, cwd)', 'Ok::<(), SubmitError>(())', 'workspace-cwd', '#813 workspace-cwd: step 0 "observe": submit scenario must match', None),
    ('timeout-disabled', 'tokio::time::sleep_until(started + Duration::from_millis(delay)).await;', 'tokio::time::sleep_until(started + Duration::from_millis(delay + 10000)).await;', 'timeout-active', '#813 timeout-active: step 2 "timeout": submit scenario must match', None),
    ('timeout-status-skipped', 'publisher.gateway_event(with_run(failure, &run));', 'let _ = failure;', 'timeout-active', '#813 timeout-active: step 2 "timeout": submit scenario must match', None),
    ('timeout-error-skipped', 'publisher.gateway_event(event);\n                    publisher.close_live();', 'let _ = event;\n                    publisher.close_live();', 'timeout-active', '#813 timeout-active: step 2 "timeout": submit scenario must match', None),
    ('timeout-abort-skipped', 'let _ = handle.abort(&format!("timeout:{}", display(&run.run_id)).into());', 'let _ = &handle;', 'timeout-active', '#813 timeout-active: step 2 "timeout": submit scenario must match', None),
    ('timeout-close-skipped', 'let close = self.owner.close(&run.session_key);', 'let close = std::future::ready(Ok::<(), crate::runtime::project::RuntimeError>(()));', 'timeout-active', '#813 timeout-active: step 2 "timeout": submit scenario must match', None),
    ('timeout-prepare-revived', 'if timed_out.load(Ordering::SeqCst) {\n            return Ok(());\n        }', 'if false && timed_out.load(Ordering::SeqCst) { return Ok(()); }', 'timeout-prepare-no-revival-live-consumer', '#813 timeout-prepare-no-revival-live-consumer: step 2 "prepare": submit scenario must match', None),
    ('deadline-skipped', '"operationDeadline",\n                deadline.as_str().into(),', '"wrongDeadline",\n                deadline.as_str().into(),', 'timeout-active', '#813 timeout-active: step 0 "observe": submit scenario must match', None),
    ('timeout-status-record-error-ignored', 'if self.host.record_failure(&run, &record).await.is_ok()', 'if self.host.record_failure(&run, &record).await.is_err()', 'timeout-status-record-failed', '#813 timeout-status-record-failed: step 2 "timeout": submit scenario must match', None),
    ('snapshot-pre-skipped', 'recorder.capture(snapshot, true).await?', 'recorder.capture(snapshot, false).await?', 'snapshot-success', '#813 snapshot-success: step 1 "accept": submit scenario must match', None),
    ('snapshot-pre-error-ignored', 'if result.state != SnapshotState::Committed', 'if false && result.state != SnapshotState::Committed', 'snapshot-pre-failed', '#813 snapshot-pre-failed: step 1 "accept": submit scenario must match', None),
    ('snapshot-post-skipped', 'let _ = recorder.capture(input, false).await;', 'let _ = input;', 'snapshot-agent-failed', '#813 snapshot-agent-failed: step 2 "terminal": submit scenario must match', None),
    ('snapshot-success-post-added', '!progress.committed || (progress.succeeded && !timed_out)', '!progress.committed', 'snapshot-success', '#813 snapshot-success: step 3 "end": submit scenario must match', None),
    ('snapshot-timeout-kind-wrong', '"timeout".into()\n            } else', '"unknown".into()\n            } else', 'timeout-snapshot', '#813 timeout-snapshot: step 2 "timeout": submit scenario must match', None),
    ('timeout-live-close-skipped', 'publisher.close_live();', 'let _ = &publisher;', 'timeout-active', '#813 timeout-active: step 2 "timeout": submit scenario must match', None),
    ('invalid-deadline-accepted', 'value.abs() > 8_640_000_000_000_000.0', 'value.abs() > f64::MAX', 'timeout-invalid-deadline', '#813 timeout-invalid-deadline: step 0 "observe": submit scenario must match', None),
    ('failureguard-model-observe-skipped', '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "turn_continued") && let Some(trip) = guard.observe(&event)', 'guard-model-trip', '#813 guard-model-trip: step 2 "terminal": submit scenario must match', None),
    ('snapshot-capability-refusal-skipped', 'if workspace.snapshots_required &&', 'if false && workspace.snapshots_required &&', 'invalid-mode', 'snapshot-recorder-unavailable: explicit capability refusal', 'snapshot-recorder-unavailable'),
    ('snapshot-cwd-refusal-skipped', 'return Err(SubmitError::new(\n                    "CAPABILITY_UNAVAILABLE",\n                    "Workspace snapshot cwd is unavailable.",\n                ));', '/* silently skip missing snapshot cwd */', 'invalid-mode', 'snapshot-cwd-unavailable: explicit capability refusal', 'snapshot-cwd-unavailable'),
    ('cwd-capability-error-ignored', 'let Err(error) = self.host.set_session_cwd(&run.session_key, cwd)', 'let Err(error) = self.host.set_session_cwd(&run.session_key, cwd).or(Ok::<(), SubmitError>(()))', 'invalid-mode', 'cwd-host-unavailable: explicit capability refusal', 'cwd-host-unavailable'),
    # Production-host mutants live in submit_host.rs; the lease guard's
    # exactly-once assertion lives in the unit tests there. The host lease
    # mutant skips the release outright: a mutation that would release
    # twice cannot be written against the FnOnce ownership type, so the
    # repeated-finish_pump counter assertion is the double-release guard.
    ('guard-config-not-provided', 'self.services\n            .run_policy()\n            .and_then(|policy| policy.get("failureGuard").cloned())', 'None', 'guard-model-trip', '#813 guard-model-trip: step 2 "terminal": submit scenario must match', None),
    ('host-lease-release-skipped', 'let release = self\n            .leases\n            .lock()\n            .unwrap_or_else(|poisoned| poisoned.into_inner())\n            .remove(&(run.session_key.clone(), run.run_id.clone()));', 'let release: Option<SubmitLeaseRelease> = None;', None, 'finish_pump_releases_a_lease_exactly_once', None),
    # opus P2-1: the host's own cwd capability refusal, distinct from the
    # submit.rs call-site mutant cwd-capability-error-ignored above.
    ('cwd-host-unavailable-ok', 'None => Err(SubmitError::new(\n                "CAPABILITY_UNAVAILABLE",\n                "Session cwd host capability is unavailable.",\n            )),', 'None => Ok(()),', 'invalid-mode', 'cwd-host-unavailable: explicit capability refusal', 'cwd-host-unavailable'),
    # DeepSeek P1-1: reverting either poison-tolerant lock to unwrap()
    # strands the lease after an unrelated panic.
    ('lease-poison-unwrap-finish', 'unwrap_or_else(|poisoned| poisoned.into_inner())\n            .remove(', 'unwrap()\n            .remove(', None, 'finish_pump_releases_a_lease_after_the_leases_mutex_is_poisoned', None),
    ('lease-poison-unwrap-prepare', 'unwrap_or_else(|poisoned| poisoned.into_inner())\n                    .insert(', 'unwrap()\n                    .insert(', None, 'finish_pump_releases_a_lease_after_the_leases_mutex_is_poisoned', None),
    # opus r2 P2-1: the new lease/after order assertion needs a mutant that
    # makes it red. Swapping the fake's record order flips the observed
    # sequence without touching production, so the assertion itself is the
    # only thing that can fail.
    # fix-r1 P2 (DeepSeek #747 r1): the tri-state lease assertion needs
    # paired negative controls for all three arms. Swapping the recorded
    # labels inverts the order assertion; skipping the acquisition and
    # inventing a lease hit the two presence arms.
    ('lease-order-records-swapped', 'fn host(&self, value: &str) {\n        self.host\n            .lock()\n            .expect("#748 submit fixture prerequisite")\n            .push(value.into());\n    }', 'fn host(&self, value: &str) {\n        let value = match value {\n            "lease_release" => "after",\n            "after" => "lease_release",\n            other => other,\n        };\n        self.host\n            .lock()\n            .expect("#748 submit fixture prerequisite")\n            .push(value.into());\n    }', None, None, None),
    # Paired negative control for the missing-lease arm: presenting
    # attachments but acquiring no lease must fail the tri-state.
    ('lease-acquisition-skipped', 'true => Some(self.services.resolve_attachments(input).await?),', 'true => None::<SubmitAttachmentLease>,', None, None, None),
    # Paired negative control for the no-lease arm: acquiring a lease for
    # an attachment-free case must fail the absence assertion.
    ('always-acquire-lease', 'false => None,', 'false => Some(self.services.resolve_attachments(input).await?),', None, None, None),
]
mutant_source = {'guard-config-not-provided': host_source,
                 'host-lease-release-skipped': host_source,
                 'cwd-host-unavailable-ok': host_source,
                 'lease-poison-unwrap-finish': host_source,
                 'lease-poison-unwrap-prepare': host_source,
                 'lease-acquisition-skipped': host_source,
                 'always-acquire-lease': host_source,
                 'lease-order-records-swapped': test_source}
# Every mutant names the test that must fail. Overrides replace the replay
# default where the lease windows have their own test.
mutant_test = {name: submit_replay for name, _, _, _, _, _ in mutants}
mutant_test.update({
    'snapshot-capability-refusal-skipped': capability_replay,
    'snapshot-cwd-refusal-skipped': capability_replay,
    'cwd-capability-error-ignored': capability_replay,
    'cwd-host-unavailable-ok': capability_replay,
    'utf16-mode-lossy': utf16_replay,
    'utf16-busy-lossy': utf16_replay,
    'host-lease-release-skipped': host_exactly_once,
    'lease-poison-unwrap-finish': host_poison,
    'lease-poison-unwrap-prepare': host_poison,
    'lease-order-records-swapped': lease_order,
    'lease-acquisition-skipped': lease_order,
    'always-acquire-lease': lease_order,
})
mutant_anchor = {
    # fix-r3 keeps the location anchors derived by ``unwrap_site``; fix-r4
    # only narrows the host-lease marker to the assertion's own message.
    'host-lease-release-skipped': ('message', 'a prepared lease must be released exactly once'),
    'lease-poison-unwrap-finish': ('location', None),
    'lease-poison-unwrap-prepare': ('location', None),
    'lease-order-records-swapped': ('message', 'lease_release must precede after_turn'),
    'lease-acquisition-skipped': ('message', 'attachments must record lease_release and after_turn'),
    'always-acquire-lease': ('message', 'lease_release recorded without an attachment lease'),
}


def build():
    oracle = json.loads((fixture / 'oracle.json').read_text())['cases']
    inventory = adapter.test_inventory(test_source, test) + adapter.test_inventory(
        host_source.with_name('submit_host_tests.rs'), host_test)
    entries = [adapter.Mutation(Case('baseline', test, '#1110 positive baseline',
                                    kind='positive', positive_tests=inventory))]
    for name, before, after, scenario, marker, capability in mutants:
        target = mutant_source.get(name, source)
        kind, anchor = mutant_anchor.get(name, ('message', marker))
        if kind == 'location':
            anchor = adapter.unwrap_site(rust, target, before, after, target.read_bytes())
        elif anchor.startswith('#813 '):
            adapter.validate_submit_marker(oracle, anchor)
            kind = 'assertion'
        binding = Case(name, mutant_test[name], anchor, kind=kind,
                       prelude=('poison the leases mutex mid-turn',) if kind == 'location' else (),
                       payload_pattern=adapter.legacy_payload(anchor) if kind == 'message' else '')
        entries.append(adapter.Mutation(binding, target, before, after, scenario, capability))
    adapter.check(entries, oracle)
    return entries, oracle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    try:
        entries, oracle = build()
        if args.check:
            print(f'turn-submit: {len(entries) - 1} mutations + 1 baseline; anchors OK')
            return 0
        if not args.output:
            parser.error('output is required unless --check')
        return adapter.run(entries, oracle, repo, args.output, args.selected, os.environ)
    except Exception as error:
        print(f'refused: {type(error).__name__}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
