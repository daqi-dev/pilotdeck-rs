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
from negative_panic import marker_kills_named_test as shared_marker_kills
from negative_panic import panic_evidence as panic_payload
from negative_process import mutating_sources, run_owned
from negative_selection import select

fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
repo = rust.parent
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
source = rust / 'pilotdeck-gateway/src/runtime/turn/submit.rs'
host_source = rust / 'pilotdeck-gateway/src/runtime/turn/submit_host.rs'
test_source = rust / 'pilotdeck-gateway/src/runtime/turn/submit_tests.rs'
# CI runs this against a commit. A sandbox without commit rights can pass the
# tree's `git stash create` object here; the mutating_sources baseline then
# still proves the mutated bytes and the restore target are the same tree.
baseline = os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')


def panic_evidence(raw, used_test):
    """fix-r1 P2: the marker must live in the named test's panic block.

    libtest prints ``thread '<test>' panicked at``; only output inside a
    block whose test name equals or extends ``used_test`` counts, so a mutant
    killed by a different test's assertion can never satisfy the anchor.
    fix-r3: every matching block counts, not just the first. libtest names a
    thread after the single test running on it, so a test that panics twice
    (the poison test's deliberate poisoning panic, then the production panic
    the mutant causes) owns both blocks; a helper thread's panic still has a
    different name and stays excluded.
    """
    evidence = []
    for segment in raw.split("thread '")[1:]:
        name = segment.split("'", 1)[0]
        if name == used_test or name.startswith(used_test + '::'):
            evidence.append(segment)
    return '\n'.join(evidence)


def marker_kills_named_test(raw, used_test, marker):
    """fix-r2 P0 (opus): the marker must appear inside the named test's own
    panic evidence. ``marker in raw`` let a marker that only another test's
    panic (or plain stdout) mentioned satisfy the anchor, judging a surviving
    mutant killed."""
    evidence = panic_evidence(raw, used_test)
    # #813 fix-r8: a watchdog/timeout payload means the test hung, not that the
    # target assertion failed.
    if any(prefix in evidence for prefix in ('#813 gateway watchdog:', '#813 timeout:')):
        return False
    if not marker or marker == 'assertion' or marker.startswith('assertion `left == right`'):
        return False
    # fix-r13: `<case>: step` matches every step of the case; it is a scenario
    # prefix, not the target assertion.
    if marker.endswith(': step'):
        return False
    return marker in evidence


def message_kills(raw, used_test, marker):
    """fix-r13: a ``#813`` marker is the complete target assertion message.

    The scenario replay prints ``#813 <case>: step <index> "<step>": submit
    scenario must match``. The old substring judge accepted the bare prefix
    ``<case>: step``, so a failure in step 1 also killed a mutant whose target
    is step 3 (host log ``snapshot-pre-skipped``). The shared judge pins the
    marker to the exact target and rejects hang payloads; non-``#813``
    capability/lease markers keep the payload-only substring rule.
    """
    if marker.endswith(': step'):
        return False
    if marker.startswith('#813 '):
        return shared_marker_kills(raw, used_test, marker, marker)
    return marker in panic_payload(raw, used_test)


def unwrap_site(anchor_source, before, original):
    """fix-r3: derive a ``location`` anchor from the original bytes.

    A poison-tolerant lock reverted to ``unwrap()`` panics at the start of the
    replaced span — ``unwrap()`` begins exactly where ``unwrap_or_else()``
    began — so the panic site is the span's own line:column. The frozen
    ``301:14``/``247:22`` anchors rotted when A7.2-pr6a-7 shifted both spans
    two lines down and the poison mutants turned INVALID; a derived anchor
    cannot rot, and it stays bound to the mutation itself rather than to a
    hand-copied line number."""
    assert before, 'a location anchor needs the mutation span'
    text = original.decode()
    assert text.count(before) == 1
    head = text[:text.index(before)]
    return f'{anchor_source.relative_to(rust)}:{head.count(chr(10)) + 1}:{len(head) - head.rfind(chr(10))}'


