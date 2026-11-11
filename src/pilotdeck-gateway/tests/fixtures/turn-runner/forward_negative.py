#!/usr/bin/env python3
"""Bounded runtime controls; commit first, caller owns CARGO_TARGET_DIR."""
import json, os, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, bind_module_file, declares_test, run_cases
from _negkit import non_head
from _negative_common import mutate
root=Path(__file__).resolve().parents[5]
base=root/'src/pilotdeck-gateway/src/runtime/turn'
# Whole-file occurrences for the static preflight; mutate() still requires
# exactly one occurrence inside the named Rust function.
ANCHOR_COUNTS = {'status-nullish': 3, 'host-callback-cancel': 2, 'pump-cleanup': 2}
# id, source, before, after, function scope, full test name, assertion anchor
test_prefix = 'runtime::turn::live::stream_tests::forwarding::'
# #1770 fix-r1: the gates shared-tail injection runs in all three cells
# (reject/cancel/open), and each cell's own test must kill it on that cell's
# #1770 assertion, so the one mutation is one row per bound test -- the
# suite's multi-Case form (negative_suite case_labels gives every Case its
# own evidence stem). The shared anchor text lives once, here.
gates_prefix = 'runtime::turn::live::stream_tests::gates_tests::'
gates_wait = '    timeout(Duration::from_secs(70), async {\n        while session\n            .state\n            .lock()\n            .expect("#855: pump gate setup/read/cleanup must succeed")\n            .active\n            .is_some()\n            || runtime.lease_count() != 0\n            || handler.disposed.load(Ordering::SeqCst) == 0\n        {\n            tokio::task::yield_now().await;\n        }\n    })\n    .await\n    .expect("#855: owned cleanup must release slot, lease and handler");'
gates_compensate = gates_wait + '\n    let _ = crate::storage::TranscriptStore::append(\n        &*store,\n        &key,\n        &crate::metadata::object([\n            ("type", JsValue::String("turn_result".into())),\n            ("result", crate::metadata::object([("type", JsValue::String("error".into()))])),\n        ]),\n    );'
cases = [
    ('cleanup-skip-lease', base / 'forward_tests.rs',
     '            || runtime.lease_count() != 0', '            || false',
     None, test_prefix + 'forward_status_drop', '#741h cleanup observer waits for lease'),
    ('cleanup-late-deadline', base / 'forward_tests.rs',
     'timeout(Duration::from_secs(10), async {', 'timeout(Duration::from_secs(20), async {',
     None, test_prefix + 'forward_status_drop', '#741h cleanup observer reaches 10-second deadline'),
    ('wire-origin', base / 'live.rs',
     '(e, true)', '(e, false)',
     'forward', test_prefix + 'forward_status_explicit', '#741h forward event order/payload'),
    ('host-origin', base / 'live.rs',
     '(JsValue::Object(e), false)', '(JsValue::Object(e), true)',
     'forward', test_prefix + 'forward_host_status', '#741h forward event order/payload'),
    ('loop-failed-dispatch', base / 'live.rs',
     'kind.equals_str("turn_failed")', 'kind.equals_str("never_failed")',
     'record_event', test_prefix + 'forward_failed_twice', '#741h forward event order/payload'),
    ('failure-raw-fields', base / 'live.rs',
     '*events.last_mut().expect("loop failure") = event;', 'let _ = event;',
     'settle_recording', test_prefix + 'forward_failed_twice', '#741h forward event order/payload'),
    ('artifact-filter', base / 'live.rs',
     'event.get("type") == Some(&"file_artifacts".into())', 'event.get("type") == Some(&"never_artifacts".into())',
     None, test_prefix + 'forward_artifacts', '#741h forward event order/payload'),
    ('status-callback', base / 'live.rs',
     'wire && kind.equals_str("agent_status")', 'false && wire && kind.equals_str("agent_status")',
     'record_event', test_prefix + 'forward_status_explicit', '#741h forward event order/payload'),
    ('status-write', base / 'runner.rs',
     'r.writer().record_agent_status_message(s, t, &status)?', '{ let _ = (r,s,t,status); () }',
     'loop_status', test_prefix + 'forward_status_explicit', '#741h forward durable callback/complete'),
    ('status-kind', base / 'runner.rs',
     'kind == "error".into()', 'kind == "status".into()',
     'loop_status', test_prefix + 'forward_status_explicit', '#741h forward event order/payload'),
    ('status-exclusion', base / 'runner.rs',
     'name != "turn_failed".into()', 'true',
     'loop_status', test_prefix + 'forward_status_turn_failed', '#741h forward event order/payload'),
    ('status-visible', base / 'runner.rs',
     'name != "turn_failed".into()', 'name != "turn_failed".into() && detail.and_then(|d| d.get("visible")) != Some(&JsValue::Bool(false))',
     'loop_status', test_prefix + 'forward_status_hidden', '#741h forward event order/payload'),
    ('status-severity', base / 'runner.rs',
     'Some(&"error".into())', 'Some(&"wrong".into())',
     'loop_status', test_prefix + 'forward_status_severity', '#741h forward event order/payload'),
    ('implicit-max_budget_reached', base / 'runner.rs',
     '"max_budget_reached"', '"disabled_max_budget_reached"',
     'loop_status', test_prefix + 'forward_status_implicit_max_budget_reached', '#741h forward event order/payload'),
    ('implicit-task_budget_reached', base / 'runner.rs',
     '"task_budget_reached"', '"disabled_task_budget_reached"',
     'loop_status', test_prefix + 'forward_status_implicit_task_budget_reached', '#741h forward event order/payload'),
    ('implicit-model_request_failed', base / 'runner.rs',
     '"model_request_failed"', '"disabled_model_request_failed"',
     'loop_status', test_prefix + 'forward_status_implicit_model_request_failed', '#741h forward event order/payload'),
    ('implicit-tool_call_recovery_exhausted', base / 'runner.rs',
     '"tool_call_recovery_exhausted"', '"disabled_tool_call_recovery_exhausted"',
     'loop_status', test_prefix + 'forward_status_implicit_tool_call_recovery_exhausted', '#741h forward event order/payload'),
    ('implicit-max_turns_reached', base / 'runner.rs',
     '"max_turns_reached"', '"disabled_max_turns_reached"',
     'loop_status', test_prefix + 'forward_status_implicit_max_turns_reached', '#741h forward event order/payload'),
    ('implicit-max_output_recovery_exhausted', base / 'runner.rs',
     '"max_output_recovery_exhausted"', '"disabled_max_output_recovery_exhausted"',
     'loop_status', test_prefix + 'forward_status_implicit_max_output_recovery_exhausted', '#741h forward event order/payload'),
    ('implicit-empty_response', base / 'runner.rs',
     '"empty_response"', '"disabled_empty_response"',
     'loop_status', test_prefix + 'forward_status_implicit_empty_response', '#741h forward event order/payload'),
    ('implicit-content_filter', base / 'runner.rs',
     '"content_filter"', '"disabled_content_filter"',
     'loop_status', test_prefix + 'forward_status_implicit_content_filter', '#741h forward event order/payload'),
    ('implicit-unknown_finish_reason', base / 'runner.rs',
     '"unknown_finish_reason"', '"disabled_unknown_finish_reason"',
     'loop_status', test_prefix + 'forward_status_implicit_unknown_finish_reason', '#741h forward event order/payload'),
    ('status-nullish', base / 'runner.rs',
     'JsValue::Null | JsValue::Undefined', 'JsValue::Undefined',
     'loop_status', test_prefix + 'forward_status_nullish', '#741h forward event order/payload'),
    ('status-text', base / 'runner.rs',
     'present(event.get("text"))', 'None::<JsValue>',
     'loop_status', test_prefix + 'forward_status_text', '#741h forward durable callback/complete'),
    ('status-detail-text', base / 'runner.rs',
     'matches!(v, JsValue::String(_))', '{ let _ = v; false }',
     'loop_status', test_prefix + 'forward_status_severity', '#741h forward durable callback/complete'),
    ('catch-unconditional', base / 'runner.rs',
     'let status = self.status(&error).await;', 'let status = if self.failure_status {self.event("skipped",[])} else {self.status(&error).await};',
     'fail_inner', test_prefix + 'forward_catch_complete_once', '#741h forward event order/payload'),
    ('receipt-required', base / 'live.rs',
     'if record_status', 'if false && record_status',
     'forward', test_prefix + 'forward_status_receipt_failure', ['#741h status awaits durable receipt', '#741h status callback cannot release successor']),
    ('malformed-failure', base / 'runner.rs',
     'matches!(error, JsValue::Null | JsValue::Undefined)', 'false',
     'loop_failed', test_prefix + 'forward_malformed_null_first', '#741h forward event order/payload'),
    ('host-callback-cancel', base / 'live.rs',
     'cancel.cancel(None);', 'let _ = &cancel;',
     'next', test_prefix + 'forward_malformed_host', '#741h LiveTurn must make progress'),
    ('pump-cleanup', base / 'live.rs',
     'let _ = execution.await;', 'let _ = &mut execution;',
     'with_pump', test_prefix + 'forward_pump_error_cleanup', '#741h pump error must await execution cleanup'),
    ('kind-falsy', base / 'runner.rs',
     'present(event.get("kind"))', 'present(event.get("kind")).filter(super::submit::truthy)',
     'loop_status', test_prefix + 'forward_status_kind_false', '#741h forward event order/payload'),
    ('text-falsy', base / 'runner.rs',
     'present(event.get("text"))', 'present(event.get("text")).filter(super::submit::truthy)',
     'loop_status', test_prefix + 'forward_status_text_false', '#741h forward durable callback/complete'),
    # Omitting null from the temporary status is equivalent: the writer drops
    # both null and missing detail. Wrap the input instead, so a nonempty object
    # incorrectly reaches the transcript; keep the original test/assertion.
    ('detail-null', base / 'runner.rs',
     'status.insert("detail".into(), detail.clone());',
     'status.insert("detail".into(), if *detail == JsValue::Null { object([("value", detail.clone())]) } else { detail.clone() });',
     'loop_status', test_prefix + 'forward_status_detail_null', '#741h forward durable callback/complete'),
    ('status-pending-retained', base / 'live.rs',
     'if self.pending_event.is_some() {', 'if self.pending_event.take().is_some() {',
     None, test_prefix + 'forward_status_cancel', '#741h status predecessor still pending'),
    # #996-pr2 fix-r5: the gates-release-pending-slot switch made this let a
    # multi-line if/else, which the scoped mutate() single-line rule rejects.
    # Anchor the else branch alone; replacing it with a typed None keeps the
    # same mutant (the io worker holds no owner Arc).
    ('status-worker-slot', base / 'runner.rs',
     'self.owner.clone()', 'Option::<Arc<dyn Send + Sync>>::None',
     'io', test_prefix + 'forward_status_drop', '#741h status dropped worker holds slot'),
    ('status-worker-lease', base / 'runner.rs',
     'let result = write(&recorder, &session, &turn);\n            drop(owner);\n            drop(lease);', 'drop(lease);\n            let result = write(&recorder, &session, &turn);\n            drop(owner);',
     None, test_prefix + 'forward_status_drop', '#741h status dropped worker holds lease'),
    # #1219 / A7.2-LIVE-DROP-COMPLETION-001-R2: the failing status worker must
    # not append a durable turn_result. Persisting one from the same blocking
    # worker must die on the case's own ordering assertion: the report is
    # recorded by OrderedStore before its TS-order panic, because that panic
    # lands in a JoinHandle nobody awaits (see forward_tests.rs OrderedStore).
    ('status-fail-completes', base / 'runner.rs',
     'let result = write(&recorder, &session, &turn);\n            drop(owner);\n            drop(lease);\n            result',
     'let result = write(&recorder, &session, &turn);\n            if result.is_err() {\n                let _ = recorder.writer().record_turn_result(&session, &turn, crate::metadata::object([("type", JsValue::String("error".into()))]));\n            }\n            drop(owner);\n            drop(lease);\n            result',
     None, test_prefix + 'forward_status_fail_drop', '#1219 [status-fail-drop]: no out-of-order turn_result from the failed worker'),
    # #1219 / A7.2-LIVE-DROP-COMPLETION-001-R2: the failing status worker still
    # owns the turn's lease until its write resolves. Releasing it before the
    # write must die on that case's own assertion.
    ('status-fail-lease-early', base / 'runner.rs',
     'let result = write(&recorder, &session, &turn);\n            drop(owner);\n            drop(lease);',
     'drop(lease);\n            let result = write(&recorder, &session, &turn);\n            drop(owner);',
     None, test_prefix + 'forward_status_fail_drop', '#1219 dropped owner leaves the failing worker its lease'),
    # #1219 / A7.2-LIVE-DROP-COMPLETION-001-R2: likewise for the session slot.
    ('status-fail-slot-early', base / 'runner.rs',
     'let result = write(&recorder, &session, &turn);\n            drop(owner);\n            drop(lease);\n            result',
     'drop(owner);\n            let result = write(&recorder, &session, &turn);\n            drop(lease);\n            result',
     None, test_prefix + 'forward_status_fail_drop', '#1219 dropped owner leaves the failing worker its slot'),
    # #1219 / A7.2-LIVE-DROP-COMPLETION-001-R2: retrying the failed status write
    # must die on the attempt counter (Rust attempts 1, TS 2).
    ('status-fail-retry', base / 'runner.rs',
     'r.writer().record_agent_status_message(s, t, &status)?',
     'r.writer().record_agent_status_message(s, t, &status).or_else(|_| r.writer().record_agent_status_message(s, t, &status))?',
     None, test_prefix + 'forward_status_fail_drop', '#1219 failed status write is attempted once'),
    # #1219 / A7.2-LIVE-DROP-COMPLETION-001-R2: leaking the lease must die on
    # the cleanup observer's deadline.
    ('status-fail-lease-hold', base / 'runner.rs',
     'let result = write(&recorder, &session, &turn);\n            drop(owner);\n            drop(lease);\n            result',
     'let result = write(&recorder, &session, &turn);\n            drop(owner);\n            std::mem::forget(lease);\n            result',
     None, test_prefix + 'forward_status_fail_drop', '#741h cleanup releases slot and lease within 10 seconds'),
    # #1219 / A7.2-LIVE-DROP-COMPLETION-001-R2: a fault that no longer fires
    # degrades the case to its success variant; the transcript guard must die.
    ('status-fail-inject-off', base / 'live_stream_tests.rs',
     'if self.attempts.load(Ordering::SeqCst) <= self.fail {',
     'if false && self.attempts.load(Ordering::SeqCst) <= self.fail {',
     None, test_prefix + 'forward_status_fail_drop', '#1219 [status-fail-drop]: the failing write must not reach the transcript'),
    # #1219 r3: the same counters must guard the R1 cells too. A turn_result
    # appended from the blocked worker on the *successful* drop path dies in
    # OrderedStore before `RecordingStore::append`, so `completes` cannot move;
    # the observable dimension there is the ordering violation the worker would
    # otherwise hide in its unawaited JoinHandle. The mutation is narrow on
    # purpose: `io()` is shared by every durable write, so mutating it (an
    # earlier draft anchored on `let result = write(...)`) rewrites the very
    # first append and the case panics before the status gate — the loop status
    # write below is the one this cell blocks on.
    ('status-drop-order', base / 'runner.rs',
     '.io(move |r, s, t| Ok(r.writer().record_agent_status_message(s, t, &status)?))',
     '.io(move |r, s, t| {\n                let written = r.writer().record_agent_status_message(s, t, &status)?;\n                let _ = r.writer().record_turn_result(s, t, crate::metadata::object([("type", JsValue::String("error".into()))]));\n                Ok(written)\n            })',
     None, test_prefix + 'forward_status_drop', '#1219 [status-drop/cancel]: no out-of-order turn_result from the blocked worker'),
    # #1770 item 4: bind the same ordering dimension to the cancel cell. The
    # plain copy of status-drop-order is INVALID there: the status write
    # succeeds, so the injected turn_result panics the blocking worker, io
    # returns a JoinError, the catch replays the status write (attempts=2)
    # and the case dies first on its replay assertion. Swallow the ordered
    # panic instead — the report is still counted by OrderedStore before it
    # panics — so the flow stays byte-identical and only the bound
    # order_violations assertion can fail.
    ('status-drop-order-cancel', base / 'runner.rs',
     '.io(move |r, s, t| Ok(r.writer().record_agent_status_message(s, t, &status)?))',
     '.io(move |r, s, t| {\n                let result = r.writer().record_agent_status_message(s, t, &status);\n                if result.is_ok() {\n                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {\n                        let _ = r.writer().record_turn_result(s, t, crate::metadata::object([("type", JsValue::String("error".into()))]));\n                    }));\n                }\n                result.map_err(RecorderError::from)\n            })',
     None, test_prefix + 'forward_status_cancel', '#1219 [status-drop/cancel]: no out-of-order turn_result from the blocked worker'),
    # #1770 item 2: give forward_real_status_write_error's two #1219
    # assertions their own controls. Dropping the catch path's durable append
    # must trip the completes assertion alone; events, error text and the
    # attempt counter ride the unchanged status writes.
    ('status-write-error-completes', base / 'runner.rs',
     '        self.io(move |r, s, t| {\n            if crate::mutants::on("gates-skip-failure-complete") {\n                drop(written);\n                Ok(())\n            } else {\n                r.complete_turn(s, t, written)\n            }\n        })\n        .await?;',
     '        self.io(move |r, s, t| {\n            let _ = (r, s, t, written);\n            Ok(())\n        })\n        .await?;',
     None, test_prefix + 'forward_real_status_write_error', '#1219 [status-write-error]: the catch path still appends one durable turn_result'),
    # #1770 item 2, ordering dimension: an out-of-order turn_result from the
    # failing loop-status worker must be observable in this case too. Inject
    # it only on the failing write and swallow OrderedStore's ordering panic,
    # so the original error (events[5], attempts=2) and the catch's own
    # durable completion stay intact and only order_violations moves.
    ('status-write-error-order', base / 'runner.rs',
     '.io(move |r, s, t| Ok(r.writer().record_agent_status_message(s, t, &status)?))',
     '.io(move |r, s, t| {\n                let result = r.writer().record_agent_status_message(s, t, &status);\n                if result.is_err() {\n                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {\n                        let _ = r.writer().record_turn_result(s, t, crate::metadata::object([("type", JsValue::String("error".into()))]));\n                    }));\n                }\n                result.map_err(RecorderError::from)\n            })',
     None, test_prefix + 'forward_real_status_write_error', '#1219 [status-write-error]: no out-of-order turn_result from the worker'),
    # #1770 item 1: the post-cleanup durable re-check in the gates pump
    # cells. A compensating turn_result appended after owned cleanup — the
    # exact window the assertion guards — must trip the re-check alone.
    # Inject it straight into the store right after the cleanup wait: an
    # earlier draft wrapped the factory's handler and appended from its
    # dispose, but the TurnScope drops when the execution future completes,
    # before the terminal is even delivered to the consumer (delivery rides
    # the pump's 500 ms tick), so the write raced the cell's in-band #1219
    # snapshot and lost determinism (both a direct and a 200 ms-delayed
    # variant fired the in-band assertion first; see neg-s1b/neg-s1c).
    # Appending after the wait — cleanup already observed done — is
    # deterministic on both sides: the in-band snapshot is long past, and
    # the re-check is the next reader of the counter.
    # #1770 fix-r1: all three cells run this shared tail, so all three
    # bind it (reject/cancel/open, same injection, one row each); a
    # compensating append introduced on any one path dies in that cell's
    # own test on its own #1770 message.
    ('gates-cleanup-compensates', base / 'gates_tests.rs',
     gates_wait, gates_compensate,
     None, gates_prefix + 'gates_pump_reject_never_starts_the_pump',
     '#1770 [reject]: owned cleanup must not append a compensating turn_result'),
    ('gates-cleanup-compensates', base / 'gates_tests.rs',
     gates_wait, gates_compensate,
     None, gates_prefix + 'gates_pump_cancel_never_starts_the_pump',
     '#1770 [cancel]: owned cleanup must not append a compensating turn_result'),
    ('gates-cleanup-compensates', base / 'gates_tests.rs',
     gates_wait, gates_compensate,
     None, gates_prefix + 'gates_pump_open_pins_the_start_order',
     '#1770 [open]: owned cleanup must not append a compensating turn_result'),
]