def anchor_self_test():
    """fix-r2 P0 self-test: a marker confined to another test's panic block
    must not count as evidence (the r1 comment promised this; ``marker in
    raw`` did not deliver). fix-r3: the multi-block rule is pinned too — the
    poison test panics twice, and only its second block carries the mutant's
    evidence."""
    marker = '#813 invalid-mode: step 0 "observe": submit scenario must match'
    used_test = 'runtime::turn::submit::tests::real_ts_submit_scenarios'
    stray = (
        "thread 'runtime::turn::submit::tests::real_ts_utf16_preflight' "
        "panicked at submit_tests.rs:1:\n" + marker + "\n"
    )
    real = (
        "thread 'runtime::turn::submit::tests::real_ts_submit_scenarios' "
        "panicked at submit_tests.rs:2:\n" + marker + "\n"
    )
    assert marker in stray, 'fixture: the stray log must contain the marker'
    assert not marker_kills_named_test(stray, used_test, marker), \
        'a marker in another test\'s panic block must be invalid evidence'
    assert marker_kills_named_test(stray + real, used_test, marker)
    # fix-r3: the real poison-test shape — the test's own deliberate panic
    # first, the mutant's production panic second. The marker must count in
    # any block under the test's thread name, not only in the first one.
    poison_test = 'runtime::turn::submit_host::tests::finish_pump_releases_a_lease_after_the_leases_mutex_is_poisoned'
    poison_log = (
        "thread '" + poison_test + "' (285708320) panicked at "
        "pilotdeck-gateway/src/runtime/turn/submit_host_tests.rs:328:9:\n"
        "poison the leases mutex mid-turn\n"
        "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n"
        "thread '" + poison_test + "' (285708320) panicked at "
        "pilotdeck-gateway/src/runtime/turn/submit_host.rs:303:14:\n"
        "called `Result::unwrap()` on an `Err` value: PoisonError { .. }\n"
    )
    deliberate = poison_log[:poison_log.index("\nthread '")]
    assert not marker_kills_named_test(deliberate, poison_test, 'PoisonError'), \
        'the deliberate poison panic alone is not the mutant\'s evidence'
    assert marker_kills_named_test(poison_log, poison_test, 'PoisonError'), \
        'the production panic after the deliberate one must count as evidence'
    # A helper thread panicking with the marker is still not the test's own
    # evidence: libtest names only the test thread after the test.
    helper = (
        "thread 'tokio-runtime-worker' (285708321) panicked at submit.rs:1:\n"
        'PoisonError\n'
    )
    assert not marker_kills_named_test(helper, poison_test, 'PoisonError'), \
        'a helper thread\'s panic block must not count as the test\'s evidence'
    # fix-r13: same case, different step. These are host-run libtest logs, not
    # synthetic ones: snapshot-pre-skipped dies at step 1 "accept" and
    # snapshot-success-post-added dies at step 3 "end". The old bare prefix
    # `<case>: step` was in both payloads, so each log killed both mutants.
    recorded = fixture / 'real-logs'
    accept = ('#813 snapshot-success: step 1 "accept": '
              'submit scenario must match')
    end = '#813 snapshot-success: step 3 "end": submit scenario must match'
    for log, own, other in (('snapshot-pre-skipped', accept, end),
                            ('snapshot-success-post-added', end, accept)):
        raw = (recorded / f'{log}.log').read_text()
        assert panic_evidence(raw, used_test), f'{log}: fixture has no payload'
        assert message_kills(raw, used_test, own), \
            f'{log}: own step must kill'
        assert not message_kills(raw, used_test, other), \
            f'{log}: the other step of the same case must not kill'
        assert not message_kills(raw, used_test, 'snapshot-success: step'), \
            f'{log}: a bare scenario prefix is not evidence'
    print('anchor self-test: PASS', flush=True)


anchor_self_test()

with mutating_sources(repo, [source, host_source, test_source], baseline=baseline) as snapshots:
    original = snapshots[source]
    host_original = snapshots[host_source]
    test_original = snapshots[test_source]
    originals = {source: original, host_source: host_original, test_source: test_original}
    env = dict(os.environ, CARGO_NET_OFFLINE='true')
    # Accept the tree-local target and this workspace's shared
    # `_build/cargo-target`; resolve both sides so a symlinked src/target still
    # matches either spelling.
    target_dir = Path(env['CARGO_TARGET_DIR']).resolve()
    assert target_dir in {(rust / 'target').resolve(), (repo / '_build/cargo-target').resolve()}
    assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
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
    # #813 fix-r8 P3: every non-location mutant must carry a scenario/assertion
    # marker; a generic diagnostic may not re-enter this table silently.
    for name, _, _, _, marker, _ in mutants:
        if marker is not None:
            assert marker != 'assertion' and not marker.startswith('assertion `left == right`'), \
                (name, 'generic assertion diagnostic is not evidence')
            # fix-r13: `<case>: step` names the scenario, not the assertion.
            # It cannot distinguish step 1 from step 3 of the same case.
            assert not marker.endswith(': step'), \
                (name, 'bare scenario prefix is not evidence')
    # Anchor kinds: `message` is a key substring of the assertion/panic message
    # for the named test; `location` is the exact panic file:line:column of
    # the reverted `unwrap()` itself, derived by ``unwrap_site`` from the
    # span the mutant replaces (fix-r3: a frozen line number rots with every
    # shift above the span). The generic "some assertion happened somewhere"
    # check is deliberately gone.
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

    def failed_tests(text):
        # libtest's failure list is the only place a test is declared failed;
        # a stray panic in a sibling test must not count.
        return {line[4:].strip() for line in text.splitlines()
                if line.startswith('    runtime::turn::')}

    def panic_blocks(text, test_name):
        # The named test's own panic segments, header retained. Only the
        # `location` anchors need the `panicked at <path>:<line>:<col>` header;
        # `message` anchors use the shared payload-only evidence (#813 fix-r4)
        # so a path, thread name or backtrace cannot satisfy them.
        #
        # Merge note: main's #747 fix-r3 `panic_evidence` also collects every
        # block owned by the named test; matching the exact name here matches
        # the shared helper's rule (#813 fix-r2), so a nested test's panic
        # cannot stand in for the named test.
        return [part for part in text.split("thread '")[1:]
                if part.startswith(f"{test_name}'")]

    summary = []
    # fix-r13: finish every selected mutant before reporting, so one bad
    # control cannot hide the rest (the r12 executor still stopped at the
    # first non-KILLED row).
    failures = []
    baseline_ok = True
    try:
        for name, before, _, _, _, _ in mutants:
            anchor_source = mutant_source.get(name, source)
            anchor_text = originals[anchor_source].decode()
            assert anchor_text.count(before) == 1, (name, 'anchor is not unique')
        (output / 'original-sha256.json').write_text(json.dumps({
            str(path.relative_to(repo)): hashlib.sha256(data).hexdigest()
            for path, data in originals.items()}, indent=2)+'\n')
        # #813 fix-r4: a subset keeps the unmutated baseline row. fix-r13: a
        # red baseline is recorded and the remaining rows still run; the exit
        # code is decided after every selected row has a verdict.
        selected = select([('baseline', None, None, None, None, None)] + mutants,
                          sys.argv[2:])
        for name, before, after, case, marker, capability in selected:
            assert all(path.read_bytes() == data for path, data in originals.items()), 'source changed outside this run'
            target = mutant_source.get(name, source)
            target_original = originals[target]
            used_test = mutant_test.get(name, test)
            anchor_kind, anchor = mutant_anchor.get(name, ('message', marker))
            if anchor_kind == 'location':
                anchor = unwrap_site(target, before, target_original)
            # A marker of None without an explicit anchor entry would silently
            # judge every run INVALID after the cargo run; refuse it up front
            # so the control fails closed before any compile.
            assert name == 'baseline' or anchor, \
                (name, 'marker is None and no anchor entry names the evidence')
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

            if name == 'baseline':
                valid = not timed_out and returncode == 0 and 'test result: ok. 4 passed' in raw and 'test result: ok. 10 passed' in raw
            else:
                # The named test must be the one that panicked, and the
                # evidence must live in that test's own output. Product panics
                # (a poison-unwrap mutant) are evidence alongside assertion
                # diffs; a marker elsewhere in the log is not.
                #
                # location: main's #747 fix-r3 derives the anchor from the
                # mutation span via unwrap_site(), so the reverting unwrap()'s
                # real file:line:column is matched even after source shifts.
                # #813 fix-r6 keeps that header match inside the named test's
                # own panic blocks only, never a sibling test's block.
                #
                # message: #813 fix-r4/r6 use the shared payload-only
                # extraction, so a marker that only appears in a thread name,
                # source path or backtrace cannot satisfy the anchor.
                if anchor_kind == 'location':
                    anchored = any(f'panicked at {anchor}:' in block
                                   for block in panic_blocks(raw, used_test))
                else:
                    anchored = message_kills(raw, used_test, anchor)
                valid = (not timed_out and returncode == 101 and 'test result: FAILED' in raw
                         and 'could not compile' not in raw and used_test in failed_tests(raw)
                         and anchored)
            if name == 'baseline':
                baseline_ok = valid
                status = 'PASS' if valid else 'INVALID'
                reason = '' if valid else 'baseline is not green'
            elif not baseline_ok:
                status, reason = 'INVALID', 'baseline is not green'
            elif valid:
                status, reason = 'KILLED', 'named test failed at its bound assertion'
            elif (not timed_out and returncode == 0 and 'test result: ok' in raw
                  and 'could not compile' not in raw
                  and f'test {used_test} ... ok' in raw):
                status, reason = 'SURVIVED', 'bound test still passed'
            else:
                status, reason = 'INVALID', 'missing bound runtime failure'
            (output / f'{name}.log').write_text('COMMAND '+' '.join(command)+'\n'+slots.stdout+slots.stderr+raw+f'\nexit={returncode}\n')
            summary.append(dict(name=name, case=case, before=before, after=after, exit=returncode, marker=marker, test=used_test, anchor=anchor, anchorKind=anchor_kind, status=status, reason=reason))
            (output/'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2)+'\n')
            print(f'{name}: {status} exit={returncode}', flush=True)
            if status not in ('PASS', 'KILLED'):
                failures.append(name)
    finally:
        for path, data in originals.items():
            path.write_bytes(data)
        for path, data in originals.items():
            assert path.read_bytes() == data
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'PASS')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()),
          flush=True)
    if failures:
        print('non-KILLED controls: ' + ', '.join(failures), flush=True)
        raise SystemExit(1)
    print(f'{len(summary)} controls checked; original bytes restored', flush=True)