def audit():
    # #1890: bind every row to the file that declares its module and accept the
    # `case!` table form; the old `short in forward_tests.rs` scan had no
    # module or level check at all. #1770: the gates row binds through the
    # sibling gates_tests module (declared as a plain #[tokio::test] fn, which
    # declares_test's fn form covers) instead of the forwarding table.
    modules = ((test_prefix, base / 'forward_tests.rs'),
               ('runtime::turn::live::stream_tests::gates_tests::', base / 'gates_tests.rs'))
    # #2303 (条 4): the session-factory / session-drop-release audit shape --
    # every row is checked and accounted (CHECKED or INVALID with its
    # reason), so a scanner refusal (ValueError) becomes one named INVALID
    # row instead of a raw traceback on the first failure.
    rows = []
    for name,path,old,new,scope,test,anchor in cases:
        try:
            mutate(path.read_text(),scope,[(old,new)])
            file, short = bind_module_file(test, modules)
            if not declares_test(file.read_text(), short, 'case!'):
                raise ValueError('bound full test not found: ' + test)
            rows.append(dict(mutation=name, test=test, assertion=anchor, status='CHECKED'))
        except Exception as error:
            rows.append(dict(mutation=name, test=test, assertion=anchor, status='INVALID',
                             reason=f'{type(error).__name__}: {error}'))
    return rows
if sys.argv[1:] == ['--audit']:
    audited = audit()
    print(json.dumps(audited,indent=2))
    raise SystemExit(int(any(row['status'] != 'CHECKED' for row in audited)))
env=os.environ.copy()
if not env.get('CARGO_TARGET_DIR'): raise SystemExit('CARGO_TARGET_DIR must be supplied by caller')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',CARGO_PROFILE_TEST_DEBUG='line-tables-only',CARGO_TERM_QUIET='false')
out=Path(sys.argv[1]);out.mkdir(parents=True,exist_ok=False)
selected=cases[:2] if sys.argv[2:]==['--sample'] else cases
if sys.argv[2:] and sys.argv[2:] != ['--sample']:
    names=set(sys.argv[2:])
    unknown=names-{row[0] for row in cases}
    if unknown: raise SystemExit(f'unknown controls: {sorted(unknown)}')
    selected=[row for row in cases if row[0] in names]
# Package cache contention is not test execution. Match the shared suite
# build allowance; runtime progress/cleanup assertions retain their own bounds.
# #1540: a green run against a pinned non-HEAD baseline is NON-HOST EVIDENCE
# (the host gate only trusts HEAD), so it exits 42 instead of 0.
baseline_source,baseline,baseline_sha=non_head.gate(root)
raise SystemExit(non_head.non_head_exit(baseline_source,baseline_sha,
 code=run_cases(root,
    [Case(n,p,old,new,test,anchor,scope) for n,p,old,new,scope,test,anchor in selected],
    out,env,mutate,timeout=900,baseline=baseline),evidence=out))
